# Testing and performance evidence

## Contributor verification first

Start with [CONTRIBUTING.md](../CONTRIBUTING.md): it provides prerequisites and a four-tier ladder for one Linux or Windows machine, from workspace checks through browser-only UI previews and optional in-game testing. Neither a second machine nor private tooling is required.

Tier 1 is:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Clippy/tests match [Linux CI](../.github/workflows/linux-ci.yml); formatting is a recommended additional local check. Linux CI runs on PRs, pushes to `main`, and manual dispatch. [Packages](../.github/workflows/release.yml) is manual-dispatch only and uploads `vosulay-linux-x86_64` and `vosulay-windows-x86_64` artifacts (Linux packages; Windows ZIP/installer). Request a maintainer run or use your fork's Actions tab with `publish=false` for packaging evidence; artifact creation is not native runtime verification.

Use the pinned [toolchain](../rust-toolchain.toml). Linux workspace tests need CEF's runtime dependencies and the `libcef.so`-only library-path setup in CONTRIBUTING; both SDL2 and SDL3 development packages are needed for complete SDL fixture coverage. Native Windows needs MSVC and discoverable CEF DLLs. Do not expose the whole Linux CEF Release directory to workspace tests: it can shadow system graphics libraries.

For packaging changes, follow [packaging verification](operations.md#packaging-verification-and-tested-on-list): build in the Ubuntu 22.04 container, install-test in disposable distro containers, and check version output, libcef dependencies, sandbox ownership/mode, sustained startup and no duplicate user launcher. Windows installer changes also need authorized native install/upgrade/uninstall checks; compiler success alone is insufficient.

## Focused native fixtures

```sh
cargo test --locked -p vosulay-overlay --test loader_scope --test sdl_filter --test late_attach -- --nocapture
```

These Linux tests compile C fixtures and locate injected libraries beside Cargo's test executables. They need no display/GPU; SDL uses its dummy driver. Loader tests protect native symbol scope; SDL tests protect event-filter callback/userdata across replacement and clearing; late-attach tests protect the GOT installer against loaded modules with over-long imported names (Mesa's libLLVM) or more than 65536 relocations (NVIDIA's EGL core). Verify `pkg-config --cflags --libs sdl2` and `pkg-config --cflags --libs sdl3`: a missing ABI can be skipped even when tests pass.

## Cross-platform and UI checks

The Tier 2 instructions in CONTRIBUTING install cargo-xwin and the MSVC Rust target on Linux. Run Windows cross-Clippy with `C_INCLUDE_PATH` unset; it checks Windows source, not runtime behavior. Windows contributors can rely on Linux CI; WSL builds/tests may be attempted with Linux prerequisites, but do not establish native Linux game support.

Tier 3 runs the editor in any Chromium with `python3 tools/manager-preview.py --serve 8765`, which assembles `assets/manager.*` as `src/ui.rs::manager_document` does and stubs the host bridge. Use it for editor layout and messages, not native dialog, save persistence, shortcut, or input claims. Native popups (`window.confirm`, native select menus, validation bubbles) do not render in off-screen CEF even if they appear in a browser preview.

For private Linux CEF smoke, install Python 3, bubblewrap and Xvfb; `bwrap` namespace creation must be permitted:

```sh
cargo build --locked -p vosulay-cef-smoke
# The build retains CEF in the executable-adjacent runtime layout the smoke needs;
# the raw SDK splits icudtl.dat from libcef.so and CEF's children then fail.
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
runtime="$(echo "${XDG_CACHE_HOME:-$HOME/.cache}/vosulay/runtime/$version"-*)"
python3 tools/cef/private-smoke.py \
  --binary target/debug/vosulay-cef-smoke --cef-dir "$runtime" \
  --xvfb /usr/bin/Xvfb --output target/private-cef-smoke
```

Use a fresh output directory. The wrapper isolates PID/IPC/network namespaces and temporary directories, creates its own X display, strips interactive desktop variables, and bounds process lifetime. Never substitute a smoke window on the user's working desktop. Smoke checks OSR transparency, JavaScript/secure scheme, manager bridge, iframe pixels, WebGL, resize, and shutdown; WebGPU compute depends on adapter availability. A software smoke does not prove hardware/game composition or input behavior.

The same binary and wrapper also exercise native per-source site bootstrap and
seven simultaneously animating 2D-canvas overlays:

```sh
python3 tools/cef/private-smoke.py \
  --binary target/debug/vosulay-cef-smoke --cef-dir "$runtime" \
  --xvfb /usr/bin/Xvfb --output target/private-site-pacing \
  --site-pacing --fps 60
```

Repeat with `--fps 180` to check a fixed high cap. A `--shared-site` run is the
deliberately broken same-site sibling-OOPIF baseline, not a supported product
setting. `pacing.json` records per-document callback timestamps and native
bootstrap exposure; the normal test rejects repeated timestamps, rates outside
the configured tolerance, and native routing on a never-authorized foreign
loopback origin. `processes.json` records private process-tree CPU, process count
and summed RSS/PSS/private memory; shared mappings are counted in each process's RSS. The wrapper
also gives the fixture fresh HOME and every XDG directory. These software
measurements do not establish native game upload, GPU delivery or scanout timing.

Add `--site-security` (requires Bun) to crash one fixture iframe through its
private loopback DevTools target, reload it, and require a new renderer identity,
no native routing before preparing, and a successful fresh handshake afterward.
It also checks a retired origin's socket cannot be claimed by another process
and a fresh, unauthorized frame there receives no native routing. Debugging is
enabled only for this explicit fixture option, inside the private namespace.



### Overlay upload cost (Linux GL, headless)

`python3 tools/overlay-upload-bench.py` measures what overlay updates cost a GL game, on any Linux machine with a GPU and its EGL driver. It builds the compositor library and lookup helper with `--lib` (building only the example does not refresh the preloaded `.so`) and the [`upload_bench`](../crates/overlay-linux/examples/upload_bench.rs) example into `target/overlay-upload-bench/`, prints the libraries' hashes, then runs a synthetic surfaceless-EGL game: a 2560x1440 pbuffer drawn uncapped with two frames in flight, with the compositor preloaded. A fake host in the same process attaches over the real control protocol and publishes into a real frame ring. No window, display connection or input is involved, but the GPU is shared, so keep runs short on a machine someone is using.

- Scenarios: `none` (no compositor), `static` (composite only), `small` (one 600x200 counter changes), `corners` (two counters in opposite corners change, published as two damage rectangles like CEF reports them), `full` (every pixel changes), at `--rates` overlay updates per second (default 60,120,360).
- `--draws` sets the game load in blended fullscreen layers per frame; the defaults `4,100` are a light game and roughly 1 ms per frame on an RTX 5090.
- Repetitions (`--reps`) are interleaved, so drift from other GPU work spreads over every scenario. The table reports medians: fps, frame time, CPU time of the hooked swap and GPU time of everything the hook submitted (timestamp queries, read without waiting).
- Each run reads back a counter pixel after the last swap and fails unless the overlay was composited (or, for `none`, was not).
- The EGL vendor defaults to NVIDIA when its glvnd file is installed, so Mesa's software renderer is never measured by accident; `--egl-vendor` overrides it. `--api es` uses an ES 3 context instead of desktop GL 3.3 core.
- `--profile` builds the compositor with its `profile` feature (benchmark builds only) and adds a second table: CPU time of each GL call on the update path (state capture, buffer orphan, map, row copy, unmap, `glTexSubImage2D`, draw, restore) and GPU time of the upload and of the composite from timestamp queries that are read without waiting.
- A second table reports frame pacing, which an average rate hides: the distribution of intervals between successive game frames (p50, p99, p99.9 and max, plus frames longer than twice the median or than 16.7 ms), with the tail taken from the worst repetition. Use `--seconds` of 6 or more so p99.9 rests on thousands of frames. `--caps` (default `0`) also runs the game with a frame limiter in ms per frame, like a game's own limit; `--caps 0,2` compares uncapped with a ~500 fps cap.
- `--compositor DIR` preloads `libvosulay_overlay.so` and `libvosulay_lookup.so` from `DIR` instead of this checkout's build, so another commit's compositor (built with `cargo build --release --locked -p vosulay-overlay -p vosulay-lookup --lib` in its own worktree) runs under the same game and host.
- `--json FILE` keeps the raw per-run records.
- After the timed part, updates stop and both counters are read back; the run fails unless they show the last published frame, which catches lost or misplaced damage.
- The environment passes through, so `VOSULAY_OVERLAY_ASYNC_UPLOAD=0` measures the in-stream upload instead of the off-stream one.
- GPU upload time is bounded by the GPU's PCIe link: check `/sys/bus/pci/devices/<gpu>/current_link_speed` and `current_link_width` before comparing machines. A Gen4 x4 link (a GPU behind the chipset) moves roughly 7 GB/s, so a full 2560x1440 update costs about 2 ms of GPU time there.

It isolates the game-side cost. Chromium's own rendering and readback, and real swapchain presentation, are outside it.

### Ruleset and manager cost isolation

Use independent Cargo target directories for every compared revision; never reuse
incremental artifacts across a baseline and candidate. Build release hosts and
`cargo build --release --locked --example manager_bench` in each worktree.

`tools/ruleset-bench.py` measures the actual native host in `--server-only` mode,
with two real broker clients and a stand-in backend publishing selected-field
`/settings/mode`, `/beatmap`, and `/play` traffic at 150 updates/s. Its normal case
sends the ruleset only initially/on change; the adversarial case repeats identical
modes at the traffic rate. It reports host CPU, socket rate, and optional
malloc-family allocation counts. It always creates a private network namespace,
isolates HOME/all XDG roots and the control endpoint, copies the executable, and
does not discover/attach to games, open CEF, or modify an installed runtime.

```sh
python3 tools/ruleset-bench.py \
  --binary current="$PWD/target/release/vosulay" \
  --cef-library "$CEF_DIR/Release/libcef.so" \
  --output "$PWD/target/autoresearch-vm/rulesets/native"
```

Install Python's `aiohttp` into your own virtual environment first. `CEF_DIR` is
the verified pinned minimal SDK directory containing `Release` and `Resources`.
Repeat `--binary LABEL=PATH` to compare builds; add `--legacy LABEL` for a binary
predating tool protocol support. The stand-in does not emulate the backend's
memory-reading/pp cost: check that separately in vosu's state-worker cadence
fixtures. A low-cost selector such as `/settings/mode` must not accelerate game
memory capture merely because it has a change-notification subscriber.

### Opt-in DevTools security smoke

Build `cargo build --locked --example manager_bench`, then run:

```sh
python3 tools/cef/devtools-check.py --binary target/debug/examples/manager_bench \
  --cef-dir /absolute/path/to/retained/runtime --output /new/private/evidence
```

Requires aiohttp, bubblewrap and Xvfb (`--xvfb` accepts a private executable).
The fixture uses the real manager assets and CEF startup options with isolated
HOME/XDG roots, network/PID/IPC namespaces and display. It checks default-off,
IPv4-only loopback listening, target enumeration, foreign Host and Origin
refusal (including an actual page-origin WebSocket), screenshots both themes,
and saving the unchecked setting before restarting with debugging off. It waits
for the manager document/editor to become ready before DOM-based checks;
DevTools target discovery alone does not imply that navigation has completed.
The benchmark fixture persists through the production config serializer, but
does not exercise game attachment, the production host's port-conflict status,
the vosu dashboard, Windows, or GPU performance.

The listener-shutdown regression reserves random available ports in 20000–32000,
outside the default Linux/Windows ephemeral ranges, excluding vosu/vosulay's
service defaults. It holds each successfully bound socket through server setup,
checks exclusive ownership before shutdown, and immediately rebinds after the
bounded shutdown. Linux failures include matching `/proc/net/tcp` rows and any
readable owning PID/process name, distinguishing ownership leaks from foreign
port reuse. No system port-range setting is changed.
Its listeners are created in a single-test subprocess so concurrent tests'
fork/exec children cannot temporarily inherit them before CLOEXEC.

`tools/cef/manager-bench.py` runs the real embedded manager assets through CEF OSR
at 2560×1440: 60 fps normally and the production 30 fps editor maximum when
editing is visible. `--fps 60` reproduces the former editor policy for a
single-variable comparison. Two distant counters consume the same realistic
selected-field traffic. It records actual paint delivery intervals/damage,
main-process and process-tree CPU per paint and per second, pump stalls, manager
long tasks and steady iframe reloads. Intervals longer than 1.5 requested periods
are counted separately; they are delayed **CEF delivery intervals**, not proof
of dropped game/display frames. It fails if counters never render or reload
during steady traffic. `--idle` stops counter traffic after the initial state;
per-paint values become `null` when no paint arrives. `--check` additionally
requires fewer than 5% full-frame paints and zero idle paints after warmup.
Comparative runs retain `partial_paint_guard` so an expensive baseline can still
be measured.

The wrapper creates private network/PID/IPC/tmp namespaces and Xvfb, strips
desktop variables, isolates HOME/all XDG roots and shader caches, and exposes
no GPU devices by default. This default explicitly requests SwiftShader: it is
the **no usable GPU fallback**, not production Linux BGRA rendering.

For the production-shaped Linux GPU-compositor → CPU-BGRA-readback path, pass
`--gpu <PCI-BDF> --vulkan-icd /absolute/path/to/the/hardware_icd.json`.
The wrapper exposes only that device's render node and matching NVIDIA nodes
where applicable. The benchmark explicitly selects ANGLE/Vulkan and refuses a
missing/software WebGL renderer, instead of silently measuring a fallback.
Use an authorized spare GPU, never the user's active desktop GPU. Hold the
shared GPU and constrained-CPU locks used by other benchmarks in the environment;
constrain CPUs/memory on the outer runner when comparing hardware profiles.
This private Xvfb run does not exercise injection, game presentation or Windows
shared-texture OSR. Its CPU numbers are process-time costs, not display latency.

`--trace` saves native Chromium startup/steady traces. `--layers` saves the
root renderer's compositor layer geometry and paint counts through DevTools
inside the private namespace; child-renderer raster work is visible in tracing.
`--css <file>` injects a controlled CSS ablation without changing shipped assets.
`--screenshot` saves the first measured 2560×1440 BGRA paint; `--theme dark|light`
selects the editor theme. Keep diagnostic runs separate from timing runs.

```sh
python3 tools/cef/manager-bench.py \
  --binary current="$PWD/target/release/examples/manager_bench" \
  --cef-dir "$CEF_DIR" --xvfb /usr/bin/Xvfb \
  --output "$PWD/target/autoresearch-vm/rulesets/cef"
```

Example measured limits (CEF 154, 2560×1440, two distant 600×200 counters,
150 updates/s, one six-second active sample per policy, 2026-10-24):

| Renderer / CPU allowance | CPU ms / delivered paint, 60 → 30 fps policy | Delivered paints/s, before → after | Process-tree CPU ms/s, before → after |
| --- | ---: | ---: | ---: |
| Spare RTX 5090 / unrestricted | 4.78 → 6.17 | 59.97 → 29.95 | 287 → 185 |
| Spare RTX 5090 / 2 cores, 4 threads, 8 GiB | 3.89 → 4.94 | 59.97 → 29.96 | 233 → 148 |
| Radeon 610M / unrestricted | 7.73 → 7.89 | 60.13 → 29.99 | 465 → 237 |
| Radeon 610M / 2 cores, 4 threads, 8 GiB | 7.69 → 7.83 | 59.98 → 30.00 | 461 → 235 |
| Forced SwiftShader / unrestricted | 122.96 → 122.22 | 60.15 → 29.98 | 7396 → 3664 |
| Forced SwiftShader / 6 cores, 12 threads, 16 GiB | 89.72 → 89.72 | 59.90 → 29.95 | 5374 → 2687 |
| Forced SwiftShader / 2 cores, 4 threads, 8 GiB | 647.50 → 158.15 | 3.33 → 13.50 | 2155 → 2135 |

All seven idle cases delivered zero paints after warmup, before and after.
The cap reduces work per second, not the cost of composing each paint; the
constrained no-GPU fallback remains CPU-bound and does **not** sustain 30 fps.
Do not extrapolate these fixture costs to game/display latency or another CPU.
The native root-renderer layer dump contained 11 layers: two 2560×1440 content
layers were cached (paint counts 2 and 1), plus the counter/border/toolbar layers;
the capture saw no root-renderer repaint events. Grid/background/chrome ablations
therefore do not justify replacing the editor's visual design.

Linux's accelerated OSR transport remains a known limitation:
the pinned CEF exposes native-pixmap handles, but vosulay implements CPU BGRA
delivery, not owned GPU-image imports/copies and synchronization. The example's
GPU-composed editor cost was roughly 4–8 process-tree CPU ms per paint; a
diagnostic RTX trace recorded 137 ms across 222 thread-side `CopyOutput` spans
(0.62 ms/span). Those elapsed trace scopes can include waiting/preemption:
they are **not** CPU accounting, complete asynchronous readback, or game latency.
Enabling shared textures alone is not a
safe fix; see [frame ownership](architecture.md#linux). Windows already uses
borrowed D3D11 callback textures copied into owned textures and is not measured
by this Linux harness.

For allocation evidence on Linux/glibc, compile the optional probe with
`cc -shared -fPIC -O2 tools/cef/allocation-probe.c -o /absolute/task/path/alloc.so`
and pass `--allocation-probe /absolute/task/path/alloc.so` to either runner.
Use the same probe for every compared build. These are malloc-family
calls/requested bytes, not all Chromium PartitionAlloc, mmap, or GPU allocations.
Keep raw JSONL/logs and describe measurement domains separately: native broker
overhead, software CEF painting, and the hardware upload benchmark are not
interchangeable frame-time or latency measurements.


## Coverage and manual boundaries

- `src/config.rs`, `tests/tosu_migration.rs`: defaults, normalization (including `vosu_profile` pins), persistence, safe/idempotent imports.
- `src/api.rs`, `tests/fixtures/backend/`: supported markup, metadata, deadlines, resolution fallback, the vosu profile registry and write access in the `counters` envelope and its absence on tosu; `src/api/vosu.rs`: the editor's vosu settings requests against a fake vosu (token only on writes, reread after a 401, name validation before any request, button links from the schema only).
- `src/backend/tests.rs`, `src/backend/credential.rs`, `src/remote/tests_broker.rs`, `src/remote/manage.rs`: shared schemas, independent filters/commands, backend key order in filtered projections, precise subscription relays, which sockets carry vosu's broker token and its reread-once/drop handling, token file matching, HTTP snapshot/proxy behavior, manage-host gating.
- `tests/manager_restart.mjs`, `tests/tool_runtime.rs`: tool/standalone routing generations, repeated revisions, local-file frame rebinding, outage recovery, expired-session read-only behavior/dashboard reopening, and fresh native process identities after isolated server-only restart.
- `tools/preview-security-smoke.mjs`: private-network real-service/browser proof using a supplied counter directory (not bundled). It verifies hostile plain/guessed/known-capability authorities, scripts/modules/CSS/fonts and sockets, reserved-route denial, local-root confinement, parent/bridge isolation, live synthetic data, capability rotation and standalone styled/live parity. `--cef-smoke` additionally checks the real CEF resolver, fonts, data and pixels on a private Xvfb. No game or desktop windows are used; fixture broker tokens/capabilities are not retained in evidence.
- `src/runtime/editor.rs`, `src/runtime/control.rs`, `src/runtime/signals.rs`: manage-page message allowlist, local-file grant checks and broadcasts; control protocol bounds and peer credentials; an exit signal after a foreign handler took over requests a graceful exit and a second one ends the process.
- `src/ruleset.rs`, `src/runtime/editor/rulesets.rs`, `src/remote.rs::profile_follow_tests`: selected-field subscription, valid/removed/unsupported rulesets, automatic profile persistence without overriding manual selection on repeated or reconnected samples, and URL-selected profile pinning. Browser smoke should also cover binding controls disappearing when the field is absent, both editor themes, embedded fonts, and an open profile picker surviving unchanged state polls.
  The worker queue-barrier regression also feeds 10,000 identical modes and asserts
  no UI replies, config writes/revision changes, or loss of a manual selection.
- `src/remote.rs`, `src/shell_policy.rs`: peer/authority/origin restrictions, pinned counter URLs on the view-only page, local capability/path confinement.
- `crates/platform-windows/src/worker_lifecycle.rs`: command-receiver ownership across an error/panic and normal shutdown without restart. These portable tests do not establish native IPC/game recovery; exercise that on authorized Windows.
- `crates/frame-transport/`, `crates/overlay-linux/tests/`: transport ownership and loader/input/late-attach fixtures.
- `crates/platform-windows/src/target_selection.rs`: focus-driven target selection, including tests available on Linux.

For that optional Linux preview proof, build `cargo build --locked -p vosulay-cef-smoke`
and the host first. Supply an installed Chromium executable and a counter directory
containing the unchanged InGame2 assets (`index.html`, `index.js`, CSS and font/deps);
the script adds only synthetic-data/isolation instrumentation while serving them.
It requires Node, Python, util-linux (`unshare`, `setpriv`), iproute2, bubblewrap and
Xvfb. The wrapper creates a private network namespace, isolates HOME/all XDG roots,
uses private loopback ports/control endpoint, strips desktop variables and gives
Chromium/CEF private `/tmp` and no GPU devices. CEF's `--no-sandbox` is confined to
this isolated test fixture, not production behavior.

```sh
node tools/preview-security-smoke.mjs \
  --binary "$PWD/target/debug/vosulay" \
  --cef-dir "$CEF_DIR" --chromium /absolute/path/to/chromium \
  --counter-dir /absolute/path/to/InGame2 --output /new/private/evidence \
  --cef-smoke "$PWD/target/debug/vosulay-cef-smoke" --xvfb /usr/bin/Xvfb
```

`results.json` records browser assertions (including standalone parity), while
`preview.png`, `cef-preview-result.json` and `cef-preview.ppm` retain the styled
counter/pixel evidence. This is not game attachment, GPU composition or Windows
runtime evidence. On authorized Windows, verify Chrome/CEF resolves the capability
hostname without hosts-file changes and preserves styled/live counters and the
standalone editor before claiming native platform compatibility.

Tier 4's checklist covers edit/interaction toggles, drag/resize/scale/crop, field wheel stepping, focus loss, tray actions and the remote `/overlay` page on your own machine. The compact inspector, its Advanced placement editor, the native vosu overlay settings modal (including its read-only state and Retry) and cross-profile paste have no automated JavaScript tests; exercise them in the Tier 3 preview with `--vosu-demo` (a fake vosu in the stub host) or on the manage page with a test vosu, which uses the real host requests. Native GPU composition, real game input release, OBS capture, desktop tray integration, and delivered-pixel performance need explicit platform checks. Never label an unsupported/failed game-surface launch as a compositor pass.

### Native lifecycle smoke with vosu

With a real vosu test-harness executable and a verified Linux staged runtime
(from `vosulay-cef package`), run:

```sh
python3 tools/test-lifecycle.py \
  --vosu /absolute/path/to/vosu --runtime /absolute/path/to/runtime \
  --evidence /absolute/path/to/evidence
```

The launcher enters `unshare -rn`, brings loopback up, and verifies that its
network namespace changed before starting applications. It isolates HOME and all XDG roots,
approves an actual user-root tool installation through vosu's dashboard API, and
uses the real service with `--server-only` (no tray, CEF view, or game attachment).
It checks standalone-first takeover, enabled-tool hand-off retaining one host,
no-vosu and disabled-tool standalone operation, vosu-exit shutdown, and clean
refusal of an unrelated lock holder without restart. It never changes the user's
installed app or app data. Tray menu appearance and native CEF shutdown remain
separate platform checks.

## Maintainer environment (optional)

The following is additional reviewer/release evidence, not a contributor prerequisite. Maintainers may use an already-running GPU guest matrix with private `windows-vm` host tooling and active-guest Cua. Do not install or imitate that tooling merely to contribute. The `vosulay-windows-live-testing` skill applies only to this environment; otherwise use cross-Clippy, hosted Windows packaging and native manual testing.

### Authorized guest safety

When the maintainer's `windows-vm` command is available, verify the selected guest and task-owned workspace before transfer or execution. Never start, stop, switch, restart, or change VM devices without explicit user authorization. GUI launches must use the verified same-account interactive session, not Session 0; observe actual guest screenshots, not a host-desktop input proxy.

Exercise focused-game attachment, counter pixels, drag/save/reopen, profiles, shortcuts, hide/show, tray actions, single-instance rejection, launcher targets, restart and heartbeat recovery. Restore configuration semantics and remove only task-owned artifacts. Stop task-owned games before replacing injected DLLs; resident asdf-overlay code need not unload when vosulay disconnects. Do not stop a guest as cleanup.

### Delivered-pixel benchmarks

`bash autoresearch.sh` is a maintainer-environment harness, not a general local test. It dispatches to the already-running GPU guest selected by `windows-vm status --json`; `--guest linux` or `--guest windows` asserts the intended guest. It fails closed for missing/ambiguous selection and never starts/stops guests. Windows runs read `VOSULAY_WINDOWS_VM` (default `windows-vm` on `PATH`), `VOSULAY_CUA_WINDOWS` (the guest Cua adapter script) and `VOSULAY_WINDOWS_ROOT` (a task-owned guest directory) from the environment. [tools/autoresearch](../tools/autoresearch/) compares the real manager/injected game compositor with a windowed browser from the same pinned CEF and deterministic counter content. Its cached offline SDK/toolchain/inputs must be provisioned in that environment; inspect the harness prerequisites before running it. Linux uses a private Weston/XWayland display; Windows requires an interactive guest session.

The primary metric is CPU microseconds per observed visible update; retain absolute CPU, throughput, game CPU, cadence, skipped IDs, invalid markers and raw pixels. Linux decodes post-present window pixels; Windows Desktop Duplication observes DWM-composed pixels at roughly display cadence. Producer/rAF/transport counts are not delivery or scanout metrics; DWM samples do not prove uncapped renderer parity. Keep platform boundaries and production-capped versus uncapped workloads separate. Do not infer end-to-end latency from callbacks or DXGI Present timings.

Retain disposable reports under `target/autoresearch-vm/`, not permanent user-facing docs. Historical single-run percentages and withdrawn samples are not current release evidence. Performance/release gates remain separate from passing unit tests or smoke checks.
