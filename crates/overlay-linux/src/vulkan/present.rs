pub unsafe extern "system" fn vkQueuePresentKHR(queue: vk::Queue, info: *const vk::PresentInfoKHR<'_>) -> vk::Result {
    let mut registry = REGISTRY.lock();
    let Some(device_handle) = registry.queue_devices.get(&queue).copied() else { return vk::Result::ERROR_DEVICE_LOST; };
    let Registry { devices, instances, swaps: tracked_swaps, .. } = &mut *registry;
    let Some(device_record) = devices.get(&device_handle) else { return vk::Result::ERROR_DEVICE_LOST; };
    let Some(present) = device_record.present else { return vk::Result::ERROR_EXTENSION_NOT_PRESENT; };
    if info.is_null() { drop(registry); return present(queue, info); }
    let original = &*info;
    // Extension-specific damage, device masks and ownership are not rewritten.
    // Preserve their exact original semantics instead of composing incorrectly.
    if !original.p_next.is_null() || original.swapchain_count == 0
        || original.swapchain_count as usize > MAX_SWAPCHAINS
        || original.wait_semaphore_count as usize > MAX_SWAPCHAINS
        || original.p_swapchains.is_null() || original.p_image_indices.is_null()
        || (original.wait_semaphore_count != 0 && original.p_wait_semaphores.is_null()) {
        drop(registry); return present(queue, info);
    }
    let Some(family) = device_record.queues.get(&queue).copied() else { drop(registry); return present(queue, info); };
    let device = &device_record.device;
    let physical = device_record.physical;
    let loader_data = device_record.loader_data;
    let instance = &instances[&device_record.instance_handle].instance;
    let swaps = slice::from_raw_parts(original.p_swapchains, original.swapchain_count as usize);
    let indices = slice::from_raw_parts(original.p_image_indices, original.swapchain_count as usize);
    let waits = if original.wait_semaphore_count == 0 { &[][..] } else {
        slice::from_raw_parts(original.p_wait_semaphores, original.wait_semaphore_count as usize)
    };
    let stages = [vk::PipelineStageFlags::ALL_COMMANDS; MAX_SWAPCHAINS];
    let mut completes = [vk::Semaphore::null(); MAX_SWAPCHAINS];
    let mut count = 0;
    for (&swapchain, &image_index) in swaps.iter().zip(indices) {
        let Some(swap) = tracked_swaps.get_mut(&swapchain) else { continue; };
        let (focused, refresh_hz) = crate::sdl::surface_status(swap.input.window)
            .unwrap_or_else(|| (swap.input.backend == 1 && crate::xfocus::focused(swap.input.window), 0.0));
        let visible = {
            let mut client = swap.input.client.lock();
            if !swap.input.active.load(Ordering::Acquire) { continue; }
            client.present(swap.extent.width, swap.extent.height, refresh_hz, focused);
            client.visible()
        };
        if swap.device != device_handle || swap.unsupported || !visible
            || image_index as usize >= swap.images.len()
            || (swap.sharing_mode == vk::SharingMode::CONCURRENT && !swap.queue_families.contains(&family)) { continue; }
        if swap.resources.is_none() {
            match gpu::Resources::create(instance, device, physical, swap.format, swap.extent, &swap.images, queue, family, loader_data) {
                Ok(resources) => swap.resources = Some(resources),
                Err(_) => { swap.unsupported = true; retire_input(&swap.input); continue; }
            }
        }
        let resources = swap.resources.as_mut().unwrap();
        // Never migrate an exclusive texture or command pool to another queue.
        if resources.queue() != queue || resources.queue_family() != family || resources.is_broken() { continue; }
        resources.poll_frame_complete(device);
        if !resources.prepare_slot(device, image_index as usize) { continue; }
        let mut mode = gpu::FrameMode::Redraw;
        let frame = if resources.can_start_frame(device) {
            swap.input.client.lock().latest()
        } else { None };
        // The mapping lease spans staging upload, not the control mutex.
        if let Some(frame) = frame
            && frame.width() == swap.extent.width && frame.height() == swap.extent.height
        {
                    if resources.copy_staging(device, frame.pixels()).is_ok() {
                        mode = gpu::FrameMode::Software;
                    } else { resources.disable(); }
                // Ring guard is released before command recording and GPU flight.
        }
        if resources.is_broken() || (mode == gpu::FrameMode::Redraw && !resources.can_redraw()) { continue; }
        if resources.record(device, swap.images[image_index as usize], image_index as usize, mode).is_err() {
            resources.disable(); continue;
        }
        // Game semaphores are consumed by precisely the first successful submit.
        // Later overlay commands run on the same externally synchronized queue.
        let submission_waits = if count == 0 { waits } else { &[] };
        match resources.submit(device, image_index as usize, submission_waits, &stages[..submission_waits.len()], mode) {
            Ok(complete) => { completes[count] = complete; count += 1; }
            Err(_) => resources.disable(),
        }
    }
    drop(registry);
    if count == 0 { return present(queue, info); }
    let mut forwarded = ptr::read(info);
    forwarded.wait_semaphore_count = count as u32;
    forwarded.p_wait_semaphores = completes.as_ptr();
    // Forward original swapchains, indices, result storage and result unchanged.
    let result = present(queue, &forwarded);
    // A failed WSI present need not consume its binary semaphore waits. Do
    // not signal those same objects again; teardown retires their queue.
    let failed_image = !original.p_results.is_null()
        && slice::from_raw_parts(original.p_results, swaps.len()).iter().any(|result| result.as_raw() < 0);
    if result.as_raw() < 0 || failed_image {
        let mut registry = REGISTRY.lock();
        for swapchain in swaps {
            if let Some(resources) = registry.swaps.get_mut(swapchain).and_then(|swap| swap.resources.as_mut()) {
                resources.disable();
            }
        }
    }
    result
}
