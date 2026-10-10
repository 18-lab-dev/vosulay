# Source map

The [README](../README.md) is the current product, runtime, and build reference.
Use this page for the first file to open when making a change and the
[testing guidance](testing.md) for how to verify it. The
[architecture overview](architecture.md) explains how the pieces fit
together.

| Area | Start here | Responsibility |
|---|---|---|
| Process entrypoint | [`src/main.rs`](../src/main.rs), [`src/runtime.rs`](../src/runtime.rs) | Linux re-executes from the retained CEF runtime directory, then enters the shared CEF host; Windows enters through CEF's sandbox bootstrap. |
| CEF host loop | [`src/runtime/host.rs`](../src/runtime/host.rs) | Lazy CEF initialization, browser lifetime, attach/detach, input heartbeat, pump scheduling. |
| Editor and settings | [`src/runtime/editor.rs`](../src/runtime/editor.rs), [`src/runtime/clipboard.rs`](../src/runtime/clipboard.rs), [`assets/manager.js`](../assets/manager.js), [`assets/manager.html`](../assets/manager.html), [`assets/manager.css`](../assets/manager.css), [`src/ui.rs`](../src/ui.rs) | Manager page, profile/overlay editing, saves, backend discovery, Linux text-field clipboard; `ui.rs` embeds the assets with safe encoding. |
| Configuration | [`src/config.rs`](../src/config.rs) | Versioned profiles/placements/settings, normalization, config path, atomic persistence. |
| tosu import | [`src/migration.rs`](../src/migration.rs), [`src/runtime/tosu.rs`](../src/runtime/tosu.rs) | Parse tosu 4.26.x layouts; discover and import them without calling tosu's API. |
| Counter catalog | [`src/api.rs`](../src/api.rs) | Fetch and parse the backend's counter-search HTML. |
| Backend telemetry broker | [`src/backend.rs`](../src/backend.rs), [`assets/backend.js`](../assets/backend.js) | Shared upstream WebSocket per schema, client-local filters, HTTP snapshot reuse, per-client precise-subscription relays. |
| Remote profile server | [`src/remote.rs`](../src/remote.rs) | View-only profile pages gated by peer allowlist and Host/Origin checks (no credential), backend proxy, local resource forwarding, loopback browser broker. |
| Local overlay authorization | [`src/shell_policy.rs`](../src/shell_policy.rs), [`src/runtime/resources.rs`](../src/runtime/resources.rs) | Capability-scoped local HTML resolution, traversal/symlink confinement, MIME types. |
| Linux game launchers | [`src/runtime/launchers.rs`](../src/runtime/launchers.rs) | Rewrite osu desktop shortcuts with preload order and Vulkan layer settings. |
| App launchers and single instance | [`src/runtime/desktop_entry.rs`](../src/runtime/desktop_entry.rs), [`src/runtime/instance.rs`](../src/runtime/instance.rs) | XDG desktop/icon and Windows Start Menu registration; per-user instance ownership. |
| Native hotkeys | [`crates/frame/src/hotkey.rs`](../crates/frame/src/hotkey.rs) | Shared parser and matching used by the host and platform input paths. |
| CEF wrapper | [`crates/browser-cef`](../crates/browser-cef) (`src/lib.rs`, `bridge.rs`, `scheme.rs`, `glib.rs`) | OSR browser, native manager bridge, `vosulay://` scheme, permission policy, Linux GLib dispatch beside the external pump; build script links the pinned CEF. |
| CEF smoke | [`crates/cef-smoke`](../crates/cef-smoke), [`tools/cef/private-smoke.py`](../tools/cef/private-smoke.py) | Standalone OSR/WebGL/bridge smoke binary and its private-Xvfb runner. |
| Editor preview | [`tools/manager-preview.py`](../tools/manager-preview.py) | Builds the editor page with a stub host for any Chromium; no CEF or game needed. |
| CEF acquisition and packaging | [`crates/cef-tools`](../crates/cef-tools), [`tools/cef/pins.json`](../tools/cef/pins.json) | Pinned download/verification, runtime retention, Linux/Windows packaging. |
| Frame model | [`crates/frame`](../crates/frame) | Browser-independent frame, damage, and editor input types. |
| Linux frame transport | [`crates/frame-transport`](../crates/frame-transport) (`ring.rs`, `ipc.rs`, `protocol.rs`) | Sealed three-slot memfd ring and authenticated `SOCK_SEQPACKET` control. |
| Linux host platform | [`crates/platform-linux`](../crates/platform-linux) | Focus-driven game selection, discovery, explicit live attachment. |
| Linux in-game compositor | [`crates/overlay-linux`](../crates/overlay-linux) (`hooks.rs`, `gl.rs`, `vulkan/`, `sdl.rs`, `sdl3.rs`, `x11input.rs`, `xfocus.rs`, `late.rs`, `lookup.rs`), [`VkLayer_vosulay_overlay.json`](../crates/overlay-linux/VkLayer_vosulay_overlay.json) | Injected `libvosulay_overlay.so`: Vulkan layer, EGL/GLX/SDL swap hooks, SDL and Xlib input capture, X11 focus observation. |
| Linux lookup helper | [`crates/overlay-lookup`](../crates/overlay-lookup) | Leading `libvosulay_lookup.so` that routes explicit `dlsym` lookups to the compositor. |
| Windows host platform | [`crates/platform-windows`](../crates/platform-windows) | asdf-overlay attachment, D3D11 GPU copies, foreground target selection, tray. |
| Native build | [`build.rs`](../build.rs), [`Cargo.toml`](../Cargo.toml), [`rust-toolchain.toml`](../rust-toolchain.toml) | Root crate links the retained CEF runtime and embeds retained compositor/lookup paths. |
| Linux release packaging | [`packaging/linux/`](../packaging/linux/) | Ubuntu 22.04 container/dependencies, runtime build, nfpm DEB/RPM/Arch metadata and system desktop entry. |
| Arch source packaging | [`packaging/arch/PKGBUILD`](../packaging/arch/PKGBUILD) | AUR source build with pinned CEF and setuid sandbox. |
| Windows installer | [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss) | Per-user Inno Setup 6 installer for the complete Windows runtime. |
| CI and releases | [`linux-ci.yml`](../.github/workflows/linux-ci.yml), [`release.yml`](../.github/workflows/release.yml) | Linux tests/Clippy on PRs and `main` pushes; manual-only **Packages** uploads Linux/Windows artifacts and optionally publishes a version-tagged release. See [testing guidance](testing.md). |
| Tests and fixtures | [`tests/`](../tests), [`crates/overlay-linux/tests`](../crates/overlay-linux/tests) | tosu migration and backend HTML fixtures; C-backed loader and SDL fixtures. |
| Benchmarks | [`autoresearch.sh`](../autoresearch.sh), [`tools/autoresearch`](../tools/autoresearch) | Maintainer environment (optional): guest-only uncapped CEF performance harness; not a contributor prerequisite. |
| Documentation | [docs/README.md](README.md), [AGENTS.md](../AGENTS.md), [CLAUDE.md](../CLAUDE.md) | Hand-maintained references; update alongside code. |

## Practical change paths

- **Add or change an overlay property:** update `src/config.rs` and its tests,
  then the editor in `assets/manager.js`; extend `tests/tosu_migration.rs` if
  imports are affected.
- **Change counter discovery:** update `src/api.rs` and its fixtures under
  `tests/fixtures/backend`; preserve response limits and source order.
- **Change the Linux compositor or loader hooks:** update `crates/overlay-linux`
  or `crates/overlay-lookup` and extend the C fixtures in
  `crates/overlay-linux/tests`; real-driver and in-game checks remain manual.
- **Change the frame transport:** update `crates/frame-transport` and its ring
  tests; ship host and compositor together because the control protocol is
  versioned.
- **Change CEF version or packaging:** update `tools/cef/pins.json` through
  `crates/cef-tools`, then run the CEF smoke and package checks described in the
  [testing guidance](testing.md).
