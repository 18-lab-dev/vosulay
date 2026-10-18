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

### Overlay upload cost (Linux GL, headless)

`python3 tools/overlay-upload-bench.py` measures what overlay updates cost a GL game, on any Linux machine with a GPU and its EGL driver. It builds the compositor library and lookup helper with `--lib` (building only the example does not refresh the preloaded `.so`) and the [`upload_bench`](../crates/overlay-linux/examples/upload_bench.rs) example into `target/overlay-upload-bench/`, prints the libraries' hashes, then runs a synthetic surfaceless-EGL game: a 2560x1440 pbuffer drawn uncapped with two frames in flight, with the compositor preloaded. A fake host in the same process attaches over the real control protocol and publishes into a real frame ring. No window, display connection or input is involved, but the GPU is shared, so keep runs short on a machine someone is using.

- Scenarios: `none` (no compositor), `static` (composite only), `small` (one 600x200 counter changes), `corners` (two counters in opposite corners change), `full` (every pixel changes), at `--rates` overlay updates per second (default 60,120,360).
- `--draws` sets the game load in blended fullscreen layers per frame; the defaults `4,100` are a light game and roughly 1 ms per frame on an RTX 5090.
- Repetitions (`--reps`) are interleaved, so drift from other GPU work spreads over every scenario. The table reports medians: fps, frame time, CPU time of the hooked swap and GPU time of everything the hook submitted (timestamp queries, read without waiting).
- Each run reads back a counter pixel after the last swap and fails unless the overlay was composited (or, for `none`, was not).
- The EGL vendor defaults to NVIDIA when its glvnd file is installed, so Mesa's software renderer is never measured by accident; `--egl-vendor` overrides it. `--api es` uses an ES 3 context instead of desktop GL 3.3 core.
- `--profile` builds the compositor with its `profile` feature (benchmark builds only) and adds a second table: CPU time of each GL call on the update path (state capture, buffer orphan, map, row copy, unmap, `glTexSubImage2D`, draw, restore) and GPU time of the upload and of the composite from timestamp queries that are read without waiting.
- `--json FILE` keeps the raw per-run records.
- GPU upload time is bounded by the GPU's PCIe link: check `/sys/bus/pci/devices/<gpu>/current_link_speed` and `current_link_width` before comparing machines. A Gen4 x4 link (a GPU behind the chipset) moves roughly 7 GB/s, so a full 2560x1440 update costs about 2 ms of GPU time there.

It isolates the game-side cost. Chromium's own rendering and readback, and real swapchain presentation, are outside it.

## Coverage and manual boundaries

- `src/config.rs`, `tests/tosu_migration.rs`: defaults, normalization (including `vosu_profile` pins), persistence, safe/idempotent imports.
- `src/api.rs`, `tests/fixtures/backend/`: supported markup, metadata, deadlines, resolution fallback, the vosu profile registry and write access in the `counters` envelope and its absence on tosu; `src/api/vosu.rs`: the editor's vosu settings requests against a fake vosu (token only on writes, reread after a 401, name validation before any request, button links from the schema only).
- `src/backend/tests.rs`, `src/backend/credential.rs`, `src/remote/tests_broker.rs`, `src/remote/manage.rs`: shared schemas, independent filters/commands, backend key order in filtered projections, precise subscription relays, which sockets carry vosu's broker token and its reread-once/drop handling, token file matching, HTTP snapshot/proxy behavior, manage-host gating.
- `src/runtime/editor.rs`, `src/runtime/control.rs`, `src/runtime/signals.rs`: manage-page message allowlist, local-file grant checks and broadcasts; control protocol bounds and peer credentials; an exit signal after a foreign handler took over requests a graceful exit and a second one ends the process.
- `src/remote.rs`, `src/shell_policy.rs`: peer/authority/origin restrictions, pinned counter URLs on the view-only page, local capability/path confinement.
- `crates/frame-transport/`, `crates/overlay-linux/tests/`: transport ownership and loader/input/late-attach fixtures.
- `crates/platform-windows/src/target_selection.rs`: focus-driven target selection, including tests available on Linux.

Tier 4's checklist covers edit/interaction toggles, drag/resize/scale/crop, field wheel stepping, focus loss, tray actions and the remote `/overlay` page on your own machine. The compact inspector, its Advanced placement editor, the native vosu overlay settings modal (including its read-only state and Retry) and cross-profile paste have no automated JavaScript tests; exercise them in the Tier 3 preview with `--vosu-demo` (a fake vosu in the stub host) or on the manage page with a test vosu, which uses the real host requests. Native GPU composition, real game input release, OBS capture, desktop tray integration, and delivered-pixel performance need explicit platform checks. Never label an unsupported/failed game-surface launch as a compositor pass.

## Maintainer environment (optional)

The following is additional reviewer/release evidence, not a contributor prerequisite. Maintainers may use an already-running GPU guest matrix with private `windows-vm` host tooling and active-guest Cua. Do not install or imitate that tooling merely to contribute. The `vosulay-windows-live-testing` skill applies only to this environment; otherwise use cross-Clippy, hosted Windows packaging and native manual testing.

### Authorized guest safety

When the maintainer's `windows-vm` command is available, verify the selected guest and task-owned workspace before transfer or execution. Never start, stop, switch, restart, or change VM devices without explicit user authorization. GUI launches must use the verified same-account interactive session, not Session 0; observe actual guest screenshots, not a host-desktop input proxy.

Exercise focused-game attachment, counter pixels, drag/save/reopen, profiles, shortcuts, hide/show, tray actions, single-instance rejection, launcher targets, restart and heartbeat recovery. Restore configuration semantics and remove only task-owned artifacts. Stop task-owned games before replacing injected DLLs; resident asdf-overlay code need not unload when vosulay disconnects. Do not stop a guest as cleanup.

### Delivered-pixel benchmarks

`bash autoresearch.sh` is a maintainer-environment harness, not a general local test. It dispatches to the already-running GPU guest selected by `windows-vm status --json`; `--guest linux` or `--guest windows` asserts the intended guest. It fails closed for missing/ambiguous selection and never starts/stops guests. Windows runs read `VOSULAY_WINDOWS_VM` (default `windows-vm` on `PATH`), `VOSULAY_CUA_WINDOWS` (the guest Cua adapter script) and `VOSULAY_WINDOWS_ROOT` (a task-owned guest directory) from the environment. [tools/autoresearch](../tools/autoresearch/) compares the real manager/injected game compositor with a windowed browser from the same pinned CEF and deterministic counter content. Its cached offline SDK/toolchain/inputs must be provisioned in that environment; inspect the harness prerequisites before running it. Linux uses a private Weston/XWayland display; Windows requires an interactive guest session.

The primary metric is CPU microseconds per observed visible update; retain absolute CPU, throughput, game CPU, cadence, skipped IDs, invalid markers and raw pixels. Linux decodes post-present window pixels; Windows Desktop Duplication observes DWM-composed pixels at roughly display cadence. Producer/rAF/transport counts are not delivery or scanout metrics; DWM samples do not prove uncapped renderer parity. Keep platform boundaries and production-capped versus uncapped workloads separate. Do not infer end-to-end latency from callbacks or DXGI Present timings.

Retain disposable reports under `target/autoresearch-vm/`, not permanent user-facing docs. Historical single-run percentages and withdrawn samples are not current release evidence. Performance/release gates remain separate from passing unit tests or smoke checks.
