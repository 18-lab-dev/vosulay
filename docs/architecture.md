# Runtime architecture

## Host and browser

One Rust host uses the pinned precompiled CEF runtime on Linux and Windows. CEF has its own renderer/GPU subprocesses; it is not a separate product host. The shared [manager assets](../assets/) are embedded by [src/ui.rs](../src/ui.rs) and render the entire profile in one transparent off-screen browser.

[src/runtime/host.rs](../src/runtime/host.rs) owns attachment, browser lifetime, input capture, browser heartbeats, visibility, and frame-rate policy. [src/runtime/editor.rs](../src/runtime/editor.rs) moves blocking catalog fetches, revision-checked configuration saves, file dialogs, and imports off the event loop. CEF initializes lazily when a game is selected. Detachment flushes edits and closes the view; the initialized runtime remains until exit because CEF cannot safely be reinitialized in one process.

The editor worker also serializes ruleset-bound profile selection. A cancellable
selected-field v2 watcher sends only ruleset changes/availability; the worker
uses its existing atomic saves and revision/broadcast path. The watcher closes
with the worker on stdin EOF or native exit, so it cannot outlive the host.
The last valid ruleset survives backend reconnects to preserve manual selections.

Both CEF and browser managers embed the same WORKSTATION/DRAFTING CSS and
base64-embedded OFL font subsets, retaining the vosulay logo and transparent game
canvas. No font request goes to vosu or a CDN. Native managers follow the OS;
the tool bootstrap passes vosu's initial `colorScheme` into the manager.
The editor copies vosu's shared 18-LAB palette, type/spacing scales, control
sizes, radii and focus treatment; `assets/manager.css` records the source
revision. Standalone never depends on fetching vosu's `theme.css`.


Linux's [entrypoint](../src/main.rs) re-executes Cargo-built binaries beside the retained CEF assets. Windows enters through the pinned CEF sandbox bootstrap and loads the application client DLL.

After claiming per-user instance ownership, startup binds
[src/runtime/control.rs](../src/runtime/control.rs)'s local control channel
before loading configuration, migrating tosu layouts, preparing resources and
starting the editor worker and remote server. A second launch with `--toggle-edit`
or `--quit` sends a bounded line-protocol request; the listener forwards it to the
host and unparks its event loop. Normal-mode requests reuse the tray's edit
arming and graceful-exit/flush paths. No running instance means a failed command,
not an implicit launch.

On Linux, [src/runtime/signals.rs](../src/runtime/signals.rs) turns SIGINT, SIGTERM and SIGHUP into the same graceful exit: the handler only records the request and writes to a pipe, and a watcher thread unparks the host; a second signal restores the default action and re-raises it. CEF installs Chromium's own handlers for these signals during initialization, and their quit request never reaches vosulay's externally pumped loop (Ctrl+C once did nothing until a second one killed the process), so the host installs its handlers again right after `CefRuntime::initialize`.

`--server-only` branches after the shared worker/server startup and before Linux
launcher refresh, platform discovery/attachment, tray startup or lazy CEF
initialization. Its loop discards manager replies, rejects in-game edit toggles,
and drains/stops/joins the worker on standalone `--quit` or managed stdin EOF. Backend and configuration overrides
still apply. Game-selection and frame-pacing CLI options are rejected.

### Supervised tool mode

`VOSU_TOOL_PROTOCOL=1` opts into vosu protocol v1. Before standalone ownership,
`src/runtime/lifecycle.rs` checks vosu's native broker for an enabled vosulay
tool and reuses the `vosux --open` flow instead of starting another host.
Absent/unreachable vosu or a disabled tool retains standalone startup.
Tool startup asks an occupied standalone host to yield via its same-user control
channel, waits boundedly for the lock, then imports its flushed configuration
only into an empty tool data directory. Supervised peers refuse takeover.
Refusal/timeout exits 0 with a diagnostic: vosu reports stopped/exited without
crash backoff. `src/tool.rs` watches stdin; EOF closes views and flushes state.
Launcher registration and Linux shortcut refresh are suppressed, never removed.
The tool tray retains editing controls but replaces Exit with dashboard access;
vosu alone owns Start/Stop/Restart. Native game rendering stays in vosulay.

`src/remote/tool.rs` binds a separate loopback port 0, authenticates every route
with the launch token and prints the flushed readiness line. It adapts the same
`ManageEditor` authority used by the standalone manager, not a second config
writer. The normal broker remains private loopback; tool mode does not bind the
configured standalone remote listener or enable its manage origin.


On Linux, CEF's external message pump runs only Chromium's task queue, so [crates/browser-cef](../crates/browser-cef/src/glib.rs) also dispatches the default GLib main context, where Chromium watches its display connection; a waker thread unparks the host when those descriptors become ready. GLib's "work due now" timeout is trusted only when a source is actually ready: Chromium's GLib work source reports it permanently under CEF's external pump, which otherwise kept the host loop and waker spinning at full CPU and delivered paints in bursts. Windowless Chromium never holds keyboard focus, and Wayland compositors only share the clipboard with a focused client, so the page's text fields use the host's clipboard: [src/runtime/clipboard.rs](../src/runtime/clipboard.rs) reads and owns the selection through wlr data-control (wlroots compositors, Hyprland, KWin), or else as an X11 `CLIPBOARD` client ([x11.rs](../src/runtime/clipboard/x11.rs)). GNOME's Xwayland bridge mirrors the clipboard without focus checks, so the X11 path covers GNOME and plain X11 sessions; Hyprland's bridge does check focus, which is why data-control comes first. The page inserts pasted text with `insertText`, and a cut deletes its selection only after the host confirms the write. INCR transfers are not implemented; text beyond one X request fails rather than truncating. CEF keeps Chromium's own Wayland/X11 choice; forcing X11 lost the WebGPU adapter on NVIDIA. Windows, or Linux without data-control or an X display, uses Chromium's own clipboard.

## Browser trust boundary

[crates/browser-cef](../crates/browser-cef/) wraps CEF and serves `vosulay://manager/index.html`. Only that exact main-frame URL receives the native manager bridge. The page captures the bridge and removes its global; counter frames never receive it. Bridge strings are limited to 256 KiB, with a bounded 64-message queue.

`vosulay-overlay://` resources require capability authorization on every request and are limited to 32 MiB. [src/runtime/resources.rs](../src/runtime/resources.rs) and [src/shell_policy.rs](../src/shell_policy.rs) enforce canonical root and symlink confinement. Non-manager frames receive [assets/backend.js](../assets/backend.js), which routes backend fetch/XHR/WebSocket traffic through the loopback broker and supplies `COUNTER_PATH` without replacing an existing string value. It does nothing on other origins. The overlay settings modal is vosulay's own UI; the host reads and writes vosu's settings over HTTP, so no vosu page is framed.

## Frame ownership and scheduling

Production uses CEF's internal damage-driven OSR clock. Automatic FPS follows the selected game display's refresh, divided down to 60-120 fps; a fixed setting or CLI override caps the browser, not game presents. A message-pump wake does not request a repaint. Unchanged pages need not publish continuous frames. Game presentation and browser scheduling are independent clocks; the consumer uses the newest complete frame rather than queueing every produced update.

### Linux

CEF retains Chromium GPU acceleration, but publishes final CPU BGRA pixels into a sealed persistent three-slot memfd ring in [crates/frame-transport](../crates/frame-transport/). Slots contain complete snapshots; damage accumulates per slot, including dropped paints. Identical successful publications can be skipped. Owned read leases keep the mapping alive through uploads and disconnects. The header also keeps a 64-entry seqlock history of each publication's changed rectangle, so a consumer can combine the damage since the frame its texture holds.

The injected [compositor](../crates/overlay-linux/) acquires the newest complete frame at present and uploads only the regions changed since the frame its texture holds. Damage stays as separate rectangles end to end: CEF's dirty rectangles (up to eight; beyond that the pair whose bounding box adds least area merges) travel through the frame ring's history as one sequence number per rectangle, so counters far apart no longer upload their bounding box. A compositor or host from before this unions those entries into the bounding box, so a mismatched pair stays correct (GL: the changed rows are copied into a pixel unpack buffer and uploaded from it, since uploads from process memory serialized each large copy into the game's frame on NVIDIA; Vulkan: staging buffer and buffer-to-image region); a new ring generation, lost texture, or a gap beyond the history window uploads the full frame. The transfer is bound by the GPU's PCIe link (about 2 ms for a full 1440p frame on a Gen4 x4 link). For EGL games ([`gl/upload.rs`](../crates/overlay-linux/src/gl/upload.rs)) a thread with its own context, sharing objects with the game's, uploads into a staging texture and fences it; the game's swap polls the fence without waiting and copies only the changed rectangle into the displayed texture on the GPU (`glCopyImageSubData`), so the game's frames no longer wait behind the transfer. GLX games (a share context would need the game's Xlib `Display` from another thread), contexts without `glCopyImageSubData`, and `VOSULAY_OVERLAY_ASYNC_UPLOAD=0` keep the in-stream upload. `tools/overlay-upload-bench.py` measures both (see [testing](testing.md)). It hooks Vulkan presentation and EGL/GLX/SDL swaps. The leading [lookup helper](../crates/overlay-lookup/) handles explicit library symbol lookups. Launch-time preload/layer attachment is available when same-user live injection is restricted. X11 focus comes from a separate XCB connection on its own thread; a surface counts as focused when X focus is on its window or on any window carrying this process's `_NET_WM_PID`, because Wine renders into an offscreen client window while its visible top-level holds focus.

Authenticated nonblocking `SOCK_SEQPACKET` control hands off the memfd and carries surface, focus, and input messages; pixels and per-present acknowledgements do not travel through the socket. The default abstract endpoint is `@vosulay-<euid>` (overridable by `VOSULAY_ENDPOINT`); peers are checked with `SO_PEERCRED`. Ship host and injected libraries together because the protocol is versioned. Linux does not require DMA-BUF sharing or a patched CEF frame-lease API.

### Windows

[crates/platform-windows](../crates/platform-windows/) attaches using pinned asdf-overlay 2.1.0. CEF's accelerated callback texture is borrowed: the host copies it into reusable application-owned D3D11 GPU textures before returning. The normal path has no CPU readback. Consumer leases and completion handling protect reuse; the callback handle must never be retained as application-owned storage.

## Focus, input, tray, and launch

Selection follows supported game focus, not process enumeration order. Unrelated foreground windows retain the selected game; another supported game takes over only after gaining focus. Editing normally ends on focus loss; the persisted setting can retain the editor and resume capture when the game regains focus.

Native edit/visibility hotkeys and the tray feed the same host state. Linux detects native chords through the injected in-game input path: SDL event hooks, or for Xlib games without SDL (Wine's X11 driver) hooks on `XCheckIfEvent`/`XNextEvent` that route key, pointer, and XInput2 events from the game's own queue. While editing, the cursors the game set through `XDefineCursor` are replaced by a standard pointer from the compositor's own XCB connection and restored afterwards; Wine sets cursors on a connection that never polls events, so the game's Display cannot be used. Windows registers global hotkeys, with edit toggle gated to game focus. Tray actions are **Edit mode**, **Hide vosulay**, and **Exit vosulay**. Startup registers a launcher entry and rejects a second concurrent instance. See [operations](operations.md).

That path sits behind any input method. While the game has text input enabled (osu! with a focused text box, or SDL games during editing, when the overlay starts SDL text input for the editor), Wayland compositors route keys to the input method first, and a chord it consumes never reaches the process: Mozc commits `Ctrl+Shift+Space` as a full-width space. The overlay does not hide `zwp_text_input_manager_v3` to work around this: SDL would fall back to its D-Bus IME, which picks IBus unless `SDL_IM_MODULE`/`XMODIFIERS` select fcitx and loses inline preedit and candidate placement under Wayland. `vosulay --toggle-edit` from a compositor keybind is the supported way to toggle editing independently of input methods.

Input capture/release and browser-heartbeat failures have a five-second safety bound. Linux releases capture; Windows may terminate the host so the injected connection closes. Graphics and input/control locks remain separate so a stalled upload must not block input release.

## Remote service

[src/remote.rs](../src/remote.rs) always provides a separate loopback backend broker; failure to bind the configurable remote listener is non-fatal. Remote pages use the same profiles and broker semantics and are always view-only; with local editing enabled, loopback clients that open `manage.localhost:<port>/overlay` get the full manager, which sends its messages through the same editor worker as the in-game editor. See [integrations](integrations.md) for authorization and precise-subscription behavior.
