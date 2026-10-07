<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/vosulay-logo-on-dark.svg">
  <img src="assets/branding/vosulay-logo.svg" alt="vosulay" width="720">
</picture>

**Cross-platform in-game overlay manager for a vosu or tosu-compatible backend — native Rust with one CEF browser runtime.**

</div>

vosulay renders existing tosu-compatible counters inside osu! and preserves the editor and profile model. Use a separately running vosu or tosu-compatible backend at `http://127.0.0.1:24050/` by default. Compatibility is detected from the public counter API, not the server's product name. Vosu remains completely independent from vosulay.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Disable individual overlays without removing their placement; re-enable them in the inspector.
- Maintain multiple overlay profiles.
- Discover installed counters from a local Vosu or Tosu instance.
- Add remote or local custom HTML overlays.
- Use Chromium GPU compositing, WebGL, WebGPU where supported, and overlay audio.
- Render inside Vulkan, EGL/OpenGL, and GLX/OpenGL games on Linux, including native Wayland and X11/XWayland games.
- Render inside osu!stable and osu!lazer on Windows through native asdf-overlay 2.1.0.
- Serve view-only profile pages to browsers and streaming clients on the local network.

## Linux

Extract the complete Linux runtime package and run `./vosulay`. Keep the CEF
libraries, resources, locales, sandbox helper, and `libvosulay_overlay.so`
beside the executable. Nothing is downloaded at application startup.

Developers use the pinned **precompiled** CEF distribution; no Qt, Electron,
Chromium source build, or CEF source build is required:

```sh
export CEF_PATH="$(python3 tools/cef/acquire.py acquire --platform linux-x86_64)"
cargo build --release --locked
cargo build --release --locked -p vosulay-overlay
python3 tools/cef/acquire.py package --platform linux-x86_64 \
  --cef-path "$CEF_PATH" --output dist/linux \
  --exe target/release/vosulay --overlay target/release/libvosulay_overlay.so \
  --overlay crates/overlay-linux/VkLayer_vosulay_overlay.json
./dist/linux/vosulay
```

The acquisition step verifies the exact archive SHA-256 and reuses the dependency
cache. `CEF_PATH` must point to that verified distribution while compiling.
Package the complete directory rather than installing the executable alone.

Vosulay discovers native same-user game processes and attempts live attachment.
If process-injection policy prevents attachment, start the game with the packaged
compositor preloaded:

```sh
LD_PRELOAD=/absolute/path/to/vosulay/libvosulay_overlay.so /path/to/game
```

For Vulkan launch-time attachment, point the Vulkan loader at the packaged layer:

```sh
VK_LAYER_PATH=/absolute/path/to/vosulay \
VK_INSTANCE_LAYERS=VK_LAYER_VOSULAY_overlay /path/to/game
```

CEF renders the complete profile in one transparent OSR browser. Chromium keeps
GPU acceleration enabled; only the final Linux frame transfer uses CPU BGRA
pixels and a persistent three-slot memfd ring. The game acquires the newest
complete frame at present and uploads it to a reusable GPU texture. Linux
DMA-BUF support is not required.

## Windows

Extract the complete x86_64 portable ZIP and run `vosulay.exe` alongside a vosu
or tosu-compatible backend. The executable is CEF's pinned sandbox bootstrap;
keep `vosulay.dll`, CEF runtime files, locales, and both asdf-overlay injection
DLLs beside it. No Node.js or Electron process is used.

The host follows the last focused discovered `osu!.exe` or `osulazer.exe`,
remains attached when focus leaves the game, switches only when another
supported game becomes focused, and recovers after game restart. It copies
CEF's callback-borrowed D3D11 texture into reusable vosulay-owned GPU textures
before returning; the normal path has no CPU readback.

Vosulay does not request elevation. If explicitly launched elevated, its browser
host keeps the selected privileges for native attachment instead of Chromium
relaunching it; CEF renderer and GPU processes remain sandboxed.

`Ctrl+Shift+Space` toggles edit mode on both platforms. While editing, Ctrl+I
toggles counter interaction, Ctrl+R refreshes the selected counter, Delete
removes it, and Escape exits editing. Profile and copy/paste shortcuts remain
configurable. Normal mode leaves game input alone.

Input capture/release and browser-heartbeat failures have a five-second bound.
Failure releases capture or terminates vosulay so the injected connection closes
and game input recovers. Restart vosulay to attach again.

To build and package on Windows with Python 3.12, Rust 1.96.1, and MSVC:

```powershell
python tools/cef/package_windows.py dist/package
```

The helper acquires and verifies cached pinned CEF/asdf assets **before** building
the native client DLL and writes `dist/vosulay-<version>-windows-x86_64.zip`.
Both platform packages use the version in `Cargo.toml`; it remains `0.2.0`
until all 1.0 release gates pass.

Configuration defaults to `%APPDATA%\vosulay\config.json`. To use portable
storage, create a file named `portable` beside `vosulay.exe`; configuration then
lives in `data\config.json`. An existing `data\config.json` also selects that
location.

Shared command-line options are `--vosu-url`/`-t`, `--fps-limit`/`-f`,
repeatable `--pid` and `--process-name`, `--config`, and `--cef-dir`.
Run `vosulay --help` (Windows: `vosulay.exe --help`) for details.

## Frame rate

Both hosts default to the attached game window's display refresh rate.
Automatic mode updates when the target moves between displays or its refresh
rate changes. Linux CPU OSR uses CEF's internal, damage-driven frame scheduler;
the injected compositor consumes the newest complete frame at game present.
Windows requests external begin frames on display/FPS timing because
asdf-overlay 2.1.0 exposes no present-cadence notification.

Settings → General → **FPS limit** persists either **Automatic** (the default,
stored as `settings.fps_limit: 0`) or a **Fixed** integer cap. Saving applies it
to the active browser immediately; no restart is required. Fixed caps stay fixed
when the target moves between displays; Automatic follows the target display.

Use `--fps-limit auto` (or `--fps-limit 0`) to select automatic mode explicitly,
or a positive integer to set a fixed cap:

```sh
vosulay --fps-limit 120
```

The short form is `-f 120`; Windows accepts the same options on `vosulay.exe`.

When supplied, a CLI value overrides the persisted setting for that run;
explicit `auto`/`0` overrides a saved fixed cap. Omitting the option uses Settings.
Fixed values range from `1` to `1,000,000`; higher rates would produce a zero
microsecond CEF interval and are rejected by the CLI or normalized on config load.

These are scheduling limits, not guaranteed rendered FPS: complex overlays
can render more slowly. Idle pages do not continuously publish unchanged frames;
the transport keeps newest complete pixels instead of accumulating a queue.

## Backend connections

vosulay brokers installed counters through the configured vosu or
tosu-compatible backend. Counter pages retain the public API paths and protocols;
their `fetch`, XMLHttpRequest, and WebSocket requests to that backend are routed
through vosulay without changing the counter files. The bootstrap also supplies
`COUNTER_PATH` before counter scripts run, preserving a server-provided value.

There is one upstream telemetry WebSocket per active schema (`/ws`, `/tokens`,
`/websocket/v2`, or `/websocket/v2/precise`). Consumers share its newest snapshot;
filters and selected-subscription state remain client-local. Matching HTTP JSON
requests use that snapshot when available and otherwise retain the backend's
HTTP behavior. Slow consumers skip stale telemetry instead of building a queue.

Settings and other commands retain separate upstream channels with each
counter's original query/identity. They are not shared: upstream authorization
and replies are client-specific. vosulay exposes no private telemetry API, and
vosu remains independent.

The in-game browser uses a separate loopback listener, so an occupied remote
server port does not prevent local counters from loading.

## Remote profile pages

vosulay listens on `0.0.0.0:5050` by default and serves the active profile at
`http://<vosulay-host>:5050/overlay`. Use `?profile=1` to select a profile by
its 1-based position. Clients must use the machine's loopback or LAN address,
not the `0.0.0.0` bind address.

The listener address and port are configurable in **Settings → Remote overlay
server → Listener address** and **Listener port**. Set the address to a
specific interface IP to restrict which network the server binds on, or leave
it at `0.0.0.0` to listen on every interface. Listener changes take effect
after restarting vosulay.

The full remote URL is shown in **Settings → Remote overlay server → Remote
access URL** after setting **Remote host**. There is no access token or credential
rotation. Old credential fields are ignored when loading configuration and are
omitted by subsequent native saves.

Remote access is controlled by **Settings → Remote overlay server → Allowed
client IPs**. Loopback is always allowed; remote entries can be exact IP
addresses or four-part IPv4 patterns such as `192.168.*.*`. There is no
authentication beyond this allowlist: any allowed client can view the profiles
and access the proxied backend. Restrict the listener and allowlist to trusted
networks and devices.

Installed counters load through vosulay, so the backend must allow the
vosulay host rather than every remote viewing device. Custom HTTP(S) overlay
URLs on unrelated origins still load directly. Local HTML resources remain
confined to their authorized roots and are served only to allowed clients;
authority/origin checks and path/symlink confinement remain in place.

## Uncapped Chromium benchmark

Run the canonical autoresearch workload from this repository:

```sh
bash autoresearch.sh
```

The entrypoint stages a digest-keyed source snapshot and offline inputs into the
**single already-running GPU VM**, selected through `windows-vm status --json`.
Builds, application launches, capture, and CPU sampling run inside that guest,
never on the host. Missing, ambiguous, or mismatched guest selection fails closed;
the harness never starts or stops either VM.

```sh
bash autoresearch.sh --guest linux
bash autoresearch.sh --guest windows
```

The Linux workload runs the real CEF manager, shared-memory transport, and
injected GLX compositor against an uncapped SDL game on a private hardware
Weston/XWayland display (1024×768 output, identical 640×480 test windows).
Windows uses the real GPU-only CEF manager and injected
DX11 composition, with a DXGI Desktop Duplication observer. The reference on
each platform is a **windowed Alloy browser linked to the exact same pinned
CEF/Chromium build**, not an unmatched installed Chrome or Helium. Both render
the same deterministic counter HTML, geometry, fonts, and fixed-seed animation.
Only the task-owned loopback fixture backend supplies workload content.

Three paired cases use seed `211`, five-second warmups, twenty-second CPU
intervals, and separate eight-second delivered-pixel intervals. CPU includes
the host/browser descendants; injected game CPU is reported separately.
The Linux MIT-SHM observer decodes changing IDs, magic, CRC8, and duplicate rows
from **post-present window pixels**. Windows decodes the same marker from
**DWM-composed desktop pixels**, copying only its 48×8 region. Producer/rAF/
transport counts are not delivery metrics. Actual observation cadence, skipped
IDs, and invalid samples are retained; observed changes remain a lower bound,
not physical scanout FPS. Linux and Windows are separate performance boundaries.

The primary metric is `cpu_us_per_visible_update` (lower is better). Secondary
metrics include absolute CPU, visible updates/s, matching-browser cost and
throughput ratios, game CPU, and observation cadence. A lower cost per update
does not justify a material absolute CPU or visible-throughput regression.
Reports, raw CPU samples, decoded transition CSVs, logs, and pixel evidence are
retained under `target/autoresearch-vm/runs/`. Windows evidence archives exclude
disposable browser caches. Keep platform baselines separate; do not compare
these uncapped measurements directly with earlier 60-FPS production samples.

`--benchmark-uncapped` is testing-only: it disables Chromium frame/vsync pacing
and overrides the normal limit with CEF's smallest positive OSR interval,
one microsecond (a `1,000,000` Hz API ceiling, not achievable promised FPS).
CEF has no unlimited OSR sentinel; zero selects its default rate. Linux external
begin frames remain disabled. Production uses the persisted Settings FPS policy
unless explicitly overridden by `--fps-limit`; testing mode overrides both and
does not change the saved setting.

Staging requires the verified pinned CEF SDKs and cached locked Cargo
dependencies. Linux also stages cached Python `3.13.13`, Pillow `12.3.0`,
and the private Weston 15 package at `target/linux-native-proof/weston/usr`.
The Linux guest must already provide Rust, a C compiler, SDL2/OpenGL and
X11/MIT-SHM development libraries, bubblewrap, XWayland, and its hardware driver.
Windows uses the existing native Rust/MSVC toolchain and verified CEF/injection
assets, with all task sources, caches, targets, and temporary files on `V:`.
Preparation downloads nothing. Linux measurement has private PID, IPC, network,
and temporary-directory namespaces and never uses the guest's interactive
desktop. Windows measurement requires the verified same-account interactive
guest session; Session-0 GUI launches are refused.

The validated guest baselines use the workloads above; no production performance
optimization is attributed to these measurements:

| Guest / path | Runtime | CPU (% of one core) | Observed changing updates/s | CPU µs / visible update |
| --- | --- | ---: | ---: | ---: |
| Linux GLX/XWayland | vosulay CEF OSR | 309.99 | 918.47 | 3,369 |
| Linux GLX/XWayland | Windowed CEF control | 257.09 | 1,346.21 | 1,918 |
| Windows DX11 / DWM capture | vosulay CEF OSR | 385.93 | 59.94 | 65,694 |
| Windows DX11 / DWM capture | Windowed CEF control | 307.83 | 58.34 | 51,337 |

Linux's cost ratio is **1.76×**, with **0.68×** the control's observed throughput.
Windows's cost ratio is **1.28×**; its roughly 60-Hz DWM observation boundary does
not establish uncapped internal rendering throughput. Neither clears the
matching-browser performance goal or the 1.0 release gate.

Evidence: `target/autoresearch-vm/linux-baseline-final/reports/run-zec72141/report.json`
and `target/autoresearch-vm/windows-baseline-corrected/reports/report.json`.
Settings smoke on Linux applied Fixed 10 and restored Automatic without
restarting host PID 44: **10.00** and **59.00** observed updates/s, respectively,
with zero invalid marker samples. Reopened Settings retained Fixed 10.
Windows's retained Settings samples observed **9.80** and **54.72** updates/s.
Receipts are under `target/autoresearch-vm/linux-fps-settings-proof/` and
`target/autoresearch-vm/windows-settings-*`.

Linux damage-aware publication now accumulates CEF's changed region separately
for each shared-memory slot, including dropped paints. Slots still contain
complete snapshots; read leases and the consumer's full-image upload are unchanged.
The frozen uncapped guest benchmark recorded these historical, pre-fix samples:

| Linux measurement | Guest baseline (run 6) | Damage publication (run 7) |
| --- | ---: | ---: |
| CEF process-tree CPU (% of one core) | 311.09 | 302.83 |
| Observed changing updates/s | 914.84 | 906.86 |
| CPU µs / observed update | 3,386 | 3,338 |
| Windowed-control CPU (% of one core) | 255.79 | 256.94 |
| Windowed-control observed updates/s | 1,377.60 | 1,083.00 |

The historical primary cost fell **1.4%**, but delivered throughput did not improve.
Control throughput varied substantially; the apparent relative-throughput gain
is not evidence of faster CEF delivery. The real-driver state probe below later
found invalid core-profile polygon restoration. Runs 6–10 are excluded from the
accepted baseline/best-cost math; the measurements remain diagnostic evidence.
Separate thread profiles observed native host main-thread CPU fall from
**12.86% to 9.66%**, while whole-tree CPU was effectively unchanged in those
diagnostic runs. Neither establishes browser parity.
Native Linux verification passed **96 tests**, Clippy with `-D warnings`, and
formatting, including partial-update slot reuse and saturated-ring damage recovery.
Run receipts and raw pixels are in `target/autoresearch-vm/runs/reports/run-xfwk9q0v/`
and `target/autoresearch-vm/runs/reports/run-xq52vo7z/`; diagnostic CPU profiles are
`target/autoresearch-vm/profile-before-threads.json` and
`target/autoresearch-vm/profile-damage-threads.json`.

Linux GL state capture now normalizes the core profile's single polygon mode
to both stored face entries. NVIDIA's GL 3.2 core query wrote only one scalar;
the untouched second entry previously caused illegal separate-face restoration.
Compatibility profiles still preserve independent front/back modes.
A private GPU-guest probe reproduced `GL_INVALID_ENUM` before the fix and passed
afterward on GL 3.2 core, GL 4.6 compatibility, and ES 3.2. It checked hostile
capabilities, color masks, wireframe/distinct-face modes, resize restoration,
and premultiplied-alpha pixels `[128, 0, 32, 255]`.
The corrected-state run 11 observed **307.43%** CEF process-tree CPU and
**918.36** changing updates/s (**3,345 µs/update**), versus **256.49%** and
**1,350.49** updates/s for the same-engine windowed control. This is a correctness
fix, not an established CPU/throughput improvement; browser parity remains unmet.
Evidence: `target/autoresearch-vm/gl-state-before/`,
`target/autoresearch-vm/gl-state-after/`, and
`target/autoresearch-vm/runs/reports/run-tytk92a8/`.

Linux no longer sends a control record on every game present for an event the
host ignored. Surface/focus/input notifications, CEF's damage-driven scheduler,
and the five-second input-failure deadline are unchanged.
Run 15 observed **307.23%** CEF process-tree CPU and **922.37** changing updates/s
(**3,331 µs/update**), versus **257.24%** and **1,369.36** updates/s for the
same-engine windowed control. The **0.4%** primary-cost change from corrected
run 11 does not establish a meaningful performance gain or browser parity.
The candidate passed **96 native Linux tests**, Clippy with `-D warnings`,
formatting, composed counter pixels, and private editor/Settings save/exit checks.
Evidence: `target/autoresearch-vm/runs/reports/run-orq8ep_s/`,
`target/autoresearch-vm/no-present-native-verification.log`, and
`target/autoresearch-vm/no-present-settings-proof/`.

Linux's unused ring eventfd is also removed: there was no waiter, and the game
already acquires the newest complete frame at present. The sealed mapping,
acquire/release atomics, complete snapshots, and owned read leases are unchanged.
The Linux control protocol is version 2 and hands off one memfd; package the
host and injected library together rather than mixing the old two-descriptor
transport with the new runtime.

Run 16 observed **302.39%** CEF process-tree CPU and **907.61** changing updates/s
(**3,321 µs/update**), versus **256.04%** and **1,334.46** updates/s for the
same-engine windowed control. Relative to corrected run 11, CPU fell **1.6%**
and observed delivery fell **1.2%**; the **0.7%** primary-cost change does not
establish a meaningful performance gain. Cost remains **1.71×** the control,
with **0.68×** its observed throughput. Keep this as removal of unused transport
work, not a browser-parity claim. All 96 native Linux workspace tests and Clippy
with `-D warnings` passed; a one-line rustfmt correction passed in the guest.
NVIDIA GL 3.2 core, GL 4.6 compatibility, and ES 3.2 probes passed hostile state,
wireframe restoration, resize, alpha, and zero GL errors.
Private editor drag/save, Escape game-input release, and
Settings persistence passed; Fixed 10 and Automatic delivered **10.00** and
**59.00** changing updates/s with zero invalid marker samples.
Suspending the verified task-owned CEF renderers while editing restored game
input in **4.87 seconds**, with subsequent game key events and changed game
background pixels. Linux intentionally releases capture without terminating
the host; the suspended browser can retain frozen editor pixels. This is an
input-release pass, not a Linux host-termination claim.
Evidence: `target/autoresearch-vm/runs/reports/run-0409q737/`,
`target/autoresearch-vm/no-wake-native-verification.log`,
`target/autoresearch-vm/no-wake-gl-state-verification.log`,
`target/autoresearch-vm/no-wake-settings-proof/`,
`target/autoresearch-vm/gl-state-no-wake/`, and
`target/autoresearch-vm/no-wake-input-recovery-result.json`.



## Performance validation

Windows comparisons use the same task-owned osu!lazer DX11 client, menu state,
counter HTML, placement, resolution, and 60 FPS overlay cap with upstream tosu
4.26.2 and native CEF. OBS was closed. Each active/hidden interval lasted 120
seconds; process totals include the same tosu backend and all overlay descendants,
but exclude the game.

| Windows measurement | tosu 4.26.2 | Native CEF |
| --- | ---: | ---: |
| Active CPU, percent of one logical core | 12.62% | 16.23% |
| Active private memory | 352.6 MiB | 382.2 MiB |
| Hidden CPU, percent of one logical core | 1.40% | 2.61% |
| Hidden private memory | 315.6 MiB | 329.4 MiB |
| Active game-present mean / p99 | 16.666 / 19.376 ms | 16.666 / 19.365 ms |

PresentMon captured about 7,200 game presents per interval. These are CPU-side
DXGI present timings, not display latency or overlay delivery. Native CEF used
more CPU/private memory in this sample; equal mean game frame times do not erase
that regression. NVIDIA adapter-wide resident memory averaged 506 MiB for tosu
and 613 MiB for CEF, but includes the game and desktop. Windows' per-process
3D-engine and GPU-memory counters were invalid and are not usable GPU evidence.

A separate native trace measured mean accelerated-callback work of 808 µs,
including opening the borrowed CEF resource (45 µs), GPU copy/conversion
submission (6 µs), and waiting for copy completion (580 µs). The GPU copy itself
averaged 11.5 µs. These are different CPU/wall/GPU boundaries, not additive
end-to-end display latency. asdf-overlay exposes no game-present acknowledgment.
A bounded `D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS` trial
showed no measured CPU gain; the standard D3D11 creation flags remain in use.

Fresh extracted-package active samples measured 13.19% of one logical core and
350.1 MiB private memory for tosu, versus 15.53% and 391.3 MiB for CEF.
A separate bounded internal-clock Windows trial used 16.76% and 374.4 MiB while
visible, and 2.25% and 330.6 MiB while hidden. It did not demonstrate a CPU gain;
Windows retains external begin frames. Neither trial clears the release gate.

Separate paired OBS Game Capture recordings decoded 55.30 changing counter
updates/second for tosu and 53.57 for the external-clock CEF host at a 60 Hz capture
boundary. Each 30-second interval contained 1,800 checksum-valid captured frames.
The p99 interval between changed counter values was 33.33 ms for tosu and 50 ms
for CEF. This measures delivered content in Game Capture, not hardware-display
or input latency; it does not clear the CPU/memory regression or the 1.0
performance gate.

The October 6 before/after rerun used one 60-second active interval per Windows
variant with the same DX11 menu, HTML, placement, resolution, backend, and 60 FPS
cap. OBS was closed during CPU/PresentMon sampling and ran separately for
delivered-content recordings.

| Windows rerun measurement | tosu 4.26.2 | CEF before | CEF after |
| --- | ---: | ---: | ---: |
| Active CPU, percent of one logical core | 12.86% | 17.81% | 14.29% |
| Active private memory | 351.3 MiB | 365.8 MiB | 360.6 MiB |
| Changed counter values in 60 Hz Game Capture | 59.40/s | 46.90/s | 54.33/s |
| Changed-counter interval p99 | 16.67 ms | 50.00 ms | 50.00 ms |

The vosulay host's own CPU fell from 4.59% to 3.32% of one core after removing
idle broker-accept polling and unnecessary host wakeups. Chromium GPU/renderer
costs also differed between these single samples; do not attribute the entire
total change to the host optimization. The final isolated recording contained
1,800 checksum-valid frames. An earlier post-change recording overlapped Linux
functional checks and observed 43.9 changes/s; it is not the isolated comparison.
CEF still used 11.1% more CPU and 2.6% more private memory than tosu in this rerun,
with fewer delivered counter changes. These samples do not clear the release
gate.

Linux comparisons run separately on an RTX 5090, inside an isolated headless
Weston/XWayland display. The October 6 rerun used three randomized 30-second
intervals per runtime, ten-second warmups, identical HTML, 640×480 surfaces, a
capped SDL game, and a 60 FPS overlay limit. CEF uses its stable internal CPU-OSR
clock. The frozen Qt-WebEngine baseline renders a desktop surface; CEF renders
inside the game. Do not combine these results with Windows.

| Linux measurement, percent of one logical core | Qt before | CEF before | Qt rerun | CEF after |
| --- | ---: | ---: | ---: | ---: |
| Browser-tree CPU | 24.70% | 23.34% | 25.17% | 23.32% |
| Separate game CPU | 0.59% | 1.50% | 0.58% | 1.37% |

CEF's browser-tree CPU was effectively unchanged. Its native host process used
6.27% before and 6.04% after; the rest includes Chromium processes. The latest
browser-tree proportional-set memory averaged 787.1 MiB for Qt and 475.3 MiB for
CEF. CEF composition added about 0.79 percentage points of game CPU versus the
desktop Qt path. These different composition boundaries do not establish
equivalent end-to-end cost or input latency.

Five bounded-region screenshots during every measured interval showed changing
composed counter pixels. These observations are not a delivered-FPS measurement.
Each animation frame retains the same local `/tick` HTTP request in both
runtimes. GPU utilization and resident-memory samples are adapter-wide, not
overlay-only.

CEF published approximately 60 complete frames/second. An earlier separate
instrumented run measured mean `OnPaint` callback/publication cost of 55 µs,
injected texture upload of 53 µs CPU / 319 µs GPU, and composition of 5.6 µs CPU / 2.3 µs GPU.
Chromium's asynchronous copy-output request averaged 2.34 ms end-to-end; that
includes scheduling and callback delivery, not isolated GPU readback time.

The pinned CEF software-rendering proof delivered paired identical CPU paint
callbacks. The Linux producer compares against its last successfully published
persistent slot and skips identical pixels without another copy, wake, or upload.
Dropped changed frames are not cached; later callbacks can retry them.

These bounded workloads do not clear the 1.0 performance gate. The optimized
package passed eight actual osu!lazer drag/save/edit-release cycles on both GLX
and Vulkan/XWayland, followed by normal game language selection and Escape.
Display/input latency remains unmeasured. Windows CPU/private-memory and counter
delivery regressions, and the differing Linux composition boundaries, remain
release risks.

## Development

See [Testing guidance](openwiki/testing/guidance.md) for the development and verification workflow.

The `vosulay-cef-smoke` binary verifies transparent OSR, secure JavaScript,
native manager-bridge initialization, iframe pixels, WebGL, resize, and clean
shutdown. It also checks WebGPU compute when Chromium exposes an adapter.
[CEF acquisition and packaging](tools/cef/acquire.py) use the exact precompiled
versions and digests in [the runtime pins](tools/cef/pins.json).
[The private Linux smoke runner](tools/cef/private-smoke.py) uses an isolated
Xvfb display instead of opening test windows on the working desktop.

A private CEF manager run verified two counter API clients using one upstream
V2 telemetry connection, independent filters/settings identities, shared HTTP
snapshots through both `fetch(Request)` and XMLHttpRequest, counter metadata,
and the token-free settings dialog. Real-socket regression tests cover snapshot
recovery, selected-subscription deltas/removals/errors, proxy responses, and
remote/local-resource authorization.

The native Windows CEF host builds with `cargo build -p vosulay --lib`.
Package the resulting `vosulay.dll` with `python3 tools/cef/acquire.py package
--platform windows-x86_64 --output DIR --client PATH --overlay ASDF_X64_DLL
--overlay ASDF_X86_DLL`; the pinned CEF bootstrap supplies `vosulay.exe`.
The [native compositor](crates/platform-windows/src/d3d11.rs) copies CEF's
callback-borrowed D3D11 texture into reusable application-owned GPU textures;
the normal Windows path rejects CPU frames.
The Linux CPU frame backend uses a sealed, persistent three-slot memfd mapping.
Owned read leases keep the mapping and pixels alive during GPU upload,
including consumer replacement or disconnect; the
producer replaces stale complete frames instead of waiting for the game.
Socket messages carry authenticated control and descriptor handoff, not pixels
or per-frame acknowledgements.
The native Linux compositor hooks Vulkan presentation and EGL/GLX/SDL swaps;
the CEF host does not render a desktop overlay window. A Vulkan layer or
`LD_PRELOAD` supports launch-time attachment where ptrace is restricted.
GL and Vulkan control/input use locks independent of graphics state. Registry
lookup and bounded socket servicing never wait for texture upload or composition;
a graphics-driver stall must not drop a browser button/key event or hold the
input-release path. Swapchain retirement disables its input owner before GPU
resource cleanup; mapped-frame leases remain valid until CPU upload finishes.

Fresh packaged RTX 5090 osu!lazer runs saved all eight consecutive drag/edit
cycles on both GLX and Vulkan/XWayland, then returned normal pointer clicks and
Escape to the game. Vulkan/XWayland and native-Wayland OpenGL also passed
windowed/fullscreen transitions, game restart, and editor entry/exit afterward.
The native-Wayland Vulkan trial showed no game surface in the private Weston
environment with or without vosulay; it is not counted as a compositor pass.
The forced X11-EGL trial failed before game-window creation and is likewise not
counted as a pass.

Private software-rendered integration runs cover real CEF counter pixels,
premultiplied alpha, EGL/GLX and native-Wayland SDL editing, resize, and
input release after host failure. These runs do not replace the hardware/game
release matrix or performance comparisons.

Private GLX pacing samples with the internal Linux clock delivered 30, 60, and
120 FPS at the corresponding configured limits, with no new OSR frames during
the idle samples. Chromium 152's external-frame completion path can retain an
earlier acknowledgement while child frame sinks are pending and fail
`CHECK(!pending_ack_)`; Linux CPU OSR therefore does not enable that path.
The internal clock retains Chromium GPU acceleration and the configured cap.

Normal startup disables Chromium's periodic stack profiler and speculative
spare renderer without disabling site isolation, GPU acceleration, or sandboxing.
The shared external message pump retains CEF's reference 33 ms maximum-delay
fallback so native work cannot stall browser startup. Pumping messages does not
request a repaint; browser frame scheduling retains the separate FPS policy.

Native Windows input verification covered `Ctrl+Shift+Space`, selection,
copy/paste, configurable profile shortcuts, and Escape. Keyboard events belong
to the attached game process even when its input HWND differs from its OpenGL
drawable; pointer coordinates remain scoped to the drawable. Normal edit-mode
exit keeps capture until held input is released, within the same five-second
bound; Escape exits the editor without opening osu!'s quit prompt. Failure and
file-dialog paths release immediately.
Suspending the task-owned CEF renderers during editing triggered the heartbeat
safety exit in 4.329 seconds; game input then worked without restarting osu!.
The optimized October 6 DX11 rerun also moved/saved a counter, left edit mode
with Escape, and passed the same renderer-suspension scenario: the host exited
in 4.853 seconds and game keyboard input worked afterward without a restart.

Attachment follows actual foreground game focus, not merely a visible game
window. A hidden launcher can retain focus; focus osu! before checking attachment
or `Ctrl+Shift+Space`. Native Windows checks exercised stable OpenGL and
compatibility mode, lazer OpenGL/DX11, profile/input switching, game restart, and
OBS Game Capture. They do not replace the remaining performance release gates.

The extracted Windows portable package also passed automatic focused-game
attachment without `--pid`, tosu-layout migration, counter loading, editor
move/save, and Escape release. The saved native configuration retained the
counter identity and dimensions; real game pointer input worked after release.

## Engineering documentation

[OpenWiki](openwiki/quickstart.md) contains detailed engineering documentation, but its generated pages await manual regeneration for the CEF cutover and still describe the retired Qt/Electron hosts. Use the current runtime, build, and verification instructions above until it is regenerated.

## Security

`vosulay-overlay` local HTML overlays run in a secure context, but overlay pages are not cross-origin isolated. As a result, `SharedArrayBuffer` is unavailable by default.

The embedded CEF manager is allowed Chromium local/loopback-network access so
it can load configured counters from the backend. This grant is restricted to
`vosulay://manager`; counter origins and unrelated browser permissions retain
Chromium's default policy. It does not disable web security or certificate checks.

The **Allow unrestricted SharedArrayBuffer** setting is a compatibility opt-in. Enabling it requires a restart and affects every remote URL and local HTML overlay. It bypasses a Chromium Spectre mitigation, so use it only when every overlay is fully trusted.

## Configuration

See [Configuration](openwiki/domain/configuration.md) for the configuration model, storage, and persistence behavior.

### Importing tosu layouts

The CEF runtime imports tosu 4.26.x `settings/__ingame__.values.json` without
calling tosu's settings API or modifying the source file. Automatic import
requires an untouched/default vosulay configuration and exactly one detected
layout. Linux checks the tosu XDG configuration directory; Windows checks the
directory of running `tosu.exe` processes. Both also check the working and
executable directories, including the legacy `static/__ingame__/settings.values.json`.

The imported native configuration preserves the selected in-game profile,
profile identities/names, repeated counters, geometry, scale, z-order, safe
custom HTTP(S) URLs, and enabled state. New opacity/rotation values default to
`1.0`/`0.0`. A saved migration marker makes automatic import idempotent.
Existing user-created profiles or settings are never overwritten automatically.

To import intentionally, enter edit mode with `Ctrl+Shift+Space`, open
**Settings → Import tosu layout**, and select the JSON file. This appends profiles
and resolves ID collisions without replacing existing profiles. Pending edits
must save successfully before import proceeds; native file picking releases
game input. Source-derived 4.26.0, 4.26.1, 4.26.2, and legacy fixtures cover
conversion, malformed/unsafe input, persistence, idempotence, and no-overwrite.

## License

[MIT](LICENSE)
