#![allow(non_snake_case, unsafe_op_in_unsafe_fn)]
mod gpu;
mod native;
include!("present.rs");
use crate::client::Client;
use ash::{vk, vk::Handle};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
    mem, ptr, slice,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) mod shaders {
    pub static VERTEX_SPV: &[u32] = include!(concat!(env!("OUT_DIR"), "/overlay.vert.rs"));
    pub static FRAGMENT_SPV: &[u32] = include!(concat!(env!("OUT_DIR"), "/overlay.frag.rs"));
}

const LAYER_NEGOTIATE_INTERFACE_STRUCT: u32 = 1;
const LAYER_LINK_INFO: u32 = 0;
const MAX_INSTANCES: usize = 16;
const MAX_PHYSICAL_DEVICES: usize = 64;
const MAX_DEVICES: usize = 16;
const MAX_SWAPCHAINS: usize = 128;
const MAX_SURFACES: usize = 128;

#[repr(C)]
pub struct NegotiateLayerInterface {
    pub s_type: u32,
    pub p_next: *mut c_void,
    pub loaderLayerInterfaceVersion: u32,
    pub pfnGetInstanceProcAddr: vk::PFN_vkGetInstanceProcAddr,
    pub pfnGetDeviceProcAddr: vk::PFN_vkGetDeviceProcAddr,
    pub pfnGetPhysicalDeviceProcAddr:
        Option<unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction>,
}

#[repr(C)]
struct LayerInstanceLink {
    next: *mut LayerInstanceLink,
    next_gipa: vk::PFN_vkGetInstanceProcAddr,
    _next_physical_gpa:
        Option<unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction>,
}

#[repr(C)]
union LayerInstanceUnion {
    layer_info: *mut LayerInstanceLink,
    _set_loader_data: Option<unsafe extern "system" fn(vk::Instance, *mut c_void) -> vk::Result>,
}

#[repr(C)]
struct LayerInstanceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    u: LayerInstanceUnion,
}

#[repr(C)]
struct LayerDeviceLink {
    next: *mut LayerDeviceLink,
    next_gipa: vk::PFN_vkGetInstanceProcAddr,
    next_gdpa: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
union LayerDeviceUnion {
    layer_info: *mut LayerDeviceLink,
    _set_loader_data: Option<unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result>,
}

#[repr(C)]
struct LayerDeviceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32,
    u: LayerDeviceUnion,
}

#[derive(Clone)]
struct InstanceRecord {
    next_gipa: vk::PFN_vkGetInstanceProcAddr,
    next_physical_gpa:
        Option<unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction>,
    instance: ash::Instance,
}
struct DeviceRecord {
    instance_handle: vk::Instance,
    physical: vk::PhysicalDevice,
    next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    device: ash::Device,
    queues: HashMap<vk::Queue, u32>,
    present: Option<vk::PFN_vkQueuePresentKHR>,
    loader_data: Option<SetDeviceLoaderData>,
}
type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;
struct SwapchainInput {
    window: u64,
    backend: u32,
    client: Mutex<Client>,
    active: AtomicBool,
}
impl SwapchainInput {
    fn focused(&self) -> bool {
        crate::sdl::surface_status(self.window)
            .map(|(focused, _)| focused)
            .unwrap_or_else(|| self.backend == 1 && native::focused(self.window))
    }
}
struct SwapchainRecord {
    device: vk::Device,
    input: Arc<SwapchainInput>,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    sharing_mode: vk::SharingMode,
    queue_families: Vec<u32>,
    resources: Option<gpu::Resources>,
    unsupported: bool,
}
#[derive(Default)]
struct Registry {
    instances: HashMap<vk::Instance, InstanceRecord>,
    physical_instances: HashMap<vk::PhysicalDevice, vk::Instance>,
    // Native backend: 0 = unavailable/Win32, 1 = X11, 2 = Wayland.
    surface_windows: HashMap<vk::SurfaceKHR, (vk::Instance, u64, u32)>,
    devices: HashMap<vk::Device, DeviceRecord>,
    queue_devices: HashMap<vk::Queue, vk::Device>,
    swaps: HashMap<vk::SwapchainKHR, SwapchainRecord>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));
// Input lookup and socket servicing never wait for the graphics registry.
static INPUTS: LazyLock<Mutex<Vec<Arc<SwapchainInput>>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(MAX_SWAPCHAINS)));

fn input_surface(surface: u64) -> Option<Arc<SwapchainInput>> {
    INPUTS
        .lock()
        .iter()
        .find(|input| input.window == surface && input.active.load(Ordering::Acquire))
        .cloned()
}

fn retire_input(input: &Arc<SwapchainInput>) {
    input.active.store(false, Ordering::Release);
    INPUTS
        .lock()
        .retain(|existing| !Arc::ptr_eq(existing, input));
    input.client.lock().disconnect();
}

static BOOTSTRAP_GIPA: LazyLock<Mutex<Option<vk::PFN_vkGetInstanceProcAddr>>> =
    LazyLock::new(|| Mutex::new(None));
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayStatus {
    Disabled = 0,
    Active = 1,
}
unsafe fn lookup_instance(
    function: vk::PFN_vkGetInstanceProcAddr,
    instance: vk::Instance,
    name: &CStr,
) -> vk::PFN_vkVoidFunction {
    function(instance, name.as_ptr())
}

unsafe fn lookup_device(
    function: vk::PFN_vkGetDeviceProcAddr,
    device: vk::Device,
    name: &CStr,
) -> vk::PFN_vkVoidFunction {
    function(device, name.as_ptr())
}

unsafe fn cast<T: Copy>(function: vk::PFN_vkVoidFunction) -> Option<T> {
    function.map(|pointer| mem::transmute_copy(&pointer))
}

unsafe fn find_instance_chain(
    create_info: *const vk::InstanceCreateInfo<'_>,
) -> Option<*mut LayerInstanceCreateInfo> {
    let mut next = (*create_info).p_next as *mut vk::BaseInStructure<'_>;
    while !next.is_null() {
        if (*next).s_type == vk::StructureType::LOADER_INSTANCE_CREATE_INFO {
            let layer = next.cast::<LayerInstanceCreateInfo>();
            if (*layer).function == LAYER_LINK_INFO {
                return Some(layer);
            }
        }
        next = (*next).p_next.cast_mut();
    }
    None
}

unsafe fn find_device_chain(
    create_info: *const vk::DeviceCreateInfo<'_>,
) -> Option<*mut LayerDeviceCreateInfo> {
    let mut next = (*create_info).p_next as *mut vk::BaseInStructure<'_>;
    while !next.is_null() {
        if (*next).s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
            let layer = next.cast::<LayerDeviceCreateInfo>();
            if (*layer).function == LAYER_LINK_INFO {
                return Some(layer);
            }
        }
        next = (*next).p_next.cast_mut();
    }
    None
}

/// # Safety
/// `interface` must point to a writable loader negotiation record with the
/// layout specified by the Vulkan loader/layer interface.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    interface: *mut NegotiateLayerInterface,
) -> vk::Result {
    if interface.is_null()
        || (*interface).s_type != LAYER_NEGOTIATE_INTERFACE_STRUCT
        || (*interface).loaderLayerInterfaceVersion < 1
    {
        return vk::Result::ERROR_INCOMPATIBLE_DRIVER;
    }
    (*interface).loaderLayerInterfaceVersion = (*interface).loaderLayerInterfaceVersion.min(2);
    (*interface).pfnGetInstanceProcAddr = vkGetInstanceProcAddr;
    (*interface).pfnGetDeviceProcAddr = vkGetDeviceProcAddr;
    (*interface).pfnGetPhysicalDeviceProcAddr = Some(vkGetPhysicalDeviceProcAddr);
    vk::Result::SUCCESS
}

/// # Safety
/// `name` must be a readable NUL-terminated string. A non-null `instance`
/// must be live; returned pointers require the named Vulkan command's ABI.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetInstanceProcAddr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = CStr::from_ptr(name);
    let own = match name.to_bytes() {
        b"vkGetInstanceProcAddr" => Some(vkGetInstanceProcAddr as *const ()),
        b"vkGetDeviceProcAddr" => Some(vkGetDeviceProcAddr as *const ()),
        b"vkCreateInstance" => Some(vkCreateInstance as *const ()),
        b"vkDestroyInstance" => Some(vkDestroyInstance as *const ()),
        b"vkCreateDevice" => Some(vkCreateDevice as *const ()),
        b"vkEnumeratePhysicalDevices" => Some(vkEnumeratePhysicalDevices as *const ()),
        b"vkEnumeratePhysicalDeviceGroups" | b"vkEnumeratePhysicalDeviceGroupsKHR" => {
            Some(vkEnumeratePhysicalDeviceGroups as *const ())
        }
        b"vkDestroyDevice" => Some(vkDestroyDevice as *const ()),
        b"vkGetDeviceQueue" => Some(vkGetDeviceQueue as *const ()),
        b"vkGetDeviceQueue2" => Some(vkGetDeviceQueue2 as *const ()),
        b"vkDeviceWaitIdle" => Some(vkDeviceWaitIdle as *const ()),
        b"vkQueueWaitIdle" => Some(vkQueueWaitIdle as *const ()),
        b"vkCreateSwapchainKHR" => Some(vkCreateSwapchainKHR as *const ()),
        b"vkDestroySwapchainKHR" => Some(vkDestroySwapchainKHR as *const ()),
        b"vkQueuePresentKHR" => Some(vkQueuePresentKHR as *const ()),
        _ => None,
    };
    let conditional = match name.to_bytes() {
        b"vkCreateWaylandSurfaceKHR" => Some(vkCreateWaylandSurfaceKHR as *const ()),
        b"vkCreateXlibSurfaceKHR" => Some(vkCreateXlibSurfaceKHR as *const ()),
        b"vkCreateXcbSurfaceKHR" => Some(vkCreateXcbSurfaceKHR as *const ()),
        b"vkDestroySurfaceKHR" => Some(vkDestroySurfaceKHR as *const ()),
        _ => None,
    };
    if let Some(pointer) = conditional {
        if instance == vk::Instance::null() {
            return None;
        }
        let registry = REGISTRY.lock();
        return registry
            .instances
            .get(&instance)
            .filter(|record| lookup_instance(record.next_gipa, instance, name).is_some())
            .map(|_| mem::transmute(pointer));
    }
    if let Some(pointer) = own {
        return Some(mem::transmute::<*const (), unsafe extern "system" fn()>(
            pointer,
        ));
    }
    if instance == vk::Instance::null() {
        return BOOTSTRAP_GIPA
            .lock()
            .as_ref()
            .and_then(|next| lookup_instance(*next, instance, name));
    }
    let registry = REGISTRY.lock();
    registry
        .instances
        .get(&instance)
        .and_then(|record| lookup_instance(record.next_gipa, instance, name))
}

/// # Safety
/// `device` must be live and `name` a readable NUL-terminated string.
/// Returned pointers require the named Vulkan command's ABI and valid usage.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceProcAddr(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let name = CStr::from_ptr(name);
    let always = match name.to_bytes() {
        b"vkGetDeviceProcAddr" => Some(vkGetDeviceProcAddr as *const ()),
        b"vkDestroyDevice" => Some(vkDestroyDevice as *const ()),
        b"vkGetDeviceQueue" => Some(vkGetDeviceQueue as *const ()),
        b"vkDeviceWaitIdle" => Some(vkDeviceWaitIdle as *const ()),
        b"vkQueueWaitIdle" => Some(vkQueueWaitIdle as *const ()),
        _ => None,
    };
    if let Some(pointer) = always {
        return Some(mem::transmute::<*const (), unsafe extern "system" fn()>(
            pointer,
        ));
    }
    let conditional = match name.to_bytes() {
        b"vkGetDeviceQueue2" => Some(vkGetDeviceQueue2 as *const ()),
        b"vkCreateSwapchainKHR" => Some(vkCreateSwapchainKHR as *const ()),
        b"vkDestroySwapchainKHR" => Some(vkDestroySwapchainKHR as *const ()),
        b"vkQueuePresentKHR" => Some(vkQueuePresentKHR as *const ()),
        _ => None,
    };
    let registry = REGISTRY.lock();
    let record = registry.devices.get(&device);
    if let Some(pointer) = conditional {
        return record
            .filter(|record| lookup_device(record.next_gdpa, device, name).is_some())
            .map(|_| mem::transmute(pointer));
    }
    record.and_then(|record| lookup_device(record.next_gdpa, device, name))
}

/// # Safety
/// Creation records, loader chain links and allocator callbacks must satisfy
/// `vkCreateInstance` valid usage; `output` must be writable for one instance.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateInstance(
    create_info: *const vk::InstanceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::Instance,
) -> vk::Result {
    if create_info.is_null() || output.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let Some(chain) = find_instance_chain(create_info) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let link = (*chain).u.layer_info;
    if link.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_gipa = (*link).next_gipa;
    let next_physical_gpa = (*link)._next_physical_gpa;
    (*chain).u.layer_info = (*link).next;
    let Some(create): Option<
        unsafe extern "system" fn(
            *const vk::InstanceCreateInfo<'_>,
            *const vk::AllocationCallbacks<'_>,
            *mut vk::Instance,
        ) -> vk::Result,
    > = cast(lookup_instance(
        next_gipa,
        vk::Instance::null(),
        c"vkCreateInstance",
    )) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    *BOOTSTRAP_GIPA.lock() = Some(next_gipa);
    let result = create(create_info, allocator, output);
    if result != vk::Result::SUCCESS {
        return result;
    }
    let instance = ash::Instance::load_with(
        |name| {
            lookup_instance(next_gipa, *output, name)
                .map(|function| function as *const c_void)
                .unwrap_or(ptr::null())
        },
        *output,
    );
    let mut registry = REGISTRY.lock();
    if registry.instances.len() >= MAX_INSTANCES {
        drop(registry);
        if let Some(destroy) = cast::<
            unsafe extern "system" fn(vk::Instance, *const vk::AllocationCallbacks<'_>),
        >(lookup_instance(next_gipa, *output, c"vkDestroyInstance"))
        {
            destroy(*output, allocator);
        }
        *output = vk::Instance::null();
        return vk::Result::ERROR_TOO_MANY_OBJECTS;
    }
    registry.instances.insert(
        *output,
        InstanceRecord {
            next_gipa,
            next_physical_gpa,
            instance,
        },
    );
    result
}

/// # Safety
/// The instance and allocator must satisfy `vkDestroyInstance` valid usage.
/// All child objects must be destroyed and concurrent instance use excluded.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroyInstance(
    instance: vk::Instance,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let record = {
        let mut registry = REGISTRY.lock();
        registry
            .physical_instances
            .retain(|_, owner| *owner != instance);
        registry
            .surface_windows
            .retain(|_, (owner, _, _)| *owner != instance);
        registry.instances.remove(&instance)
    };
    if let Some(record) = record
        && let Some(destroy) =
            cast::<unsafe extern "system" fn(vk::Instance, *const vk::AllocationCallbacks<'_>)>(
                lookup_instance(record.next_gipa, instance, c"vkDestroyInstance"),
            )
    {
        destroy(instance, allocator);
    }
}

unsafe fn remember_surface(
    instance: vk::Instance,
    output: *mut vk::SurfaceKHR,
    result: vk::Result,
    window: u64,
    backend: u32,
) -> vk::Result {
    if result == vk::Result::SUCCESS && !output.is_null() && *output != vk::SurfaceKHR::null() {
        let mut registry = REGISTRY.lock();
        if registry.surface_windows.len() < MAX_SURFACES {
            registry
                .surface_windows
                .insert(*output, (instance, window.max(1), backend));
        }
    }
    result
}

/// # Safety
/// Instance, Wayland objects, creation records and allocator must satisfy
/// `vkCreateWaylandSurfaceKHR` valid usage; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateWaylandSurfaceKHR(
    instance: vk::Instance,
    create_info: *const vk::WaylandSurfaceCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::SurfaceKHR,
) -> vk::Result {
    if create_info.is_null() || output.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_gipa = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.instances.get(&instance) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        record.next_gipa
    };
    let Some(create): Option<
        unsafe extern "system" fn(
            vk::Instance,
            *const vk::WaylandSurfaceCreateInfoKHR<'_>,
            *const vk::AllocationCallbacks<'_>,
            *mut vk::SurfaceKHR,
        ) -> vk::Result,
    > = cast(lookup_instance(
        next_gipa,
        instance,
        c"vkCreateWaylandSurfaceKHR",
    )) else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    remember_surface(
        instance,
        output,
        create(instance, create_info, allocator, output),
        (*create_info).surface as usize as u64,
        2,
    )
}

/// # Safety
/// Instance, Xlib objects, creation records and allocator must satisfy
/// `vkCreateXlibSurfaceKHR` valid usage; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateXlibSurfaceKHR(
    instance: vk::Instance,
    create_info: *const vk::XlibSurfaceCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::SurfaceKHR,
) -> vk::Result {
    if create_info.is_null() || output.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_gipa = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.instances.get(&instance) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        record.next_gipa
    };
    let Some(create): Option<
        unsafe extern "system" fn(
            vk::Instance,
            *const vk::XlibSurfaceCreateInfoKHR<'_>,
            *const vk::AllocationCallbacks<'_>,
            *mut vk::SurfaceKHR,
        ) -> vk::Result,
    > = cast(lookup_instance(
        next_gipa,
        instance,
        c"vkCreateXlibSurfaceKHR",
    )) else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    let window = (*create_info).window;
    remember_surface(
        instance,
        output,
        create(instance, create_info, allocator, output),
        window,
        1,
    )
}
/// # Safety
/// Instance, XCB objects, creation records and allocator must satisfy
/// `vkCreateXcbSurfaceKHR` valid usage; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateXcbSurfaceKHR(
    instance: vk::Instance,
    create_info: *const vk::XcbSurfaceCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::SurfaceKHR,
) -> vk::Result {
    if create_info.is_null() || output.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let next_gipa = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.instances.get(&instance) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        record.next_gipa
    };
    let Some(create): Option<
        unsafe extern "system" fn(
            vk::Instance,
            *const vk::XcbSurfaceCreateInfoKHR<'_>,
            *const vk::AllocationCallbacks<'_>,
            *mut vk::SurfaceKHR,
        ) -> vk::Result,
    > = cast(lookup_instance(
        next_gipa,
        instance,
        c"vkCreateXcbSurfaceKHR",
    )) else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    remember_surface(
        instance,
        output,
        create(instance, create_info, allocator, output),
        u64::from((*create_info).window),
        1,
    )
}
/// # Safety
/// The surface must belong to the instance and have no live swapchains.
/// Allocation callbacks and synchronization must satisfy `vkDestroySurfaceKHR`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroySurfaceKHR(
    instance: vk::Instance,
    surface: vk::SurfaceKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let next_gipa = {
        let registry = REGISTRY.lock();
        registry
            .instances
            .get(&instance)
            .map(|record| record.next_gipa)
    };
    if let Some(next_gipa) = next_gipa
        && let Some(destroy) =
            cast::<
                unsafe extern "system" fn(
                    vk::Instance,
                    vk::SurfaceKHR,
                    *const vk::AllocationCallbacks<'_>,
                ),
            >(lookup_instance(next_gipa, instance, c"vkDestroySurfaceKHR"))
    {
        destroy(instance, surface, allocator);
    }
    REGISTRY.lock().surface_windows.remove(&surface);
}
/// # Safety
/// The instance must be live. `count` must be readable/writable; non-null
/// `devices` must have writable capacity for its initial count.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkEnumeratePhysicalDevices(
    instance: vk::Instance,

    count: *mut u32,
    devices: *mut vk::PhysicalDevice,
) -> vk::Result {
    let record = REGISTRY.lock().instances.get(&instance).cloned();
    let Some(record) = record else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next): Option<
        unsafe extern "system" fn(vk::Instance, *mut u32, *mut vk::PhysicalDevice) -> vk::Result,
    > = cast(lookup_instance(
        record.next_gipa,
        instance,
        c"vkEnumeratePhysicalDevices",
    )) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let result = next(instance, count, devices);
    if !devices.is_null()
        && !count.is_null()
        && matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
    {
        let mut registry = REGISTRY.lock();
        for physical in slice::from_raw_parts(devices, *count as usize) {
            if registry.physical_instances.len() >= MAX_PHYSICAL_DEVICES {
                break;
            }
            registry.physical_instances.insert(*physical, instance);
        }
    }
    result
}

/// # Safety
/// The instance and output records must satisfy Vulkan enumeration valid
/// usage; `count` is readable/writable and bounds the writable `groups` array.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkEnumeratePhysicalDeviceGroups(
    instance: vk::Instance,
    count: *mut u32,
    groups: *mut vk::PhysicalDeviceGroupProperties<'_>,
) -> vk::Result {
    let record = REGISTRY.lock().instances.get(&instance).cloned();
    let Some(record) = record else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    type EnumerateGroups = unsafe extern "system" fn(
        vk::Instance,
        *mut u32,
        *mut vk::PhysicalDeviceGroupProperties<'_>,
    ) -> vk::Result;
    let next: Option<EnumerateGroups> = cast(lookup_instance(
        record.next_gipa,
        instance,
        c"vkEnumeratePhysicalDeviceGroups",
    ))
    .or_else(|| {
        cast(lookup_instance(
            record.next_gipa,
            instance,
            c"vkEnumeratePhysicalDeviceGroupsKHR",
        ))
    });
    let Some(next) = next else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let result = next(instance, count, groups);
    if !groups.is_null()
        && !count.is_null()
        && matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
    {
        let mut registry = REGISTRY.lock();
        for group in slice::from_raw_parts(groups, *count as usize) {
            let physical_count =
                (group.physical_device_count as usize).min(group.physical_devices.len());
            for physical in &group.physical_devices[..physical_count] {
                if registry.physical_instances.len() >= MAX_PHYSICAL_DEVICES {
                    break;
                }
                registry.physical_instances.insert(*physical, instance);
            }
        }
    }
    result
}

/// # Safety
/// Physical device, creation records, loader links and allocation callbacks
/// must satisfy `vkCreateDevice` valid usage; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateDevice(
    physical: vk::PhysicalDevice,
    create_info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::Device,
) -> vk::Result {
    if create_info.is_null() || output.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let Some(chain) = find_device_chain(create_info) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let link = (*chain).u.layer_info;
    if link.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let mut loader_data = None;
    let mut node = (*create_info).p_next as *const vk::BaseInStructure<'_>;
    while !node.is_null() {
        if (*node).s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
            let entry = node.cast::<LayerDeviceCreateInfo>();
            if (*entry).function == 1 {
                loader_data = (*entry).u._set_loader_data;
            }
        }
        node = (*node).p_next;
    }
    let next_gipa = (*link).next_gipa;
    let next_gdpa = (*link).next_gdpa;
    (*chain).u.layer_info = (*link).next;
    let instance = {
        let registry = REGISTRY.lock();
        let Some(instance_handle) = registry.physical_instances.get(&physical).copied() else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(record) = registry.instances.get(&instance_handle).cloned() else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        (instance_handle, record)
    };
    let Some(create): Option<
        unsafe extern "system" fn(
            vk::PhysicalDevice,
            *const vk::DeviceCreateInfo<'_>,
            *const vk::AllocationCallbacks<'_>,
            *mut vk::Device,
        ) -> vk::Result,
    > = cast(lookup_instance(next_gipa, instance.0, c"vkCreateDevice")) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let result = create(physical, create_info, allocator, output);
    if result != vk::Result::SUCCESS {
        return result;
    }
    let device = ash::Device::load_with(
        |name| {
            lookup_device(next_gdpa, *output, name)
                .map(|function| function as *const c_void)
                .unwrap_or(ptr::null())
        },
        *output,
    );
    let mut registry = REGISTRY.lock();
    if registry.devices.len() >= MAX_DEVICES {
        if let Some(destroy) = cast::<
            unsafe extern "system" fn(vk::Device, *const vk::AllocationCallbacks<'_>),
        >(lookup_device(next_gdpa, *output, c"vkDestroyDevice"))
        {
            destroy(*output, allocator);
        }
        *output = vk::Device::null();
        return vk::Result::ERROR_TOO_MANY_OBJECTS;
    }
    registry.devices.insert(
        *output,
        DeviceRecord {
            instance_handle: instance.0,
            physical,
            next_gdpa,
            device,
            queues: HashMap::new(),
            present: cast(lookup_device(next_gdpa, *output, c"vkQueuePresentKHR")),
            loader_data,
        },
    );
    result
}

/// # Safety
/// Device, allocator, completed work and child lifetimes must satisfy
/// `vkDestroyDevice` valid usage. Concurrent use of the device is forbidden.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroyDevice(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let (record, swaps) = {
        let mut registry = REGISTRY.lock();
        let record = registry.devices.remove(&device);
        registry.queue_devices.retain(|_, owner| *owner != device);
        let keys: Vec<_> = registry
            .swaps
            .iter()
            .filter_map(|(swapchain, record)| (record.device == device).then_some(*swapchain))
            .collect();
        let swaps = keys
            .into_iter()
            .filter_map(|swapchain| registry.swaps.remove(&swapchain))
            .collect::<Vec<_>>();
        (record, swaps)
    };
    for swap in &swaps {
        retire_input(&swap.input);
    }
    if let Some(record) = record {
        for swap in swaps {
            if let Some(resources) = swap.resources {
                let _ = resources.retire(&record.device);
            }
        }
        if let Some(destroy) =
            cast::<unsafe extern "system" fn(vk::Device, *const vk::AllocationCallbacks<'_>)>(
                lookup_device(record.next_gdpa, device, c"vkDestroyDevice"),
            )
        {
            destroy(device, allocator);
        }
    }
}

/// # Safety
/// Device, family and index must identify a queue requested at device creation
/// under `vkGetDeviceQueue` valid usage; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceQueue(
    device: vk::Device,
    family: u32,
    index: u32,
    output: *mut vk::Queue,
) {
    let (next_gdpa, handle) = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.devices.get(&device) else {
            return;
        };
        (record.next_gdpa, record.device.handle())
    };
    if let Some(next) = cast::<unsafe extern "system" fn(vk::Device, u32, u32, *mut vk::Queue)>(
        lookup_device(next_gdpa, handle, c"vkGetDeviceQueue"),
    ) {
        next(device, family, index, output);
    }
    if !output.is_null() && *output != vk::Queue::null() {
        let mut registry = REGISTRY.lock();
        if let Some(record) = registry.devices.get_mut(&device) {
            record.queues.insert(*output, family);
        }
        registry.queue_devices.insert(*output, device);
    }
}

/// # Safety
/// Device and readable queue-info record must satisfy `vkGetDeviceQueue2`
/// valid usage; `output` must be writable for one queue handle.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetDeviceQueue2(
    device: vk::Device,
    info: *const vk::DeviceQueueInfo2<'_>,
    output: *mut vk::Queue,
) {
    if info.is_null() {
        return;
    }
    let (next_gdpa, handle) = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.devices.get(&device) else {
            return;
        };
        (record.next_gdpa, record.device.handle())
    };
    if let Some(next) = cast::<
        unsafe extern "system" fn(vk::Device, *const vk::DeviceQueueInfo2<'_>, *mut vk::Queue),
    >(lookup_device(next_gdpa, handle, c"vkGetDeviceQueue2"))
    {
        next(device, info, output);
    }
    if !output.is_null() && *output != vk::Queue::null() {
        let mut registry = REGISTRY.lock();
        if let Some(record) = registry.devices.get_mut(&device) {
            record.queues.insert(*output, (*info).queue_family_index);
        }
        registry.queue_devices.insert(*output, device);
    }
}
/// # Safety
/// The device must be live and all its queues externally synchronized as
/// required by `vkDeviceWaitIdle`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDeviceWaitIdle(device: vk::Device) -> vk::Result {
    let next_gdpa = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.devices.get(&device) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        record.next_gdpa
    };
    let Some(wait_idle): Option<unsafe extern "system" fn(vk::Device) -> vk::Result> =
        cast(lookup_device(next_gdpa, device, c"vkDeviceWaitIdle"))
    else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let result = wait_idle(device);
    if result == vk::Result::SUCCESS {
        let mut registry = REGISTRY.lock();
        for record in registry
            .swaps
            .values_mut()
            .filter(|record| record.device == device)
        {
            if let Some(resources) = record.resources.as_mut() {
                resources.mark_queue_idle();
            }
        }
    }
    result
}

/// # Safety
/// The queue and its device must be live. Queue access must be externally
/// synchronized as required by `vkQueueWaitIdle`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkQueueWaitIdle(queue: vk::Queue) -> vk::Result {
    let (device, next_gdpa) = {
        let registry = REGISTRY.lock();
        let Some(device) = registry.queue_devices.get(&queue).copied() else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        let Some(record) = registry.devices.get(&device) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        (device, record.next_gdpa)
    };
    let Some(wait_idle): Option<unsafe extern "system" fn(vk::Queue) -> vk::Result> =
        cast(lookup_device(next_gdpa, device, c"vkQueueWaitIdle"))
    else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let result = wait_idle(queue);
    if result == vk::Result::SUCCESS {
        let mut registry = REGISTRY.lock();
        for record in registry
            .swaps
            .values_mut()
            .filter(|record| record.device == device)
        {
            if let Some(resources) = record.resources.as_mut()
                && resources.queue() == queue
            {
                resources.mark_queue_idle();
            }
        }
    }
    result
}
/// # Safety
/// Device, surface, creation records and allocator must satisfy
/// `vkCreateSwapchainKHR` valid usage; `output` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkCreateSwapchainKHR(
    device: vk::Device,
    create_info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::SwapchainKHR,
) -> vk::Result {
    if create_info.is_null() || output.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let (next_gdpa, handle, physical, instance_record) = {
        let registry = REGISTRY.lock();
        let Some(record) = registry.devices.get(&device) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        let Some(instance) = registry.instances.get(&record.instance_handle).cloned() else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        (
            record.next_gdpa,
            record.device.handle(),
            record.physical,
            instance,
        )
    };
    let Some(create): Option<
        unsafe extern "system" fn(
            vk::Device,
            *const vk::SwapchainCreateInfoKHR<'_>,
            *const vk::AllocationCallbacks<'_>,
            *mut vk::SwapchainKHR,
        ) -> vk::Result,
    > = cast(lookup_device(next_gdpa, handle, c"vkCreateSwapchainKHR")) else {
        return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
    };
    let original = &*create_info;
    let original_color = original
        .image_usage
        .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT);
    let ordinary_present = !matches!(
        original.present_mode,
        vk::PresentModeKHR::SHARED_DEMAND_REFRESH | vk::PresentModeKHR::SHARED_CONTINUOUS_REFRESH
    );
    let mut overlay_capable = original.image_array_layers == 1
        && original.image_extent.width > 0
        && original.image_extent.height > 0
        && !original
            .flags
            .contains(vk::SwapchainCreateFlagsKHR::PROTECTED)
        && ordinary_present
        && gpu::formats_supported(&instance_record.instance, physical, original.image_format)
        && (original_color
            || surface_supports_color_attachment(&instance_record, physical, original.surface));
    let mut forwarded = ptr::read(create_info);
    if overlay_capable {
        forwarded.image_usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
    }
    let mut result = create(
        device,
        if overlay_capable {
            &forwarded
        } else {
            create_info
        },
        allocator,
        output,
    );
    if matches!(
        result,
        vk::Result::ERROR_FORMAT_NOT_SUPPORTED | vk::Result::ERROR_INITIALIZATION_FAILED
    ) && overlay_capable
        && !original_color
    {
        overlay_capable = false;
        result = create(device, create_info, allocator, output);
    }
    if result != vk::Result::SUCCESS {
        return result;
    }
    let Some(get_images): Option<
        unsafe extern "system" fn(
            vk::Device,
            vk::SwapchainKHR,
            *mut u32,
            *mut vk::Image,
        ) -> vk::Result,
    > = cast(lookup_device(next_gdpa, handle, c"vkGetSwapchainImagesKHR")) else {
        return result;
    };
    let mut count = 0;
    if get_images(device, *output, &mut count, ptr::null_mut()) != vk::Result::SUCCESS
        || count == 0
        || count as usize > gpu::MAX_IMAGES
    {
        return result;
    }
    let mut images = vec![vk::Image::null(); count as usize];
    let image_result = get_images(device, *output, &mut count, images.as_mut_ptr());
    if !matches!(image_result, vk::Result::SUCCESS | vk::Result::INCOMPLETE) {
        return result;
    }
    images.truncate(count as usize);
    let info = original;
    let queue_families =
        if info.queue_family_index_count == 0 || info.p_queue_family_indices.is_null() {
            Vec::new()
        } else {
            slice::from_raw_parts(
                info.p_queue_family_indices,
                info.queue_family_index_count as usize,
            )
            .to_vec()
        };
    let mut registry = REGISTRY.lock();
    if registry.swaps.len() >= MAX_SWAPCHAINS {
        return result;
    }
    let (window, backend) = registry
        .surface_windows
        .get(&info.surface)
        .map(|(_, window, backend)| (*window, *backend))
        .unwrap_or((info.surface.as_raw().max(1), 0));
    if backend == 1 {
        native::observe_x11();
    }
    let input = Arc::new(SwapchainInput {
        window,
        backend,
        client: Mutex::new(Client::new(window)),
        active: AtomicBool::new(overlay_capable),
    });
    if overlay_capable {
        INPUTS.lock().push(Arc::clone(&input));
    }
    registry.swaps.insert(
        *output,
        SwapchainRecord {
            device,
            input,
            format: info.image_format,
            extent: info.image_extent,
            images,
            sharing_mode: info.image_sharing_mode,
            queue_families,
            resources: None,
            unsupported: !overlay_capable,
        },
    );
    result
}

/// # Safety
/// Device, swapchain, allocator and completed presentation work must satisfy
/// `vkDestroySwapchainKHR` valid usage, including external synchronization.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkDestroySwapchainKHR(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    let (record, device_record) = {
        let mut registry = REGISTRY.lock();
        let record = registry.swaps.remove(&swapchain);
        let device_record = registry
            .devices
            .get(&device)
            .map(|record| (record.next_gdpa, record.device.clone()));
        (record, device_record)
    };
    if let Some(record) = record.as_ref() {
        retire_input(&record.input);
    }
    if let (Some(record), Some((_, ash_device))) = (record, device_record.as_ref())
        && let Some(resources) = record.resources
    {
        let _ = resources.retire(ash_device);
    }
    if let Some((next_gdpa, ash_device)) = device_record
        && let Some(destroy) = cast::<
            unsafe extern "system" fn(
                vk::Device,
                vk::SwapchainKHR,
                *const vk::AllocationCallbacks<'_>,
            ),
        >(lookup_device(
            next_gdpa,
            ash_device.handle(),
            c"vkDestroySwapchainKHR",
        ))
    {
        destroy(device, swapchain, allocator);
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn vosulay_vulkan_status(swapchain: vk::SwapchainKHR) -> OverlayStatus {
    REGISTRY
        .lock()
        .swaps
        .get(&swapchain)
        .and_then(|record| record.resources.as_ref())
        .filter(|resources| resources.is_rendering())
        .map(|_| OverlayStatus::Active)
        .unwrap_or(OverlayStatus::Disabled)
}

#[unsafe(no_mangle)]
pub extern "system" fn vosulay_vulkan_frame_count(swapchain: vk::SwapchainKHR) -> u64 {
    REGISTRY
        .lock()
        .swaps
        .get(&swapchain)
        .and_then(|record| record.resources.as_ref())
        .map(gpu::Resources::frame_count)
        .unwrap_or(0)
}
unsafe fn surface_supports_color_attachment(
    instance: &InstanceRecord,
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> bool {
    let Some(get_capabilities): Option<
        unsafe extern "system" fn(
            vk::PhysicalDevice,

            vk::SurfaceKHR,
            *mut vk::SurfaceCapabilitiesKHR,
        ) -> vk::Result,
    > = cast(lookup_instance(
        instance.next_gipa,
        instance.instance.handle(),
        c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
    )) else {
        return false;
    };
    let mut capabilities = vk::SurfaceCapabilitiesKHR::default();
    get_capabilities(physical, surface, &mut capabilities) == vk::Result::SUCCESS
        && capabilities
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
}

pub(crate) fn focus(surface: u64, focused: bool) {
    let inputs = INPUTS.lock();
    for input in inputs.iter().filter(|input| input.window == surface) {
        let mut client = input.client.lock();
        if input.active.load(Ordering::Acquire) {
            client.focus(focused);
        }
    }
}

pub(crate) fn route_surface_input(
    surface: u64,
    input: vosulay_frame::BrowserInput,
) -> Option<bool> {
    let surface = input_surface(surface)?;
    let focused = surface.focused();
    let mut client = surface.client.lock();
    if !surface.active.load(Ordering::Acquire) {
        return Some(false);
    }
    client.focus(focused);
    Some(client.route_input(input))
}

pub(crate) fn capturing_surface(surface: u64) -> Option<bool> {
    let surface = input_surface(surface)?;
    let focused = surface.focused();
    let mut client = surface.client.lock();
    if !surface.active.load(Ordering::Acquire) {
        return Some(false);
    }
    client.service();
    client.focus(focused);
    Some(client.capturing())
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkGetPhysicalDeviceProcAddr(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if name.is_null() {
        return None;
    }
    let registry = REGISTRY.lock();
    let record = registry.instances.get(&instance)?;
    if let Some(next) = record.next_physical_gpa {
        next(instance, name)
    } else {
        (record.next_gipa)(instance, name)
    }
}
