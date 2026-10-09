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
libraries, resources, locales, sandbox helper, `libvosulay_overlay.so`, and
`libvosulay_lookup.so` beside the executable. Nothing is downloaded at startup.

With Rust 1.96.1 and the native platform toolchain installed, developers can
build directly:

```sh
cargo build
```

When `CEF_PATH` is unset, Cargo acquires the pinned **precompiled** CEF
distribution automatically. It verifies the pinned archive size, SHA1/SHA256,
and runtime files, then reuses the verified cache on subsequent builds.
No Chromium or CEF source build is required.

Install from a checkout with Cargo:

```sh
cargo install --path . --locked
vosulay
```

Use `--force` when reinstalling an existing installation. Cargo installs only
the executable, so the build also retains verified CEF files in
`$XDG_CACHE_HOME/vosulay/runtime` and both the native compositor and lookup helper
in `$XDG_CACHE_HOME/vosulay/overlay` (under `~/.cache` when unset). The installed
launcher runs beside those retained CEF assets; no `LD_LIBRARY_PATH` or
`--cef-dir` setup is needed. Keep these two caches while using the installation;
if they are removed, reinstall to recreate them. Nothing is downloaded at
application startup. `cargo run` uses the same retained runtime.

To build and assemble the complete portable package:

```sh
cargo build --release --locked
cargo build --release --locked -p vosulay-overlay -p vosulay-lookup
CEF_PATH="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
cargo run --quiet --locked -p vosulay-cef-tools -- package --platform linux-x86_64 \
  --cef-path "$CEF_PATH" --output dist/linux \
  --exe target/release/vosulay --overlay target/release/libvosulay_overlay.so \
  --overlay target/release/libvosulay_lookup.so \
  --overlay crates/overlay-linux/VkLayer_vosulay_overlay.json
./dist/linux/vosulay
```

The default CEF cache is `$XDG_CACHE_HOME/vosulay/cef`, or
`~/.cache/vosulay/cef` when `XDG_CACHE_HOME` is unset. Cargo and packaging share
the [Rust acquisition helper](crates/cef-tools/src/lib.rs); the first SDK
download requires internet access. An explicit `CEF_PATH` overrides acquisition
and must point to a verified distribution; cached and explicit-path builds need
no network access.

For distribution to another machine, package the complete directory rather
than copying a Cargo-installed executable alone. Portable packages use their
adjacent runtime files instead of the build machine's caches.

On Linux startup, vosulay refreshes recognized osu desktop shortcuts with the
current compositor and lookup helper's absolute preload paths and Vulkan settings.
Existing foreign preloads, launch arguments, and OBS capture flags are preserved;
obsolete vosulay preload paths and duplicate environment assignments are removed.
System shortcuts get per-user overrides in the XDG applications directory; no
system files or privilege settings are changed. This affects the next game
launch, not an already-running game.

Vosulay also discovers native same-user game processes and attempts live
attachment. If process-injection policy prevents attachment, restart the game
through its refreshed shortcut, or preload the packaged compositor manually:

```sh
LD_PRELOAD="/absolute/path/to/vosulay/libvosulay_lookup.so:${LD_PRELOAD:+$LD_PRELOAD:}/absolute/path/to/vosulay/libvosulay_overlay.so" /path/to/game
```

For Vulkan launch-time attachment, point the Vulkan loader at the packaged layer:

```sh
VK_LAYER_PATH="${VK_LAYER_PATH:+$VK_LAYER_PATH:}/absolute/path/to/vosulay" \
VK_INSTANCE_LAYERS="${VK_INSTANCE_LAYERS:+$VK_INSTANCE_LAYERS:}VK_LAYER_VOSULAY_overlay" /path/to/game
```

CEF renders the complete profile in one transparent OSR browser. Chromium keeps
GPU acceleration enabled; only the final Linux frame transfer uses CPU BGRA
pixels and a persistent three-slot memfd ring. The game acquires the newest
complete frame at present and uploads it to a reusable GPU texture. Linux
DMA-BUF support is not required.

Linux selection follows authenticated game focus notifications, not process
enumeration order. Focusing an unrelated window retains the selected game;
another supported game takes over only after gaining focus. If the selected
game exits, vosulay waits for a supported game to gain focus instead of selecting
an already-running background game.

Focus tracking needs neither eBPF/CN_PROC probes nor elevation. It does not
bypass Linux injection policy; use the preload or Vulkan-layer launch routes
above when live attachment is restricted.

### OBS game capture

Linux shortcuts use `lookup helper → foreign preloads → compositor` preload order.
The lookup-only helper catches explicit SDL library lookups even when OBS supplies
its own `dlsym`; it preserves native caller scope for `RTLD_NEXT` and `RTLD_DEFAULT`.
OBS's GL capture preload stays before the compositor, and its installed Vulkan
layer stays before vosulay. OBS copies the game before vosulay draws the overlay.
SDL2/SDL3 GL swaps defer composition to the native EGL/GLX swap; EGL damage swaps
use OBS's standard full-buffer swap route so capture still happens before drawing.
Keep this hook order for manual launches too.

**Display Capture includes the overlay.** Exclusion applies to game capture, not
screen capture. Vosulay does not change OBS scenes, capture sources, or settings.
An OBS image-export failure is not hidden or repaired by vosulay.


## Windows

Extract the complete x86_64 portable ZIP and run `vosulay.exe` alongside a vosu
or tosu-compatible backend. The executable is CEF's pinned sandbox bootstrap;
keep `vosulay.dll`, CEF runtime files, locales, and both asdf-overlay injection
DLLs beside it.

The host follows the last focused supported `osu!.exe` or `osulazer.exe`,
remains attached when focus leaves the game, switches only when another
supported game becomes focused, and recovers after game restart. It copies
CEF's callback-borrowed D3D11 texture into reusable vosulay-owned GPU textures
before returning; the normal path has no CPU readback.

Vosulay does not request elevation. If explicitly launched elevated, its browser
host keeps the selected privileges for native attachment instead of Chromium
relaunching it; CEF renderer and GPU processes remain sandboxed.

On Windows, OBS's **Capture third-party overlays** setting controls when OBS
copies the swapchain; it is not a guaranteed exclusion switch. Vosulay's pinned
asdf-overlay draws before forwarding `Present`/`Present1`, so exclusion with that
setting disabled still depends on which Present hook runs first. This is
source-verified for the DXGI path, not native Windows capture verification.

## Editing, shortcuts, and tray

`Ctrl+Shift+Space` is the default edit-mode toggle. Configure it and the
hide/show shortcut in **Settings → Shortcuts**; hide/show is initially unbound.
Both native shortcuts require Ctrl, Alt, or Meta plus a natively mappable key.
On Linux they work through the in-game input path while the game is focused.
Windows registers global hotkeys; edit toggle acts only while the game is focused.
Clear hide/show with Backspace or Delete while recording the shortcut.

While editing, Ctrl+I toggles counter interaction, Delete removes the selected
counter, and Escape exits editing. Ctrl+Z / Ctrl+Shift+Z undo/redo edits;
Ctrl+R refreshes the selected counter and Ctrl+Shift+R refreshes the active
profile's counters. Interaction, profile, copy/paste, undo/redo, and refresh
shortcuts are configurable. Interaction also requires Ctrl, Alt, or Meta.
Text fields keep native undo/redo; normal mode leaves game input alone.

Both platforms provide **Edit mode**, **Hide vosulay**, and **Exit vosulay**
in the tray; the first two are checkable. Linux requires StatusNotifierItem
tray support. Use **Exit vosulay** to quit; hiding does not exit the app.

On startup, vosulay registers an app-launcher entry. Linux installs
`~/.local/share/applications/com.vosu.vosulay.desktop` with an absolute Exec
and `~/.local/share/icons/hicolor/scalable/apps/com.vosu.vosulay.svg`
(respecting `XDG_DATA_HOME`), unless a system-wide entry exists. Windows
creates `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Vosulay.lnk`.
A second launch while vosulay is running prints a message and exits immediately.

By default, editing ends when the game loses focus. Turn off **Settings →
General → Leave edit mode when the game loses focus** to keep the editor on
screen while another window, such as a screenshot tool, is focused; input
returns to the editor when the game is focused again.

The mouse wheel acts on the counter under the pointer while editing: over its
middle it changes the layer, over an edge it grows (wheel up) or shrinks (wheel
down) that side by 10 px, and over a corner it changes the scale by 0.05 while
the opposite corner stays fixed. Continued scrolling keeps targeting the same
edge or corner until the wheel pauses, so the pointer can stay in place. Each
scroll gesture is one undo step. The right-click menu also offers **Refresh**.

The Linux editor shows the native system cursor and temporarily disables
relative mouse mode. Leaving editing, losing focus, disconnecting, or destroying
the presentation surface restores the game's latest requested cursor state.
Game focus events and swap-interval requests still reach the game unchanged.

**Keep overlays in view** constrains only the selected counter, and only while
editing. Unselected counters and game-window resizes in normal mode never clamp
or save new placement coordinates.

CEF starts lazily when a game is selected. Detaching closes the in-game view
after pending edits are flushed and releases its telemetry connections and Linux
frame ring. The initialized CEF runtime remains until exit: CEF cannot safely be
reinitialized in the same process. A retained runtime without a view has no
active-view rendering clock. Remote profile clients remain independently usable.

Input capture/release and browser-heartbeat failures have a five-second bound.
Failure releases capture or terminates vosulay so the injected connection closes
and game input recovers. Restart vosulay to attach again.

Plain `cargo build` also acquires the pinned Windows CEF distribution
automatically when `CEF_PATH` is unset. To build and package on Windows with
Rust 1.96.1 and the native MSVC toolchain:

```powershell
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
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
rate changes. Both hosts use CEF's internal, damage-driven OSR frame scheduler.
Linux's injected compositor consumes the newest complete frame at game present;
Windows publishes completed application-owned GPU textures to asdf-overlay.
Automatic mode applies the display rate without an external begin-frame loop.

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

The overlay's CEF limit does not change the game's FPS cap, swap interval, or
window focus. The Linux release build retains an optimized in-game compositor
even though Cargo builds it as a build dependency.

## Backend connections

vosulay brokers installed counters through the configured vosu or
tosu-compatible backend. Counter pages retain the public API paths and protocols;
their `fetch`, XMLHttpRequest, and WebSocket requests to that backend are routed
through vosulay without changing the counter files. The bootstrap also supplies
`COUNTER_PATH` before counter scripts run, preserving a server-provided value.

There is one upstream telemetry WebSocket per active schema (`/ws`, `/tokens`,
`/websocket/v2`, or `/websocket/v2/precise`). Consumers share its newest snapshot;
filters remain client-local. Matching HTTP JSON requests use that snapshot when
available and otherwise retain the backend's HTTP behavior. Slow consumers skip
stale telemetry instead of building a queue.

Selected-field subscriptions (`/websocket/v2/subscribe`) are relayed per client
to the backend, which publishes them at its precise cadence. Projecting them from
the shared `/websocket/v2` stream would delay hit errors and other precise
fields by up to the backend's regular poll interval.

Settings and other commands retain separate upstream channels with each
counter's original query/identity. They are not shared: upstream authorization
and replies are client-specific. vosulay exposes no private telemetry API, and
vosu remains independent.

The in-game browser uses a separate loopback listener, so an occupied remote
server port does not prevent local counters from loading.

## Remote profile pages

vosulay listens on `127.0.0.1:5050` by default and serves the active profile at
`http://<listener address>:<listener port>/overlay` (for example
`http://127.0.0.1:5050/overlay`). Append `?profile=<index>` to select a profile
by its 1-based position in the profile list; profiles can be reordered in the
editor to change their index.

The listener address and port are configurable in **Settings → Remote overlay
server → Listener address** and **Listener port**. Set the address to a LAN
interface IP, or to `0.0.0.0` to listen on every interface, to reach the pages
from other devices; clients then use the machine's LAN address, not
`0.0.0.0`. Listener changes take effect after restarting vosulay. There is no
access token. Old credential and remote-host fields are ignored when loading
configuration and are omitted by subsequent saves.

**Settings → Remote overlay server → Allowed client IPs** controls which
clients may connect to vosulay's listener; it does not change the backend's own
allowlist. Loopback is always allowed; remote entries can be exact IP addresses
or four-part IPv4 patterns such as `192.168.*.*`. There is no authentication
beyond this allowlist: any allowed client can view the profiles and access the
proxied backend. Restrict the listener and allowlist to trusted networks and
devices.

Installed counters load through vosulay, so the backend must allow the
vosulay host rather than every remote viewing device. Custom HTTP(S) overlay
URLs on unrelated origins still load directly. Local HTML resources remain
confined to their authorized roots and are served only to allowed clients;
authority/origin checks and path/symlink confinement remain in place.

## Troubleshooting

If counters are missing, focus a supported game, check the backend URL,
clear **Hide vosulay**, and verify placements are enabled and in view.
On Linux, restart the game through its refreshed shortcut if live attachment
is restricted. Keep the host, injected libraries, and CEF runtime from the same
package. If a shortcut is already registered by another Windows app, choose
another chord. See the [operations runbook](docs/operations.md) for more detail.



## Development and engineering documentation

See the hand-maintained [engineering documentation](docs/README.md) for
[architecture](docs/architecture.md), [configuration](docs/configuration.md),
[integrations](docs/integrations.md), [build/package operations](docs/operations.md),
[testing and private benchmark methodology](docs/testing.md), and the
[source map](docs/source-map.md). Update these pages with code changes.

Linux CI runs workspace Clippy/tests on pull requests and pushes to `main`.
Windows build/release workflows remain manual-dispatch only.
Use the private CEF smoke runner for headless checks, never the user's desktop.
Successful smoke/tests do not establish cleared 1.0 performance or release gates.

## Security

`vosulay-overlay` local HTML overlays run in a secure context, but overlay pages are not cross-origin isolated. As a result, `SharedArrayBuffer` is unavailable by default.

The embedded CEF manager is allowed Chromium local/loopback-network access so
it can load configured counters from the backend. This grant is restricted to
`vosulay://manager`; counter origins and unrelated browser permissions retain
Chromium's default policy. It does not disable web security or certificate checks.

The **Allow unrestricted SharedArrayBuffer** setting is a compatibility opt-in. Enabling it requires a restart and affects every remote URL and local HTML overlay. It bypasses a Chromium Spectre mitigation, so use it only when every overlay is fully trusted.

## Configuration

See [Configuration](docs/configuration.md) for the model, storage, and persistence.

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
