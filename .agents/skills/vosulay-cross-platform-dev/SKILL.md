---
name: vosulay-cross-platform-dev
description: Build and maintain vosulay's shared Rust/CEF host, native Linux/Windows composition, and manager frontend.
---

# Cross-platform development

Work inside the vosulay repository, not the umbrella or other submodules. Read `CONTRIBUTING.md`, `docs/README.md`, and `docs/source-map.md` first. The contributor verification tiers work on one Linux or Windows machine without private tools. One Rust host and `assets/manager.{html,js,css}` serve both platforms; do not fork the frontend or add a second configuration convention.

## Source contracts

- `src/config.rs` owns serialized defaults/normalization; manager page state mirrors it. Update both and relevant round-trip/migration fixtures when the model changes.
- `src/api.rs` detects backend compatibility from public counter markup, not the product name. Preserve source order, explicit unrecognized-markup errors, and response limits.
- `src/backend.rs` shares telemetry by schema; precise subscriptions and commands are per-client relays, not projected shared snapshots.
- `src/runtime/host.rs` owns CEF/browser lifetime, editing/visibility, frame policy, and input safety. Shared native hotkey parsing is in `crates/frame/src/hotkey.rs`.
- Linux publishes CPU BGRA through a persistent memfd ring; Windows copies borrowed CEF D3D11 textures into application-owned GPU textures before returning. Preserve frame ownership and bounded input-release behavior.

## Build and package

Install rustup and use `rust-toolchain.toml`. Linux prerequisites (including SDL2/SDL3 development packages) are in `CONTRIBUTING.md` and `.github/workflows/linux-ci.yml`; native Windows needs MSVC Build Tools with Desktop development with C++ and a Windows SDK. Cargo acquires verified precompiled CEF when `CEF_PATH` is unset; explicit paths must match `tools/cef/pins.json`. Allow first-download time and several GB of disk space. No runtime startup download is allowed.

```sh
cargo build --locked
cargo install --path . --locked
```

Complete Linux package commands are in `docs/operations.md`; Cargo install depends on retained runtime/overlay caches and is not a portable bundle. Native Windows packaging is:

```powershell
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
```

`Cargo.toml` supplies workspace package versioning. Linux CI runs on PRs and pushes to main; **Packages** (`.github/workflows/release.yml`) is manual-dispatch only and uploads `vosulay-linux-x86_64` and `vosulay-windows-x86_64` artifacts (Linux packages and Windows ZIP/installer). Request a Packages run or use the Actions tab in a fork with `publish=false`; never publish releases as verification. Inno Setup 6 installer commands and the tagged publication process are in `docs/operations.md`.

## Verification and safety

Use `vosulay-local-ci` for Tier 1 workspace formatting/Clippy/tests and optional Tier 2 Windows cross-Clippy (including installation instructions). Tier 3 in `CONTRIBUTING.md` covers Chromium previews assembled like `src/ui.rs::manager_document`, a stubbed host bridge, and private Linux CEF smoke. Native popup surfaces do not render in off-screen CEF; do not rely on browser `confirm`, native select menus, or validation bubbles. Never launch unrequested GUI tests on the user's working desktop.

Runtime claims require actual pixels and game input evidence, not accepted launch/input commands or producer counters. When authorized, Tier 4 can run on the contributor's own machine; no guest tooling is required.

For packaging changes, build in the Ubuntu 22.04 container and install-test DEB/RPM/Arch packages in disposable distro containers as described in `docs/operations.md`. Check version output, libcef dependencies, root-owned `4755` sandbox, sustained startup and no duplicate user launcher. Installer compilation does not replace authorized Windows install/upgrade/uninstall testing.

### Maintainer environment (optional)

The `vosulay-windows-live-testing` skill applies only when the maintainer's private `windows-vm` host command is available. It is not required for contributor development. Never start/stop/switch a VM without explicit user authorization; fall back to cross-Clippy, a manual Packages Actions run with `publish=false`, and native Windows manual testing when unavailable.

Report only exercised checks, exact artifacts, and remaining platform boundaries. Update hand-maintained `docs/` with permanent code changes.
