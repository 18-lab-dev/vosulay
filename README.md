<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/vosulay-logo-on-dark.svg">
  <img src="assets/branding/vosulay-logo.svg" alt="vosulay" width="720">
</picture>

**Cross-platform in-game overlay manager for vosu — native Rust + Qt 6 on Linux, in-game Electron injection on Windows.**

</div>

vosulay displays Vosu/Tosu-compatible counters and provides an edit mode for arranging them. Layouts are stored as profiles, and overlays can come from a Vosu or Tosu-compatible counter catalog, HTTP(S) URLs, or local HTML files. Vosu remains a separate process; the Windows in-game renderer is packaged only with vosulay.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Maintain multiple overlay profiles.
- Discover installed counters from a local Vosu or Tosu instance.
- Add remote or local custom HTML overlays.
- Render WebGPU overlays through Dawn's native Vulkan backend.
- Run click-through overlays on Wayland and X11.
- Render directly in osu! stable and lazer windows on Windows.
- Control Linux visibility, edit mode, geometry, and shutdown through Linux's external local IPC interface (Linux-specific).
- Serve view-only profile pages to browsers and streaming clients on the local network.

## Linux

The Linux build requires Qt 6.11.1 development packages and a Dawn-enabled Qt
WebEngine. Stock Qt WebEngine builds disable Dawn, so `build.rs` rejects them
instead of silently exposing `navigator.gpu` without a working adapter.

```sh
cargo build --release --locked
./target/release/vosulay
```

That's it — no manual Qt WebEngine setup is required. If `build.rs` can't find
a Dawn-enabled Qt WebEngine on `PKG_CONFIG_PATH`, it builds one automatically
into `$XDG_CACHE_HOME/vosulay/qt-webgpu` (`~/.cache/vosulay/qt-webgpu` by
default) using `extra/build-webgpu-qt.sh`, then picks it up for the rest of
the build. This only happens once per machine: it's a full Qt
WebEngine/Chromium source compile and takes roughly 20-40+ minutes depending
on hardware, but every subsequent `cargo build`/`cargo install` reuses the
cached result in seconds. The normal Qt WebEngine build dependencies, `git`,
`cmake`, `ninja`, `pkg-config`, `gperf`, and Python's `html5lib` must be
installed for that first build to succeed.

`build.rs` records the validated library, subprocess, resource, and locale
paths in the executable via an rpath, so a `cargo install --path .` binary
runs without any extra environment variables. Non-empty runtime
`QTWEBENGINE*` variables still override the recorded defaults.

To use a specific prefix instead of the default cache location (for example a
prefix built ahead of time, shared across machines, or pinned in CI), set
`VOSULAY_WEBGPU_QT_PREFIX` to that directory before building:

```sh
extra/build-webgpu-qt.sh "$HOME/.local/opt/vosulay-qt-webgpu"
export VOSULAY_WEBGPU_QT_PREFIX="$HOME/.local/opt/vosulay-qt-webgpu"
cargo build --release --locked
```

Setting `VOSULAY_SKIP_AUTO_WEBGPU_BUILD=1` disables the automatic build
entirely; `build.rs` then fails fast with instructions instead of compiling
Qt WebEngine, which is useful for CI or packaging environments that want to
provision a Dawn-enabled Qt WebEngine themselves and fail loudly if it's
missing rather than spend 20-40 minutes building one.

For Linux command-line options, monitor selection, frame limiting, attachment mode, and IPC controls, see [Build and run](openwiki/operations/build-and-run.md).

## Windows

The Windows host requires 64-bit Windows and a separately running vosu instance. It follows the most recently focused discovered `osu!.exe` or `osulazer.exe` client, remains attached while another application has focus, and switches only when another game client receives focus. It attaches only that game client rather than both simultaneously, and renders the manager UI through `@asdf-overlay`'s shared GPU surface inside the game presentation rather than a transparent desktop window. The composed overlay is therefore available to OBS Game Capture.

Ctrl+Shift+Space is the sole Windows global shortcut; press it in game to enter or leave edit mode. While editing, Ctrl+I toggles overlay interaction and Ctrl+R refreshes the selected overlay; Delete removes the selected overlay and Escape exits edit mode. Reload, edit, and exit controls are also available from the system tray.

If a native input capture or release request stalls for five seconds, vosulay exits immediately so Windows closes the injected connection and restores input to osu!. Restart vosulay to attach again.

To build from source with Node.js 22 or newer:

```powershell
cd windows
npm ci
npm run typecheck
npm test
npm run dist:win
```

The build produces two x64 distributions: `windows\dist\vosulay-<version>-setup.exe` is the NSIS installer, while `windows\dist\vosulay-<version>-portable.exe` is the portable executable. Install the setup executable for the normal installed distribution, or run the portable executable directly.

The installed distribution stores its configuration at `%APPDATA%\vosulay\config.json`. The portable distribution stores its configuration beside the executable at `<executable directory>\data\config.json`.

The Windows host accepts `--vosu-url`/`-t`, `--fps-limit`/`-f`, repeatable `--pid`, and repeatable `--process-name` options.

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

The full remote access URL, including the shared credential, is shown in
**Settings → Remote overlay server → Remote access credential/URL**; click
**Regenerate** to rotate the credential and invalidate previously shared
URLs.

Remote access is controlled by **Settings → Remote overlay server → Allowed
client IPs**. Loopback is always allowed; remote entries can be exact IP
addresses or four-part IPv4 patterns such as `192.168.*.*`. The profile page
loads vosu counters directly rather than proxying them, so the same remote
client IPs must also be allowed by vosu. HTTP(S) custom overlays load directly;
local HTML overlay resources remain confined to their authorized roots and are
served through vosulay to allowed clients.

## Measured overlay overhead

These are machine-specific observations, not performance promises. On 2026-07-26, a Ryzen 9 9950X3D/RTX 5090 host ran sequential 8-vCPU Windows and Linux VMs through three counterbalanced ON-OFF pairs, with 30 seconds of warmup and 120 seconds of measurement per condition. Both products rendered the same HTML at 1280x720. The table reports the median of the three within-pair ON minus OFF deltas.

| Primary full-stack ON-OFF delta | Upstream tosu on Windows | vosulay on Linux |
| --- | ---: | ---: |
| CPU (percentage points of one logical CPU) | +31.114192 (+3.8893% of 8-vCPU capacity) | +14.000311 (+1.7500% of 8-vCPU capacity) |
| Private memory | +384.654894 MiB | +412.209097 MiB |
| Working set / summed RSS | +498.781637 MiB working set | +1121.972011 MiB summed RSS |
| GPU utilization | +12.214876 percentage points | +2.520661 percentage points |
| VRAM | +19.181818 MiB | +107.512397 MiB |
| GPU power | Not captured | +11.568760 W |

ON ran the game, required daemon, and overlay renderer/host; OFF ran the same game and required daemon with only the overlay renderer/host disabled. The primary paired delta therefore captures all work induced by enabling the overlay, including work charged outside renderer processes, rather than measuring the renderer process tree alone. Process-tree memory sums can double-count shared pages.

Windows and Linux use different display stacks and frame policies. The side-by-side values are therefore directional context for these tested configurations, not a platform-neutral comparison or a universal winner.

## Development

See [Testing guidance](openwiki/testing/guidance.md) for the development and verification workflow.

## Engineering documentation

[OpenWiki](openwiki/quickstart.md) is the canonical source for detailed engineering documentation. Start with the [quickstart](openwiki/quickstart.md) for architecture, workflows, integrations, testing guidance, and the source map.

## Security

`vosulay-overlay` local HTML overlays run in a secure context, but overlay pages are not cross-origin isolated. As a result, `SharedArrayBuffer` is unavailable by default.

The **Allow unrestricted SharedArrayBuffer** setting is a compatibility opt-in. Enabling it requires a restart and affects every remote URL and local HTML overlay. It bypasses a Chromium Spectre mitigation, so use it only when every overlay is fully trusted.

## Configuration

See [Configuration](openwiki/domain/configuration.md) for the configuration model, storage, and persistence behavior.

## License

[MIT](LICENSE)
