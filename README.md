<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/vosulay-logo-on-dark.svg">
  <img src="assets/branding/vosulay-logo.svg" alt="vosulay" width="720">
</picture>

**Cross-platform in-game overlay manager for a vosu or tosu-compatible backend — native Rust with one CEF browser runtime.**

</div>

vosulay renders existing tosu-compatible counters inside osu! and preserves the editor and profile model. Use a separately running vosu or tosu-compatible backend at `http://127.0.0.1:24050/` by default; change it in **Settings → Advanced → vosu URL**. Compatibility is detected from the public counter API, not the server's product name. Vosu remains completely independent from vosulay.

Need help? See [SUPPORT.md](SUPPORT.md) for where to ask questions and report bugs. Contributing? Start with [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites and verification on one Linux or Windows machine.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Disable individual overlays without removing their placement; re-enable them in the inspector.
- Maintain multiple overlay profiles; rename the active one from the profile menu (☰ → Rename).
- Discover installed counters from a local Vosu or Tosu instance, and search them by name, author or folder.
- Edit a counter's vosu settings and settings profiles from the editor, and pin a placement to one vosu profile so it keeps its settings when vosu's active profile changes (vosu with settings profiles only).
- Copy one or more selected overlays and paste them into another profile; the copies keep every property, including their vosu profile pin.
- Add remote or local custom HTML overlays.
- Use Chromium GPU compositing, WebGL, WebGPU where supported, and overlay audio.
- Render inside Vulkan, EGL/OpenGL, and GLX/OpenGL games on Linux, including native Wayland and X11/XWayland games and osu!stable under Wine (for example osu-winello).
- Render inside osu!stable and osu!lazer on Windows through native asdf-overlay 2.1.0.
- Serve view-only profile pages to browsers and streaming clients on the local network, and optionally the full editor to OBS or a browser on the same computer.

## Install

Download an x86_64 package from [GitHub Releases](https://github.com/shiruviaa/vosulay/releases).
Replace `<v>` below with the release version. Published releases include
`SHA256SUMS` for checking the downloads. Releases after v1.2.0 also carry a
detached OpenPGP signature `<file>.sig` for every file, made with sxlvia's
release key `6F0F 0D30 C09D 3A03 6246  A810 2CB6 524B B54B 2CDA`
([public key](packaging/release-signing-key.asc)); check one with
`gpg --import packaging/release-signing-key.asc` and
`gpg --verify <file>.sig <file>`.

### Code signing policy

<!--
SignPath signing is hidden until SignPath Foundation approves the project; until
then every Windows release is unsigned. Restore this attribution once releases
are signed through SignPath.

Free code signing provided by [SignPath.io](https://signpath.io/), certificate by
[SignPath Foundation](https://signpath.org/).
-->
Windows binaries are unsigned, so Windows SmartScreen may warn before they run.
See the [Code signing policy](docs/code-signing.md) for signing status, team roles,
and the [privacy policy](docs/code-signing.md#privacy-policy). Before installing,
read the [security and installation disclosure](docs/security.md) for injection,
input hooks, listeners, autostart, registry/files, and complete removal instructions.

### Linux

Choose the package for your distribution:

| Distribution | Install |
| --- | --- |
| Ubuntu 22.04+ / Debian 12+ | `sudo apt install ./vosulay_<v>-1_amd64.deb` |
| Fedora | `sudo dnf install ./vosulay-<v>-1.x86_64.rpm` |
| Arch Linux | `sudo pacman -U vosulay-<v>-1-x86_64.pkg.tar.zst` |

Then launch **Vosulay** from your applications menu or run `vosulay`.
Packages install the complete runtime and a system launcher; startup does not
create a duplicate per-user launcher. Linux release binaries are built on
Ubuntu 22.04 and require glibc 2.35 or newer.

For other distributions, download `vosulay-<v>-linux-x86_64.tar.gz`, extract it,
and run `./vosulay` inside the extracted directory. Install your distribution's
CEF runtime dependencies (see [package metadata](packaging/linux/nfpm.yaml)).
Keep the executable, CEF resources/locales, sandbox helper, overlay/lookup
libraries and Vulkan manifest together. Startup registers a user launcher
pointing at that location; do not move the directory without launching again.
Nothing is downloaded at startup.

**Tarball sandbox on Ubuntu 24.04+:** if restricted unprivileged user namespaces
prevent Chromium from starting, install the `.deb` instead, or configure the
trusted release's helper from inside the extracted directory:

```sh
sudo chown root:root chrome-sandbox && sudo chmod 4755 chrome-sandbox
```

Distribution packages already install this helper setuid root.
To uninstall a package, use `sudo apt remove vosulay`, `sudo dnf remove vosulay`,
or `sudo pacman -R vosulay`.

No apt, dnf, or pacman repository is configured, so a system upgrade does not
pick up new versions. To upgrade, quit vosulay from the tray, download the newer
release package, and install it with the same command; the package manager
replaces the old version. Arch users can alternatively install the
`vosulay-git` AUR package once it is published; it builds the current `main`
branch from [`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD), and an AUR
helper rebuilds it when `main` moves.

### Windows

Run `vosulay-<v>-windows-x86_64-setup.exe`. The Inno Setup installer needs no
administrator privileges and installs to `%LOCALAPPDATA%\Programs\Vosulay`.
It creates a Start Menu shortcut; desktop and start-with-Windows shortcuts
are optional and unchecked by default. A final checkbox lets you launch the app.
The installer is unsigned, so SmartScreen may warn.

Alternatively, unpack `vosulay-<v>-windows-x86_64.zip` and run `vosulay.exe`.
Keep `vosulay.dll`, CEF runtime files/locales and both injection DLLs beside it.
The ZIP uses the same user configuration as the installer, not portable config.

Before upgrading or uninstalling, choose **tray → Exit vosulay** when prompted;
hiding the app is not quitting it. To upgrade, run the newer release's installer
over the existing installation, or replace the unpacked ZIP directory.
Uninstall from Windows **Apps & features**.
Uninstall keeps `%APPDATA%\vosulay\config.json`.

### Build from source

Install rustup and the [native prerequisites](CONTRIBUTING.md), then use the
pinned toolchain from a checkout:

```sh
cargo install --path . --locked
vosulay
```

Use `--force` to reinstall. When `CEF_PATH` is unset, Cargo acquires and verifies
the pinned **precompiled** CEF distribution; no Chromium source build is needed.
The SDK cache is `$XDG_CACHE_HOME/vosulay/cef` (`~/.cache/vosulay/cef` by default).
An explicit `CEF_PATH` must point to a verified distribution; cached or explicit
inputs need no acquisition download.

On Linux, Cargo installs only the executable and retains CEF files in
`$XDG_CACHE_HOME/vosulay/runtime` and compositor/lookup libraries in
`$XDG_CACHE_HOME/vosulay/overlay` (`~/.cache` when unset). Keep both caches;
reinstall if removed. No `LD_LIBRARY_PATH` or `--cef-dir` setup is needed;
`cargo run` uses the same retained runtime. For redistribution, build a complete
package rather than copying the Cargo-installed executable; see
[packaging operations](docs/operations.md#installation-and-packaging).

## Linux

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

### osu!stable under Wine

osu!stable running through Wine's X11 driver (osu-winello, Lutris, or plain
Wine) is supported with the same preload. Launch it through its refreshed
shortcut or with the `LD_PRELOAD` line above; preloads added later by a launch
script, such as osu-winello's OBS capture setting, may appear in any order.
Live attachment does not reach Wine games: a game already started with the
preload connects by itself, and one started without it gets a single warning
asking for a relaunch.
Wine draws into an offscreen window while the visible window holds focus, so
vosulay treats the game as focused whenever any window of the game process has
X input focus. Editing reads input from Wine's own X event queue: shortcuts
work in game, and while editing the overlay receives the keyboard and mouse,
the game's hidden cursor is replaced by a normal pointer, and raw mouse input
is held back from the game.

### OBS game capture

Linux shortcuts use `lookup helper → foreign preloads → compositor` preload order.
The lookup-only helper catches explicit SDL library lookups even when OBS supplies
its own `dlsym`; it preserves native caller scope for `RTLD_NEXT` and `RTLD_DEFAULT`.
OBS's installed Vulkan layer stays before vosulay. OBS copies the game before
vosulay draws the overlay. When OBS's GL capture library is loaded, GLX and EGL
swaps, including ones Wine resolves through `dlsym` and EGL damage swaps, first
pass through OBS's swap wrapper, which captures the clean frame and then chains
back to vosulay for composition. SDL2/SDL3 GL swaps defer composition to the
native EGL/GLX swap.

**Display Capture includes the overlay.** Exclusion applies to game capture, not
screen capture. Vosulay does not change OBS scenes, capture sources, or settings.
An OBS image-export failure is not hidden or repaired by vosulay.


## Windows

Install the Windows installer or ZIP described [above](#windows), then run
vosulay alongside a vosu or tosu-compatible backend. The packaged `vosulay.exe`
is CEF's pinned sandbox bootstrap.

The host follows the last focused supported `osu!.exe` or `osulazer.exe`,
remains attached when focus leaves the game, switches only when another
supported game becomes focused, and recovers after game restart. It copies
CEF's callback-borrowed D3D11 texture into reusable vosulay-owned GPU textures
before returning; the normal path has no CPU readback.

Vosulay does not request elevation. If explicitly launched elevated, its browser
host keeps the selected privileges for native attachment instead of Chromium
relaunching it; CEF renderer and GPU processes remain sandboxed.

If you use tosu, turn off tosu's own in-game overlay before starting vosulay:
in the tosu dashboard open **Settings → Overlay** and disable **In-Game
Overlay**, or set `ENABLE_INGAME_OVERLAY=false` in the `tosu.env` beside
`tosu.exe`. When enabled, tosu launches `tosu-ingame-overlay.exe`, which injects
its own asdf-overlay into the same osu! process; vosulay does not support two
overlay hosts hooking one game's Present and input. tosu's in-game overlay is
Windows-only, so Linux needs no change.

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

On Linux, an input method gets keys before the game whenever text input is
enabled. In osu! that is while a text box such as song select search is
focused, and always while vosulay is editing. If the input method claims the
chord, the game never receives it. Mozc's default keymap, for example, turns
`Ctrl+Shift+Space` into a full-width space. Escape still exits editing. To
toggle independently of input methods, bind `vosulay --toggle-edit` (see below)
in your compositor, which handles keybinds before any input method. On Hyprland
with Lua configuration (`hyprland.lua`):

```lua
hl.bind("CTRL + SHIFT + space", hl.dsp.exec_cmd("vosulay --toggle-edit"))
```

or with `hyprland.conf`:

```ini
bind = CTRL SHIFT, SPACE, exec, vosulay --toggle-edit
```

The compositor bind takes the chord from every application, and the in-game
shortcut no longer sees it.

While editing, Ctrl+I toggles counter interaction, Delete removes the selected
counter, and Escape exits editing. Ctrl+Z / Ctrl+Shift+Z undo/redo edits;
Ctrl+R refreshes the selected counter and Ctrl+Shift+R refreshes the active
profile's counters. Interaction, profile, copy/paste, undo/redo, and refresh
shortcuts are configurable. Interaction also requires Ctrl, Alt, or Meta.
Text fields keep native undo/redo; normal mode leaves game input alone.

The compact inspector has refresh and close controls in its header, position
and size fields in two columns, then a **Scale / Layer** row and a
**Settings / Advanced** row. Hover or focus an icon for its label.
**Advanced** opens the full placement editor: enabled state, title, position,
size, scale, layer, opacity, rotation, crop, reset size, custom-overlay URL and
vosu profile pin, plus removal with an inline confirmation. The Delete key also
removes the selected placement; undo restores it.

When vosu exposes overlay settings profiles, the inspector's **Settings**
button opens a native editor in vosulay's Settings modal, not an embedded
dashboard page. Search settings or jump between their sections, edit values,
and save them to vosu. The **This placement uses** dropdown chooses the settings
profile and pins the selected placement to it, so its settings remain the same
when vosu's default profile changes. Choose **Default profile** to follow vosu's
current default instead. Selecting a named profile does not change that default.
The menu creates an empty profile, copies, renames, deletes or uses **Set as
default** to change vosu's default profile; default markers identify that profile
in the picker and menu. Pins are placement properties and participate in
vosulay's normal save and undo history. Pinned placements show the profile after
their name.

Native settings remain available when vosu reports read-only access, including
when no local write token is available. Values load with disabled inputs; the
reported reason and **Retry** replace **Save**. Creating, copying, renaming,
deleting and setting the default profile are disabled, but choosing a settings
profile for this placement remains available. **Retry** refreshes backend
capabilities so editing can resume once write access is restored; read-only
access never silently redirects to a browser.

The menu's explicit **Open in vosu dashboard** action remains available to open
the dashboard in a browser. With tosu, an older vosu, or any backend that does
not expose the native settings-profile catalog, the **Settings** action opens
the counter's settings page in the browser instead. Creating or renaming a
profile from this menu pins the placement you are editing to it, and deleting
its profile unpins it. Any other placement pinned to a profile that was renamed
or deleted, here or elsewhere, is marked missing and reloads with vosu's default
profile; refresh view-only pages in OBS or a browser yourself.

Copy (`Ctrl+C`) keeps the selected overlays across profile switches; paste
(`Ctrl+V`) adds them to the active profile at the same position.

Both platforms provide **Edit mode**, **Hide vosulay**, and **Exit vosulay**
in the tray; the first two are checkable. Linux requires StatusNotifierItem
tray support. Use **Exit vosulay** to quit; hiding does not exit the app.
Editing only captures input while the game is focused, so choosing **Edit mode**
from the tray starts editing as soon as the game is focused again; it stays
checked until then, and choosing it again cancels.

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

Drag a counter to move it. Drag a selected counter's corner handles, or the
edge and corner bands just inside its border, to change it: a plain drag
resizes, **Ctrl**+drag scales it while the opposite corner (or the middle of the
opposite side) stays fixed, and **Shift**+drag crops that side while the
counter's content stays in place. The band under the pointer is highlighted in
the color of the edit it would make. The bands also work for counters flush
with the screen edge, whose handles fall off-screen. The first press on an
unselected counter only selects and moves it.

Right-click a counter for its properties, which include the crop of each side
in counter pixels. The mouse wheel changes the field under the pointer by its
step, without clicking into it; hold Shift for ten steps. Scrolling one field is
one undo step. **Reset size** restores the counter's size, scale 1 and no crop.
The right-click menu also offers **Refresh**.

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

The helper acquires and verifies cached pinned CEF assets, builds both
asdf-overlay DLLs from their pinned source (this needs rustup's
`i686-pc-windows-msvc` target and the MSVC x86 tools as well as x64), then builds
the native client DLL and writes `dist/vosulay-<version>-windows-x86_64.zip`.
Both platform packages use the version in `Cargo.toml`.

Configuration defaults to `%APPDATA%\vosulay\config.json`. To use portable
storage, create a file named `portable` beside `vosulay.exe`; configuration then
lives in `data\config.json`. An existing `data\config.json` also selects that
location.

Shared command-line options are `--vosu-url`/`-t`, `--fps-limit`/`-f`,
repeatable `--pid` and `--process-name`, `--config`, `--cef-dir`, and
`--server-only`. Run `vosulay --help` (Windows: `vosulay.exe --help`) for details.

Use `vosulay --toggle-edit` from a compositor keybind to toggle editing in the
running instance, or `vosulay --quit` to save and exit gracefully. On Linux,
Ctrl+C in the terminal that started vosulay, `kill`, or closing that terminal
exits the same graceful way; a second Ctrl+C exits at once. These two
commands are mutually exclusive, communicate only through a per-user local
control channel, and exit with status 1 if no instance is running or the command
is rejected. A plain second launch still exits successfully without starting
another instance.

For browser/OBS overlays without a game, run:

```sh
vosulay --server-only --config /path/to/config.json
```

This keeps the backend broker, remote `/overlay` pages and editor worker, but
does not discover, inject into or attach to games, rewrite Linux game launchers,
create a tray, or initialize CEF. The startup message identifies the configured
listener and local editor URLs. `--vosu-url` still overrides the backend for this
run; `--pid`, `--process-name`, `--fps-limit` and `--benchmark-uncapped` are
rejected in server-only mode. `--toggle-edit` is rejected there because no
in-game overlay exists; use the editor at `http://manage.localhost:<port>/overlay`
instead (see below). Remote access and local editing still follow the configured
default-deny policies, so allow editing on this computer first.

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
Filtered snapshots keep the backend's field order, so counters that iterate
object keys render the same rows in the same order as with a direct connection.

Selected-field subscriptions (`/websocket/v2/subscribe`) are relayed per client
to the backend, which publishes them at its precise cadence. Projecting them from
the shared `/websocket/v2` stream would delay hit errors and other precise
fields by up to the backend's regular poll interval.

Settings and other commands retain separate upstream channels with each
counter's original query/identity. They are not shared: upstream authorization
and replies are client-specific. vosulay exposes no private telemetry API, and
vosu remains independent.

Endpoints that only vosu provides, such as `/websocket/v2/subscribe`, are not
available from tosu; counters written for them stay empty there and reconnect
periodically.

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

### Editing from OBS or a browser on this computer

Enable **Settings → Advanced → Allow editing on this computer** (off by default)
to get vosulay's full editor at `http://manage.localhost:<port>/overlay`, for
example inside an OBS browser source's **Interact** window, a browser tab, or an
overlay window of your own. Press the edit shortcut (`Ctrl+Shift+Space` by
default) inside the page and use the same editor as in game: profiles, settings,
HTML files and imports included (file choices open native dialogs on this
computer). Changes apply as they do in game; the in-game editor picks them up at
once and other open editor pages within about two seconds. The editor is served
only to this computer, and only under the `manage.localhost` name; the normal
`/overlay` address stays view-only for everyone. As in game, counters on the
editor take no clicks outside edit mode, so the page keeps the keyboard for the
edit shortcut; use the interaction shortcut (`Ctrl+I`) while editing to use them.

#### Using vosulay as the frontend of other overlay software

Overlay renderers that draw a web page over the game themselves (for example
idk-overlay, or a Linux window built for tosu's in-game page) can use vosulay's
editor as their whole UI, with `vosulay --server-only` if vosulay should not
draw in game too. Load `http://manage.localhost:<port>/overlay` as a top-level
page sized to the game: outside edit mode it shows only the counters on a
transparent background, and in edit mode it is the full in-game editor. Switch
edit mode the way tosu's in-game overlay does: run
`window.postMessage('editingStarted')` or `window.postMessage('editingEnded')`
in the page. The page posts the same strings when it enters or leaves edit mode
itself (shortcut, Escape, **Done**), so the host can listen for `message`
events to capture input or restore click-through. Messages from counter frames
are ignored.

The switch takes effect without a restart. Counters on the editor load from the
normal listener address, a different origin than `manage.localhost`, so neither
they nor pages on other websites can use the editing routes, and the editor
cannot add a local HTML file unless you chose it in a file dialog. The editor
refuses to load inside another page's frame, so it cannot be overlaid with
deceptive content to trick clicks.

## Troubleshooting

If counters are missing, focus a supported game, check the backend URL,
clear **Hide vosulay**, and verify placements are enabled and in view.
On Linux, restart the game through its refreshed shortcut if live attachment
is restricted. Keep the host, injected libraries, and CEF runtime from the same
package. If a shortcut is already registered by another Windows app, choose
another chord. On Windows with tosu, disable tosu's in-game overlay as described
[above](#windows-1). See the [operations runbook](docs/operations.md) for more
detail.



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

Read the [security and installation disclosure](docs/security.md) before installing:
it covers game-process injection, Linux shortcut changes and the setuid sandbox
helper, clipboard/input access, network exposure, and unresolved third-party behavior.
Use only trusted overlays, especially with unrestricted SharedArrayBuffer enabled.

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
`1.0`/`0.0`, and imported counters are uncropped. A saved migration marker makes automatic import idempotent.
Existing user-created profiles or settings are never overwritten automatically.

To import intentionally, enter edit mode with `Ctrl+Shift+Space`, open
**Settings → Import tosu layout**, and select the JSON file. This appends profiles
and resolves ID collisions without replacing existing profiles. Pending edits
must save successfully before import proceeds; native file picking releases
game input. Source-derived 4.26.0, 4.26.1, 4.26.2, and legacy fixtures cover
conversion, malformed/unsafe input, persistence, idempotence, and no-overwrite.

## License

[MIT](LICENSE) for the source code. The vosulay logo and icons are © 2026
sxlvia under [CC BY 4.0](assets/branding/LICENSE.md); showing them while using
or talking about vosulay (streams, videos, screenshots) needs no credit.
Third-party terms and artifact origins: [notices](THIRD_PARTY_NOTICES.md) and [provenance](docs/provenance.md).
