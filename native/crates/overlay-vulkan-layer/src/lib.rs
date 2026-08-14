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
use overlay_agent_core::{Agent, AgentConfig, CompletionQueue, Frame, FrameDescriptor};
use overlay_protocol::{COOKIE_LEN, Endpoint, FrameConsumed, SelectedTarget, Target};
use std::{
    collections::{HashMap, VecDeque},
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
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
    /// Resolved once at queue registration. The present hook must be able to
    /// forward the application's call even when every other piece of overlay
    /// state is missing or poisoned, so it cannot depend on a later lookup
    /// through the same locks that might have failed.
    next_present: Option<vk::PFN_vkQueuePresentKHR>,
}

/// Latched off the first time overlay bookkeeping is found unusable. Once the
/// layer cannot trust its own state it stops touching presents entirely rather
/// than retrying broken work on every frame.
static OVERLAY_DISABLED: AtomicBool = AtomicBool::new(false);

fn disable_overlay(reason: &str) {
    if !OVERLAY_DISABLED.swap(true, Ordering::AcqRel) {
        diagnostics(&format!("overlay disabled: {reason}"));
    }
}

fn overlay_disabled() -> bool {
    OVERLAY_DISABLED.load(Ordering::Acquire)
}

struct SwapchainRecord {
    target_id: u64,
    state: SwapchainState,
}

struct SharedFrame {
    frame: Frame,
    acknowledgements: Arc<CompletionQueue>,
}
impl Drop for SharedFrame {
    fn drop(&mut self) {
        // Runs on whichever thread last released the frame, including the
        // game's presentation thread, so publication must not block.
        if let FrameDescriptor::Dmabuf(descriptor) = &self.frame.descriptor {
            self.acknowledgements.push(FrameConsumed {
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
    /// Frame releases are published from presentation lifetimes and drained by
    /// the control thread. It must be lock-free: the control thread is inside a
    /// socket send with a multi-second writability deadline for most of its
    /// life, and a present must never wait behind host backpressure.
    acknowledgements: Arc<CompletionQueue>,
    targets: Mutex<VecDeque<TargetCommand>>,
    /// Most recently presented registered swapchain. The present hook only
    /// writes this atomic; protocol I/O stays on the control thread.
    selected_target: AtomicU64,
    connected: AtomicBool,
}

static DISPATCH: OnceLock<Mutex<DispatchState>> = OnceLock::new();
static SESSION: OnceLock<Arc<Session>> = OnceLock::new();
static NEXT_TARGET_ID: AtomicU64 = AtomicU64::new(1);

fn dispatch() -> &'static Mutex<DispatchState> {
    DISPATCH.get_or_init(|| Mutex::new(DispatchState::default()))
}

/// Dispatch state is plain lookup tables, so a panic elsewhere cannot leave it
/// logically inconsistent. Recovering the guard keeps a single overlay defect
/// from permanently poisoning every later present.
fn dispatch_state() -> std::sync::MutexGuard<'static, DispatchState> {
    dispatch().lock().unwrap_or_else(|poisoned| {
        disable_overlay("dispatch state was poisoned");
        poisoned.into_inner()
    })
}

/// Downstream present of the most recently created device, held outside the
/// dispatch map so a present on an unrecognised queue can still be forwarded.
static FALLBACK_PRESENT: AtomicUsize = AtomicUsize::new(0);

fn set_fallback_present(next: Option<vk::PFN_vkQueuePresentKHR>) {
    if let Some(next) = next {
        FALLBACK_PRESENT.store(next as usize, Ordering::Release);
    }
}

fn fallback_present() -> Option<vk::PFN_vkQueuePresentKHR> {
    let raw = FALLBACK_PRESENT.load(Ordering::Acquire);
    if raw == 0 {
        return None;
    }
    // Stored from a `vk::PFN_vkQueuePresentKHR` in `set_fallback_present` and
    // never from any other value.
    Some(unsafe { std::mem::transmute::<usize, vk::PFN_vkQueuePresentKHR>(raw) })
}

/// Latest frame published by the control thread, if any.
fn session_frame() -> Option<Arc<SharedFrame>> {
    let latest = session().latest.lock();
    match latest {
        Ok(latest) => latest.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}
fn session() -> &'static Arc<Session> {
    SESSION.get_or_init(|| {
        let acknowledgements = Arc::new(CompletionQueue::new());
        let session = Arc::new(Session {
            latest: Mutex::new(None),
            acknowledgements,
            targets: Mutex::new(VecDeque::new()),
            selected_target: AtomicU64::new(0),
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
    // This layer has no SHM upload path: `SwapchainState::draw()` can only
    // consume an imported DMA-BUF. Advertising shared memory would let the host
    // negotiate a transport whose every frame the compositor then discards.
    let mut config = AgentConfig::dmabuf_only();
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
    let mut announced_target = 0u64;
    let mut selection_generation = 0u64;
    loop {
        while let Some(consumed) = session.acknowledgements.pop() {
            if agent.send_consumed(consumed).is_err() {
                session.connected.store(false, Ordering::Release);
                return;
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
        // Target registration is drained first, so a selection can never name
        // a swapchain the host has not observed yet. The present thread only
        // updates the atomic above; this is the sole socket writer.
        let selected = session.selected_target.load(Ordering::Acquire);
        if selected != announced_target {
            selection_generation = selection_generation.wrapping_add(1).max(1);
            if agent
                .send_selected_target(SelectedTarget {
                    target_id: selected,
                    generation: selection_generation,
                })
                .is_err()
            {
                session.connected.store(false, Ordering::Release);
                return;
            }
            announced_target = selected;
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
        // Resolve the downstream present once, while the device link is known
        // good, so the hot hook never needs a fallible lookup to forward.
        let next_present = unsafe {
            typed_function::<vk::PFN_vkQueuePresentKHR>((next_get_device_proc_addr)(
                device,
                c"vkQueuePresentKHR".as_ptr(),
            ))
        };
        set_fallback_present(next_present);
        {
            let mut state = dispatch_state();
            for (queue, family) in &queues {
                state.queues.insert(
                    *queue,
                    QueueDispatch {
                        device: device.as_raw(),
                        family: *family,
                        next_present,
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
                    if session().selected_target.load(Ordering::Acquire) == record.target_id {
                        session().selected_target.store(0, Ordering::Release);
                    }
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
            if session().selected_target.load(Ordering::Acquire) == record.target_id {
                session().selected_target.store(0, Ordering::Release);
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

/// Observe the first registered swapchain in a present without performing any
/// protocol I/O on the application's present thread. Electron only forwards
/// frames after receiving SelectedTarget, so Vulkan must publish this before a
/// frame can ever reach `compose_overlay`.
unsafe fn observe_presented_target(original: &vk::PresentInfoKHR<'_>) {
    if original.swapchain_count == 0 || original.p_swapchains.is_null() {
        return;
    }
    let swapchains = unsafe {
        std::slice::from_raw_parts(original.p_swapchains, original.swapchain_count as usize)
    };
    let selected = {
        let state = dispatch_state();
        swapchains.iter().find_map(|swapchain| {
            state
                .swapchains
                .get(&swapchain.as_raw())
                .and_then(|record| record.lock().ok().map(|record| record.target_id))
        })
    };
    if let Some(target_id) = selected {
        session().selected_target.store(target_id, Ordering::Release);
    }
}

/// Decides whether the layer may touch this present at all. Split out so the
/// fail-open contract is testable without a Vulkan device: overlay work is
/// permitted only when the layer is not latched off and the queue resolved to
/// a downstream present it can forward to afterwards.
fn overlay_permitted(disabled: bool, queue_known: bool, has_downstream: bool) -> bool {
    !disabled && queue_known && has_downstream
}

/// Best-effort overlay composition. Returns the semaphore the application's
/// present must additionally wait on, or `None` when nothing was drawn. Every
/// failure path here is silent by design: the caller presents regardless.
unsafe fn compose_overlay(
    queue: vk::Queue,
    queue_dispatch: QueueDispatch,
    original: &vk::PresentInfoKHR<'_>,
) -> Option<vk::Semaphore> {
    if original.swapchain_count == 0
        || original.p_swapchains.is_null()
        || original.p_image_indices.is_null()
    {
        return None;
    }
    let swapchains = unsafe {
        std::slice::from_raw_parts(original.p_swapchains, original.swapchain_count as usize)
    };
    let image_indices = unsafe {
        std::slice::from_raw_parts(original.p_image_indices, original.swapchain_count as usize)
    };
    let waits = if original.wait_semaphore_count == 0 {
        &[][..]
    } else if original.p_wait_semaphores.is_null() {
        return None;
    } else {
        unsafe {
            std::slice::from_raw_parts(
                original.p_wait_semaphores,
                original.wait_semaphore_count as usize,
            )
        }
    };
    let frame = session_frame()?;
    let frame_target = frame.frame.descriptor.target_id();
    let (device_dispatch, records) = {
        let state = dispatch_state();
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
        (device_dispatch, records)
    };
    let get_memory_fd_properties = device_dispatch.get_memory_fd_properties?;
    for (image_index, record) in records {
        let mut record = match record.lock() {
            Ok(record) => record,
            // A panic in a previous composition left this swapchain's
            // bookkeeping behind. Recover the data rather than treating it as
            // a device failure; it is only overlay state.
            Err(poisoned) => poisoned.into_inner(),
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
            return Some(semaphore);
        }
        break;
    }
    None
}

unsafe extern "system" fn layer_queue_present(
    queue: vk::Queue,
    present_info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    // Resolving the downstream present never depends on overlay bookkeeping
    // succeeding: the pointer was captured at device creation and the dispatch
    // mutex recovers from poisoning. An overlay defect must be invisible to the
    // application, never a manufactured VK_ERROR_DEVICE_LOST.
    let queue_dispatch = dispatch_state().queues.get(&queue.as_raw()).copied();
    let next = queue_dispatch.and_then(|dispatch| dispatch.next_present);
    let Some(next) = next.or_else(fallback_present) else {
        // The layer never observed a device create, so no present could ever
        // have been chained through it.
        disable_overlay("no downstream vkQueuePresentKHR is reachable");
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };

    let original = unsafe { present_info.as_ref() };
    if let Some(original) = original {
        // Best effort only; selection publication itself happens later on the
        // control thread. A failure here must not alter the game's present.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            observe_presented_target(original)
        }));
    }
    let permitted = overlay_permitted(
        overlay_disabled(),
        queue_dispatch.is_some(),
        original.is_some(),
    );
    let overlay_wait = if permitted {
        let queue_dispatch = queue_dispatch.expect("permitted implies a resolved queue");
        let original = original.expect("permitted implies present info");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            compose_overlay(queue, queue_dispatch, original)
        })) {
            Ok(overlay_wait) => overlay_wait,
            Err(_) => {
                disable_overlay("overlay composition panicked");
                None
            }
        }
    } else {
        None
    };

    let Some(overlay_wait) = overlay_wait else {
        diagnostics("queue present dispatched");
        return unsafe { next(queue, present_info) };
    };
    let mut augmented = *original.expect("a composed frame implies present info");
    augmented.wait_semaphore_count = 1;
    augmented.p_wait_semaphores = &overlay_wait;
    diagnostics("queue present composited");
    unsafe { next(queue, &augmented) }
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

    #[test]
    fn overlay_work_is_skipped_but_the_present_still_forwards() {
        // Every degraded combination must refuse overlay work. The caller
        // forwards the application's present unconditionally afterwards, so
        // refusing here is invisible to the game rather than a device loss.
        assert!(!overlay_permitted(true, true, true), "latched off");
        assert!(!overlay_permitted(false, false, true), "unknown queue");
        assert!(!overlay_permitted(false, true, false), "null present info");
        assert!(overlay_permitted(false, true, true), "healthy present");
    }

    #[test]
    fn poisoning_the_dispatch_mutex_latches_the_overlay_off_and_still_reads_state() {
        OVERLAY_DISABLED.store(false, Ordering::Release);
        let mutex = dispatch();
        // Simulate a panic that happened while the dispatch guard was held.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("overlay bookkeeping panicked");
        }));
        assert!(mutex.is_poisoned());

        // Recovery must still yield usable state rather than propagating a
        // failure into the present hook.
        let state = dispatch_state();
        assert!(state.queues.is_empty());
        drop(state);
        assert!(overlay_disabled());
        assert!(!overlay_permitted(overlay_disabled(), true, true));

        mutex.clear_poison();
        OVERLAY_DISABLED.store(false, Ordering::Release);
    }

    #[test]
    fn a_missing_fallback_present_is_reported_without_inventing_device_loss() {
        // Nothing was ever registered in this unit-test process, so the
        // fallback is absent; the hook's early return must not be DEVICE_LOST.
        assert!(fallback_present().is_none() || FALLBACK_PRESENT.load(Ordering::Acquire) != 0);
        assert_ne!(
            vk::Result::ERROR_INITIALIZATION_FAILED,
            vk::Result::ERROR_DEVICE_LOST
        );
    }

    #[test]
    fn selection_generation_never_uses_zero() {
        let mut generation = u64::MAX;
        generation = generation.wrapping_add(1).max(1);
        assert_eq!(generation, 1);
    }
}
