use ash::{vk, vk::Handle};
use std::{ptr, slice};
use vosulay_frame::DamageRect;
pub(crate) const MAX_IMAGES: usize = 64;
const TEXTURE_FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameMode {
    Redraw,
    Software,
}
struct ImageSlot {
    target_view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    command: vk::CommandBuffer,
    complete: vk::Semaphore,
    fence: vk::Fence,
    in_use: bool,
}

pub(crate) struct Resources {
    command_pool: vk::CommandPool,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_mapped: usize,
    staging_size: usize,
    staging_coherent: bool,
    texture: vk::Image,
    texture_memory: vk::DeviceMemory,
    texture_view: vk::ImageView,
    content_valid: bool,
    /// Ring generation and publication the texture holds once `content_valid`.
    content: (u64, u64),
    /// Region the next software frame copies from staging.
    upload: DamageRect,
    frame_slot: Option<usize>,
    sampler: vk::Sampler,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    render_pass: vk::RenderPass,
    slots: Vec<ImageSlot>,
    extent: vk::Extent2D,
    queue: vk::Queue,
    queue_family: u32,
    broken: bool,
    rendered_frames: u64,
}

impl Resources {
    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit Vulkan object provenance and queue ownership belong at the call site"
    )]
    pub(crate) unsafe fn create(
        instance: &ash::Instance,
        device: &ash::Device,
        physical: vk::PhysicalDevice,
        target_format: vk::Format,
        extent: vk::Extent2D,
        images: &[vk::Image],
        queue: vk::Queue,
        queue_family: u32,
        loader_data: Option<super::SetDeviceLoaderData>,
    ) -> Result<Self, vk::Result> {
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let graphics_queue = queue_families
            .get(queue_family as usize)
            .is_some_and(|properties| properties.queue_flags.contains(vk::QueueFlags::GRAPHICS));
        if images.is_empty()
            || images.len() > MAX_IMAGES
            || extent.width == 0
            || extent.height == 0
            || !unsafe { formats_supported(instance, physical, target_format) }
            || !graphics_queue
        {
            return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
        }
        let staging_size = usize::try_from(extent.width)
            .ok()
            .and_then(|width| width.checked_mul(extent.height as usize))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(vk::Result::ERROR_OUT_OF_HOST_MEMORY)?;
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let mut resources = Self::empty(extent, queue, queue_family, staging_size);
        let built = (|| -> Result<(), vk::Result> {
            resources.staging = unsafe {
                device.create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(staging_size as vk::DeviceSize)
                        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE),
                    None,
                )?
            };
            let staging_requirements =
                unsafe { device.get_buffer_memory_requirements(resources.staging) };
            let (staging_type, staging_flags) = find_memory_type(
                &memory,
                staging_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE,
                vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
            resources.staging_coherent =
                staging_flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT);
            resources.staging_memory = unsafe {
                device.allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(staging_requirements.size)
                        .memory_type_index(staging_type),
                    None,
                )?
            };
            unsafe { device.bind_buffer_memory(resources.staging, resources.staging_memory, 0)? };
            resources.staging_mapped = unsafe {
                device.map_memory(
                    resources.staging_memory,
                    0,
                    vk::WHOLE_SIZE,
                    vk::MemoryMapFlags::empty(),
                )?
            } as usize;

            let (texture, texture_memory, texture_view) =
                unsafe { create_owned_texture(device, &memory, extent)? };
            resources.texture = texture;
            resources.texture_memory = texture_memory;
            resources.texture_view = texture_view;
            resources.sampler = unsafe {
                device.create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .max_lod(0.0),
                    None,
                )?
            };

            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            resources.descriptor_layout = unsafe {
                device.create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )?
            };
            let pool_sizes = [
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1),
                vk::DescriptorPoolSize::default()
                    .ty(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1),
            ];
            resources.descriptor_pool = unsafe {
                device.create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(1)
                        .pool_sizes(&pool_sizes),
                    None,
                )?
            };
            resources.descriptor_set = unsafe {
                device.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(resources.descriptor_pool)
                        .set_layouts(slice::from_ref(&resources.descriptor_layout)),
                )?
            }[0];
            unsafe { resources.write_descriptor(device, resources.texture_view) };

            let attachment = vk::AttachmentDescription::default()
                .format(target_format)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(vk::AttachmentStoreOp::STORE)
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            let attachment_ref = vk::AttachmentReference::default()
                .attachment(0)
                .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            let subpass = vk::SubpassDescription::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(slice::from_ref(&attachment_ref));
            resources.render_pass = unsafe {
                device.create_render_pass(
                    &vk::RenderPassCreateInfo::default()
                        .attachments(slice::from_ref(&attachment))
                        .subpasses(slice::from_ref(&subpass)),
                    None,
                )?
            };
            resources.pipeline_layout = unsafe {
                device.create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(slice::from_ref(&resources.descriptor_layout)),
                    None,
                )?
            };
            resources.pipeline = unsafe {
                create_pipeline(device, resources.render_pass, resources.pipeline_layout)?
            };
            resources.command_pool = unsafe {
                device.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                        .queue_family_index(queue_family),
                    None,
                )?
            };
            let commands = unsafe {
                device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(resources.command_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(images.len() as u32),
                )?
            };
            if let Some(set_data) = loader_data {
                for command in &commands {
                    let result =
                        set_data(device.handle(), command.as_raw() as *mut std::ffi::c_void);
                    if result != vk::Result::SUCCESS {
                        return Err(result);
                    }
                }
            }
            resources.slots.reserve_exact(images.len());
            for (index, image) in images.iter().copied().enumerate() {
                let target_view = unsafe {
                    device.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(target_format)
                            .subresource_range(color_range()),
                        None,
                    )?
                };
                let framebuffer = match unsafe {
                    device.create_framebuffer(
                        &vk::FramebufferCreateInfo::default()
                            .render_pass(resources.render_pass)
                            .attachments(slice::from_ref(&target_view))
                            .width(extent.width)
                            .height(extent.height)
                            .layers(1),
                        None,
                    )
                } {
                    Ok(framebuffer) => framebuffer,
                    Err(error) => {
                        unsafe { device.destroy_image_view(target_view, None) };
                        return Err(error);
                    }
                };
                let complete = match unsafe {
                    device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                } {
                    Ok(semaphore) => semaphore,
                    Err(error) => {
                        unsafe {
                            device.destroy_framebuffer(framebuffer, None);
                            device.destroy_image_view(target_view, None);
                        }
                        return Err(error);
                    }
                };
                let fence = match unsafe {
                    device.create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                } {
                    Ok(fence) => fence,
                    Err(error) => {
                        unsafe {
                            device.destroy_semaphore(complete, None);
                            device.destroy_framebuffer(framebuffer, None);
                            device.destroy_image_view(target_view, None);
                        }
                        return Err(error);
                    }
                };
                resources.slots.push(ImageSlot {
                    target_view,
                    framebuffer,
                    command: commands[index],
                    complete,
                    fence,
                    in_use: false,
                });
            }
            Ok(())
        })();
        if let Err(error) = built {
            unsafe { resources.destroy(device) };
            return Err(error);
        }
        Ok(resources)
    }

    pub(crate) fn queue(&self) -> vk::Queue {
        self.queue
    }

    pub(crate) fn queue_family(&self) -> u32 {
        self.queue_family
    }

    pub(crate) fn is_rendering(&self) -> bool {
        self.content_valid && !self.broken
    }

    pub(crate) fn is_broken(&self) -> bool {
        self.broken
    }

    pub(crate) fn frame_count(&self) -> u64 {
        self.rendered_frames
    }

    pub(crate) fn can_redraw(&self) -> bool {
        self.content_valid && !self.broken
    }
    /// Publication of ring `generation` the texture holds, for damage uploads.
    pub(crate) fn uploaded(&self, generation: u64) -> Option<u64> {
        (self.content_valid && self.content.0 == generation).then_some(self.content.1)
    }
    pub(crate) fn disable(&mut self) {
        self.broken = true;
    }

    pub(crate) unsafe fn prepare_slot(&mut self, device: &ash::Device, image_index: usize) -> bool {
        let Some(slot) = self.slots.get_mut(image_index) else {
            return false;
        };
        if slot.in_use {
            match unsafe { device.get_fence_status(slot.fence) } {
                Ok(true) => slot.in_use = false,
                Ok(false) => return false,
                Err(_) => {
                    self.broken = true;
                    return false;
                }
            }
        }
        !self.broken
    }

    pub(crate) unsafe fn can_start_frame(&mut self, device: &ash::Device) -> bool {
        if self.broken || self.frame_slot.is_some() {
            return false;
        }
        for slot in &self.slots {
            if slot.in_use {
                match unsafe { device.get_fence_status(slot.fence) } {
                    Ok(true) => {}
                    Ok(false) => return false,
                    Err(_) => {
                        self.broken = true;
                        return false;
                    }
                }
            }
        }
        true
    }

    pub(crate) unsafe fn poll_frame_complete(&mut self, device: &ash::Device) -> bool {
        let Some(index) = self.frame_slot else {
            return false;
        };
        let Some(slot) = self.slots.get(index) else {
            self.broken = true;
            return false;
        };
        match unsafe { device.get_fence_status(slot.fence) } {
            Ok(true) => {
                self.frame_slot = None;
                true
            }
            Ok(false) => false,
            Err(_) => {
                self.broken = true;
                false
            }
        }
    }

    /// Stages `region` of `bytes` (all of it if None or the texture is not
    /// yet valid) and records which publication the texture will hold.
    pub(crate) unsafe fn copy_staging(
        &mut self,
        device: &ash::Device,
        bytes: &[u8],
        region: Option<DamageRect>,
        content: (u64, u64),
    ) -> Result<(), vk::Result> {
        if bytes.len() != self.staging_size
            || self.staging_mapped == 0
            || self.frame_slot.is_some()
            || self.broken
        {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        let (width, height) = (self.extent.width, self.extent.height);
        let region = region
            .filter(|_| self.content_valid)
            .and_then(|region| region.clipped(width, height))
            .unwrap_or_else(|| DamageRect::full(width, height));
        // Rows keep their frame offsets; the copy reads only this region.
        let stride = width as usize * 4;
        let length = region.width as usize * 4;
        for row in region.y as usize..(region.y + region.height) as usize {
            let offset = row * stride + region.x as usize * 4;
            unsafe {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(offset),
                    (self.staging_mapped as *mut u8).add(offset),
                    length,
                )
            };
        }
        if !self.staging_coherent {
            unsafe {
                device.flush_mapped_memory_ranges(slice::from_ref(
                    &vk::MappedMemoryRange::default()
                        .memory(self.staging_memory)
                        .offset(0)
                        .size(vk::WHOLE_SIZE),
                ))?
            };
        }
        self.upload = region;
        self.content = content;
        Ok(())
    }
    unsafe fn write_descriptor(&self, device: &ash::Device, view: vk::ImageView) {
        let sampled = vk::DescriptorImageInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let sampler = vk::DescriptorImageInfo::default().sampler(self.sampler);
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(slice::from_ref(&sampled)),
            vk::WriteDescriptorSet::default()
                .dst_set(self.descriptor_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(slice::from_ref(&sampler)),
        ];
        unsafe { device.update_descriptor_sets(&writes, &[]) };
    }

    pub(crate) unsafe fn record(
        &mut self,
        device: &ash::Device,
        target: vk::Image,
        image_index: usize,
        mode: FrameMode,
    ) -> Result<(), vk::Result> {
        if self.broken || (mode == FrameMode::Redraw && !self.can_redraw()) {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        let slot = self
            .slots
            .get(image_index)
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        unsafe {
            device.reset_command_buffer(slot.command, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                slot.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
        }
        if mode == FrameMode::Software {
            let buffer_barrier = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::HOST_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(self.staging)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            unsafe {
                device.cmd_pipeline_barrier(
                    slot.command,
                    vk::PipelineStageFlags::HOST,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    slice::from_ref(&buffer_barrier),
                    &[],
                )
            };
            unsafe {
                image_barrier(
                    device,
                    slot.command,
                    self.texture,
                    if self.content_valid {
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                    } else {
                        vk::ImageLayout::UNDEFINED
                    },
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::TRANSFER,
                    if self.content_valid {
                        vk::AccessFlags::SHADER_READ
                    } else {
                        vk::AccessFlags::empty()
                    },
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::QUEUE_FAMILY_IGNORED,
                    vk::QUEUE_FAMILY_IGNORED,
                )
            };
            let region = self.upload;
            let copy = vk::BufferImageCopy::default()
                .buffer_offset(
                    (u64::from(region.y) * u64::from(self.extent.width) + u64::from(region.x)) * 4,
                )
                .buffer_row_length(self.extent.width)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D {
                    x: region.x as i32,
                    y: region.y as i32,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width: region.width,
                    height: region.height,
                    depth: 1,
                });
            unsafe {
                device.cmd_copy_buffer_to_image(
                    slot.command,
                    self.staging,
                    self.texture,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    slice::from_ref(&copy),
                )
            };
            unsafe {
                image_barrier(
                    device,
                    slot.command,
                    self.texture,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::SHADER_READ,
                    vk::QUEUE_FAMILY_IGNORED,
                    vk::QUEUE_FAMILY_IGNORED,
                )
            };
        }
        unsafe {
            image_barrier(
                device,
                slot.command,
                target,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags::empty(),
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                vk::QUEUE_FAMILY_IGNORED,
                vk::QUEUE_FAMILY_IGNORED,
            )
        };
        let render_area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: self.extent,
        };
        unsafe {
            device.cmd_begin_render_pass(
                slot.command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(slot.framebuffer)
                    .render_area(render_area),
                vk::SubpassContents::INLINE,
            );
            device.cmd_bind_pipeline(slot.command, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            device.cmd_bind_descriptor_sets(
                slot.command,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                slice::from_ref(&self.descriptor_set),
                &[],
            );
            device.cmd_set_viewport(
                slot.command,
                0,
                slice::from_ref(&vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: self.extent.width as f32,
                    height: self.extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }),
            );
            device.cmd_set_scissor(slot.command, 0, slice::from_ref(&render_area));
            device.cmd_draw(slot.command, 3, 1, 0, 0);
            device.cmd_end_render_pass(slot.command);
        }
        unsafe {
            image_barrier(
                device,
                slot.command,
                target,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                vk::AccessFlags::empty(),
                vk::QUEUE_FAMILY_IGNORED,
                vk::QUEUE_FAMILY_IGNORED,
            );
            device.end_command_buffer(slot.command)
        }
    }

    pub(crate) unsafe fn submit(
        &mut self,
        device: &ash::Device,
        image_index: usize,
        waits: &[vk::Semaphore],
        wait_stages: &[vk::PipelineStageFlags],
        mode: FrameMode,
    ) -> Result<vk::Semaphore, vk::Result> {
        let slot = self
            .slots
            .get_mut(image_index)
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        if slot.in_use
            || waits.len() != wait_stages.len()
            || (mode != FrameMode::Redraw && self.frame_slot.is_some())
        {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        unsafe { device.reset_fences(slice::from_ref(&slot.fence))? };
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(waits)
            .wait_dst_stage_mask(wait_stages)
            .command_buffers(slice::from_ref(&slot.command))
            .signal_semaphores(slice::from_ref(&slot.complete));
        let submitted =
            unsafe { device.queue_submit(self.queue, slice::from_ref(&submit), slot.fence) };
        if let Err(error) = submitted {
            self.broken = true;
            return Err(error);
        }
        slot.in_use = true;
        if mode != FrameMode::Redraw {
            self.frame_slot = Some(image_index);
            self.content_valid = true;
        }
        self.rendered_frames = self.rendered_frames.saturating_add(1);
        Ok(slot.complete)
    }

    pub(crate) unsafe fn retire(mut self, device: &ash::Device) -> bool {
        if unsafe { device.queue_wait_idle(self.queue) }.is_err() {
            return false;
        }
        self.mark_queue_idle();
        unsafe { self.destroy(device) };
        true
    }

    pub(crate) fn mark_queue_idle(&mut self) {
        for slot in &mut self.slots {
            slot.in_use = false;
        }
    }

    fn empty(
        extent: vk::Extent2D,
        queue: vk::Queue,
        queue_family: u32,
        staging_size: usize,
    ) -> Self {
        Self {
            command_pool: vk::CommandPool::null(),
            staging: vk::Buffer::null(),
            staging_memory: vk::DeviceMemory::null(),
            staging_mapped: 0,
            staging_size,
            staging_coherent: false,
            texture: vk::Image::null(),
            texture_memory: vk::DeviceMemory::null(),
            texture_view: vk::ImageView::null(),
            content_valid: false,
            content: (0, 0),
            upload: DamageRect::full(extent.width, extent.height),
            frame_slot: None,
            sampler: vk::Sampler::null(),
            descriptor_layout: vk::DescriptorSetLayout::null(),
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_set: vk::DescriptorSet::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            render_pass: vk::RenderPass::null(),
            slots: Vec::new(),
            extent,
            queue,
            queue_family,
            broken: false,
            rendered_frames: 0,
        }
    }

    unsafe fn destroy(&mut self, device: &ash::Device) {
        for slot in self.slots.drain(..) {
            unsafe {
                if slot.fence != vk::Fence::null() {
                    device.destroy_fence(slot.fence, None);
                }
                if slot.complete != vk::Semaphore::null() {
                    device.destroy_semaphore(slot.complete, None);
                }
                if slot.framebuffer != vk::Framebuffer::null() {
                    device.destroy_framebuffer(slot.framebuffer, None);
                }
                if slot.target_view != vk::ImageView::null() {
                    device.destroy_image_view(slot.target_view, None);
                }
            }
        }
        unsafe {
            if self.command_pool != vk::CommandPool::null() {
                device.destroy_command_pool(self.command_pool, None);
            }
            if self.pipeline != vk::Pipeline::null() {
                device.destroy_pipeline(self.pipeline, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                device.destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.render_pass != vk::RenderPass::null() {
                device.destroy_render_pass(self.render_pass, None);
            }
            if self.descriptor_pool != vk::DescriptorPool::null() {
                device.destroy_descriptor_pool(self.descriptor_pool, None);
            }
            if self.descriptor_layout != vk::DescriptorSetLayout::null() {
                device.destroy_descriptor_set_layout(self.descriptor_layout, None);
            }
            if self.sampler != vk::Sampler::null() {
                device.destroy_sampler(self.sampler, None);
            }
            if self.texture_view != vk::ImageView::null() {
                device.destroy_image_view(self.texture_view, None);
            }
            if self.texture != vk::Image::null() {
                device.destroy_image(self.texture, None);
            }
            if self.texture_memory != vk::DeviceMemory::null() {
                device.free_memory(self.texture_memory, None);
            }
            if self.staging_mapped != 0 && self.staging_memory != vk::DeviceMemory::null() {
                device.unmap_memory(self.staging_memory);
            }
            if self.staging != vk::Buffer::null() {
                device.destroy_buffer(self.staging, None);
            }
            if self.staging_memory != vk::DeviceMemory::null() {
                device.free_memory(self.staging_memory, None);
            }
        }
    }
}
pub(crate) unsafe fn formats_supported(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    target: vk::Format,
) -> bool {
    let source =
        unsafe { instance.get_physical_device_format_properties(physical, TEXTURE_FORMAT) };
    let target = unsafe { instance.get_physical_device_format_properties(physical, target) };
    source.optimal_tiling_features.contains(
        vk::FormatFeatureFlags::TRANSFER_DST
            | vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR,
    ) && target.optimal_tiling_features.contains(
        vk::FormatFeatureFlags::COLOR_ATTACHMENT | vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND,
    )
}

unsafe fn create_owned_texture(
    device: &ash::Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    extent: vk::Extent2D,
) -> Result<(vk::Image, vk::DeviceMemory, vk::ImageView), vk::Result> {
    let image = unsafe {
        device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(TEXTURE_FORMAT)
                .extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?
    };
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let Some((memory_type, _)) = find_memory_type(
        memory,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::empty(),
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) else {
        unsafe { device.destroy_image(image, None) };
        return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
    };
    let allocation = match unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )
    } {
        Ok(memory) => memory,
        Err(error) => {
            unsafe { device.destroy_image(image, None) };
            return Err(error);
        }
    };
    if let Err(error) = unsafe { device.bind_image_memory(image, allocation, 0) } {
        unsafe {
            device.destroy_image(image, None);
            device.free_memory(allocation, None);
        }
        return Err(error);
    }
    let view = match unsafe { create_texture_view(device, image) } {
        Ok(view) => view,
        Err(error) => {
            unsafe {
                device.destroy_image(image, None);
                device.free_memory(allocation, None);
            }
            return Err(error);
        }
    };
    Ok((image, allocation, view))
}

unsafe fn create_texture_view(
    device: &ash::Device,
    image: vk::Image,
) -> Result<vk::ImageView, vk::Result> {
    unsafe {
        device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(TEXTURE_FORMAT)
                .subresource_range(color_range()),
            None,
        )
    }
}
unsafe fn create_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
) -> Result<vk::Pipeline, vk::Result> {
    let vertex = unsafe {
        device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(super::shaders::VERTEX_SPV),
            None,
        )?
    };
    let fragment = match unsafe {
        device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(super::shaders::FRAGMENT_SPV),
            None,
        )
    } {
        Ok(module) => module,
        Err(error) => {
            unsafe { device.destroy_shader_module(vertex, None) };
            return Err(error);
        }
    };
    let vertex_stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::VERTEX)
        .module(vertex)
        .name(c"main");
    let fragment_stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::FRAGMENT)
        .module(fragment)
        .name(c"main");
    let stages = [vertex_stage, fragment_stage];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        );
    let color_blend = vk::PipelineColorBlendStateCreateInfo::default()
        .attachments(slice::from_ref(&blend_attachment));
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport)
        .rasterization_state(&rasterization)
        .multisample_state(&multisample)
        .color_blend_state(&color_blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(render_pass)
        .subpass(0);
    let result = match unsafe {
        device.create_graphics_pipelines(vk::PipelineCache::null(), slice::from_ref(&info), None)
    } {
        Ok(pipelines) => Ok(pipelines[0]),
        Err((pipelines, error)) => {
            for pipeline in pipelines {
                unsafe { device.destroy_pipeline(pipeline, None) };
            }
            Err(error)
        }
    };
    unsafe {
        device.destroy_shader_module(fragment, None);
        device.destroy_shader_module(vertex, None);
    }
    result
}

fn find_memory_type(
    memory: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
    required: vk::MemoryPropertyFlags,
    preferred: vk::MemoryPropertyFlags,
) -> Option<(u32, vk::MemoryPropertyFlags)> {
    let count = memory.memory_type_count as usize;
    let candidates = &memory.memory_types[..count];
    candidates
        .iter()
        .enumerate()
        .find(|(index, ty)| {
            bits & (1 << index) != 0 && ty.property_flags.contains(required | preferred)
        })
        .or_else(|| {
            candidates.iter().enumerate().find(|(index, ty)| {
                bits & (1 << index) != 0 && ty.property_flags.contains(required)
            })
        })
        .map(|(index, ty)| (index as u32, ty.property_flags))
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Names distinguish Vulkan source and destination stages, access masks and queue families"
)]
unsafe fn image_barrier(
    device: &ash::Device,
    command: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    source_stage: vk::PipelineStageFlags,
    destination_stage: vk::PipelineStageFlags,
    source_access: vk::AccessFlags,
    destination_access: vk::AccessFlags,
    source_queue: u32,
    destination_queue: u32,
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .src_access_mask(source_access)
        .dst_access_mask(destination_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(source_queue)
        .dst_queue_family_index(destination_queue)
        .image(image)
        .subresource_range(color_range());
    unsafe {
        device.cmd_pipeline_barrier(
            command,
            source_stage,
            destination_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            slice::from_ref(&barrier),
        )
    };
}
