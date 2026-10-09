# Testing and performance evidence

## Linux workspace checks

Use Rust 1.96.1 with Clippy/rustfmt. [Linux CI](../.github/workflows/linux-ci.yml) lists the native packages: compiler/pkg-config, Wayland/EGL and SDL2/SDL3 development files, and CEF's NSS/NSPR, GLib, ATK/AT-SPI, CUPS, D-Bus, X11/XCB, GBM, Pango/Cairo, ALSA, and related runtime libraries. CI requires both `pkg-config --cflags --libs sdl2` and `pkg-config --cflags --libs sdl3`; otherwise the SDL fixture can skip an ABI while passing.

Acquire CEF and expose only `libcef.so` to Cargo test binaries, not the entire CEF Release directory (which would shadow system EGL/GLES/Vulkan libraries):

```sh
cef="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
mkdir -p target/cef-lib
ln -sf "$cef/Release/libcef.so" target/cef-lib/libcef.so
export LD_LIBRARY_PATH="$PWD/target/cef-lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Check `ldd "$cef/Release/libcef.so"` for unresolved dependencies first. Always use `--workspace`: Cargo defaults to the root package, excluding transport/compositor/tooling tests. Formatting can be checked with `cargo fmt --all --check`; Linux CI runs Clippy/tests, not formatting.

For focused native loader/SDL evidence:

```sh
cargo test --locked -p vosulay-overlay --test loader_scope --test sdl_filter -- --nocapture
```

These compile C fixtures and locate the injected libraries beside Cargo's test executables. They need no display/GPU; the SDL fixture uses the dummy driver. Loader tests protect native symbol scope; SDL tests protect event-filter callback/userdata across replacement and clearing.

## Windows checks and packaging

From Linux, with cargo-xwin installed:

```sh
env -u C_INCLUDE_PATH cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
```

This checks Windows source, not Windows runtime behavior. Native Windows can run workspace tests/Clippy with `--target x86_64-pc-windows-msvc`. Packaging uses `cargo run --locked -p vosulay-cef-tools -- package-windows dist/package`; see [operations](operations.md). Windows build/release workflows are manual-dispatch only; release publishing is not part of routine local verification.

## Private CEF smoke

Never open test windows on the user's desktop. Build the maintained smoke binary and run its private-Xvfb wrapper with Python 3, bubblewrap, and an installed Xvfb:

```sh
cargo build --locked -p vosulay-cef-smoke
python3 tools/cef/private-smoke.py \
  --binary target/debug/vosulay-cef-smoke --cef-dir "$cef" \
  --xvfb /usr/bin/Xvfb --output target/private-cef-smoke
```

Use a fresh output directory. The wrapper isolates PID/IPC/network namespaces and temporary directories, creates its own X display, strips interactive desktop variables, and bounds process lifetime. Smoke checks OSR transparency, JavaScript/secure scheme, manager bridge, iframe pixels, WebGL, resize, and clean shutdown; WebGPU compute depends on adapter availability. A software smoke does not prove native hardware/game composition or input behavior.

## Windows VM safety

Use the canonical host command `windows-vm status --json`, `windows-vm exec 'POWERSHELL'`, `windows-vm put LOCAL REMOTE`, and `windows-vm get REMOTE LOCAL`. Verify the already-running guest and task-owned workspace first. Never start, stop, switch, restart, or change VM devices without explicit user authorization. GUI launches must run in the verified same-account interactive session, not Session 0. Use the current Windows VM computer-use skill for active-guest framebuffer input and fresh screenshot evidence; never proxy through the host desktop.

Exercise focused-game attachment, catalog/counter pixels, drag/save/reopen, profiles, configurable shortcuts, hide/show, the three tray actions, single-instance rejection, launcher target, restart, and heartbeat input recovery. Restore configuration semantics and remove only task-owned artifacts. Stop task-owned games before replacing injected DLLs; resident asdf-overlay code need not unload when vosulay disconnects. Do not stop the VM as cleanup.

## Coverage and remaining manual boundaries

- `src/config.rs`, `tests/tosu_migration.rs`: defaults, normalization, persistence, safe/idempotent imports.
- `src/api.rs`, `tests/fixtures/backend/`: supported markup, metadata, deadlines, resolution fallback.
- `src/backend/tests.rs`, `src/remote/tests_broker.rs`: shared schemas, independent filters/commands, precise subscription relays, HTTP snapshot/proxy behavior.
- `src/remote.rs`, `src/shell_policy.rs`: peer/authority/origin restrictions, local capability/path confinement.
- `crates/frame-transport/`, `crates/overlay-linux/tests/`: transport ownership and loader/input fixtures.
- `crates/platform-windows/src/target_selection.rs`: focus-driven target selection, including tests available on Linux.

Native GPU composition, real game input release, OBS capture, desktop tray integration, and delivered-pixel performance still need explicit platform checks. Never label an unsupported/failed game-surface launch as a compositor pass.

## Benchmark methodology

`bash autoresearch.sh` dispatches the current CEF workload into the single already-running GPU VM selected by `windows-vm status --json`; `--guest linux` or `--guest windows` asserts the intended guest. It fails closed for missing/ambiguous selection and never starts/stops VMs. [tools/autoresearch](../tools/autoresearch/) uses the real manager and injected game compositor against a windowed browser built from the same pinned CEF, identical deterministic counter content, and offline inputs. Linux runs on a private Weston/XWayland display; Windows requires an interactive guest session. The harness has workstation-specific cached SDK/toolchain/input paths: inspect prerequisites before invoking it.

The primary metric is CPU microseconds per observed visible update; also retain absolute CPU, throughput, game CPU, cadence, skipped IDs, invalid markers, and raw pixels. Linux decodes post-present window pixels; Windows Desktop Duplication observes DWM-composed pixels at roughly display cadence. Producer/rAF/transport counts are not delivery or scanout metrics, and Windows DWM samples do not prove uncapped renderer parity. Keep platform boundaries and production-capped versus uncapped workloads separate. Do not infer end-to-end latency from callbacks or DXGI Present timings.

Retain new reports under `target/autoresearch-vm/`, but do not turn disposable local receipts into permanent user-facing documentation. Historical single-run optimization percentages and withdrawn capture samples are not current release evidence. The 1.0 performance/release gates remain separate from successful unit tests or smoke checks.
