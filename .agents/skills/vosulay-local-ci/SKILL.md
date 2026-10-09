---
name: vosulay-local-ci
description: Run requested contributor checks on one Linux or native Windows machine, with optional Windows cross-Clippy and browser/headless UI smoke.
---

# Local CI

Run only verification requested by the task. Do not push, publish releases, launch the manager on the user's desktop, or alter user launcher files/configuration during tests. Use task-owned temporary directories and injectable paths for launch-registration tests.

Read `CONTRIBUTING.md` for the single-machine verification ladder. Private guest tools, game installs and offline caches are not prerequisites for Tiers 1–3. Use rustup with `rust-toolchain.toml`; first CEF acquisition needs network access and several GB of disk space for caches/builds.

## Linux preparation

Use Rust 1.96.1 and the prerequisite packages in `.github/workflows/linux-ci.yml`: compiler/pkg-config, Wayland/EGL development files, SDL2 and SDL3 development files, and CEF's runtime shared libraries. Confirm both SDL ABIs are reachable:

```sh
pkg-config --cflags --libs sdl2
pkg-config --cflags --libs sdl3
cef="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
ldd "$cef/Release/libcef.so"
mkdir -p target/cef-lib
ln -sf "$cef/Release/libcef.so" target/cef-lib/libcef.so
export LD_LIBRARY_PATH="$PWD/target/cef-lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

Resolve any `not found` dependencies before tests. Expose only `libcef.so`, not the whole CEF Release directory, to avoid shadowing the system graphics libraries used by native fixtures. CEF acquisition verifies pinned archive/runtime digests and caches the SDK; `CEF_PATH` can select an already verified pinned SDK.

## Native Windows preparation

Use rustup's MSVC toolchain and MSVC Build Tools with Desktop development with C++ and a Windows SDK. In developer PowerShell, make the automatically acquired CEF DLLs discoverable for tests:

```powershell
$cef = (cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform windows-x86_64).Trim()
$env:PATH = "$cef\Release;$env:PATH"
```

An empty cache is supported. Native packaging uses `cargo run --locked -p vosulay-cef-tools -- package-windows dist/package`; keep the resulting portable runtime together.

## Workspace checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Always use `--workspace`; root-only defaults miss compositor, transport, browser, and tooling coverage. Formatting is a recommended local check, not currently a Linux CI step; Clippy/tests match CI exactly. Loader/SDL fixture details and skip semantics are in `docs/testing.md`.

## Windows source check from Linux

This is optional Tier 2, not a requirement to own another machine. Install Clang/LLVM (Ubuntu: `clang`, `llvm`, `lld`), cargo-xwin and the target first; follow [cargo-xwin's prerequisites and SDK/CRT license notice](https://github.com/rust-cross/cargo-xwin). Unset any Linux-only `CEF_PATH` override so the Windows SDK can be acquired:

```sh
cargo install cargo-xwin --locked
rustup target add x86_64-pc-windows-msvc
rustup component add llvm-tools
env -u C_INCLUDE_PATH cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
```

`C_INCLUDE_PATH` must be unset to avoid host-header contamination. Cross-Clippy is not native Windows execution. Native Windows workspace checks use the commands above (the native target is selected by the toolchain). Windows contributors can rely on Linux CI; WSL builds/tests may be attempted with Linux prerequisites but do not establish native game behavior. Linux CI runs on PRs and main pushes; **Packages** (`.github/workflows/release.yml`) is manual dispatch from the Actions tab (maintainer run or a fork, `publish=false`) and uploads `vosulay-linux-x86_64` and `vosulay-windows-x86_64` artifacts containing Linux packages and the Windows ZIP/installer. Do not claim that pushing any branch runs both workflows.

For packaging changes, follow `docs/operations.md`: build in the Ubuntu 22.04 container and install-test in disposable distro containers. Verify version output, libcef dependencies, root-owned mode `4755` sandbox, sustained startup and no duplicate user launcher. Windows installer changes need authorized native install/upgrade/uninstall checks, not only compilation.

## Chromium editor preview

Run `python3 tools/manager-preview.py --serve 8765` and open `http://127.0.0.1:8765/` in any Chromium (headless automation is fine). It assembles `assets/manager.*` like `src/ui.rs::manager_document`, loads demo profiles/counters (or `--config PATH`, read-only), enters edit mode, logs outbound messages to `window.__VOSULAY_SENT__`, and acknowledges saves without persisting. Inject more host messages with `window.__VOSULAY_RECEIVE__`. Preview evidence is DOM/layout/message evidence, not persisted saves, native shortcuts or game input. Native popups do not render in off-screen CEF; avoid `window.confirm`, native select popups and validation bubbles even if the browser preview displays them.

## Private headless smoke

Linux only. Requires Python 3, bubblewrap and Xvfb (`bubblewrap`/`xvfb` on Ubuntu), with namespace creation permitted. Use a new output directory:

```sh
cef="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
cargo build --locked -p vosulay-cef-smoke
python3 tools/cef/private-smoke.py \
  --binary target/debug/vosulay-cef-smoke --cef-dir "$cef" \
  --xvfb /usr/bin/Xvfb --output target/private-cef-smoke
```

The wrapper isolates the X display and namespaces; never replace it with a launch on the working desktop. Hardware/game behavior needs separate authorized native manual testing (Tier 4); a guest environment is optional, not required.

## Report

Report exact commands and observed results, skipped fixture ABIs, smoke output, native versus cross-check boundaries, package artifact paths/checksums when generated, and blockers. Do not claim hosted CI, Windows runtime behavior, or cleared performance gates from local source checks. Workflow dispatch/push/publication requires authorization; release publication is never routine verification.
