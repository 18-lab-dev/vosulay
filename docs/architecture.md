# Runtime architecture

## Host and browser

One Rust host uses the pinned precompiled CEF runtime on Linux and Windows. CEF has its own renderer/GPU subprocesses; it is not a separate product host. The shared [manager assets](../assets/) are embedded by [src/ui.rs](../src/ui.rs) and render the entire profile in one transparent off-screen browser.

[src/runtime/host.rs](../src/runtime/host.rs) owns attachment, browser lifetime, input capture, browser heartbeats, visibility, and frame-rate policy. [src/runtime/editor.rs](../src/runtime/editor.rs) moves blocking catalog fetches, revision-checked configuration saves, file dialogs, and imports off the event loop. CEF initializes lazily when a game is selected. Detachment flushes edits and closes the view; the initialized runtime remains until exit because CEF cannot safely be reinitialized in one process.

Linux's [entrypoint](../src/main.rs) re-executes Cargo-built binaries beside the retained CEF assets. Windows enters through the pinned CEF sandbox bootstrap and loads the application client DLL.

On Linux, CEF's external message pump runs only Chromium's task queue, so [crates/browser-cef](../crates/browser-cef/src/glib.rs) also dispatches the default GLib main context, where Chromium watches its display connection; a waker thread unparks the host when those descriptors become ready. When an X display exists, CEF uses X11 rather than Wayland: windowless Chromium never holds Wayland keyboard focus, so a Wayland compositor would never give it the clipboard. Xwayland bridges X11 selections only while an X11 window is focused, so with a native Wayland game focused the page's text fields use the host's clipboard instead: [src/runtime/clipboard.rs](../src/runtime/clipboard.rs) reads and owns the selection through wlr data-control, and the page inserts pasted text with `insertText`. Without data-control (X11 sessions, compositors lacking it) and on Windows, Chromium's own clipboard is used.

## Browser trust boundary

[crates/browser-cef](../crates/browser-cef/) wraps CEF and serves `vosulay://manager/index.html`. Only that exact main-frame URL receives the native manager bridge. The page captures the bridge and removes its global; counter frames never receive it. Bridge strings are limited to 256 KiB, with a bounded 64-message queue.

`vosulay-overlay://` resources require capability authorization on every request and are limited to 32 MiB. [src/runtime/resources.rs](../src/runtime/resources.rs) and [src/shell_policy.rs](../src/shell_policy.rs) enforce canonical root and symlink confinement. Non-manager frames receive [assets/backend.js](../assets/backend.js), which routes backend fetch/XHR/WebSocket traffic through the loopback broker and supplies `COUNTER_PATH` without replacing an existing string value.

## Frame ownership and scheduling

Production uses CEF's internal damage-driven OSR clock. Automatic FPS follows the selected game display; a fixed setting or CLI override caps the browser, not game presents. A message-pump wake does not request a repaint. Unchanged pages need not publish continuous frames. Game presentation and browser scheduling are independent clocks; the consumer uses the newest complete frame rather than queueing every produced update.

### Linux

CEF retains Chromium GPU acceleration, but publishes final CPU BGRA pixels into a sealed persistent three-slot memfd ring in [crates/frame-transport](../crates/frame-transport/). Slots contain complete snapshots; damage accumulates per slot, including dropped paints. Identical successful publications can be skipped. Owned read leases keep the mapping alive through uploads and disconnects.

The injected [compositor](../crates/overlay-linux/) acquires the newest complete frame at present and uploads it to a reusable GPU texture. It hooks Vulkan presentation and EGL/GLX/SDL swaps. The leading [lookup helper](../crates/overlay-lookup/) handles explicit library symbol lookups. Launch-time preload/layer attachment is available when same-user live injection is restricted. X11 focus comes from a separate XCB connection on its own thread; a surface counts as focused when X focus is on its window or on any window carrying this process's `_NET_WM_PID`, because Wine renders into an offscreen client window while its visible top-level holds focus.

Authenticated nonblocking `SOCK_SEQPACKET` control hands off the memfd and carries surface, focus, and input messages; pixels and per-present acknowledgements do not travel through the socket. The default abstract endpoint is `@vosulay-<euid>` (overridable by `VOSULAY_ENDPOINT`); peers are checked with `SO_PEERCRED`. Ship host and injected libraries together because the protocol is versioned. Linux does not require DMA-BUF sharing or a patched CEF frame-lease API.

### Windows

[crates/platform-windows](../crates/platform-windows/) attaches using pinned asdf-overlay 2.1.0. CEF's accelerated callback texture is borrowed: the host copies it into reusable application-owned D3D11 GPU textures before returning. The normal path has no CPU readback. Consumer leases and completion handling protect reuse; the callback handle must never be retained as application-owned storage.

## Focus, input, tray, and launch

Selection follows supported game focus, not process enumeration order. Unrelated foreground windows retain the selected game; another supported game takes over only after gaining focus. Editing normally ends on focus loss; the persisted setting can retain the editor and resume capture when the game regains focus.

Native edit/visibility hotkeys and the tray feed the same host state. Linux detects native chords through the injected in-game input path: SDL event hooks, or for Xlib games without SDL (Wine's X11 driver) hooks on `XCheckIfEvent`/`XNextEvent` that route key, pointer, and XInput2 events from the game's own queue. While editing, the cursors the game set through `XDefineCursor` are replaced by a standard pointer from the compositor's own XCB connection and restored afterwards; Wine sets cursors on a connection that never polls events, so the game's Display cannot be used. Windows registers global hotkeys, with edit toggle gated to game focus. Tray actions are **Edit mode**, **Hide vosulay**, and **Exit vosulay**. Startup registers a launcher entry and rejects a second concurrent instance. See [operations](operations.md).

Input capture/release and browser-heartbeat failures have a five-second safety bound. Linux releases capture; Windows may terminate the host so the injected connection closes. Graphics and input/control locks remain separate so a stalled upload must not block input release.

## Remote service

[src/remote.rs](../src/remote.rs) always provides a separate loopback backend broker; failure to bind the configurable remote listener is non-fatal. View-only remote pages use the same profiles and broker semantics. See [integrations](integrations.md) for authorization and precise-subscription behavior.
