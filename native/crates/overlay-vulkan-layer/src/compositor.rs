use super::SharedFrame;
use ash::{Device, vk};
use overlay_agent_core::FrameDescriptor;
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::Arc,
};

pub(crate) enum DrawOutcome {
    Submitted(vk::Semaphore),
    Busy,
}

pub(crate) type GetMemoryFdProperties = vk::PFN_vkGetMemoryFdPropertiesKHR;
pub(crate) type GetImageMemoryRequirements2 = vk::PFN_vkGetImageMemoryRequirements2;
pub(crate) type GetPhysicalDeviceImageFormatProperties2 =
    vk::PFN_vkGetPhysicalDeviceImageFormatProperties2;
const MAX_IMPORTED_SURFACES: usize = 4;
const MAX_PRESENT_WAITS: usize = 16;

pub(crate) struct SwapchainState {
    pub(crate) device: vk::Device,
    pub(crate) format: vk::Format,
    pub(crate) extent: vk::Extent2D,
    pub(crate) images: Vec<vk::Image>,
    resources: Option<Resources>,
    /// The frame each image's last submission sampled. Fences are polled on
    /// every present, including presents with no current overlay frame, so a
    /// hide/clear cannot strand Chromium release callbacks indefinitely.
    retained_frames: Vec<Option<Arc<SharedFrame>>>,
    disabled: bool,
}

impl SwapchainState {
    pub(crate) fn new(
        device: vk::Device,
        format: vk::Format,
        extent: vk::Extent2D,
        images: Vec<vk::Image>,
    ) -> Self {
        let retained_frames = (0..images.len()).map(|_| None).collect();
        Self {
            device,
            format,
            extent,
            images,
            resources: None,
            retained_frames,
            disabled: false,
        }
    }

    /// Release producer frames whose per-image submissions have completed.
    /// This is deliberately independent of drawing another overlay frame.
    pub(crate) unsafe fn retire_completed(&mut self, raw: &Device) -> Result<(), vk::Result> {
        let Some(resources) = self.resources.as_mut() else {
            return Ok(());
        };
        for index in resources.completed_images(raw)? {
            self.retained_frames[index].take();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn draw(
        &mut self,
        raw: &Device,
        physical_device: vk::PhysicalDevice,
        queue: vk::Queue,
        queue_family: u32,
        image_index: u32,
        wait_semaphores: &[vk::Semaphore],
        get_memory_fd_properties: GetMemoryFdProperties,
        get_image_memory_requirements2: GetImageMemoryRequirements2,
        get_image_format_properties2: GetPhysicalDeviceImageFormatProperties2,
        frame: Arc<SharedFrame>,
    ) -> Result<DrawOutcome, vk::Result> {
        if self.disabled || image_index as usize >= self.images.len() {
            return Err(vk::Result::ERROR_UNKNOWN);
        }
        unsafe { self.retire_completed(raw)? };
        let descriptor = match &frame.frame.descriptor {
            FrameDescriptor::Dmabuf(descriptor) if descriptor.planes.len() == 1 => descriptor,
            _ => return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED),
        };
        if self.resources.is_none() {
            self.resources = Some(unsafe { Resources::new(raw, self, queue_family)? });
        }
        let resources = self.resources.as_mut().expect("resources initialized");
        if resources.queue_family != queue_family {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }
        let result = unsafe {
            resources.draw(
                raw,
                physical_device,
                queue,
                image_index,
                self.images[image_index as usize],
                self.extent,
                wait_semaphores,
                get_memory_fd_properties,
                get_image_memory_requirements2,
                get_image_format_properties2,
                descriptor,
                &frame.frame.fds,
            )
        };
        match result {
            Ok(DrawOutcome::Submitted(_)) => {
                self.retained_frames[image_index as usize] = Some(frame);
            }
            Err(_) => self.disabled = true,
            Ok(DrawOutcome::Busy) => {}
        }
        result
    }

    pub(crate) unsafe fn destroy(&mut self, raw: &Device) {
        if let Some(resources) = self.resources.take() {
            unsafe { resources.destroy(raw) };
        }
        // `Resources::destroy` waited for device idle, so no submission can
        // still be reading these frames.
        self.retained_frames.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DmabufIdentity {
    device: u64,
    inode: u64,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DmabufKey {
    identity: DmabufIdentity,
    width: u32,
    height: u32,
    fourcc: u32,
    modifier: u64,
    offset: u64,
    stride: u32,
    plane_size: u64,
}

struct ImportedSurface {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    key: DmabufKey,
    last_used: u64,
}

struct Resources {
    queue_family: u32,
    render_pass: vk::RenderPass,
    target_views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    descriptor_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    /// One set per swapchain image. A single shared set would be rewritten
    /// while an earlier image's submission is still sampling through it.
    descriptor_sets: Vec<vk::DescriptorSet>,
    sampler: vk::Sampler,
    command_pool: vk::CommandPool,
    /// One command buffer per swapchain image, so recording for the current
    /// image never has to wait for an unrelated image to retire.
    command_buffers: Vec<vk::CommandBuffer>,
    finished: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
    /// Bounded producer-buffer cache. Each in-flight swapchain image records
    /// which key it samples, so an idle LRU entry can be evicted without ever
    /// destroying an import still referenced by GPU work.
    imports: Vec<ImportedSurface>,
    image_imports: Vec<Option<DmabufKey>>,
    import_serial: u64,
}

impl Resources {
    unsafe fn new(
        raw: &Device,
        swapchain: &SwapchainState,
        queue_family: u32,
    ) -> Result<Self, vk::Result> {
        let attachment = vk::AttachmentDescription::default()
            .format(swapchain.format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        let color = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color));
        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );
        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(std::slice::from_ref(&attachment))
            .subpasses(std::slice::from_ref(&subpass))
            .dependencies(std::slice::from_ref(&dependency));
        let render_pass = unsafe { raw.create_render_pass(&render_pass_info, None)? };

        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT);
        let descriptor_layout_info =
            vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding));
        let descriptor_layout =
            unsafe { raw.create_descriptor_set_layout(&descriptor_layout_info, None)? };
        let set_layouts = [descriptor_layout];
        let pipeline_layout_info =
            vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        let pipeline_layout = unsafe { raw.create_pipeline_layout(&pipeline_layout_info, None)? };

        let vertex_module =
            unsafe { shader_module(raw, include_bytes!("../shaders/overlay.vert.spv"))? };
        let fragment_module =
            unsafe { shader_module(raw, include_bytes!("../shaders/overlay.frag.spv"))? };
        let main = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex_module)
                .name(main),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment_module)
                .name(main),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
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
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let blend = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(std::slice::from_ref(&blend_attachment));
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(pipeline_layout)
            .render_pass(render_pass)
            .subpass(0);
        let pipeline = match unsafe {
            raw.create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
        } {
            Ok(mut pipelines) => pipelines.remove(0),
            Err((_, error)) => {
                unsafe {
                    raw.destroy_shader_module(vertex_module, None);
                    raw.destroy_shader_module(fragment_module, None);
                }
                return Err(error);
            }
        };
        unsafe {
            raw.destroy_shader_module(vertex_module, None);
            raw.destroy_shader_module(fragment_module, None);
        }

        let image_count = swapchain.images.len().max(1) as u32;
        let pool_size = vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(image_count);
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(image_count)
            .pool_sizes(std::slice::from_ref(&pool_size));
        let descriptor_pool = unsafe { raw.create_descriptor_pool(&pool_info, None)? };
        let per_image_layouts = vec![set_layouts[0]; image_count as usize];
        let allocate_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(descriptor_pool)
            .set_layouts(&per_image_layouts);
        let descriptor_sets = unsafe { raw.allocate_descriptor_sets(&allocate_info)? };
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .max_lod(0.0);
        let sampler = unsafe { raw.create_sampler(&sampler_info, None)? };

        let command_pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family);
        let command_pool = unsafe { raw.create_command_pool(&command_pool_info, None)? };
        let command_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(image_count);
        let command_buffers = unsafe { raw.allocate_command_buffers(&command_info)? };

        // Per-image synchronisation, created up front so the draw path has no
        // allocation branch. Fences start signalled: nothing is in flight yet.
        let mut finished = Vec::with_capacity(image_count as usize);
        let mut fences = Vec::with_capacity(image_count as usize);
        for _ in 0..image_count {
            finished
                .push(unsafe { raw.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? });
            fences.push(unsafe {
                raw.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?
            });
        }
        let mut target_views = Vec::with_capacity(swapchain.images.len());
        let mut framebuffers = Vec::with_capacity(swapchain.images.len());
        for image in &swapchain.images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(*image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(swapchain.format)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            let view = unsafe { raw.create_image_view(&view_info, None)? };
            let framebuffer_info = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(std::slice::from_ref(&view))
                .width(swapchain.extent.width)
                .height(swapchain.extent.height)
                .layers(1);
            let framebuffer = unsafe { raw.create_framebuffer(&framebuffer_info, None)? };
            target_views.push(view);
            framebuffers.push(framebuffer);
        }

        Ok(Self {
            queue_family,
            render_pass,
            target_views,
            framebuffers,
            descriptor_layout,
            pipeline_layout,
            pipeline,
            descriptor_pool,
            descriptor_sets,
            sampler,
            command_pool,
            command_buffers,
            finished,
            fences,
            imports: Vec::with_capacity(MAX_IMPORTED_SURFACES),
            image_imports: vec![None; image_count as usize],
            import_serial: 0,
        })
    }

    fn completed_images(&mut self, raw: &Device) -> Result<Vec<usize>, vk::Result> {
        let mut completed = Vec::new();
        for (index, fence) in self.fences.iter().copied().enumerate() {
            if self.image_imports[index].is_some() && unsafe { raw.get_fence_status(fence)? } {
                self.image_imports[index] = None;
                completed.push(index);
            }
        }
        Ok(completed)
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw(
        &mut self,
        raw: &Device,
        physical_device: vk::PhysicalDevice,
        queue: vk::Queue,
        image_index: u32,
        target: vk::Image,
        extent: vk::Extent2D,
        wait_semaphores: &[vk::Semaphore],
        get_memory_fd_properties: GetMemoryFdProperties,
        get_image_memory_requirements2: GetImageMemoryRequirements2,
        get_image_format_properties2: GetPhysicalDeviceImageFormatProperties2,
        descriptor: &overlay_protocol::DmabufDescriptor,
        fds: &[OwnedFd],
    ) -> Result<DrawOutcome, vk::Result> {
        let index = image_index as usize;
        let (Some(&fence), Some(&finished)) = (self.fences.get(index), self.finished.get(index))
        else {
            return Err(vk::Result::ERROR_UNKNOWN);
        };
        if !unsafe { raw.get_fence_status(fence)? } {
            return Ok(DrawOutcome::Busy);
        }
        self.image_imports[index] = None;
        let key = dmabuf_key(
            descriptor,
            fds.first().ok_or(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?,
        )?;
        self.import_serial = self.import_serial.wrapping_add(1).max(1);
        let serial = self.import_serial;
        let (surface_index, newly_imported) =
            if let Some(index) = self.imports.iter().position(|surface| surface.key == key) {
                self.imports[index].last_used = serial;
                (index, false)
            } else {
                let slot = if self.imports.len() < MAX_IMPORTED_SURFACES {
                    self.imports.len()
                } else {
                    let Some((slot, _)) = self
                        .imports
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| {
                            !self
                                .image_imports
                                .iter()
                                .flatten()
                                .any(|in_flight| *in_flight == candidate.key)
                        })
                        .min_by_key(|(_, candidate)| candidate.last_used)
                    else {
                        return Ok(DrawOutcome::Busy);
                    };
                    slot
                };
                let imported = self.import_surface(
                    raw,
                    physical_device,
                    descriptor,
                    fds,
                    get_memory_fd_properties,
                    get_image_memory_requirements2,
                    get_image_format_properties2,
                    key,
                    serial,
                )?;
                if slot == self.imports.len() {
                    self.imports.push(imported);
                } else {
                    let retired = std::mem::replace(&mut self.imports[slot], imported);
                    unsafe { destroy_imported_surface(raw, retired) };
                }
                (slot, true)
            };
        let descriptor_set = self.descriptor_sets[index];
        let command_buffer = self.command_buffers[index];
        let imported_image = self.imports[surface_index].image;
        let imported_view = self.imports[surface_index].view;
        let image_info = vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(imported_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let write = vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image_info));
        unsafe { raw.update_descriptor_sets(&[write], &[]) };
        unsafe { raw.reset_fences(&[fence])? };
        unsafe { raw.reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())? };
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe { raw.begin_command_buffer(command_buffer, &begin)? };
        let acquire = vk::ImageMemoryBarrier::default()
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(if newly_imported {
                vk::ImageLayout::UNDEFINED
            } else {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            })
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
            .dst_queue_family_index(self.queue_family)
            .image(imported_image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        unsafe {
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[acquire],
            );
        }
        let to_color = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_READ)
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            )
            .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .image(target)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        unsafe {
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_color],
            );
        }
        let render_begin = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffers[index])
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent,
            });
        unsafe {
            raw.cmd_begin_render_pass(command_buffer, &render_begin, vk::SubpassContents::INLINE);
            raw.cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            );
            raw.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[descriptor_set],
                &[],
            );
            raw.cmd_set_viewport(
                command_buffer,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            raw.cmd_set_scissor(
                command_buffer,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent,
                }],
            );
            raw.cmd_draw(command_buffer, 3, 1, 0, 0);
            raw.cmd_end_render_pass(command_buffer);
            let release = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(self.queue_family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .image(imported_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[release],
            );
            raw.end_command_buffer(command_buffer)?;
        }
        if wait_semaphores.len() > MAX_PRESENT_WAITS {
            return Ok(DrawOutcome::Busy);
        }
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT; MAX_PRESENT_WAITS];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(wait_semaphores)
            .wait_dst_stage_mask(&wait_stages[..wait_semaphores.len()])
            .command_buffers(std::slice::from_ref(&command_buffer))
            .signal_semaphores(std::slice::from_ref(&finished));
        unsafe { raw.queue_submit(queue, &[submit], fence)? };
        self.image_imports[index] = Some(key);
        Ok(DrawOutcome::Submitted(finished))
    }

    #[allow(clippy::too_many_arguments)]
    fn import_surface(
        &self,
        raw: &Device,
        physical_device: vk::PhysicalDevice,
        descriptor: &overlay_protocol::DmabufDescriptor,
        fds: &[OwnedFd],
        get_memory_fd_properties: GetMemoryFdProperties,
        get_image_memory_requirements2: GetImageMemoryRequirements2,
        get_image_format_properties2: GetPhysicalDeviceImageFormatProperties2,
        key: DmabufKey,
        last_used: u64,
    ) -> Result<ImportedSurface, vk::Result> {
        if descriptor.planes.len() != 1 || fds.len() != 1 {
            return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
        }
        let format =
            fourcc_format(descriptor.fourcc).ok_or(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?;
        ensure_import_supported(
            physical_device,
            format,
            descriptor.modifier,
            get_image_format_properties2,
        )?;
        let plane = &descriptor.planes[0];
        // VkImageDrmFormatModifierExplicitCreateInfoEXT requires every
        // VkSubresourceLayout::size entry to be zero. The allocation size is
        // learned from the image's memory requirements, not copied from the FD.
        let plane_layout = vk::SubresourceLayout::default()
            .offset(plane.offset)
            .row_pitch(plane.stride as u64)
            .size(0);
        let modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(descriptor.modifier)
            .plane_layouts(std::slice::from_ref(&plane_layout));
        let mut external_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        external_info.p_next = (&raw const modifier_info).cast();
        let image_info = vk::ImageCreateInfo::default()
            .push_next(&mut external_info)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: descriptor.width,
                height: descriptor.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { raw.create_image(&image_info, None)? };

        let requirements_info = vk::ImageMemoryRequirementsInfo2::default().image(image);
        let mut dedicated_requirements = vk::MemoryDedicatedRequirements::default();
        let mut requirements2 =
            vk::MemoryRequirements2::default().push_next(&mut dedicated_requirements);
        unsafe {
            get_image_memory_requirements2(raw.handle(), &requirements_info, &mut requirements2)
        };
        let requirements = requirements2.memory_requirements;

        let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
        let properties_result = unsafe {
            get_memory_fd_properties(
                raw.handle(),
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                fds[0].as_raw_fd(),
                &mut fd_properties,
            )
        };
        if properties_result != vk::Result::SUCCESS {
            unsafe { raw.destroy_image(image, None) };
            return Err(properties_result);
        }
        let memory_types = requirements.memory_type_bits & fd_properties.memory_type_bits;
        if memory_types == 0 {
            unsafe { raw.destroy_image(image, None) };
            return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE);
        }
        let duplicated = unsafe { libc::fcntl(fds[0].as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated < 0 {
            unsafe { raw.destroy_image(image, None) };
            return Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
        }
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(duplicated);
        if dedicated_requirements.requires_dedicated_allocation != 0
            || dedicated_requirements.prefers_dedicated_allocation != 0
        {
            import_info.p_next =
                (&mut dedicated_info as *mut vk::MemoryDedicatedAllocateInfo<'_>).cast();
        }
        let allocation = vk::MemoryAllocateInfo::default()
            .push_next(&mut import_info)
            .allocation_size(requirements.size)
            .memory_type_index(memory_types.trailing_zeros());
        let memory = match unsafe { raw.allocate_memory(&allocation, None) } {
            Ok(memory) => memory,
            Err(error) => {
                unsafe {
                    libc::close(duplicated);
                    raw.destroy_image(image, None)
                };
                return Err(error);
            }
        };
        if let Err(error) = unsafe { raw.bind_image_memory(image, memory, 0) } {
            unsafe {
                raw.free_memory(memory, None);
                raw.destroy_image(image, None)
            };
            return Err(error);
        }
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        let view = match unsafe { raw.create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(error) => {
                unsafe {
                    raw.free_memory(memory, None);
                    raw.destroy_image(image, None)
                };
                return Err(error);
            }
        };
        Ok(ImportedSurface {
            image,
            memory,
            view,
            key,
            last_used,
        })
    }

    unsafe fn destroy(self, raw: &Device) {
        let _ = unsafe { raw.device_wait_idle() };
        for surface in self.imports {
            unsafe { destroy_imported_surface(raw, surface) };
        }
        for framebuffer in self.framebuffers {
            unsafe { raw.destroy_framebuffer(framebuffer, None) };
        }
        for view in self.target_views {
            unsafe { raw.destroy_image_view(view, None) };
        }
        unsafe {
            for fence in self.fences {
                raw.destroy_fence(fence, None);
            }
            for semaphore in self.finished {
                raw.destroy_semaphore(semaphore, None);
            }
            raw.destroy_command_pool(self.command_pool, None);
            raw.destroy_sampler(self.sampler, None);
            raw.destroy_descriptor_pool(self.descriptor_pool, None);
            raw.destroy_pipeline(self.pipeline, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
            raw.destroy_descriptor_set_layout(self.descriptor_layout, None);
            raw.destroy_render_pass(self.render_pass, None);
        }
    }
}

fn ensure_import_supported(
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
    modifier: u64,
    get_image_format_properties2: GetPhysicalDeviceImageFormatProperties2,
) -> Result<(), vk::Result> {
    let mut drm_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let format_info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .push_next(&mut external_info)
        .push_next(&mut drm_info);
    let mut external_properties = vk::ExternalImageFormatProperties::default();
    let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external_properties);
    let result =
        unsafe { get_image_format_properties2(physical_device, &format_info, &mut properties) };
    if result != vk::Result::SUCCESS {
        return Err(result);
    }
    let external = external_properties.external_memory_properties;
    if !external
        .external_memory_features
        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        || !external
            .compatible_handle_types
            .contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
    {
        return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
    }
    Ok(())
}

unsafe fn shader_module(raw: &Device, bytes: &[u8]) -> Result<vk::ShaderModule, vk::Result> {
    if !bytes.len().is_multiple_of(4) {
        return Err(vk::Result::ERROR_INVALID_SHADER_NV);
    }
    let code = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u32>(), bytes.len() / 4) };
    unsafe { raw.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(code), None) }
}

/// Map a DRM fourcc to the Vulkan format that reads the same bytes.
fn fourcc_format(fourcc: u32) -> Option<vk::Format> {
    match fourcc {
        overlay_protocol::DRM_FORMAT_ARGB8888 => Some(vk::Format::B8G8R8A8_UNORM),
        overlay_protocol::DRM_FORMAT_ABGR8888 => Some(vk::Format::R8G8B8A8_UNORM),
        _ => None,
    }
}

unsafe fn destroy_imported_surface(raw: &Device, surface: ImportedSurface) {
    unsafe {
        raw.destroy_image_view(surface.view, None);
        raw.destroy_image(surface.image, None);
        raw.free_memory(surface.memory, None);
    }
}

fn dmabuf_identity(fd: &OwnedFd) -> Result<DmabufIdentity, vk::Result> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(vk::Result::ERROR_INVALID_EXTERNAL_HANDLE);
    }
    let stat = unsafe { stat.assume_init() };
    let size =
        u64::try_from(stat.st_size).map_err(|_| vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
    Ok(DmabufIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
        size,
    })
}

fn dmabuf_key(
    descriptor: &overlay_protocol::DmabufDescriptor,
    fd: &OwnedFd,
) -> Result<DmabufKey, vk::Result> {
    let [plane] = descriptor.planes.as_slice() else {
        return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
    };
    Ok(DmabufKey {
        identity: dmabuf_identity(fd)?,
        width: descriptor.width,
        height: descriptor.height,
        fourcc: descriptor.fourcc,
        modifier: descriptor.modifier,
        offset: plane.offset,
        stride: plane.stride,
        plane_size: plane.size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIXEL: [u8; 4] = [0x10, 0x20, 0x30, 0xFF];

    fn maps_to(fourcc: u32, expected: vk::Format) -> bool {
        fourcc_format(fourcc) == Some(expected)
    }

    #[test]
    fn explicit_modifier_plane_layout_uses_zero_size() {
        let layout = vk::SubresourceLayout::default()
            .offset(64)
            .row_pitch(4096)
            .size(0);
        assert_eq!(layout.size, 0);
    }

    #[test]
    fn argb8888_reads_as_bgra_in_memory_order() {
        assert!(maps_to(
            overlay_protocol::DRM_FORMAT_ARGB8888,
            vk::Format::B8G8R8A8_UNORM
        ));
        let (blue, green, red, alpha) = (PIXEL[0], PIXEL[1], PIXEL[2], PIXEL[3]);
        assert_eq!((blue, green, red, alpha), (0x10, 0x20, 0x30, 0xFF));
        assert!(!maps_to(
            overlay_protocol::DRM_FORMAT_ARGB8888,
            vk::Format::R8G8B8A8_UNORM
        ));
    }

    #[test]
    fn abgr8888_reads_as_rgba_in_memory_order() {
        assert!(maps_to(
            overlay_protocol::DRM_FORMAT_ABGR8888,
            vk::Format::R8G8B8A8_UNORM
        ));
        assert!(!maps_to(
            overlay_protocol::DRM_FORMAT_ABGR8888,
            vk::Format::B8G8R8A8_UNORM
        ));
    }

    #[test]
    fn fourcc_codes_are_the_ascii_they_claim_to_be() {
        assert_eq!(
            overlay_protocol::DRM_FORMAT_ARGB8888.to_le_bytes(),
            *b"AR24"
        );
        assert_eq!(
            overlay_protocol::DRM_FORMAT_ABGR8888.to_le_bytes(),
            *b"AB24"
        );
    }

    #[test]
    fn unknown_fourcc_is_rejected_rather_than_guessed() {
        assert!(fourcc_format(u32::from_le_bytes(*b"XR24")).is_none());
        assert!(fourcc_format(0).is_none());
    }
}
