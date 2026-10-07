//! GPU-only transfer from callback-borrowed NT handles to legacy overlay handles.
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use asdf_overlay_common::event::surface::GpuLuid;
use vosulay_frame::{BrowserFrame, BrowserFrameBackend};
use windows::{
    Win32::{
        Foundation::{HANDLE, HMODULE, WAIT_OBJECT_0},
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_UNKNOWN, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile,
                ID3DBlob,
            },
            Direct3D11::{
                D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BIND_RENDER_TARGET,
                D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_FENCE_FLAG_NONE, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
                D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
                D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext,
                ID3D11DeviceContext4, ID3D11Fence, ID3D11PixelShader, ID3D11Query,
                ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
                ID3D11VertexShader,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIAdapter,
                IDXGIFactory1, IDXGIKeyedMutex, IDXGIResource,
            },
        },
        System::Threading::{CreateEventW, WaitForSingleObject},
    },
    core::{Interface, PCSTR, PCWSTR, s},
};

// CEF uses premultiplied BGRA; asdf-overlay 2.1 blends SRC_ALPHA.
// Integer Load on a cropped private input preserves exact texel coordinates,
// including transparent edges, without filtering or a CPU readback.
const VERTEX_SHADER: &str = r#"
float4 main(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
}
"#;
const PIXEL_SHADER: &str = r#"
Texture2D<float4> input_pixels : register(t0);
float4 main(float4 position : SV_Position) : SV_Target {
    float4 pixel = input_pixels.Load(int3(int2(position.xy), 0));
    return float4(pixel.a > 0.0 ? pixel.rgb / pixel.a : float3(0.0, 0.0, 0.0), pixel.a);
}
"#;

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // The blob owns this bytecode for the duration of shader creation.
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
}

fn compile_shader(source: &str, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("vosulay-alpha"),
            None,
            None,
            s!("main"),
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let diagnostics = errors
            .as_ref()
            .map(|blob| String::from_utf8_lossy(blob_bytes(blob)));
        bail!("compiling alpha-normalization shader: {error}; {diagnostics:?}");
    }
    code.context("D3DCompile returned no shader bytecode")
}

use crate::gpu_mailbox::{Binding, GpuMailbox, HandleUpdate};

/// The lease owns only the producer resource, not an immediate context. D3D11
/// resource COM reference counting is free-threaded (the SDK implements
/// Send/Sync for ID3D11Texture2D); consumers merely retain/drop this Arc across
/// the RPC ACK. All GPU writes and context calls stay in FrameBackend.
/// Legacy KMT handles are not CloseHandle-able and die with their resource.
pub(crate) struct TextureLease {
    _texture: ID3D11Texture2D,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TextureSpec {
    gpu: GpuLuid,
    keyed: bool,
    width: u32,
    height: u32,
}

impl TextureSpec {
    fn for_binding(binding: Binding) -> Self {
        Self {
            gpu: binding.info.gpu_id,
            keyed: binding.info.keyed_mutex,
            width: binding.width,
            height: binding.height,
        }
    }
}

struct CompletionFence {
    context: ID3D11DeviceContext4,
    fence: ID3D11Fence,
    event: OwnedHandle,
    value: u64,
}

impl CompletionFence {
    fn new(base: &ID3D11Device, context: &ID3D11DeviceContext) -> Option<Self> {
        let device = base.cast::<ID3D11Device5>().ok()?;
        let context = context.cast::<ID3D11DeviceContext4>().ok()?;
        let mut fence = None;
        unsafe { device.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence) }.ok()?;
        let fence = fence?;
        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.ok()?;
        Some(Self {
            context,
            fence,
            event: unsafe { OwnedHandle::from_raw_handle(event.0) },
            value: 0,
        })
    }
}

struct Device {
    gpu: GpuLuid,
    base: ID3D11Device,
    shared: ID3D11Device1,
    context: ID3D11DeviceContext,
    completion: ID3D11Query,
    fence: Option<CompletionFence>,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    // A failed completion cannot be reused by another callback.
    failed: bool,
}

impl Device {
    fn new(gpu: GpuLuid) -> Result<Self> {
        // A known LUID is mandatory when supplied. OpenGL can report an absent
        // all-zero adapter hint; only then try the primary hardware adapter.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .context("creating DXGI factory for overlay GPU selection")?;
        let unknown_gpu = gpu.low == 0 && gpu.high == 0;
        let mut index = 0;
        let adapter = loop {
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
                    bail!(
                        "overlay GPU LUID {:08x}:{:08x} has no matching hardware adapter; the game adapter must remain available",
                        gpu.high as u32,
                        gpu.low
                    );
                }
                Err(error) => return Err(error).context("enumerating DXGI adapters"),
            };
            let desc = unsafe { adapter.GetDesc1() }.context("reading DXGI adapter LUID")?;
            if (unknown_gpu && desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0)
                || (!unknown_gpu
                    && desc.AdapterLuid.LowPart == gpu.low
                    && desc.AdapterLuid.HighPart == gpu.high)
            {
                break adapter
                    .cast::<IDXGIAdapter>()
                    .context("querying DXGI adapter")?;
            }
            index += 1;
        };
        let mut base = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut base),
                None,
                Some(&mut context),
            )
        }
        .context("creating BGRA D3D11 device on the game's GPU")?;
        let base = base.context("D3D11CreateDevice returned no device")?;
        let shared = base
            .cast::<ID3D11Device1>()
            .context("D3D11.1 is required to open CEF NT shared handles")?;
        let vertex = compile_shader(VERTEX_SHADER, s!("vs_4_0"))?;
        let pixel = compile_shader(PIXEL_SHADER, s!("ps_4_0"))?;
        let mut vertex_shader = None;
        let mut pixel_shader = None;
        let mut completion = None;
        unsafe {
            base.CreateVertexShader(blob_bytes(&vertex), None, Some(&mut vertex_shader))
                .context("creating alpha-normalization vertex shader")?;
            base.CreatePixelShader(blob_bytes(&pixel), None, Some(&mut pixel_shader))
                .context("creating alpha-normalization pixel shader")?;
            base.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut completion),
            )
            .context("creating persistent GPU completion query")?;
        }
        let context = context.context("D3D11CreateDevice returned no immediate context")?;
        let fence = CompletionFence::new(&base, &context);
        Ok(Self {
            gpu,
            base,
            shared,
            context,
            completion: completion.context("CreateQuery returned no completion query")?,
            fence,
            vertex_shader: vertex_shader.context("CreateVertexShader returned no shader")?,
            pixel_shader: pixel_shader.context("CreatePixelShader returned no shader")?,
            failed: false,
        })
    }

    fn owned_texture(&self, spec: TextureSpec) -> Result<OwnedTexture> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: spec.width,
            Height: spec.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            // SHARED and SHARED_KEYEDMUTEX are mutually exclusive. Neither
            // uses SHARED_NTHANDLE: asdf's Kmt protocol opens a legacy handle.
            MiscFlags: if spec.keyed {
                D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32
            } else {
                D3D11_RESOURCE_MISC_SHARED.0 as u32
            },
        };
        let mut texture = None;
        unsafe { self.base.CreateTexture2D(&desc, None, Some(&mut texture)) }.with_context(
            || {
                format!(
                    "creating {}x{} legacy shared BGRA overlay texture (keyed={})",
                    spec.width, spec.height, spec.keyed
                )
            },
        )?;
        let texture = texture.context("CreateTexture2D returned no overlay texture")?;
        let resource = texture
            .cast::<IDXGIResource>()
            .context("querying overlay DXGI resource")?;
        let handle =
            unsafe { resource.GetSharedHandle() }.context("getting legacy KMT overlay handle")?;
        let value = handle.0 as usize;
        ensure!(
            value != 0 && value <= u32::MAX as usize,
            "legacy shared handle {value:#x} cannot be represented by asdf's u32 Kmt protocol"
        );
        let mutex = if spec.keyed {
            Some(
                texture
                    .cast::<IDXGIKeyedMutex>()
                    .context("querying keyed overlay mutex")?,
            )
        } else {
            None
        };
        // Private cropped input avoids relying on shader-resource bind flags
        // on the borrowed CEF texture. Neither it nor the views are per-frame.
        let input_desc = D3D11_TEXTURE2D_DESC {
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            MiscFlags: 0,
            ..desc
        };
        let mut input = None;
        unsafe {
            self.base
                .CreateTexture2D(&input_desc, None, Some(&mut input))
        }
        .context("creating private premultiplied input texture")?;
        let input = input.context("CreateTexture2D returned no private input")?;
        let mut input_view = None;
        let mut target_view = None;
        unsafe {
            self.base
                .CreateShaderResourceView(&input, None, Some(&mut input_view))
                .context("creating private input shader view")?;
            self.base
                .CreateRenderTargetView(&texture, None, Some(&mut target_view))
                .context("creating straight-alpha output render target")?;
        }
        Ok(OwnedTexture {
            spec,
            handle: value as u32,
            mutex,
            lease: Arc::new(TextureLease { _texture: texture }),
            content_binding: None,
            input,
            input_view: input_view.context("CreateShaderResourceView returned no view")?,
            target_view: target_view.context("CreateRenderTargetView returned no view")?,
        })
    }

    fn wait_for_completion(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        // Supported D3D11.4 adapters signal one persistent kernel event instead
        // of rounding every completion up to a one-millisecond query poll.
        if let Some(fence) = self.fence.as_mut()
            && let Some(value) = fence.value.checked_add(1)
        {
            fence.value = value;
            let event = HANDLE(fence.event.as_raw_handle());
            if unsafe { fence.context.Signal(&fence.fence, value) }.is_ok()
                && unsafe { fence.fence.SetEventOnCompletion(value, event) }.is_ok()
            {
                unsafe { self.context.Flush() };
                let completed = unsafe { WaitForSingleObject(event, 2_000) } == WAIT_OBJECT_0
                    && unsafe { fence.fence.GetCompletedValue() } >= value;
                if let Err(error) = unsafe { self.base.GetDeviceRemovedReason() } {
                    self.failed = true;
                    return Err(error)
                        .context("overlay D3D11 device lost while awaiting borrowed CEF source");
                }
                if completed {
                    return Ok(());
                }
            }
        }
        // The D3D11.1 EVENT query remains the supported completion path for
        // older adapters or a failed optional fence. Only S_OK proves safety;
        // GetData's windows-rs Result wrapper loses S_FALSE, so call its vtable.
        unsafe {
            self.context.End(&self.completion);
            self.context.Flush();
        }
        loop {
            let status = unsafe {
                (Interface::vtable(&self.context).GetData)(
                    Interface::as_raw(&self.context),
                    Interface::as_raw(&self.completion),
                    std::ptr::null_mut(),
                    0,
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                )
            };
            if status.0 != 0 && status.0 != 1 {
                self.failed = true;
                unsafe { self.base.GetDeviceRemovedReason() }
                    .context("overlay D3D11 device lost while awaiting borrowed CEF source")?;
                // A query error alone does not prove the GPU stopped reading.
                // Do not unwind the callback and recycle CEF's borrowed frame.
                eprintln!(
                    "Fatal overlay GPU completion failure ({status:?}) without device removal; aborting before CEF can recycle its borrowed source. Restart Vosulay and reattach the game surface; check the GPU driver."
                );
                std::process::abort();
            }
            if let Err(error) = unsafe { self.base.GetDeviceRemovedReason() } {
                self.failed = true;
                return Err(error)
                    .context("overlay D3D11 device lost while awaiting borrowed CEF source");
            }
            if status.0 == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.failed = true;
                // Abort while source/guard locals are still alive: returning an
                // error would let CEF recycle memory the GPU may still access.
                eprintln!(
                    "Fatal overlay GPU completion timeout after two seconds; aborting before CEF can recycle its borrowed source. Restart Vosulay and reattach the game surface; check the GPU driver."
                );
                std::process::abort();
            }
            // EVENT queries are available on D3D11.1 hardware without requiring
            // the optional D3D11.4 fence interface. Sleep rather than busy-spin.
            thread::sleep(Duration::from_millis(1));
        }
    }
}

struct OwnedTexture {
    spec: TextureSpec,
    handle: u32,
    mutex: Option<IDXGIKeyedMutex>,
    lease: Arc<TextureLease>,
    input: ID3D11Texture2D,
    input_view: ID3D11ShaderResourceView,
    target_view: ID3D11RenderTargetView,
    // Only advertise a texture after a successful copy for this binding.
    content_binding: Option<Binding>,
}

/// A successful acquisition must release key zero on every exit path.
struct KeyGuard<'a> {
    mutex: &'a IDXGIKeyedMutex,
    active: bool,
}

impl<'a> KeyGuard<'a> {
    fn acquire(mutex: &'a IDXGIKeyedMutex, label: &str) -> Result<Option<Self>> {
        // windows-rs AcquireSync uses HRESULT::ok(), which incorrectly treats
        // positive WAIT_TIMEOUT / WAIT_ABANDONED values as acquisition. Inspect
        // the raw status and accept only S_OK. Timeout zero never waits.
        let status =
            unsafe { (Interface::vtable(mutex).AcquireSync)(Interface::as_raw(mutex), 0, 0) };
        match status.0 {
            0 => Ok(Some(Self {
                mutex,
                active: true,
            })),
            0x102 => Ok(None),
            0x80 => bail!("{label} keyed mutex was abandoned; recreate the shared resource"),
            _ => Err(anyhow!("{label} AcquireSync(0, 0) failed: {status:?}")),
        }
    }

    fn release(mut self, label: &str) -> Result<()> {
        self.active = false;
        unsafe { self.mutex.ReleaseSync(0) }
            .with_context(|| format!("releasing {label} keyed mutex at key zero"))
    }
}

impl Drop for KeyGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            // Error-unwind safety; normal paths explicitly release and report
            // failures rather than relying on this best-effort cleanup.
            let _ = unsafe { self.mutex.ReleaseSync(0) };
        }
    }
}

pub(crate) struct FrameBackend {
    mailbox: Arc<GpuMailbox>,
    device: Option<Device>,
    owned: Option<OwnedTexture>,
    viewport: Option<(u32, u32)>,
    published: Option<(Binding, Option<u32>)>,
}

impl FrameBackend {
    pub(crate) fn new(mailbox: Arc<GpuMailbox>) -> Self {
        Self {
            mailbox,
            device: None,
            owned: None,
            viewport: None,
            published: None,
        }
    }

    fn binding(&self) -> Result<Option<Binding>> {
        Ok(*self
            .mailbox
            .binding
            .lock()
            .map_err(|_| anyhow!("GPU binding mutex poisoned"))?)
    }

    fn publish(&mut self, binding: Binding) -> Result<()> {
        // Keep binding stable through replacement; worker must also check the
        // generation after taking the newest-only update. No stale handle can
        // be published against a replacement surface during a callback.
        let current = self
            .mailbox
            .binding
            .lock()
            .map_err(|_| anyhow!("GPU binding mutex poisoned"))?;
        if *current != Some(binding) {
            return Ok(());
        }
        let owned = self.owned.as_ref().filter(|owned| {
            owned.spec == TextureSpec::for_binding(binding)
                && owned.content_binding == Some(binding)
                && self.mailbox.visible.load(Ordering::Acquire)
        });
        let handle = owned.map(|owned| owned.handle);
        if self.published == Some((binding, handle)) {
            return Ok(());
        }
        let lease = owned.map(|owned| Arc::clone(&owned.lease));
        *self
            .mailbox
            .update
            .lock()
            .map_err(|_| anyhow!("GPU update mutex poisoned"))? = Some(HandleUpdate {
            binding,
            handle,
            lease,
        });
        self.published = Some((binding, handle));
        self.mailbox.worker_wakeup.notify_one();
        Ok(())
    }
}

impl BrowserFrameBackend for FrameBackend {
    fn resize(&mut self, width: u32, height: u32, _scale: f32) -> Result<()> {
        self.viewport = Some((width, height));
        Ok(())
    }

    fn submit_frame(&mut self, frame: BrowserFrame<'_>) -> Result<()> {
        let (handle, width, height, source_x, source_y) = match frame {
            BrowserFrame::D3d11 {
                handle,
                width,
                height,
                source_x,
                source_y,
            } => (handle, width, height, source_x, source_y),
            BrowserFrame::Bgra { .. } => bail!(
                "Windows native composition requires CEF accelerated D3D11 frames; CPU frames are disabled"
            ),
        };
        let Some(binding) = self.binding()? else {
            return Ok(());
        };
        if !self.mailbox.visible.load(Ordering::Acquire) {
            return Ok(());
        }
        // Resize callbacks may still reference the old viewport for a few
        // frames. Never publish those textures to the new game surface.
        if width == 0
            || height == 0
            || (width, height) != (binding.width, binding.height)
            || self.viewport.is_some_and(|size| size != (width, height))
        {
            return Ok(());
        }
        ensure!(
            !handle.is_null(),
            "CEF supplied a null D3D11 NT shared handle"
        );
        let spec = TextureSpec::for_binding(binding);
        if self
            .device
            .as_ref()
            .is_none_or(|device| device.gpu != spec.gpu)
        {
            let device = Device::new(spec.gpu)?;
            self.owned = None;
            self.device = Some(device);
        }
        let device = self
            .device
            .as_mut()
            .context("overlay GPU device was not initialized")?;
        ensure!(
            !device.failed,
            "overlay GPU completion previously failed; reattach the game surface before accepting another CEF frame"
        );
        // Open on EVERY callback. The NT handle is borrowed and may be reused
        // by CEF for an entirely different resource after this method returns.
        // Neither the NT handle nor this source COM reference enters the lease.
        let source: ID3D11Texture2D = unsafe { device.shared.OpenSharedResource1(HANDLE(handle)) }
            .context("opening callback-borrowed CEF NT texture on the game GPU; cross-adapter CEF textures are unsupported")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        let right = source_x
            .checked_add(width)
            .context("CEF texture crop overflowed")?;
        let bottom = source_y
            .checked_add(height)
            .context("CEF texture crop overflowed")?;
        ensure!(
            right <= desc.Width && bottom <= desc.Height,
            "CEF visible texture rectangle exceeds the {}x{} shared resource",
            desc.Width,
            desc.Height
        );
        ensure!(
            desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM
                && desc.SampleDesc.Count == 1
                && desc.SampleDesc.Quality == 0
                && desc.MipLevels == 1
                && desc.ArraySize == 1,
            "CEF texture must be single-sample BGRA8 with one mip/layer; descriptor: {desc:?}"
        );
        if self.owned.as_ref().is_none_or(|owned| owned.spec != spec) {
            self.owned = Some(device.owned_texture(spec)?);
        }
        let owned = self
            .owned
            .as_mut()
            .context("overlay shared texture was not initialized")?;
        let destination_guard = match owned.mutex.as_ref() {
            Some(mutex) => match KeyGuard::acquire(mutex, "overlay destination")? {
                Some(guard) => Some(guard),
                None => return Ok(()),
            },
            None => None,
        };
        owned.content_binding = None;
        unsafe {
            let region = D3D11_BOX {
                left: source_x,
                top: source_y,
                front: 0,
                right,
                bottom,
                back: 1,
            };
            device.context.CopySubresourceRegion(
                &owned.input,
                0,
                0,
                0,
                0,
                &source,
                0,
                Some(&region),
            );
            device
                .context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            device.context.VSSetShader(&device.vertex_shader, None);
            device.context.PSSetShader(&device.pixel_shader, None);
            device.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            device
                .context
                .PSSetShaderResources(0, Some(&[Some(owned.input_view.clone())]));
            device
                .context
                .OMSetRenderTargets(Some(&[Some(owned.target_view.clone())]), None);
            // Private immediate context: default blend state writes straight
            // RGBA unchanged; no sampler, vertex buffer, or depth buffer.
            device.context.Draw(3, 0);
            // Context must not retain the output across its ACK lease lifetime
            // or keep the private input bound when the next frame copies to it.
            device.context.PSSetShaderResources(0, Some(&[None]));
            device.context.OMSetRenderTargets(None, None);
        }
        // CEF explicitly supplies NO keyed mutex. Keep its callback-local COM
        // reference alive until this proves all reads (and owned writes) done.
        let completion = device.wait_for_completion();
        let destination_release = destination_guard
            .map(|guard| guard.release("overlay destination"))
            .transpose();
        completion?;
        destination_release?;
        owned.content_binding = Some(binding);
        self.publish(binding)
    }

    fn set_visible(&mut self, visible: bool) -> Result<()> {
        self.mailbox.visible.store(visible, Ordering::Release);
        if !visible {
            // The worker's acknowledged lease keeps the published texture alive
            // until it processes the following None update. The frame backend
            // need not retain its private textures while CEF is hidden.
            self.owned = None;
        }
        if let Some(binding) = self.binding()? {
            self.publish(binding)?;
        }
        Ok(())
    }
}
