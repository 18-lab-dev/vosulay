//! Fail-open Vulkan loader layer for vosulay's in-game compositor.
//!
//! The layer is deliberately a strict pass-through until a host session is
//! authenticated. It owns only loader dispatch metadata on the normal path.

#![allow(clippy::missing_safety_doc)]
mod compositor;

use ash::{
    Device,
    vk::{self, Handle},
};
use compositor::SwapchainState;
use overlay_agent_core::{Agent, AgentConfig, Frame, FrameDescriptor};
use overlay_protocol::{COOKIE_LEN, Endpoint, FrameConsumed, Target};
use std::{
    collections::{HashMap, VecDeque},
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};
const VK_LAYER_LINK_INFO: i32 = 0;
const LOADER_LAYER_INTERFACE_VERSION: u32 = 2;

#[repr(C)]
pub struct NegotiateLayerInterface {
    s_type: u32,
    p_next: *mut c_void,
    loader_layer_interface_version: u32,
    pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pfn_get_physical_device_proc_addr: vk::PFN_vkVoidFunction,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LayerInstanceLink {
    p_next: *mut LayerInstanceLink,
    pfn_next_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pfn_next_get_physical_device_proc_addr: vk::PFN_vkVoidFunction,
}

#[repr(C)]
#[derive(Clone, Copy)]
union LayerInstanceCreateInfoUnion {
    p_layer_info: *mut LayerInstanceLink,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LayerInstanceCreateInfo {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    function: i32,
    u: LayerInstanceCreateInfoUnion,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct LayerDeviceLink {
    p_next: *mut LayerDeviceLink,
    pfn_next_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pfn_next_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
}

#[repr(C)]
#[derive(Clone, Copy)]
union LayerDeviceCreateInfoUnion {
    p_layer_info: *mut LayerDeviceLink,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LayerDeviceCreateInfo {
    s_type: vk::StructureType,
    p_next: *mut c_void,
    function: i32,
    u: LayerDeviceCreateInfoUnion,
}

struct InstanceDispatch {
    next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    instance: ash::Instance,
}

struct DeviceDispatch {
    next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    raw: Device,
    queues: Vec<u64>,
    compositor_enabled: bool,
    get_memory_fd_properties: Option<compositor::GetMemoryFdProperties>,
    get_swapchain_images: Option<vk::PFN_vkGetSwapchainImagesKHR>,
}
impl Clone for DeviceDispatch {
    fn clone(&self) -> Self {
        Self {
            next_get_device_proc_addr: self.next_get_device_proc_addr,
            raw: self.raw.clone(),
            queues: self.queues.clone(),
            compositor_enabled: self.compositor_enabled,
            get_memory_fd_properties: self.get_memory_fd_properties,
            get_swapchain_images: self.get_swapchain_images,
        }
    }
}

#[derive(Default)]
struct DispatchState {
    instances: HashMap<u64, InstanceDispatch>,
    devices: HashMap<u64, DeviceDispatch>,
    queues: HashMap<u64, QueueDispatch>,
    swapchains: HashMap<u64, Arc<Mutex<SwapchainRecord>>>,
    physical_devices: HashMap<u64, u64>,
}
#[derive(Clone, Copy)]
struct QueueDispatch {
    device: u64,
    family: u32,
}

struct SwapchainRecord {
    target_id: u64,
    state: SwapchainState,
}

struct SharedFrame {
    frame: Frame,
    acknowledgements: Arc<Mutex<VecDeque<FrameConsumed>>>,
}
impl Drop for SharedFrame {
    fn drop(&mut self) {
        if let FrameDescriptor::Dmabuf(descriptor) = &self.frame.descriptor
            && let Ok(mut acknowledgements) = self.acknowledgements.lock()
        {
            acknowledgements.push_back(FrameConsumed {
                target_id: descriptor.target_id,
                sequence: descriptor.sequence,
            });
        }
    }
}

enum TargetCommand {
    Upsert(Target),
    Destroy(u64),
}

struct Session {
    latest: Mutex<Option<Arc<SharedFrame>>>,
    acknowledgements: Arc<Mutex<VecDeque<FrameConsumed>>>,
    targets: Mutex<VecDeque<TargetCommand>>,
    connected: AtomicBool,
}

static DISPATCH: OnceLock<Mutex<DispatchState>> = OnceLock::new();
static SESSION: OnceLock<Arc<Session>> = OnceLock::new();
static NEXT_TARGET_ID: AtomicU64 = AtomicU64::new(1);

fn dispatch() -> &'static Mutex<DispatchState> {
    DISPATCH.get_or_init(|| Mutex::new(DispatchState::default()))
}
fn session() -> &'static Arc<Session> {
    SESSION.get_or_init(|| {
        let acknowledgements = Arc::new(Mutex::new(VecDeque::new()));
        let session = Arc::new(Session {
            latest: Mutex::new(None),
            acknowledgements,
            targets: Mutex::new(VecDeque::new()),
            connected: AtomicBool::new(false),
        });
        if let Some((endpoint, cookie)) = session_configuration() {
            let thread_session = session.clone();
            let _ = thread::Builder::new()
                .name("vosulay-vulkan-control".into())
                .spawn(move || run_session(thread_session, endpoint, cookie));
        }
        session
    })
}

fn session_configuration() -> Option<(Endpoint, [u8; COOKIE_LEN])> {
    let name = decode_hex(&std::env::var("VOSULAY_AGENT_SOCKET").ok()?)?;
    let endpoint = Endpoint::from_name(&name).ok()?;
    let cookie: [u8; COOKIE_LEN] = decode_hex(&std::env::var("VOSULAY_AGENT_COOKIE").ok()?)?
        .try_into()
        .ok()?;
    Some((endpoint, cookie))
}

fn run_session(session: Arc<Session>, endpoint: Endpoint, cookie: [u8; COOKIE_LEN]) {
    let mut config = AgentConfig::current();
    if let Ok(pid) = std::env::var("VOSULAY_AGENT_HOST_PID").and_then(|value| {
        value
            .parse::<u32>()
            .map_err(|_| std::env::VarError::NotPresent)
    }) {
        config.expected_host_pid = pid;
    }
    let Ok(mut agent) = Agent::connect(&endpoint, cookie, config) else {
        return;
    };
    if agent.set_nonblocking().is_err() {
        return;
    }
    session.connected.store(true, Ordering::Release);
    loop {
        if let Ok(mut acknowledgements) = session.acknowledgements.lock() {
            while let Some(consumed) = acknowledgements.pop_front() {
                if agent.send_consumed(consumed).is_err() {
                    session.connected.store(false, Ordering::Release);
                    return;
                }
            }
        }
        if let Ok(mut targets) = session.targets.lock() {
            while let Some(command) = targets.pop_front() {
                let result = match command {
                    TargetCommand::Upsert(target) => agent.send_target_create(target),
                    TargetCommand::Destroy(target_id) => agent.send_target_destroy(target_id),
                };
                if result.is_err() {
                    session.connected.store(false, Ordering::Release);
                    return;
                }
            }
        }
        let _ = agent.watchdog_tick();
        match agent.poll() {
            Ok(overlay_agent_core::AgentEvent::Frame(_)) => {
                if let Some(frame) = agent.take_latest()
                    && let Ok(mut latest) = session.latest.lock()
                {
                    *latest = Some(Arc::new(SharedFrame {
                        frame,
                        acknowledgements: session.acknowledgements.clone(),
                    }));
                }
            }
            Ok(
                overlay_agent_core::AgentEvent::Visible(false)
                | overlay_agent_core::AgentEvent::Clear(_),
            ) => {
                if let Ok(mut latest) = session.latest.lock() {
                    latest.take();
                }
            }
            Ok(overlay_agent_core::AgentEvent::Shutdown)
            | Err(overlay_agent_core::AgentError::Disconnected) => break,
            Err(overlay_agent_core::AgentError::Protocol(overlay_protocol::ProtocolError::Io(
                error,
            ))) if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) => {}
            Err(_) => break,
            Ok(_) => {}
        }
        thread::sleep(Duration::from_millis(2));
    }
    session.connected.store(false, Ordering::Release);
    if let Ok(mut latest) = session.latest.lock() {
        latest.take();
    }
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

fn diagnostics(message: &str) {
    if std::env::var_os("VOSULAY_NATIVE_DIAGNOSTICS").is_some() {
        eprintln!("vosulay Vulkan layer: {message}");
    }
}

unsafe fn void_function<T: Copy>(function: T) -> vk::PFN_vkVoidFunction {
    debug_assert_eq!(
        std::mem::size_of::<T>(),
        std::mem::size_of::<vk::PFN_vkVoidFunction>()
    );
    unsafe { std::mem::transmute_copy(&function) }
}

unsafe fn typed_function<T: Copy>(function: vk::PFN_vkVoidFunction) -> Option<T> {
    function.map(|function| unsafe { std::mem::transmute_copy(&function) })
}

unsafe fn name<'a>(pointer: *const c_char) -> Option<&'a CStr> {
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(pointer) })
    }
}

unsafe fn instance_link_info(
    create_info: *const vk::InstanceCreateInfo<'_>,
) -> Option<NonNull<LayerInstanceCreateInfo>> {
    let mut next = unsafe { create_info.as_ref() }?.p_next;
    while !next.is_null() {
        let base = unsafe { &*(next.cast::<vk::BaseInStructure<'_>>()) };
        if base.s_type == vk::StructureType::LOADER_INSTANCE_CREATE_INFO {
            let layer = NonNull::new(next.cast_mut().cast::<LayerInstanceCreateInfo>())?;
            if unsafe { layer.as_ref() }.function == VK_LAYER_LINK_INFO {
                return Some(layer);
            }
        }
        next = base.p_next.cast();
    }
    None
}

unsafe fn device_link_info(
    create_info: *const vk::DeviceCreateInfo<'_>,
) -> Option<NonNull<LayerDeviceCreateInfo>> {
    let mut next = unsafe { create_info.as_ref() }?.p_next;
    while !next.is_null() {
        let base = unsafe { &*(next.cast::<vk::BaseInStructure<'_>>()) };
        if base.s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
            let layer = NonNull::new(next.cast_mut().cast::<LayerDeviceCreateInfo>())?;
            if unsafe { layer.as_ref() }.function == VK_LAYER_LINK_INFO {
                return Some(layer);
            }
        }
        next = base.p_next.cast();
    }
    None
}

unsafe fn next_instance_proc(
    instance: vk::Instance,
    proc_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    let next = dispatch().lock().ok().and_then(|state| {
        state
            .instances
            .get(&instance.as_raw())
            .map(|state| state.next_get_instance_proc_addr)
    });
    next.and_then(|next| unsafe { next(instance, proc_name) })
}

unsafe fn next_device_proc(device: vk::Device, proc_name: *const c_char) -> vk::PFN_vkVoidFunction {
    let next = dispatch()
        .lock()
        .ok()
        .and_then(|state| state.devices.get(&device.as_raw()).cloned())
        .map(|state| state.next_get_device_proc_addr);
    next.and_then(|next| unsafe { next(device, proc_name) })
}

#[unsafe(export_name = "vkNegotiateLoaderLayerInterfaceVersion")]
pub unsafe extern "system" fn negotiate_loader_layer_interface_version(
    version: *mut NegotiateLayerInterface,
) -> vk::Result {
    std::panic::catch_unwind(|| {
        let Some(version) = (unsafe { version.as_mut() }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        if version.loader_layer_interface_version == 0 {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        version.loader_layer_interface_version = version
            .loader_layer_interface_version
            .min(LOADER_LAYER_INTERFACE_VERSION);
        version.pfn_get_instance_proc_addr = Some(layer_get_instance_proc_addr);
        version.pfn_get_device_proc_addr = Some(layer_get_device_proc_addr);
        version.pfn_get_physical_device_proc_addr = None;
        diagnostics("loader interface negotiated");
        vk::Result::SUCCESS
    })
    .unwrap_or(vk::Result::ERROR_INITIALIZATION_FAILED)
}
#[unsafe(export_name = "vkGetInstanceProcAddr")]
pub unsafe extern "system" fn exported_get_instance_proc_addr(
    instance: vk::Instance,
    proc_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    unsafe { layer_get_instance_proc_addr(instance, proc_name) }
}

#[unsafe(export_name = "vkGetDeviceProcAddr")]
pub unsafe extern "system" fn exported_get_device_proc_addr(
    device: vk::Device,
    proc_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    unsafe { layer_get_device_proc_addr(device, proc_name) }
}

unsafe extern "system" fn layer_get_instance_proc_addr(
    instance: vk::Instance,
    proc_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    std::panic::catch_unwind(|| {
        let proc_name_value = (unsafe { name(proc_name) })?;
        match proc_name_value.to_bytes() {
            b"vkGetInstanceProcAddr" => unsafe {
                void_function(layer_get_instance_proc_addr as vk::PFN_vkGetInstanceProcAddr)
            },
            b"vkGetDeviceProcAddr" => unsafe {
                void_function(layer_get_device_proc_addr as vk::PFN_vkGetDeviceProcAddr)
            },
            b"vkCreateInstance" => unsafe {
                void_function(layer_create_instance as vk::PFN_vkCreateInstance)
            },
            b"vkDestroyInstance" => unsafe {
                void_function(layer_destroy_instance as vk::PFN_vkDestroyInstance)
            },
            b"vkEnumeratePhysicalDevices" => unsafe {
                void_function(
                    layer_enumerate_physical_devices as vk::PFN_vkEnumeratePhysicalDevices,
                )
            },
            b"vkCreateDevice" => unsafe {
                void_function(layer_create_device as vk::PFN_vkCreateDevice)
            },
            _ if instance != vk::Instance::null() => unsafe {
                next_instance_proc(instance, proc_name)
            },
            _ => None,
        }
    })
    .unwrap_or(None)
}

unsafe extern "system" fn layer_get_device_proc_addr(
    device: vk::Device,
    proc_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    std::panic::catch_unwind(|| {
        let proc_name_value = (unsafe { name(proc_name) })?;
        match proc_name_value.to_bytes() {
            b"vkGetDeviceProcAddr" => unsafe {
                void_function(layer_get_device_proc_addr as vk::PFN_vkGetDeviceProcAddr)
            },
            b"vkDestroyDevice" => unsafe {
                void_function(layer_destroy_device as vk::PFN_vkDestroyDevice)
            },
            b"vkCreateSwapchainKHR" => unsafe {
                void_function(layer_create_swapchain as vk::PFN_vkCreateSwapchainKHR)
            },
            b"vkDestroySwapchainKHR" => unsafe {
                void_function(layer_destroy_swapchain as vk::PFN_vkDestroySwapchainKHR)
            },
            b"vkQueuePresentKHR" => unsafe {
                void_function(layer_queue_present as vk::PFN_vkQueuePresentKHR)
            },
            _ if device != vk::Device::null() => unsafe { next_device_proc(device, proc_name) },
            _ => None,
        }
    })
    .unwrap_or(None)
}

unsafe extern "system" fn layer_create_instance(
    create_info: *const vk::InstanceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    instance: *mut vk::Instance,
) -> vk::Result {
    std::panic::catch_unwind(|| {
        let Some(mut layer_info) = (unsafe { instance_link_info(create_info) }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = unsafe { layer_info.as_ref().u.p_layer_info.as_ref() };
        let Some(link) = link else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next_get_instance_proc_addr) = link.pfn_next_get_instance_proc_addr else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        unsafe {
            layer_info.as_mut().u.p_layer_info = link.p_next;
        }
        let create = unsafe {
            typed_function::<vk::PFN_vkCreateInstance>(next_get_instance_proc_addr(
                vk::Instance::null(),
                c"vkCreateInstance".as_ptr(),
            ))
        };
        let Some(create) = create else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = unsafe { create(create_info, allocator, instance) };
        if result == vk::Result::SUCCESS {
            let Some(instance) = (unsafe { instance.as_ref() }).copied() else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            if let Ok(mut state) = dispatch().lock() {
                state.instances.insert(
                    instance.as_raw(),
                    InstanceDispatch {
                        next_get_instance_proc_addr,
                        instance: unsafe {
                            ash::Instance::load_with(
                                |name| {
                                    next_get_instance_proc_addr(instance, name.as_ptr())
                                        .map_or(std::ptr::null(), |function| {
                                            function as *const c_void
                                        })
                                },
                                instance,
                            )
                        },
                    },
                );
            }
            diagnostics("instance created");
        }
        result
    })
    .unwrap_or(vk::Result::ERROR_INITIALIZATION_FAILED)
}

unsafe extern "system" fn layer_destroy_instance(
    instance: vk::Instance,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let _ = std::panic::catch_unwind(|| {
        let removed = dispatch().lock().ok().and_then(|mut state| {
            state
                .physical_devices
                .retain(|_, owner| *owner != instance.as_raw());
            state.instances.remove(&instance.as_raw())
        });
        if let Some(removed) = removed {
            let next = unsafe {
                typed_function::<vk::PFN_vkDestroyInstance>((removed.next_get_instance_proc_addr)(
                    instance,
                    c"vkDestroyInstance".as_ptr(),
                ))
            };
            if let Some(next) = next {
                unsafe { next(instance, allocator) };
            }
        }
        diagnostics("instance destroyed");
    });
}

unsafe extern "system" fn layer_enumerate_physical_devices(
    instance: vk::Instance,
    count: *mut u32,
    devices: *mut vk::PhysicalDevice,
) -> vk::Result {
    std::panic::catch_unwind(|| {
        let next = unsafe {
            typed_function::<vk::PFN_vkEnumeratePhysicalDevices>(next_instance_proc(
                instance,
                c"vkEnumeratePhysicalDevices".as_ptr(),
            ))
        };
        let Some(next) = next else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = unsafe { next(instance, count, devices) };
        if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
            && !devices.is_null()
            && let Some(count) = unsafe { count.as_ref() }.copied()
            && let Ok(mut state) = dispatch().lock()
        {
            let returned = unsafe { std::slice::from_raw_parts(devices, count as usize) };
            for physical_device in returned {
                state
                    .physical_devices
                    .insert(physical_device.as_raw(), instance.as_raw());
            }
        }
        result
    })
    .unwrap_or(vk::Result::ERROR_INITIALIZATION_FAILED)
}

unsafe extern "system" fn layer_create_device(
    physical_device: vk::PhysicalDevice,
    create_info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    std::panic::catch_unwind(|| {
        let Some(mut layer_info) = (unsafe { device_link_info(create_info) }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let link = unsafe { layer_info.as_ref().u.p_layer_info.as_ref() };
        let Some(link) = link else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next_get_instance_proc_addr) = link.pfn_next_get_instance_proc_addr else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next_get_device_proc_addr) = link.pfn_next_get_device_proc_addr else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        unsafe {
            layer_info.as_mut().u.p_layer_info = link.p_next;
        }
        let instance = dispatch().lock().ok().and_then(|state| {
            let instance = *state.physical_devices.get(&physical_device.as_raw())?;
            state
                .instances
                .get(&instance)
                .map(|dispatch| dispatch.instance.clone())
        });
        let create = unsafe {
            typed_function::<vk::PFN_vkCreateDevice>(next_get_instance_proc_addr(
                instance
                    .as_ref()
                    .map_or(vk::Instance::null(), |instance| instance.handle()),
                c"vkCreateDevice".as_ptr(),
            ))
        };
        let Some(create) = create else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let original_create_info = unsafe { create_info.as_ref() };
        let required_extensions = [
            vk::KHR_EXTERNAL_MEMORY_FD_NAME,
            vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME,
            vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME,
            vk::EXT_QUEUE_FAMILY_FOREIGN_NAME,
        ];
        let mut extension_names = Vec::new();
        let mut compositor_enabled = false;
        let original_extensions = original_create_info.and_then(|original| {
            if original.enabled_extension_count == 0 {
                Some(&[][..])
            } else if original.pp_enabled_extension_names.is_null() {
                None
            } else {
                Some(unsafe {
                    std::slice::from_raw_parts(
                        original.pp_enabled_extension_names,
                        original.enabled_extension_count as usize,
                    )
                })
            }
        });
        if session_configuration().is_some()
            && let (Some(instance), Some(enabled)) = (&instance, original_extensions)
            && let Ok(supported) =
                unsafe { instance.enumerate_device_extension_properties(physical_device) }
            && required_extensions.iter().all(|required| {
                supported
                    .iter()
                    .any(|property| property.extension_name_as_c_str().ok() == Some(*required))
            })
        {
            extension_names.extend(enabled.iter().copied());
            for required in required_extensions {
                if !extension_names
                    .iter()
                    .any(|name| unsafe { CStr::from_ptr(*name) } == required)
                {
                    extension_names.push(required.as_ptr());
                }
            }
            compositor_enabled = true;
        }
        let mut augmented_create_info = original_create_info.copied().unwrap_or_default();
        if compositor_enabled {
            augmented_create_info.enabled_extension_count = extension_names.len() as u32;
            augmented_create_info.pp_enabled_extension_names = extension_names.as_ptr();
        }
        let forwarded_create_info = if compositor_enabled {
            &raw const augmented_create_info
        } else {
            create_info
        };
        let result = unsafe { create(physical_device, forwarded_create_info, allocator, device) };
        if result != vk::Result::SUCCESS {
            return result;
        }
        let Some(device) = (unsafe { device.as_ref() }).copied() else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let get_queue = unsafe {
            typed_function::<vk::PFN_vkGetDeviceQueue>(next_get_device_proc_addr(
                device,
                c"vkGetDeviceQueue".as_ptr(),
            ))
        };
        let mut queues = Vec::new();
        if let (Some(get_queue), Some(create_info)) = (get_queue, unsafe { create_info.as_ref() }) {
            let queue_infos = if create_info.queue_create_info_count == 0
                || create_info.p_queue_create_infos.is_null()
            {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        create_info.p_queue_create_infos,
                        create_info.queue_create_info_count as usize,
                    )
                }
            };
            for queue_info in queue_infos {
                for queue_index in 0..queue_info.queue_count {
                    let mut queue = vk::Queue::null();
                    unsafe {
                        get_queue(
                            device,
                            queue_info.queue_family_index,
                            queue_index,
                            &mut queue,
                        )
                    };
                    if queue != vk::Queue::null() {
                        queues.push((queue.as_raw(), queue_info.queue_family_index));
                    }
                }
            }
        }
        if let Ok(mut state) = dispatch().lock() {
            for (queue, family) in &queues {
                state.queues.insert(
                    *queue,
                    QueueDispatch {
                        device: device.as_raw(),
                        family: *family,
                    },
                );
            }
            state.devices.insert(
                device.as_raw(),
                DeviceDispatch {
                    next_get_device_proc_addr,
                    raw: unsafe {
                        Device::load_with(
                            |name| {
                                (next_get_device_proc_addr)(device, name.as_ptr())
                                    .map_or(std::ptr::null(), |function| function as *const c_void)
                            },
                            device,
                        )
                    },
                    queues: queues.iter().map(|(queue, _)| *queue).collect(),
                    compositor_enabled,
                    get_memory_fd_properties: if compositor_enabled {
                        unsafe {
                            typed_function::<vk::PFN_vkGetMemoryFdPropertiesKHR>(
                                next_get_device_proc_addr(
                                    device,
                                    c"vkGetMemoryFdPropertiesKHR".as_ptr(),
                                ),
                            )
                        }
                    } else {
                        None
                    },
                    get_swapchain_images: if compositor_enabled {
                        unsafe {
                            typed_function::<vk::PFN_vkGetSwapchainImagesKHR>(
                                next_get_device_proc_addr(
                                    device,
                                    c"vkGetSwapchainImagesKHR".as_ptr(),
                                ),
                            )
                        }
                    } else {
                        None
                    },
                },
            );
        }
        diagnostics("device created");
        result
    })
    .unwrap_or(vk::Result::ERROR_INITIALIZATION_FAILED)
}

unsafe extern "system" fn layer_destroy_device(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let _ = std::panic::catch_unwind(|| {
        let removed = dispatch().lock().ok().and_then(|mut state| {
            let device_dispatch = state.devices.remove(&device.as_raw())?;
            for queue in &device_dispatch.queues {
                state.queues.remove(queue);
            }
            let handles = state
                .swapchains
                .iter()
                .filter_map(|(handle, record)| {
                    record
                        .lock()
                        .ok()
                        .and_then(|record| (record.state.device == device).then_some(*handle))
                })
                .collect::<Vec<_>>();
            let swapchains = handles
                .into_iter()
                .filter_map(|handle| state.swapchains.remove(&handle))
                .collect::<Vec<_>>();
            Some((device_dispatch, swapchains))
        });
        if let Some((removed, swapchains)) = removed {
            for record in swapchains {
                if let Ok(mut record) = record.lock() {
                    unsafe { record.state.destroy(&removed.raw) };
                    if let Ok(mut commands) = session().targets.lock() {
                        commands.push_back(TargetCommand::Destroy(record.target_id));
                    }
                }
            }
            let next = unsafe {
                typed_function::<vk::PFN_vkDestroyDevice>((removed.next_get_device_proc_addr)(
                    device,
                    c"vkDestroyDevice".as_ptr(),
                ))
            };
            if let Some(next) = next {
                unsafe { next(device, allocator) };
            }
        }
        diagnostics("device destroyed");
    });
}

unsafe extern "system" fn layer_create_swapchain(
    device: vk::Device,
    create_info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    swapchain: *mut vk::SwapchainKHR,
) -> vk::Result {
    std::panic::catch_unwind(|| {
        let next = unsafe {
            typed_function::<vk::PFN_vkCreateSwapchainKHR>(next_device_proc(
                device,
                c"vkCreateSwapchainKHR".as_ptr(),
            ))
        };
        let Some(next) = next else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = unsafe { next(device, create_info, allocator, swapchain) };
        if result == vk::Result::SUCCESS {
            let Some(created) = (unsafe { swapchain.as_ref() }).copied() else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let Some(create_info) = (unsafe { create_info.as_ref() }) else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let device_dispatch = dispatch()
                .lock()
                .ok()
                .and_then(|state| state.devices.get(&device.as_raw()).cloned());
            if let Some(device_dispatch) = device_dispatch
                && device_dispatch.compositor_enabled
                && let Some(get_images) = device_dispatch.get_swapchain_images
            {
                let mut count = 0;
                let first =
                    unsafe { get_images(device, created, &mut count, std::ptr::null_mut()) };
                if matches!(first, vk::Result::SUCCESS | vk::Result::INCOMPLETE) && count > 0 {
                    let mut images = vec![vk::Image::null(); count as usize];
                    let second =
                        unsafe { get_images(device, created, &mut count, images.as_mut_ptr()) };
                    if matches!(second, vk::Result::SUCCESS | vk::Result::INCOMPLETE) {
                        images.truncate(count as usize);
                        let target_id = NEXT_TARGET_ID.fetch_add(1, Ordering::Relaxed);
                        if let Ok(mut state) = dispatch().lock() {
                            state.swapchains.insert(
                                created.as_raw(),
                                Arc::new(Mutex::new(SwapchainRecord {
                                    target_id,
                                    state: SwapchainState::new(
                                        device,
                                        create_info.image_format,
                                        create_info.image_extent,
                                        images,
                                    ),
                                })),
                            );
                        }
                        if let Ok(mut commands) = session().targets.lock() {
                            commands.push_back(TargetCommand::Upsert(Target {
                                target_id,
                                width: create_info.image_extent.width,
                                height: create_info.image_extent.height,
                            }));
                        }
                    }
                }
            }
            diagnostics("swapchain created");
        }
        result
    })
    .unwrap_or(vk::Result::ERROR_INITIALIZATION_FAILED)
}

unsafe extern "system" fn layer_destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let _ = std::panic::catch_unwind(|| {
        let removed = dispatch().lock().ok().and_then(|mut state| {
            let record = state.swapchains.remove(&swapchain.as_raw())?;
            let device_dispatch = state.devices.get(&device.as_raw()).cloned();
            Some((record, device_dispatch))
        });
        if let Some((record, device_dispatch)) = removed
            && let Ok(mut record) = record.lock()
        {
            if let Some(device_dispatch) = device_dispatch {
                unsafe { record.state.destroy(&device_dispatch.raw) };
            }
            if let Ok(mut commands) = session().targets.lock() {
                commands.push_back(TargetCommand::Destroy(record.target_id));
            }
        }
        let next = unsafe {
            typed_function::<vk::PFN_vkDestroySwapchainKHR>(next_device_proc(
                device,
                c"vkDestroySwapchainKHR".as_ptr(),
            ))
        };
        if let Some(next) = next {
            unsafe { next(device, swapchain, allocator) };
        }
        diagnostics("swapchain destroyed");
    });
}

unsafe extern "system" fn layer_queue_present(
    queue: vk::Queue,
    present_info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    std::panic::catch_unwind(|| {
        let Some(original) = (unsafe { present_info.as_ref() }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let queue_dispatch = dispatch()
            .lock()
            .ok()
            .and_then(|state| state.queues.get(&queue.as_raw()).copied());
        let Some(queue_dispatch) = queue_dispatch else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        let device = vk::Device::from_raw(queue_dispatch.device);
        let next = unsafe {
            typed_function::<vk::PFN_vkQueuePresentKHR>(next_device_proc(
                device,
                c"vkQueuePresentKHR".as_ptr(),
            ))
        };
        let Some(next) = next else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        let swapchains = if original.swapchain_count == 0 {
            &[]
        } else if original.p_swapchains.is_null() || original.p_image_indices.is_null() {
            return unsafe { next(queue, present_info) };
        } else {
            unsafe {
                std::slice::from_raw_parts(original.p_swapchains, original.swapchain_count as usize)
            }
        };
        let image_indices = if original.swapchain_count == 0 {
            &[]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    original.p_image_indices,
                    original.swapchain_count as usize,
                )
            }
        };
        let waits = if original.wait_semaphore_count == 0 {
            &[]
        } else if original.p_wait_semaphores.is_null() {
            return unsafe { next(queue, present_info) };
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    original.p_wait_semaphores,
                    original.wait_semaphore_count as usize,
                )
            }
        };
        let latest = session()
            .latest
            .lock()
            .ok()
            .and_then(|latest| latest.clone());
        let mut overlay_wait = None;
        if let Some(frame) = latest {
            let frame_target = frame.frame.descriptor.target_id();
            let device_and_records = dispatch().lock().ok().and_then(|state| {
                let device_dispatch = state.devices.get(&queue_dispatch.device).cloned()?;
                let records = swapchains
                    .iter()
                    .zip(image_indices)
                    .filter_map(|(swapchain, image_index)| {
                        state
                            .swapchains
                            .get(&swapchain.as_raw())
                            .cloned()
                            .map(|record| (*image_index, record))
                    })
                    .collect::<Vec<_>>();
                Some((device_dispatch, records))
            });
            if let Some((device_dispatch, records)) = device_and_records
                && let Some(get_memory_fd_properties) = device_dispatch.get_memory_fd_properties
            {
                for (image_index, record) in records {
                    let Ok(mut record) = record.lock() else {
                        continue;
                    };
                    if record.target_id != frame_target {
                        continue;
                    }
                    if let Ok(compositor::DrawOutcome::Submitted(semaphore)) = unsafe {
                        record.state.draw(
                            &device_dispatch.raw,
                            queue,
                            queue_dispatch.family,
                            image_index,
                            waits,
                            get_memory_fd_properties,
                            frame.clone(),
                        )
                    } {
                        overlay_wait = Some(semaphore);
                    }
                    break;
                }
            }
        }
        let Some(overlay_wait) = overlay_wait else {
            diagnostics("queue present dispatched");
            return unsafe { next(queue, present_info) };
        };
        let mut augmented = *original;
        augmented.wait_semaphore_count = 1;
        augmented.p_wait_semaphores = &overlay_wait;
        diagnostics("queue present composited");
        unsafe { next(queue, &augmented) }
    })
    .unwrap_or(vk::Result::ERROR_DEVICE_LOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_negotiation_is_rejected() {
        let result = unsafe { negotiate_loader_layer_interface_version(std::ptr::null_mut()) };
        assert_eq!(result, vk::Result::ERROR_INITIALIZATION_FAILED);
    }

    #[test]
    fn negotiation_installs_dispatch_entrypoints() {
        let mut interface = NegotiateLayerInterface {
            s_type: 1,
            p_next: std::ptr::null_mut(),
            loader_layer_interface_version: 2,
            pfn_get_instance_proc_addr: None,
            pfn_get_device_proc_addr: None,
            pfn_get_physical_device_proc_addr: None,
        };
        let result = unsafe { negotiate_loader_layer_interface_version(&mut interface) };
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(interface.loader_layer_interface_version, 2);
        assert!(interface.pfn_get_instance_proc_addr.is_some());
        assert!(interface.pfn_get_device_proc_addr.is_some());
    }
}
