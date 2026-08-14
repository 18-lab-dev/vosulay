---
type: Runbook
title: Build, launch, operate, and package vosulay
description: Practical runbook for the shared Electron host and Linux Rust in-game backend, including native staging, attach and Vulkan launch modes, diagnostics, shutdown, and packaging.
resource: desktop/package.json
tags: [operations, build, electron, rust, linux, windows, appimage]
---

# Build, launch, operate, and package vosulay

## Prerequisites

All hosts require Node.js 22 or newer and npm. The Linux native build additionally requires:

- Linux x86-64;
- Rust `1.96.1` with `rustfmt` and `clippy` (pinned by [`rust-toolchain.toml`](../../rust-toolchain.toml));
- Cargo and a C toolchain/linker;
- development/runtime libraries needed by EGL, GLX, X11, SDL3, Vulkan, and Linux `AF_UNIX`/DMA-BUF integration;
- .NET 8 SDK for publishing the self-contained CLR profiler attach tool;
- AppImage/electron-builder system prerequisites when packaging.

Qt, CMake, CXX, LayerShellQt, a custom Dawn/WebEngine build, and Qt runtime path variables are not required.

## Install and verify the Electron host

```sh
cd desktop
npm ci
npm run typecheck
npm test
npm run build
```

`npm test` runs the TypeScript service, configuration, provider, Linux OSR/input, remote-server, and reconciliation tests. The normal source build emits `desktop/out/` but does not stage Linux native binaries.

The application entrypoint accepts:

- `--vosu-url <URL>` / `-t<URL>`: absolute HTTP(S) base, default `http://127.0.0.1:24050/`;
- `--fps-limit <1..240>` / `-f<n>`: Chromium offscreen frame limit, default `60`;
- repeatable `--pid <PID>`: attach explicit targets;
- repeatable `--process-name <filename>`: replace default discovery names `osu!.exe` and `osulazer.exe`;
- `--launch-vulkan <executable> -- <arguments...>`: create a secure session and launch a Vulkan/DXVK target;
- `--ipc-toggle-overlay` / `-i`: tell an existing instance to toggle visibility.

`--launch-vulkan` and `--pid` are mutually exclusive. Unsupported options fail before Electron readiness.

## Build the Linux native workspace

```sh
cd native
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
npm ci
npm run build
```

The npm build invokes napi-rs and writes `native/crates/overlay-node/overlay-node.linux-x64-gnu.node`. Native libraries are written below `native/target/{debug,release}`.

Run the Node-API fake-agent lifecycle after building the addon and fake agent:

```sh
cd native
cargo build -p overlay-agent-core --bin overlay-fake-agent
VOSULAY_NATIVE_ADDON="$PWD/crates/overlay-node/overlay-node.linux-x64-gnu.node" \
VOSULAY_FAKE_AGENT="$PWD/target/debug/overlay-fake-agent" \
npm test
```

This integration path proves the JavaScript/addon/host/seqpacket/agent lifecycle without injecting a graphics application.

## Stage source-tree native resources

The Electron Linux provider expects one resource root containing the addon, agents, Vulkan manifest/layer, profiler library, and attach executable. Build exactly that layout with:

```sh
cd desktop
npm run stage:native
```

`desktop/scripts/stage-native.mjs` performs a release napi build, release GL/profiler/Vulkan builds, and self-contained `dotnet publish`; then replaces `desktop/build/native/`. Every required file must be non-empty. Set this variable for an unpackaged source run:

```sh
export VOSULAY_NATIVE_ROOT="$PWD/build/native"
```

The provider otherwise loads `@vosu/overlay-node` in development. `VOSULAY_NATIVE_ADDON` may point directly to a built `.node` file for a focused addon test. `VOSULAY_NATIVE_DIAGNOSTICS=1` enables first-target/first-frame diagnostics; do not enable verbose hot-path logging for normal use.

## Run against native osu!lazer

Start vosu/Tosu first if counter discovery is needed. Then start native lazer and pass its actual CLR process PID:

```sh
cd desktop
VOSULAY_NATIVE_ROOT="$PWD/build/native" npm exec electron-vite dev -- --pid <lazer-pid>
```

A packaged equivalent is:

```sh
vosulay --pid <lazer-pid>
```

The addon creates a random authenticated endpoint, invokes the packaged DiagnosticsClient attach executable, loads `liboverlay_dotnet_bootstrap.so`, and starts `liboverlay_agent_gl.so` in the target. With `VOSULAY_NATIVE_DIAGNOSTICS=1`, the host prints attach/target/forward progress while the agent prints hook and compositor lines into the *game's* stderr:

```text
Native overlay target for PID ...
Attached vosulay overlay to PID ...
Native overlay paint: transport=bitmap|shared-texture size=...
Forwarded native overlay bitmap|DMA-BUF frame ...
vosulay target transport: dmabuf=<bool> bgra_shm=<bool>
vosulay SDL_GL_SwapWindow hook active: ...
vosulay shared-memory|DMA-BUF compositor first frame: ...
```

`dmabuf=false` is expected for GLX targets (Wine, and SDL games on X11): those contexts cannot sample an imported DMA-BUF, so frames arrive as shared memory. A `DMA-BUF import rejected: ...` line records why zero-copy was refused; the game must keep rendering either way.

To inspect what a target actually presents, set `VOSULAY_FRAME_DUMP_DELAY=<seconds>` alongside diagnostics. The agent reads back the framebuffer before and after compositing once the target has settled and writes `/tmp/vosulay-frame-<pid>-pre.ppm` and `-post.ppm`. This proves composition inside the target without relying on any compositor screenshot path, which is the only reliable evidence when the game runs fullscreen or headless.

Display mode passes game input through. `Ctrl+Shift+Space` enables edit mode; the GL agent captures SDL input only while authenticated editing is active and forwards it to Electron. Disconnecting the host must immediately end capture.

## Run osu! stable through Wine

Stable is a Windows build, so it runs under Wine and is launched by the host, which preloads the agent into the child:

```sh
cd "$HOME/.local/share/osu-wine/osu!"
WINEPREFIX="$HOME/.local/share/wineprefixes/osu-wineprefix" \
  vosulay --launch-vulkan "$HOME/.local/share/osuconfig/wine-osu/bin/wine" -- 'osu!.exe'
```

An osu!winello installation supplies a working prefix directly: its `wine-osu` build plus a prefix with real .NET 4.x installed and Wine Mono removed. A prefix that still loads Wine Mono crashes stable before the agent can connect, which surfaces as `overlay agent did not connect before attach deadline`.

## Run a Vulkan or DXVK target

A Vulkan layer must be present before instance creation. Launch the target through vosulay:

```sh
vosulay --launch-vulkan /path/to/target -- <target arguments>
```

For source development:

```sh
cd desktop
VOSULAY_NATIVE_ROOT="$PWD/build/native" \
npm exec electron-vite dev -- --launch-vulkan /path/to/target -- <target arguments>
```

The launcher sets the session endpoint/cookie, `VK_LAYER_PATH`, and `VOSULAY_VULKAN_LAYER=1` only in the child environment. [`native/VkLayer_VOSULAY_overlay.json`](../../native/VkLayer_VOSULAY_overlay.json) resolves `liboverlay_vulkan_layer.so` relative to the staged manifest. Without a valid session the implicit layer remains dormant. `DISABLE_VOSULAY_VULKAN_LAYER=1` explicitly disables it.

For Wine/Proton, use the Wine executable as the launched executable and the Windows game executable as a target argument. Use one prefix with a known-good game runtime; do not infer native overlay failure from Wine-Mono/.NET startup failure. The Vulkan/Wine path currently composites display frames but does not implement the GL agent's SDL edit-input redirection.

## Runtime controls

- `Ctrl+Shift+Space`: enter/leave edit mode.
- `Ctrl+Alt+Shift+H`: show/hide overlays.
- While editing: `Ctrl+I` toggles selected-overlay interaction, `Ctrl+R` reloads it, Delete removes it, and Escape exits editing.
- Tray: edit, visibility, reload, and exit actions.

The manager is in-game only; there is no transparent desktop overlay window. If no overlay appears, check target PID/process discovery, attach/profiler logs, target dimensions, Vulkan launch mode, DMA-BUF compatibility, and whether visibility is disabled before changing frontend CSS.

## Remote profile server

The host starts a view-only HTTP server on `0.0.0.0:5050`; a bind failure is logged but does not abort native overlay startup. Copy the complete credential-bearing **Remote access URL** from Settings. Configure both **Remote access host** and **Allowed client IPs** for LAN use; allowed peers do not bypass exact authority, credential, origin, or Fetch Metadata checks. Rotating the credential immediately revokes old links. See [integrations](../integrations/vosu-and-overlays.md).

## Shutdown and recovery

SIGTERM, SIGINT, the Electron session-end event, tray Exit, and app quit use one shutdown path. It flushes pending manager state, stops the remote server, destroys the provider/native session, clears input capture, destroys the tray, and exits. If a target survives a host crash, the native agent's socket EOF/watchdog must release capture and leave the original game present path active.

## Package Linux

```sh
cd desktop
npm ci
npm run dist:linux
```

This runs native staging, Electron build, electron-builder AppImage packaging, and package verification. Outputs:

- `desktop/dist/vosulay-<version>-x64.AppImage`
- `desktop/dist/linux-unpacked/`

The package verifier requires manager assets, preload, addon, GL agent, profiler bootstrap/tool, Vulkan layer/manifest, and canonical package identity. It rejects opposite-platform asdf/paymo binaries. Native resources remain outside ASAR.

## Package Windows

On Windows x64:

```powershell
cd desktop
npm ci
npm run dist:win
```

Outputs are `vosulay-<version>-setup.exe` and `vosulay-<version>-portable.exe`. Verification requires the asdf/paymo native resources and rejects Linux addon/agent artifacts. Portable configuration remains next to the executable under `data/`.

The Windows build workflow typechecks, tests, and bundles on pushes/pull requests. Tag `v*` runs the Windows release workflow and publishes the setup and portable artifacts. OpenWiki updates remain manual/local; never add a scheduled or manually dispatched OpenWiki workflow.
