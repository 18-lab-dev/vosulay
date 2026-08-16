<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/vosulay-logo-on-dark.svg">
  <img src="assets/branding/vosulay-logo.svg" alt="vosulay" width="720">
</picture>

**Cross-platform in-game Electron overlay manager for vosu.**

</div>

vosulay displays Vosu/Tosu-compatible counters and provides an edit mode for arranging them. Layouts are stored as profiles, and overlays can come from a Vosu or Tosu-compatible counter catalog, HTTP(S) URLs, or local HTML files. Vosu remains a separate process.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Maintain multiple overlay profiles.
- Discover installed counters from a local Vosu or Tosu instance.
- Add remote or local custom HTML overlays.
- Render overlays inside the game presentation rather than through a desktop overlay window.
- Use one Electron manager, configuration store, local-overlay boundary, remote server, and tray on Linux and Windows.
- Serve credential-protected, view-only profile pages to browsers and streaming clients on the local network.

## Runtime architecture

`desktop/` contains the canonical Electron application for Linux and Windows. It renders the manager in a hidden GPU-accelerated offscreen `BrowserWindow`, while a platform provider composites that renderer into osu!:

- Windows uses `@asdf-overlay/core` and `@asdf-overlay/electron`.
- Linux uses the Rust workspace in `native/`: an authenticated Node-API host, GL agent, .NET profiler bootstrap for late attach to native lazer, and a Vulkan loader layer for launch-time targets such as Wine/DXVK.

Electron frames reach the game through the transport its graphics context can actually use. Vulkan imports DMA-BUF directly. Desktop GL/GLX prefers `GL_EXT_memory_object` + `GL_EXT_memory_object_fd`, importing Chromium's native-pixmap FD with `glImportMemoryFdEXT`/`glTexStorageMem2DEXT`; EGL/GLES can use EGLImage import. Shared-memory BGRA remains the compatibility path. A failed real DMA-BUF import withdraws the capability and disconnects the native session so the manager can recreate the offscreen renderer in CPU/SHM mode. The native agents keep the latest usable frame, never wait for the host on a normal game presentation path, reject an unusable import instead of erasing the game's frame, and fall through to the original swap/present when overlay state is unavailable. Qt, CXX, LayerShellQt, and the transparent Linux desktop overlay are no longer runtime or build dependencies.

See the [architecture overview](openwiki/architecture/overview.md) for the component and security boundaries.

## Controls

`Ctrl+Shift+Space` enters or leaves edit mode. `Ctrl+Alt+Shift+H` shows or hides overlays. While editing, `Ctrl+I` toggles overlay interaction and `Ctrl+R` refreshes the selected overlay; Delete removes the selected overlay and Escape exits edit mode. Reload, edit, visibility, and exit controls are also available from the tray.

Display-mode input passes to the game. The Linux GL agent captures SDL input only while editing and redirects it to Electron; host or agent disconnect clears capture. The Vulkan/Wine path currently provides in-game presentation but does not implement the GL agent's SDL input redirection.

## Build and package

Node.js 22 or newer and Bun 1.3.14 are required. Bun is the package manager and script launcher; Node remains available for Electron and Node-API tooling. Build and test the shared Electron host from `desktop/`:

```sh
cd desktop
bun install
bun run format:check
bun run lint
bun run typecheck
bun run test
bun run build
```

`format:check` runs oxfmt and `lint` runs oxlint; `bun run format` rewrites files and `bun run lint:fix` applies the autofixable subset. Formatting is configured in `desktop/.oxfmtrc.json` and linting in `desktop/oxlint.config.ts`, which denies every stable oxlint category and enables the vendored anti-slop plugin under `desktop/tools/oxlint/`.

Build Windows setup and portable artifacts on Windows:

```powershell
cd desktop
bun install
bun run dist:win
```

Build the Linux x64 AppImage on Linux with Rust 1.96.1, Cargo, and the .NET 8 SDK installed:

```sh
cd desktop
bun install
bun run dist:linux
```

`dist:linux` builds the napi-rs addon, GL agent, profiler bootstrap and self-contained attach tool, Vulkan layer and manifest; stages them outside ASAR; builds Electron; packages the AppImage; and verifies the required resources. Artifacts are written to `desktop/dist/`.

For native-only development:

```sh
cd native
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
bun install
bun run build
bun run test
```

See [Build and run](openwiki/operations/build-and-run.md) for launch modes and prerequisites, and [Testing guidance](openwiki/testing/guidance.md) for the complete verification matrix.

## Linux launch modes

Native osu!lazer can be attached after it starts through .NET's profiler-attach API:

```sh
vosulay --pid <lazer-pid>
```

The packaged host supplies the attach tool, profiler bootstrap, and GL agent automatically. In source-tree development, set `VOSULAY_NATIVE_ROOT` to a staged native-resource directory.

Wine/stable GLX uses the same GL agent. When the presenting context supports the required GL memory-object and sync extensions, DMA-BUF is enabled by default. With `VOSULAY_NATIVE_DIAGNOSTICS=1`, a successful zero-copy import reports:

```text
vosulay GL agent: DMA-BUF import path: GL_EXT_memory_object_fd
```

Set `VOSULAY_GL_SHM_ONLY=1` to force the CPU/SHM compatibility path while diagnosing driver or native-pixmap import problems. If a real DMA-BUF import fails, vosulay withdraws DMA-BUF for that target process and re-attaches using SHM rather than trying the unreliable EGLImage-in-GLX path.

Vulkan must be enabled before the target creates its Vulkan instance, so vosulay launches that target:

```sh
vosulay --launch-vulkan <executable> -- <target arguments>
```

For Wine, pass the Wine executable as `<executable>` and the Windows game path in `<target arguments>`. vosulay sets the explicit layer path and per-session authenticated socket variables only for that child process; the layer stays dormant without a valid host session.

Common options are `--vosu-url`/`-t`, `--fps-limit`/`-f`, repeatable `--pid`, and repeatable `--process-name`. `--launch-vulkan` cannot be combined with `--pid`.

## Configuration

Installed builds store configuration under the platform application-data directory as `vosulay/config.json`. Windows portable builds use `<executable directory>/data/config.json`. Writes are revision-checked and atomic. The schema and normalization rules are documented in [Configuration](openwiki/domain/configuration.md).

## Remote profile pages

vosulay listens on `0.0.0.0:5050`. Copy the complete **Remote access URL** from Settings; protected routes use a persistent generated credential:

```text
http://<vosulay-host>:5050/access/<credential>/overlay
```

Add `?profile=1` to select a profile by its 1-based position. Clients must use the machine's loopback or configured LAN host, not the `0.0.0.0` bind address.

Remote access is controlled by **Settings → Remote overlay server → Allowed client IPs**. Loopback is always allowed; remote entries can be exact IP addresses or four-part IPv4 patterns such as `192.168.*.*`. The profile page loads vosu counters directly, so the same client addresses may also need access to vosu. HTTP(S) custom overlays load directly. Local HTML resources remain confined to authorized roots and are forwarded through opaque capability URLs in sandboxed frames.

## Security

`vosulay-overlay` local HTML overlays run in a secure context, but overlay pages are not cross-origin isolated. `SharedArrayBuffer` is therefore unavailable by default.

The **Allow unrestricted SharedArrayBuffer** setting is a compatibility opt-in. Enabling it requires a restart and affects every remote URL and local HTML overlay. It bypasses a Chromium Spectre mitigation; enable it only when every overlay is trusted.

The Linux native socket is an abstract `AF_UNIX` `SOCK_SEQPACKET` endpoint with a random suffix and 256-bit cookie. Both sides validate peer credentials, target identity, protocol version, message bounds, descriptor fields, and ancillary FD counts. It is never exposed over TCP.

## Engineering documentation

[OpenWiki](openwiki/quickstart.md) is the canonical engineering documentation. Updates remain manual and local; do not add an OpenWiki GitHub Actions workflow.

## License

[MIT](LICENSE)
