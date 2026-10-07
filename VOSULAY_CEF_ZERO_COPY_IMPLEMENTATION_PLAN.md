# Vosulay Linux CEF Zero-Copy Renderer
## Implementation plan for Codex / OMP

**Status:** execution-ready architecture and migration plan  
**Product base:** `shiruviaa/vosulay@main`  
**Native reference implementation:** closed PR #2, commit `d1027c9b89a6aca55a155cc0771b2220e380bfb1`  
**CEF baseline:** `chromiumembedded/cef@d08600e4c535c17bfd23bec4628c0fcc0b1b5316`  
**Chromium baseline:** `refs/tags/151.0.7922.47`  
**depot_tools baseline:** `94e89b10b92cc9d6e58fc8d1b6474b7d29e8a114`  
**Primary graphics target:** native osu!lazer on NVIDIA Linux, OpenGL through SDL/GLX or EGL  
**Secondary graphics target:** osu!stable under Wine/Winello, GLX on NVIDIA  
**Deferred target:** Vulkan composition after the GL path is production-ready  
**Primary rendering objective:** asdf-overlay-style shared-allocation rendering with no CPU framebuffer copy and no permanent GPU normalization copy  
**Compatibility fallback:** client-owned GPU normalization copy; then persistent SHM only when GPU sharing is unavailable or proven incorrect

---

## 1. Executive decision

Implement a focused CEF fork that changes only accelerated OSR frame ownership.

Stock CEF returns Chromium's accelerated frame to its internal pool immediately after `OnAcceleratedPaint` returns. Vosulay needs to retain that frame until the game has completed its final GPU sample. The fork will expose a refcounted lease object whose release completes CEF's existing `FrameSinkVideoConsumerFrameCallbacks::Done()` callback.

The final Linux path is:

```text
Chromium compositor
    │ renders
    ▼
Chromium-owned DMA-BUF pool
    │
    │ leased frame; no copy
    ▼
vosulay-renderd
    │ BufferRegister once per unique allocation
    │ FrameReady once per produced frame
    ▼
authenticated SOCK_SEQPACKET broker
    ▼
libvosulay-present.so inside osu!
    │ import each unique allocation once
    │ reuse latest texture on every game present
    │ insert one final-use fence when retiring a texture
    ▼
FrameRelease
    ▼
CEF lease Release()
    ▼
Chromium may reuse that pool allocation
```

The migration must **not** make Electron the canonical Linux host. Use `main` as the product base and transplant the well-tested native transport and injection stack from PR #2. Replace Qt/QtWebEngine with a separate CEF renderer process only after the leased-buffer proof is working.

The permanent design must keep three clocks separate:

1. **Game composition cadence:** draw the latest completed overlay on every eligible game present.
2. **Browser production cadence:** generate fresh Chromium frames at `min(game rate, configured cap, measured render capacity)`.
3. **Buffer lifetime:** release a leased Chromium allocation only after it is superseded and its final game-side GPU fence signals.

---

## 2. Codex / OMP operating contract

Treat this document as normative. Do not compress the work into one implementation pass or one large pull request.

### 2.1 Required workflow

For every phase:

1. Read the relevant source and repository guidance before editing.
2. Write or update tests before completing the implementation.
3. Make the smallest coherent code change for that phase.
4. Run the phase-local test commands and the broader required checks.
5. Perform an independent adversarial review focused on ownership, synchronization, fail-open behavior, and hot-path work.
6. Commit only after the phase gate passes.
7. Stop at a failed gate. Do not mask a failed zero-copy proof by silently introducing a permanent copy path.

### 2.2 Branching

Use a new branch from current `main`:

```sh
git switch main
git pull --ff-only
git switch -c feat/linux-cef-zero-copy
```

Recover the native implementation from PR #2 without restoring the Electron host:

```sh
git fetch origin refs/pull/2/head:refs/remotes/origin/pr-2
git rev-parse origin/pr-2
# Expected: d1027c9b89a6aca55a155cc0771b2220e380bfb1

git restore --source=origin/pr-2 -- native
```

If the pull ref is unavailable, fetch the commit directly:

```sh
git fetch origin d1027c9b89a6aca55a155cc0771b2220e380bfb1
git restore --source=d1027c9b89a6aca55a155cc0771b2220e380bfb1 -- native
```

Do not restore `desktop/`, Electron packaging, Electron frame pacing, or the old PR's root-level host rewrite into the production branch. They remain references only.

### 2.3 Commit discipline

Use phase-scoped commits such as:

```text
native: recover authenticated overlay transport from pr 2
cef: add accelerated frame lease API
native: add leased buffer protocol v2
gl: cache imported leased buffers by generation
renderer: add cef osr daemon
pacing: drive external begin frames from game presents
linux: add obs-private launch chain
linux: replace qt shell with cef supervisor
```

Avoid drive-by formatting, unrelated dependency upgrades, and mass renames.

### 2.4 Safety rules for autonomous agents

Codex / OMP must never:

- block the game presentation thread on browser, socket, fence, or process work;
- call `glFinish()` during normal presentation;
- release a Chromium lease merely because a timeout elapsed;
- assume a DMA-BUF is linear;
- assume every accelerated frame is one plane unless the negotiated capability explicitly says so;
- assume CEF's internal pool has exactly four buffers;
- use frame demand as a release acknowledgement;
- infer the active game drawable from creation order;
- hide import or synchronization failures by claiming zero-copy;
- remove the SHM fallback before the DMA-BUF path passes real-hardware validation;
- make OBS capture exclusion depend on undefined preload ordering;
- put CEF or Chromium code inside the game process;
- merge the host migration before the standalone leased-frame proof passes.

---

## 3. Scope and non-goals

### 3.1 In scope

- Focused CEF accelerated-frame lifetime patch.
- Stable allocation identity and generation handling.
- Explicit or accurately reported implicit producer synchronization.
- Cached DMA-BUF imports in native OpenGL.
- Final-use GPU fence retirement.
- Native game-present-driven CEF BeginFrame pacing.
- OBS game-capture exclusion through deterministic launch-time hook ordering.
- Rust supervisor migration away from Qt and QtWebEngine.
- Existing edit/input, target selection, authentication, and restart behavior.
- Persistent SHM compatibility path.
- Packaging pinned CEF artifacts and the native agent.
- Real NVIDIA validation on native lazer and Wine stable.

### 3.2 Deferred until the GL path is complete

- Full Vulkan parity.
- Atlas packing and multi-widget instanced composition.
- Multiple simultaneously rendered game clients.
- Zero-copy popup composition as a first milestone.
- Dynamic OBS-private mode switching without restarting the game.
- A generalized third-party renderer plug-in ABI.
- Upstreaming the CEF patch.

### 3.3 Explicit non-goals

- Forking Chromium's compositor.
- Replacing Chromium's renderer.
- Implementing a custom HTML/CSS engine.
- Copying full frames through Rust or C++ CPU memory in the primary path.
- Maintaining Electron solely for Linux rendering.
- Relying on a transparent desktop overlay window.
- Matching an uncapped 500–1000 Hz game with 500–1000 Hz Chromium rendering.

---

## 4. Target repository architecture

```text
vosulay/
├── src/
│   ├── main.rs
│   ├── app.rs                         # application lifecycle
│   ├── overlay_supervisor.rs          # target selection + session orchestration
│   ├── renderer.rs                    # spawn/control vosulay-renderd
│   ├── internal_server.rs             # private scene/control HTTP + WebSocket
│   ├── config.rs
│   ├── api.rs
│   ├── remote.rs
│   ├── ui.rs
│   └── ...
├── native/
│   ├── crates/
│   │   ├── overlay-protocol/
│   │   ├── overlay-host/
│   │   ├── overlay-agent-core/
│   │   ├── overlay-agent-gl/
│   │   ├── overlay-dotnet-bootstrap/
│   │   ├── overlay-vulkan-layer/      # retained, deferred
│   │   ├── overlay-renderer-client/   # new Rust staticlib/C ABI for renderd
│   │   └── overlay-node/              # reference only; remove from final build
│   └── test/
├── renderer/
│   └── cef/
│       ├── CMakeLists.txt
│       ├── main.cc
│       ├── app.{h,cc}
│       ├── browser_client.{h,cc}
│       ├── render_handler.{h,cc}
│       ├── frame_registry.{h,cc}
│       ├── pacing_controller.{h,cc}
│       ├── input_bridge.{h,cc}
│       └── tests/
├── third_party/
│   └── cef/
│       ├── VERSION
│       ├── SHA256SUMS
│       ├── PATCHES
│       └── patches/
│           ├── 0001-osr-add-accelerated-frame-lease.patch
│           └── 0002-osr-begin-frame-timing.patch   # optional; not applied initially
├── tools/
│   └── cef/
│       ├── build-patched-cef.sh
│       ├── fetch-prebuilt-cef.sh
│       ├── package-cef.sh
│       └── verify-cef.sh
└── extra/
    ├── vosulay-launch
    └── obs-private-launch
```

### 4.1 Process boundary

```text
┌────────────────────────────────────────────────────────────┐
│ vosulay supervisor — Rust                                  │
│ config, counter discovery, local authorization, remote UI, │
│ target discovery, attach/restart, renderer lifecycle       │
└───────────────┬───────────────────────────────┬────────────┘
                │ renderer control              │ agent control
                ▼                               ▼
┌─────────────────────────────┐     ┌──────────────────────────┐
│ vosulay-renderd             │     │ target game             │
│ CEF + Chromium subprocesses │     │ libvosulay-present.so   │
│ no game hooks               │     │ no browser code         │
└─────────────────────────────┘     └──────────────────────────┘
```

The supervisor may relay file descriptors, but it must never map or copy image memory. Buffer registration is infrequent; frame-ready messages contain only identifiers, metadata, and optionally one synchronization FD.

### 4.2 Reuse versus replacement

| Component | Action | Reason |
|---|---|---|
| `src/config.rs`, `src/api.rs`, `src/remote.rs`, `src/ui.rs` | Keep and adapt | Existing Rust product/domain authority |
| `src/qt/*`, `src/ffi.rs`, Qt-specific `build.rs` logic | Remove after CEF integration | Replaced by renderer process and local control transport |
| PR #2 `overlay-protocol` | Reuse and version | Bounded explicit wire format and FD validation |
| PR #2 `overlay-host` | Reuse and expose directly to root Rust | Authentication/session ownership already exists |
| PR #2 `overlay-agent-core` | Reuse | Latest-wins mailbox, control thread, fail-open behavior |
| PR #2 `overlay-agent-gl` | Reuse, then refactor | State restoration, GL hooks, target selection, fences |
| PR #2 `overlay-dotnet-bootstrap` | Reuse | Native lazer late attach |
| PR #2 `overlay-vulkan-layer` | Preserve but defer | Avoid regressing future Vulkan support |
| PR #2 `overlay-node` | Do not use in final host | N-API existed only for Electron |
| Electron `LinuxOverlayProcess` | Reference only | Its release accounting informs CEF lease ownership |
| Electron JS pacing | Replace | Native monotonic scheduler must drive CEF |
| idk-overlay CEF producer | Reference/fallback | Useful GPU staging and demand-loop proof |
| patched CEF | New dependency | Required for safe retained Chromium buffers |

---

## 5. Non-negotiable runtime invariants

1. **Original game presentation is always attempted.**
2. **The present hook never waits for a browser frame.**
3. **The present hook never performs socket I/O.**
4. **The present hook never performs ordinary blocking GPU synchronization.**
5. **After warm-up, the present hook performs no allocation and no DMA-BUF import.**
6. **A Chromium allocation is never released while the game can still sample it.**
7. **A superseded READY frame that was never drawn may be released without a GPU fence.**
8. **A CURRENT frame is fenced only when it is retired, not on every repeated draw.**
9. **A frame-demand message does not transfer or release ownership.**
10. **Buffer identity is scoped by renderer generation.**
11. **Target creation is not target selection.**
12. **Import failure disables the overlay path, not the game.**
13. **Synchronization capability is reported truthfully.**
14. **The primary path contains zero CPU framebuffer copies and zero GPU framebuffer copies.**
15. **The fallback GPU-copy path is separately named and measured; it must not be reported as zero-copy.**
16. **Disconnect immediately releases input capture.**
17. **No panic or C++ exception may escape into the target process or CEF callback boundary.**
18. **Every lease release is idempotent and observable exactly once.**
19. **Renderer teardown and agent teardown are ordered; timeout alone is not completion proof.**
20. **OBS-private mode is guaranteed only when the hook chain is established before first capture.**

---

## 6. CEF fork specification

## 6.1 Pinning

Create or use a dedicated CEF fork branch:

```text
upstream commit:
d08600e4c535c17bfd23bec4628c0fcc0b1b5316

Chromium:
refs/tags/151.0.7922.47

depot_tools:
94e89b10b92cc9d6e58fc8d1b6474b7d29e8a114

fork branch:
vosulay/151.3.11-leased-osr
```

Record all three revisions in:

```text
third_party/cef/VERSION
third_party/cef/PATCHES
third_party/cef/SHA256SUMS
```

Normal vosulay builds must consume a verified prebuilt CEF artifact. Do not trigger a full CEF/Chromium source build from `cargo build`.

## 6.2 Public API

Append a new callback and a new refcounted interface. Preserve the existing callback for source compatibility.

Conceptual API:

```cpp
class CefAcceleratedFrameLease : public virtual CefBaseRefCounted {
 public:
  // Opaque, stable only within pool_generation.
  virtual uint64_t GetBufferId() = 0;

  // Changes when the OSR pool or producer generation is reset.
  virtual uint64_t GetPoolGeneration() = 0;

  // Idempotent. May be called from any thread.
  virtual void Release() = 0;

  virtual bool IsReleased() = 0;
};

class CefRenderHandler : public virtual CefBaseRefCounted {
 public:
  // Existing method remains unchanged.
  virtual void OnAcceleratedPaint(
      CefRefPtr<CefBrowser> browser,
      PaintElementType type,
      const RectList& dirtyRects,
      const CefAcceleratedPaintInfo& info) {}

  // New method is appended at the end of the interface.
  //
  // Default behavior calls the old callback. If the client does not retain
  // |lease|, its last reference is dropped when this callback returns and the
  // frame is released exactly as stock CEF behaves.
  virtual void OnAcceleratedPaintWithLease(
      CefRefPtr<CefBrowser> browser,
      PaintElementType type,
      const RectList& dirtyRects,
      const CefAcceleratedPaintInfo& info,
      CefRefPtr<CefAcceleratedFrameLease> lease) {
    OnAcceleratedPaint(browser, type, dirtyRects, info);
  }
};
```

The CEF documentation must state:

- `info` plane FDs remain valid while the lease is retained.
- A client that passes FDs through `SCM_RIGHTS` may close its duplicate after import.
- Chromium cannot reuse the allocation until the lease is released.
- `Release()` is idempotent.
- Destruction without an explicit `Release()` performs an automatic release.
- Retaining too many leases will naturally backpressure the finite producer pool.
- `buffer_id` is opaque and must be combined with `pool_generation`.

## 6.3 Internal implementation

Primary patch points:

```text
include/cef_render_handler.h
include/cef_accelerated_frame_lease.h             # new
libcef/browser/osr/video_consumer_osr.{h,cc}
libcef/browser/osr/render_widget_host_view_osr.{h,cc}
libcef/browser/osr/accelerated_frame_lease.{h,cc} # new
tests/ceftests/...                                # locate existing OSR tests
```

Generated translator outputs must be regenerated rather than hand-edited.

Implement `CefAcceleratedFrameLeaseImpl` with:

```text
- atomic released flag
- opaque buffer_id
- pool_generation
- owning reference to the GPU-memory-buffer handle or duplicated plane FDs
- PendingRemote<FrameSinkVideoConsumerFrameCallbacks>
- original sequenced task runner
- Release() that atomically wins once and posts Done() to the original sequence
- destructor that calls Release()
```

Modify `CefVideoConsumerOSR::OnFrameCaptured`:

```text
CPU path:
    keep existing scoped Done behavior

accelerated path:
    do not create ScopedVideoFrameDone
    construct lease from callbacks + data + identity
    build CefAcceleratedPaintInfo
    call OnAcceleratedPaintWithLease(...)
    return
```

The view/client dispatch must pass the lease without converting it into a raw pointer or stack-only object.

## 6.4 Buffer identity

Do not depend on an internal Chromium `GpuMemoryBuffer` ID unless a stable public value is already present at the exact pinned revision.

Derive identity in the CEF layer from every unique DMA-BUF plane:

```text
fstat(fd):
    st_dev
    st_ino

plus:
    plane offset
    plane stride
    plane size
    modifier
    coded size
    format
```

Normalize duplicate plane FDs before hashing. Map this identity to a monotonic `buffer_id` within the current pool generation.

Generation policy:

- allocate a unique initial generation per `CefVideoConsumerOSR`;
- increment on fixed-size capture restart;
- increment on view resize that recreates capture constraints;
- increment on GPU/Viz loss or explicit renderer reset;
- never reuse a `buffer_id` in the same generation;
- emit a generation-reset event before publishing frames from a new generation.

The game cache key is:

```text
{renderer_session_id, pool_generation, buffer_id, layer_id, GL context/share group}
```

## 6.5 Lease budget

Configure a maximum of four client-held leases, but do not assume CEF's internal pool size is four.

Expected steady state:

```text
one CURRENT
zero or one READY
zero or one RETIRING
one buffer potentially being rendered internally by Chromium
```

When the client-held lease budget is full:

- do not request another external BeginFrame;
- reuse the current game texture;
- release any unsubmitted superseded frame;
- never block CEF's UI thread waiting for the game.

## 6.6 CEF test gate

Add deterministic tests for:

- callback returns without retained lease → `Done()` runs once;
- retained lease → `Done()` does not run at callback return;
- explicit release → `Done()` runs once;
- repeated release → still once;
- destructor release → once;
- release from a worker thread → completion occurs on the correct sequence;
- shutdown with outstanding leases → no use-after-free and no duplicate completion;
- CPU `OnPaint` path remains unchanged;
- old handler overriding only `OnAcceleratedPaint` retains stock behavior;
- unique DMA-BUF identities map to unique IDs;
- repeated identity in one generation maps to the same ID;
- generation reset invalidates prior IDs.

**Gate CEF-1:** all lease tests pass under ASan/UBSan where supported.  
**Stop condition:** do not start host integration until `Done()` lifetime is proven by tests and a standalone capture demo.

---

## 7. Synchronization design

Two independent ownership edges must be proven:

```text
producer rendering finished
    ↓
game may sample
    ↓
game's final sample finished
    ↓
producer may overwrite
```

## 7.1 Producer-to-game acquire synchronization

Keep the CEF patch small. Do not initially patch Chromium to export a fence.

In `vosulay-renderd`, after receiving the leased `OnAcceleratedPaint` callback:

1. Duplicate the relevant DMA-BUF FD if required by the transport queue.
2. Call `DMA_BUF_IOCTL_EXPORT_SYNC_FILE` with `DMA_BUF_SYNC_READ`.
3. For a one-plane buffer, attach that `sync_file` FD to `FrameReady`.
4. For multiple distinct DMA-BUF plane objects, merge their sync files with `SYNC_IOC_MERGE`.
5. If the ioctl is unsupported, set the session to negotiated implicit-sync mode and record a diagnostic.
6. Never advertise `CAP_ACQUIRE_SYNC_FILE` when no actual sync-file export occurred.

Initial GLX policy:

- agent control thread polls the acquire sync FD;
- only after it signals does the control thread publish the frame as READY to the lock-free mailbox;
- the game present hook therefore never waits.

Future EGL/Vulkan optimization:

- import the native fence into a GPU wait primitive;
- preserve the same protocol and capability bit;
- do not make this a prerequisite for the first GL production release.

## 7.2 Game-to-producer release synchronization

On frame switch:

```text
old CURRENT → RETIRING
new READY   → CURRENT
insert glFenceSync after all old-frame draws already queued
glFlush
```

Poll retirement fences with timeout zero on later presentations. Once signaled:

```text
RETIRING → releasable
control queue emits FrameRelease
renderd calls lease.Release()
```

Do not create or replace a fence while the same CURRENT texture is repeatedly drawn. Commands in one GL context are ordered; a single fence inserted when switching away is ordered after all prior samples.

Exceptional failures:

- `GL_WAIT_FAILED` is not completion.
- A failed fence query may invoke one recovery `glFinish()` outside normal behavior, withdraw DMA-BUF capability, release proven-complete frames, and restart in fallback mode.
- A failed fence creation after a draw requires the same exceptional completion proof before lease release.
- Every such event must be diagnostic and counted.

## 7.3 Disconnect and teardown

Renderer initiated shutdown:

1. Stop new BeginFrames.
2. Stop publishing frames.
3. Ask the agent to clear the overlay.
4. Wait for agent retirement acknowledgements within the normal orderly-shutdown path.
5. Release all confirmed-complete leases.
6. Destroy the browser and renderer process.

Agent or game process exit:

- process exit destroys the consumer GPU context;
- supervisor observes pidfd/process termination;
- renderer may release leases after the target is confirmed gone;
- do not release solely on a socket timeout while the game is still alive.

Renderer crash:

- Chromium producer process tree exits, so it cannot overwrite buffers;
- game keeps or drops stale imports but receives no new frame;
- supervisor tears down the session and restarts renderd;
- new renderd uses a new session and generation.

---

## 8. Protocol v2

Retain the existing explicit little-endian `SOCK_SEQPACKET` protocol and version it. Do not expose Rust struct layout.

## 8.1 Version and capabilities

Bump the protocol minor version. Keep major compatibility while old message kinds remain valid.

Add:

```rust
pub const CAP_DMABUF_LEASED: u64       = 1 << 4;
pub const CAP_BUFFER_CACHE: u64         = 1 << 5;
pub const CAP_ACQUIRE_SYNC_FILE: u64    = 1 << 6;
pub const CAP_FRAME_TIMESTAMPS: u64     = 1 << 7;
pub const CAP_MULTI_PLANE_DMABUF: u64   = 1 << 8;
```

Negotiation rules:

- leased mode requires `CAP_DMABUF | CAP_DMABUF_LEASED | CAP_BUFFER_CACHE`;
- explicit acquire synchronization additionally requires `CAP_ACQUIRE_SYNC_FILE`;
- initial NVIDIA release may gate to one-plane AR24/AB24 while still using v2;
- old `Dmabuf` and `Consumed` remain available for Electron/reference or fallback clients;
- SHM remains valid without leased capabilities.

## 8.2 New message kinds

Reserve exact IDs after the current highest kind:

```rust
BufferRegister      = 19,
FrameReady          = 20,
FrameRelease        = 21,
BufferImported      = 22,
BufferImportFailed  = 23,
GenerationReset     = 24,
PresentDemand       = 25,
RendererState       = 26,
```

Do not overload the old `FrameRequest` payload. `PresentDemand` is a new timestamped demand message.

## 8.3 BufferRegister

Sent once per unique buffer allocation per generation and layer.

Header fields:

```text
target_id            u64
pool_generation      u64
buffer_id            u64
layer_id             u32  # 0=view, 1=popup
width                u32
height               u32
fourcc               u32
pixel_format         u32  # straight/premultiplied; RGBA/BGRA
origin               u32  # top-left or bottom-left
plane_count          u32
flags                u32
modifier             u64
```

Each plane:

```text
stride               u32
reserved             u32 = 0
offset               u64
size                 u64
```

Ancillary data:

```text
1..=4 DMA-BUF FDs, one for each transmitted plane entry
```

Validation:

- dimensions bounded by protocol maximum;
- plane count and FD count agree;
- checked arithmetic for row/plane bounds;
- format/FourCC combination is supported;
- modifier is preserved exactly;
- duplicate FDs are allowed only when multiple planes share an allocation and metadata is consistent;
- no unbounded vectors.

## 8.4 FrameReady

Sent once per leased Chromium frame.

Fixed fields:

```text
target_id               u64
pool_generation         u64
buffer_id               u64
frame_sequence          u64
present_demand_sequence u64
capture_counter         u64
producer_timestamp_ns   u64

visible_x               i32
visible_y               i32
visible_width           u32
visible_height          u32

damage_count            u32
flags                   u32
```

Followed by at most 16 damage rectangles:

```text
x i32
y i32
width u32
height u32
```

Flags include:

```text
HAS_ACQUIRE_SYNC_FD
FULL_DAMAGE
POPUP_VISIBLE
```

Ancillary data:

```text
0 FDs in implicit-sync mode
1 sync_file FD when HAS_ACQUIRE_SYNC_FD
```

A `FrameReady` must never carry the image DMA-BUF again.

## 8.5 FrameRelease

```text
target_id          u64
pool_generation    u64
buffer_id          u64
frame_sequence     u64
reason             u32
reserved           u32
```

Reasons:

```text
GPU_COMPLETE
SUPERSEDED_BEFORE_DRAW
TARGET_DESTROYED
GENERATION_RESET_AFTER_COMPLETION
IMPORT_REJECTED
```

`TARGET_DESTROYED` and generation reset are valid only after completion or confirmed target process/context destruction.

## 8.6 BufferImported / BufferImportFailed

Import occurs on a thread with the real game GL context, usually the next present after registration.

Success:

```text
pool_generation u64
buffer_id       u64
context_id      u64
import_path     u32
```

Failure:

```text
pool_generation u64
buffer_id       u64
context_id      u64
error_class     u32
gl_or_egl_error u32
```

The control thread sends the result. The present hook only records it in a bounded queue.

## 8.7 GenerationReset

```text
target_id           u64
old_generation      u64
new_generation      u64
reason              u32
```

On reset:

- no new old-generation frame can become CURRENT;
- READY old-generation frames are dropped;
- CURRENT/RETIRING frames complete retirement or are destroyed with the dead context;
- cached imports are deleted only after safe completion;
- renderd does not release leases prematurely.

## 8.8 PresentDemand

```text
target_id             u64
demand_sequence       u64
present_time_ns       u64
estimated_period_ns   u64
drawable_width        u32
drawable_height       u32
flags                 u32
reserved              u32
```

Demand is latest-wins and coalescible. It has no ownership meaning.

## 8.9 Protocol tests

Add:

- exact encode/decode fixtures in Rust;
- malformed payload and FD-count tests;
- integer-overflow cases;
- one-to-four-plane fixtures;
- optional sync FD validation;
- stale generation rejection;
- duplicate release rejection;
- cross-language fixture tests against the C ABI used by CEF;
- fuzz targets for every new decoder.

**Gate PROTO-1:** all existing v1 tests remain green and every v2 decoder rejects malformed length/FD combinations without leaks.

---

## 9. OpenGL agent refactor

## 9.1 Split hook roles

Build two libraries:

```text
libvosulay-present.so
    presentation hooks
    overlay composition
    buffer cache
    no dlsym/dlvsym interposition

libvosulay-wine-resolver.so
    optional dlsym/dlvsym/proc-address interception
    needed only for Wine/late-attach paths that bypass ordinary symbol interposition
```

The split prevents direct conflict with obs-vkcapture, which also interposes symbol lookup.

## 9.2 Real-symbol resolution

`libvosulay-present.so` must resolve the real provider without calling an interposable `dlsym`/`dlvsym`.

Use one of:

- `dl_iterate_phdr` plus ELF dynamic symbol/GNU hash lookup;
- a small audited `elfhacks`-style provider resolver.

Resolve from actual provider modules:

```text
libGLX.so / libGLX_nvidia.so
libEGL.so / provider
```

Validate resolved function ownership with `dladdr`. Never accept a pointer back into vosulay itself.

## 9.3 Hook coverage

At minimum:

```text
glXSwapBuffers
glXSwapBuffersMscOML
eglSwapBuffers
eglSwapBuffersWithDamageEXT
eglSwapBuffersWithDamageKHR
glXGetProcAddress
glXGetProcAddressARB
eglGetProcAddress
```

The proc-address hooks belong in the Wine resolver where possible.

Add a thread-local reentrancy guard. Nested or recursive swaps call the next function without drawing again.

## 9.4 Buffer cache

Key imports by:

```text
GL context/share-group identity
target ID
pool generation
buffer ID
layer ID
```

States:

```text
REGISTERED_PENDING_IMPORT
IMPORTED
READY
CURRENT
RETIRING
FAILED
STALE
```

First import may happen in the presentation callback because that is where the real context is current. This is allowed only once per unique allocation. Limit first-import work to one pending buffer per present to bound startup hitch.

After warm-up:

- no image FDs arrive per frame;
- no texture or memory-object creation occurs per frame;
- `FrameReady` selects an existing import.

## 9.5 Import paths

### Native EGL/OpenGL

Preferred:

```text
DMA-BUF
  → eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT, modifier attrs)
  → glEGLImageTargetTexture2DOES
  → GL_TEXTURE_2D
```

### Wine/GLX on NVIDIA

Preferred:

```text
native allocation FD
  → glCreateMemoryObjectsEXT
  → glImportMemoryFdEXT(GL_HANDLE_TYPE_OPAQUE_FD_EXT)
  → glTexStorageMem2DEXT
  → GL_TEXTURE_2D
```

Do not fall through to EGLImage in a GLX context when the memory-object import fails; that path can appear successful and sample black.

### Validation

On first import path use per process/context:

- draw a sequence-coded checkerboard;
- verify dimensions/internal format;
- optionally run a tiny readback outside gameplay during setup;
- detect channel swap, vertical inversion, opaque black, and modifier/layout corruption.

If direct import is API-successful but visually/layout incorrect:

1. mark direct import unsupported for that driver/context;
2. enable the named **GPU-normalized fallback**;
3. copy into a vosulay-owned GBM/Vulkan external-memory pool outside the game present thread;
4. keep SHM as the last fallback.

Do not silently use the copy path while reporting zero-copy.

## 9.6 Presentation hot path

Warmed pseudo-code:

```rust
fn present_hook(args...) {
    if REENTRANT.replace(true) {
        return real_present(args...);
    }

    record_present_timestamp_atomically();
    poll_retirement_fences_timeout_zero();
    process_at_most_one_pending_import_if_needed();

    if let Some(newest_ready) = mailbox.take_newest_ready() {
        if newest_ready != current {
            retire_current_with_one_fence();
            current = newest_ready;
        }
    }

    if visible && current.is_some() {
        save_minimum_required_state();
        draw_premultiplied_overlay(current);
        restore_state();
    }

    REENTRANT.set(false);
    real_present(args...)
}
```

Normal path exclusions:

```text
no socket calls
no logging
no heap allocation
no blocking mutex
no glFinish
no waiting for acquire fences
no browser callbacks
no fd imports after pool warm-up
```

## 9.7 Blending and state

Normalize or describe Chromium output correctly. The preferred game blend is premultiplied alpha:

```text
glBlendFuncSeparate(ONE, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA)
```

Use:

- fullscreen triangle generated from `gl_VertexID`, or a static immutable VAO;
- one program and sampler per context;
- explicit default draw framebuffer;
- verified vertical origin;
- sRGB behavior tested rather than assumed.

Preserve at least the state actually mutated:

```text
program
VAO
draw framebuffer
viewport/scissor
blend enable/function/equation
color mask
depth/stencil/cull enables
active texture, texture and sampler
framebuffer sRGB
```

Keep the existing conservative harness first. Add an osu-specific reduced-state fast path only after instrumentation proves end-of-frame invariants.

## 9.8 Fence-on-retirement conversion

Replace the current “replace the frame's latest fence on every repeated draw” model with:

```text
same CURRENT frame presented again:
    no new fence

switch CURRENT A → B:
    draw history of A is already ordered
    insert one fence
    A becomes RETIRING
    B becomes CURRENT
```

This is a measurable optimization at uncapped game rates and must be covered by a harness that proves A is not released before all of its prior draws.

**Gate GL-1:** sequence checkerboard renders correctly through native lazer and Wine GLX; each unique pool buffer is imported once; no ordinary game-thread waits occur.

---

## 10. `vosulay-renderd` design

## 10.1 Process model

Use a dedicated CEF browser process executable. It also acts as CEF's subprocess executable when launched with `--type=...`.

Startup order:

```text
main()
    CefExecuteProcess()
    if subprocess: return

    parse inherited renderer endpoint/cookie
    initialize renderer transport
    CefInitialize()
    create windowless browser
    CefRunMessageLoop()
```

Do not connect to the supervisor before `CefExecuteProcess()` has returned the browser-process path.

## 10.2 CEF configuration

Initial settings:

```text
windowless_rendering_enabled = true
shared_texture_enabled = true
external_begin_frame_enabled = true
transparent background
per-session ephemeral cache
no persistent cookies/storage unless required
remote debugging disabled by default
```

Command-line behavior to test explicitly on NVIDIA:

```text
ANGLE EGL path
X11 Ozone under XWayland where required
renderer/background throttling disabled for the visible OSR scene
GPU compositing enabled
```

Do not cargo-cult flags. Record the final tested set and a `chrome://gpu` diagnostic dump in the hardware validation report.

Use normal CEF message loop plus worker/control threads. A dedicated scheduler posts BeginFrame work to CEF's UI thread.

## 10.3 Rust staticlib renderer client

Add:

```text
native/crates/overlay-renderer-client/
```

Build it as:

```toml
crate-type = ["staticlib"]
```

Expose a narrow C ABI:

```c
typedef struct vosulay_renderer_client vosulay_renderer_client;

int vosulay_renderer_connect(...);
int vosulay_renderer_register_buffer(...);
int vosulay_renderer_publish_frame(...);
int vosulay_renderer_poll_release(...);
int vosulay_renderer_poll_demand(...);
int vosulay_renderer_generation_reset(...);
void vosulay_renderer_destroy(...);
```

Rules:

- no Rust panic crosses the ABI;
- structs have fixed-width fields and explicit size/version;
- ownership of every passed FD is documented;
- calls from CEF callback code are nonblocking or enqueue into a bounded queue;
- full image bytes never cross the ABI;
- cross-language protocol fixtures prove field parity.

The C++ renderer keeps `CefRefPtr<CefAcceleratedFrameLease>` values in a thread-safe lease table keyed by:

```text
{pool_generation, buffer_id, frame_sequence}
```

A release message removes the table entry and calls `Release()` exactly once.

## 10.4 Frame registry

On `OnAcceleratedPaintWithLease`:

1. Reject unsupported paint types or store popup separately.
2. Read generation and buffer ID from lease.
3. If this buffer is new in the generation:
   - duplicate plane FDs;
   - enqueue `BufferRegister`;
   - mark registration pending.
4. Export acquire sync file.
5. Store the lease in the lease table.
6. Enqueue `FrameReady`.
7. Return immediately.

If any queue or validation step fails:

- release this lease locally;
- increment a drop/error counter;
- preserve the previous game frame;
- never block the CEF UI thread.

If a newer frame for the same layer supersedes an unsent frame:

- release the unsent lease locally;
- send no frame to the game.

## 10.5 Popups

CEF delivers `PET_POPUP` separately.

Milestone policy:

- Phase 1 may suppress popups and document that selects/context menus are incomplete.
- Production edit mode must support popup layer `layer_id = 1`.
- Keep the popup rect from `OnPopupSize`.
- Register/cache popup buffers like view buffers.
- Draw view then popup with correct clipping and z-order.
- Popup hide releases/drops its current frame after safe retirement.

Do not blend popup pixels permanently into the view buffer; that would require a copy.

## 10.6 Renderer diagnostics

Expose structured counters:

```text
cef_frames_received
leases_current
leases_peak
buffer_registrations
unique_buffer_ids
frame_ready_sent
frames_dropped_budget
frames_dropped_queue
acquire_fence_export_ok
acquire_fence_export_fallback
release_gpu_complete
release_superseded
begin_frames_requested
begin_frames_coalesced
render_latency_us histogram
frame_age_at_present_us histogram
```

Diagnostics must be rate-limited and disabled in the game hot path.

---

## 11. Native frame pacing

## 11.1 Demand source

Every selected-target game present updates atomics:

```text
present_sequence
last_present_time_ns
drawable width/height
visibility/focus flags
```

The existing agent control thread coalesces these observations and sends only the newest `PresentDemand`. The presentation hook itself sends nothing.

## 11.2 Scheduler

Implement a dedicated native scheduler in `vosulay-renderd`.

Use `CLOCK_MONOTONIC` and absolute sleeps (`clock_nanosleep` or timerfd). Do not use JavaScript timers.

Estimate game period from a bounded ring of recent timestamps using a robust statistic:

```text
- reject impossible/paused intervals
- median or trimmed mean over recent samples
- smooth phase/period without drifting to an independent clock
```

Effective browser rate:

```text
min(
    measured selected-target present rate,
    configured vosulay FPS cap,
    measured CEF sustainable rate,
    optional output-refresh cap
)
```

Game composition always remains at game-present cadence.

## 11.3 BeginFrame admission

Request one frame only when all are true:

```text
overlay visible
selected target exists
no BeginFrame/paint already in flight
client-held lease count below budget
a newer demand sequence exists
configured deadline permits another frame
```

Schedule at:

```text
predicted_next_present
    - adaptive_render_lead_time
```

Measure:

```text
SendExternalBeginFrame timestamp
→ OnAcceleratedPaintWithLease timestamp
```

Use p90/p95 latency plus a small safety margin as adaptive lead time.

If the browser misses the target:

- reuse the prior texture;
- never wait in the game;
- update the latency model.

## 11.4 `requestAnimationFrame`

Initial policy:

- use CEF external BeginFrames as the authoritative rAF cadence;
- do not monkeypatch `requestAnimationFrame`;
- do not replace `performance.now()`;
- keep the page logically visible and unthrottled;
- set `windowless_frame_rate` above or equal to the configured cap.

Acceptance measurements:

```text
60 Hz
120/144 Hz
240 Hz
360 Hz where supported
game rate above browser cap
game rate below browser cap
variable-rate / hitch sequence
hidden and restored overlay
```

Measure both rAF callbacks and accelerated-paint callbacks.

Only if public CEF BeginFrame timestamps fail acceptance, apply optional patch 0002:

```cpp
SendExternalBeginFrameAt(
    base::TimeTicks frame_time,
    base::TimeTicks deadline,
    base::TimeDelta interval);
```

Keep this second patch separate so the ownership patch remains minimal.

## 11.5 Pacing acceptance

- fresh-overlay rate within ±5% of configured cap under steady load;
- no independent timer drift relative to game presents;
- one paint in flight maximum;
- no render requests while all usable leases are held;
- no work while hidden except bounded health checks;
- frame age at game present reported and bounded;
- game present never waits for browser production.

**Gate PACE-1:** 144/240/480/uncapped game tests show composition on every present and browser work capped independently.

---

## 12. OBS game-capture exclusion

## 12.1 Required call chain

Private-to-OBS mode:

```text
game
  → obs-vkcapture glX/egl swap wrapper
      → capture clean game framebuffer
      → libvosulay-present swap wrapper
          → draw overlay
          → real provider swap
```

Deterministic preload order:

```sh
LD_PRELOAD="/path/libobs_glcapture.so:/path/libvosulay-present.so"
OBS_VKCAPTURE=1
exec osu-lazer "$@"
```

Do not invoke the stock OBS wrapper inside a pre-existing vosulay preload and assume the order is correct.

## 12.2 Launcher

Add `extra/obs-private-launch` or fold it into the main launcher.

Responsibilities:

- discover the installed obs-vkcapture library;
- preserve unrelated preload libraries without breaking the required prefix;
- set the exact order;
- set the vosulay session endpoint/cookie;
- launch the target;
- report the resulting chain;
- fail with an actionable diagnostic when the OBS library cannot be found.

## 12.3 Runtime validation

On first swap, use `dladdr` and the direct provider resolver to report:

```text
caller/outer hook: libobs_glcapture.so
current hook:      libvosulay-present.so
next provider:     libGLX_nvidia.so / libEGL provider
policy:            obs-private
```

If the chain is wrong:

- do not claim private mode;
- optionally skip drawing and report failure, based on user policy;
- never destabilize capture or game presentation.

## 12.4 Limitations

- Guaranteed only when the chain is installed before OBS caches the next swap function.
- Late attach may render but cannot guarantee exclusion.
- PipeWire monitor/window capture sees the final displayed overlay.
- This feature excludes vosulay only from in-process OBS game capture.

## 12.5 OBS test gate

Automated or semi-automated test:

1. Render a clean game-color pattern.
2. Draw an unmistakable vosulay test overlay after OBS capture.
3. Capture with obs-vkcapture.
4. Verify:
   - screen contains overlay;
   - game-capture frame does not;
   - reversed preload order does contain overlay as a control;
   - no double-present or recursion occurs.

**Gate OBS-1:** private-mode chain is reproducible on native lazer and documented as launch-only.

---

## 13. Rust host migration

Do not begin this phase until the standalone CEF lease and GL consumer gates pass.

## 13.1 Preserve Rust authority

Keep the current Rust modules as the canonical source for:

- configuration and profile persistence;
- counter discovery;
- local-overlay authorization;
- remote view-only server;
- command-line parsing;
- process/session lifecycle.

Add:

```text
src/app.rs
src/overlay_supervisor.rs
src/renderer.rs
src/internal_server.rs
src/input_bridge.rs
```

## 13.2 Use `overlay-host` directly

Replace the PR's N-API boundary with direct Rust calls to `overlay-host`.

`overlay_supervisor.rs` owns:

```text
Host / HostSession
target process identity
attach or launch mode
selected target
renderer child
agent control thread
frame relay
shutdown state
```

Use pidfd or equivalent liveness observation where available.

## 13.3 Private renderer page

Prefer a private loopback-only internal server for the first production integration:

```text
127.0.0.1:ephemeral
random 256-bit capability in URL/path
no wildcard bind
no remote control routes
strict origin checks
bounded WebSocket messages
```

Routes:

```text
/internal/scene/<token>
/internal/assets/<token>/...
/internal/control/<token>
```

The scene receives normalized profile/config state and sends editing/save messages to Rust. Reuse the existing safe document assembly and local-file authorization logic.

Do not expose native control through the existing remote view-only endpoint.

## 13.4 Renderer lifecycle

One CEF renderer process per active selected game session.

On target switch:

1. stop old demand;
2. clear/retire old target;
3. resize scene;
4. create a new renderer generation;
5. resume demand from the new selected target.

Do not run multiple Chromium process trees merely because multiple candidate game PIDs are discoverable.

## 13.5 Qt removal order

Only after CEF integration passes:

1. stop calling `run_qt_application`;
2. remove `src/qt/*`;
3. remove `src/ffi.rs` Qt bridge usage;
4. remove `cxx` and `cxx-build`;
5. replace Qt-specific `build.rs` with native/CEF artifact validation;
6. remove Qt/LayerShellQt/Dawn setup scripts and runtime path logic;
7. update packaging and documentation;
8. verify no Qt libraries appear in `ldd` or packaged artifacts.

Keep the old Qt path behind a temporary build feature only if needed for one migration checkpoint. Remove it before release.

---

## 14. Fallback hierarchy

The renderer chooses a named mode and reports it.

### Mode A — leased direct zero-copy

```text
CEF/Chromium DMA-BUF
→ cached game import
→ overlay draw
```

Expected per fresh browser frame after warm-up:

```text
CPU framebuffer copies: 0
GPU framebuffer copies: 0
DMA-BUF imports:        0
image FD transfers:     0
game overlay draws:     1 per game present
```

### Mode B — GPU-normalized shared pool

Only after direct import is proven incompatible:

```text
CEF DMA-BUF
→ renderd GPU blit into vosulay-owned external image
→ cached game import
```

Expected:

```text
CPU framebuffer copies: 0
GPU framebuffer copies: 1 per fresh browser frame
```

This mode needs its own pool and explicit completion tracking. It must never run on the game present thread.

### Mode C — persistent SHM

Compatibility only:

```text
CEF BGRA pixels
→ persistent mapped ring
→ game-side texture upload
```

Improve the PR's per-frame memfd design to:

- fixed persistent memfd ring;
- sequence/state atomics;
- bounded damage rectangles;
- no new memfd per frame;
- direct BGRA upload when supported.

### Mode selection

```text
try Mode A
    direct import + visual self-test + sync validation
if incompatible:
    try Mode B
if unavailable:
    Mode C
```

Never fall directly from a successful-but-corrupt import into continued zero-copy operation.

---

## 15. Phased implementation roadmap

## Phase 0 — recover and stabilize the native foundation

### Goal

Bring only the PR #2 native stack onto a new branch from `main`, preserving current product code.

### Tasks

- [ ] Restore `native/` from commit `d1027c9...`.
- [ ] Remove `overlay-node` from default build targets, but keep source temporarily for reference.
- [ ] Verify the nested native workspace independently.
- [ ] Run all existing protocol, host, GL, Vulkan, and attach harnesses.
- [ ] Add a short `native/ORIGIN.md` recording the source commit.
- [ ] Document which PR behaviors are intentionally preserved.
- [ ] Add no CEF code yet.

### Checks

```sh
cargo fmt --manifest-path native/Cargo.toml --all --check
cargo clippy --manifest-path native/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path native/Cargo.toml --workspace
native/test/run-gl-harnesses.sh
```

### Gate

Existing native tests are green before protocol changes.

---

## Phase 1 — CEF fork build and artifact pipeline

### Goal

Produce a reproducible unpatched CEF baseline and then a patched artifact.

### Tasks

- [ ] Fork CEF at the pinned commit.
- [ ] Record Chromium and depot_tools pins.
- [ ] Add patch manifest to vosulay.
- [ ] Add `tools/cef/build-patched-cef.sh`.
- [ ] Add `tools/cef/package-cef.sh`.
- [ ] Produce checksummed release artifact.
- [ ] Verify a stock windowless shared-texture sample on the target NVIDIA host.
- [ ] Capture baseline rAF and `OnAcceleratedPaint` rates.

### Gate

Unpatched pinned CEF builds and its sample runs before modifying ownership.

---

## Phase 2 — accelerated frame lease patch

### Goal

Delay CEF's underlying `Done()` until explicit lease release.

### Tasks

- [ ] Add public lease interface.
- [ ] Add backward-compatible callback.
- [ ] Implement thread-safe idempotent release.
- [ ] Retain callback/data lifetime.
- [ ] Add allocation identity registry.
- [ ] Add pool generation.
- [ ] Regenerate CEF translator output/API hashes.
- [ ] Add unit and teardown tests.
- [ ] Build patched artifact.

### Gate

`Done()` timing is proven by tests and a sample that intentionally holds N leases.

---

## Phase 3 — standalone zero-copy proof

### Goal

Prove the full shared-allocation lifecycle before integrating vosulay UI or host.

### Producer

- [ ] CEF page renders frame sequence and checkerboard.
- [ ] Callback sends `BufferRegister` once.
- [ ] Callback sends `FrameReady` thereafter.
- [ ] Lease table holds buffers until release.
- [ ] Acquire sync export is measured.

### Consumer

Build a standalone GL sample or adapt `glx-state-harness`:

- [ ] import each unique buffer once;
- [ ] draw newest ready buffer;
- [ ] reuse current buffer across repeated swaps;
- [ ] retire with one fence;
- [ ] send release after signal.

### Instrumentation

Prove:

```text
unique allocation count
import count
frame count
release count
copy operations
lease peak
```

Use RenderDoc/Nsight/driver counters where practical to show no blit/copy command in the primary path.

### Gate

A 10-minute stress run produces no corruption, early reuse, deadlock, or unbounded lease growth.

**Hard stop:** if this phase fails, fix CEF ownership/sync. Do not start the host migration.

---

## Phase 4 — protocol v2

### Goal

Add reusable buffer registration and frame-ready/release semantics while preserving v1.

### Tasks

- [ ] Add capabilities and message kinds.
- [ ] Implement bounded structs/codecs.
- [ ] Support 1–4 planes in v2.
- [ ] Add optional sync-file ancillary FD.
- [ ] Add stale-generation checks.
- [ ] Add cross-language fixtures.
- [ ] Add fuzz targets.
- [ ] Update `overlay-host` events and send APIs.
- [ ] Update `overlay-agent-core` control dispatch.

### Gate

Old SHM/v1 harnesses and new leased/v2 fixtures all pass.

---

## Phase 5 — cached GL consumer

### Goal

Integrate the standalone consumer into `overlay-agent-gl`.

### Tasks

- [ ] Split present and Wine resolver libraries.
- [ ] Add direct provider resolution.
- [ ] Add per-context buffer registry.
- [ ] Add pending first-import queue.
- [ ] Add explicit acquire-fence control-thread wait.
- [ ] Add latest-ready mailbox.
- [ ] Convert to fence-on-retirement.
- [ ] Add buffer import success/failure messages.
- [ ] Preserve target selection and state restoration.
- [ ] Add TLS reentrancy guard.
- [ ] Add generation-safe teardown.

### Gate

Native lazer and Wine stable draw the standalone producer with one import per pool allocation.

---

## Phase 6 — CEF renderer daemon

### Goal

Replace the standalone producer with `vosulay-renderd`.

### Tasks

- [ ] Add C++ CEF executable.
- [ ] Add Rust staticlib renderer client.
- [ ] Load synthetic scene.
- [ ] Implement lease/registry/transport.
- [ ] Add popup-disabled diagnostics.
- [ ] Add orderly shutdown.
- [ ] Package CEF subprocess/resources.
- [ ] Add crash/restart harness.

### Gate

Renderer may be killed/restarted while the game continues presenting normally.

---

## Phase 7 — game-driven pacing

### Goal

Synchronize browser work to game presentation without running Chromium at uncapped game rate.

### Tasks

- [ ] Add `PresentDemand`.
- [ ] Add monotonic period/phase estimator.
- [ ] Add one-BeginFrame-in-flight controller.
- [ ] Add lease-budget admission.
- [ ] Add adaptive lead-time measurement.
- [ ] Add visibility/target gating.
- [ ] Measure rAF and paint rates.
- [ ] Decide whether optional BeginFrame-timestamp CEF patch is required.

### Gate

144/240/480/uncapped tests meet pacing acceptance without game-thread waits.

---

## Phase 8 — OBS-private launch mode

### Goal

Make game capture omit the overlay while the user still sees it.

### Tasks

- [ ] Add deterministic launcher.
- [ ] Discover OBS library path.
- [ ] Enforce `obs → vosulay → provider` chain.
- [ ] Add `dladdr` diagnostics.
- [ ] Add reversed-order control test.
- [ ] Document launch-only guarantee.
- [ ] Ensure late attach reports “privacy not guaranteed.”

### Gate

OBS game-capture test excludes the test overlay; monitor capture includes it.

---

## Phase 9 — root Rust supervisor integration

### Goal

Use current `main` as product host and replace Qt only after renderer/agent are proven.

### Tasks

- [ ] Add direct `overlay-host` integration.
- [ ] Add renderer child supervision.
- [ ] Add private internal scene server.
- [ ] Connect config/profile state to scene.
- [ ] Connect edit/input bridge.
- [ ] Preserve remote server behavior.
- [ ] Preserve target/client switch behavior.
- [ ] Preserve restart behavior.
- [ ] Add bounded shutdown.
- [ ] Disable Qt path by default.

### Gate

Normal vosulay workflow runs without Qt and with the same persisted profile semantics.

---

## Phase 10 — popup, input, Vulkan, fallback, packaging

Execute as separate PRs:

- [ ] CEF popup layer.
- [ ] Complete input/IME/cursor forwarding.
- [ ] Persistent SHM ring.
- [ ] GPU-normalized fallback.
- [ ] Vulkan leased-buffer integration.
- [ ] Packaging/signing/update flow.
- [ ] Qt dependency removal.
- [ ] Full performance campaign.
- [ ] Documentation/OpenWiki update.

---

## 16. Test matrix

## 16.1 Unit tests

### CEF

- lease lifetime;
- sequence affinity;
- buffer identity;
- generation reset;
- callback compatibility.

### Protocol

- valid/invalid payloads;
- FD counts;
- overflow;
- multi-plane metadata;
- optional fence FDs;
- stale generation;
- duplicate release.

### Agent state machine

- register → import → ready → current → retire → release;
- ready superseded before draw;
- current reused;
- target reset;
- context destroyed;
- import failure;
- fence failure;
- disconnect.

### Pacing

- constant rates;
- variable rates;
- coalescing;
- hidden state;
- lease budget full;
- render latency adaptation.

## 16.2 Native harnesses

Extend:

```text
native/test/glx-state-harness.c
native/test/egl-state-harness.c
```

Add:

```text
native/test/leased-buffer-harness.c
native/test/acquire-fence-harness.c
native/test/obs-chain-harness.sh
native/test/renderer-restart-harness.py
```

Verify:

- GL state restoration;
- one-time imports;
- no duplicate draw on nested swap;
- original swap always called;
- no release before fence;
- context/generation cleanup;
- correct premultiplied alpha;
- vertical orientation;
- AR24/AB24 channel mapping.

## 16.3 Hardware matrix

Record for every run:

```text
GPU
driver version
kernel
desktop session
X11/XWayland/Wayland
CEF/Chromium revisions
Wine/Winello version
DXVK version where applicable
osu build
OBS/obs-vkcapture version
monitor refresh
game frame limit
browser frame cap
```

Required:

| Target | Backend | Session | Rates |
|---|---|---|---|
| native osu!lazer | GLX | XWayland/X11 | 144, 240, 480, uncapped |
| native osu!lazer | EGL if available | Wayland/XWayland | 144, 240 |
| osu!stable Wine/Winello | GLX | XWayland | 144, 240, uncapped |
| standalone harness | GLX + EGL | X11/XWayland | synthetic stress |

## 16.4 Fault injection

Inject:

- dropped `FrameReady`;
- dropped `FrameRelease`;
- duplicate release;
- stale generation;
- renderer crash;
- game exit during GPU use;
- agent disconnect;
- socket backpressure;
- invalid FD;
- import failure;
- acquire-fence timeout;
- `GL_WAIT_FAILED`;
- target resize/recreation;
- rapid hide/show;
- rapid profile updates.

Expected result is stale/disappearing overlay, never a stalled or corrupted game.

---

## 17. Performance acceptance

Measure overlay OFF, SHM, GPU-normalized fallback, and leased zero-copy separately.

### 17.1 Present-hook CPU

Target after warm-up:

```text
median < 25 µs
p95    < 50 µs
p99    < 75 µs
```

These are initial engineering targets, not release claims. Record hardware and renderer state.

### 17.2 Hot-path audit

After warm-up, prove:

```text
allocations per present:       0
socket syscalls per present:   0
fd imports per fresh frame:    0
blocking waits per present:    0
ordinary glFinish calls:       0
browser synchronization calls: 0
```

### 17.3 Image path

Primary mode:

```text
CPU framebuffer bytes copied: 0
GPU framebuffer copies:       0
DMA-BUF registration:         once per unique pool allocation/generation
```

### 17.4 Long runs

Minimum formal run:

```text
30 s warm-up
120 s measure
3 counterbalanced ON/OFF pairs
```

Repeat at:

```text
144 Hz
240 Hz
480 Hz
uncapped
```

Collect:

- game frame times and low percentiles;
- present-hook CPU duration;
- browser CPU/RSS/private memory;
- GPU utilization/power/VRAM;
- browser frame rate;
- frame age at presentation;
- imports, leases, releases, and drops;
- OBS behavior.

### 17.5 Release blockers

- black or corrupt frame;
- early buffer reuse;
- duplicate/missing release that grows without bound;
- game-thread wait;
- normal-path `glFinish`;
- per-frame import after warm-up;
- incorrect OBS-private claim;
- renderer crash taking down game;
- Qt dependency remaining in final Linux package.

---

## 18. Pull request slicing

| PR | Scope | Must not include |
|---|---|---|
| 1 | Recover `native/` foundation | CEF or host rewrite |
| 2 | CEF build pin/artifact scripts | Lease behavior |
| 3 | CEF lease API and tests | Vosulay integration |
| 4 | Standalone leased DMA-BUF demo | Product UI |
| 5 | Protocol v2 | GL implementation |
| 6 | GL cached import + retirement | CEF renderer |
| 7 | `vosulay-renderd` synthetic scene | Root host migration |
| 8 | Native pacing | OBS and UI rewrite |
| 9 | OBS-private launcher | Late-injection hacks |
| 10 | Rust supervisor + private scene | Vulkan |
| 11 | Popup/input completion | Atlas work |
| 12 | GPU fallback + SHM ring | Claiming both are zero-copy |
| 13 | Vulkan parity | GL refactor |
| 14 | Packaging/performance/docs | New architecture |

Each PR needs:

- design summary;
- ownership diagram;
- test evidence;
- failure behavior;
- performance impact;
- explicit statement of hot-path work.

---

## 19. Definition of done

The Linux migration is complete when all of the following are true:

- [ ] Qt, QtWebEngine, LayerShellQt, CXX Qt bridge, and custom Dawn Qt build are absent from the default Linux build and package.
- [ ] CEF is pinned and reproducibly packaged.
- [ ] Stock CEF's callback lifetime is replaced by a tested refcounted lease.
- [ ] Native lazer and Wine stable directly sample leased Chromium DMA-BUF allocations.
- [ ] Each unique pool allocation is imported at most once per GL context/generation.
- [ ] No CPU framebuffer copy occurs in primary mode.
- [ ] No GPU framebuffer copy occurs in primary mode.
- [ ] Chromium never overwrites an allocation before final game-side use completes.
- [ ] The game presentation path never waits for the browser or host.
- [ ] Fresh browser frames are game-phase-driven and independently capped.
- [ ] `requestAnimationFrame` cadence is measured and acceptable at configured rates.
- [ ] OBS-private launch mode reliably excludes the overlay from obs-vkcapture.
- [ ] Monitor capture limitation is documented.
- [ ] Renderer restart, game restart, resize, target switch, hide/show, and teardown pass stress tests.
- [ ] SHM and GPU-normalized fallback modes are explicit, functional, and honestly reported.
- [ ] Existing profile/config/remote/local-file security behavior is preserved.
- [ ] Input capture always fails open on disconnect.
- [ ] All unit, native, integration, hardware, and packaging gates are green.
- [ ] Formal performance results are attached to the release decision.

---

## 20. First Codex / OMP assignment

Use this exact initial assignment rather than asking an agent to implement the whole plan:

> Work only on Phase 0 of `VOSULAY_CEF_ZERO_COPY_IMPLEMENTATION_PLAN.md`. Start from current `main`. Recover the `native/` directory from PR #2 commit `d1027c9b89a6aca55a155cc0771b2220e380bfb1` without restoring `desktop/` or the Electron host. Read repository guidance first. Run the complete native workspace and GL harness test set, fix only integration issues caused by recovery, document the source commit in `native/ORIGIN.md`, and produce a focused commit. Do not add CEF, change the root host, or alter the protocol in this phase. Report the files changed, commands run, failures encountered, and the Phase 0 gate result.

After Phase 0 is merged, assign Phase 1 independently. Never instruct one autonomous run to execute all phases.
