---
name: vosulay-windows-live-testing
description: "Build, deploy, drive, inspect, and clean up vosulay in the project's Windows VM, including a safe no-VFIO fallback"
---

# vosulay Windows live testing

Use this procedure for end-to-end validation of the Windows Electron overlay host against a real Windows guest and osu!lazer.

## Safety invariants

- Follow the umbrella and `vosulay/` repository guidance.
- Never open the Windows qcow2 or OVMF variable store from two QEMU processes.
- Treat unexpected VM switching as concurrent user activity. Identify the controlling session before changing services.
- If the Linux gaming VM must retain the passthrough GPU, boot Windows without the VFIO devices but keep its existing virtio-vga, guest agent, user networking, and VNC devices.
- Stop osu before replacing a package: the injected `asdf_overlay-x64.dll` keeps the package file locked until the target process exits.
- Temporary CDP enablement must never ship in source or the final package.

## Direct Windows bridge

Use the local Windows bridge for guest computer use. Never operate the Linux desktop as a proxy.

1. Call `windows_health` before input. Require `ok: true`, `qmp.ok: true`, and `frame.available: true`; record the reported frame source, dimensions, and input route.
2. Call `windows_observe` and use its returned framebuffer dimensions and source as authoritative. Coordinates are guest-framebuffer coordinates, never Linux desktop or compositor coordinates.
3. Send input only through `windows_move`, `windows_click`, `windows_double_click`, `windows_drag`, `windows_scroll`, `windows_key`, `windows_hotkey`, and `windows_type_text`. Observe again after every meaningful action. A successful tool response or an incremented accepted-input counter proves only that QMP accepted the command; it does not prove a guest UI transition.
4. Keep computer use on `/home/user/VMs/vosu-windows/qmp-agent.sock`. Never connect the VM-owned primary QMP socket or inject QMP input directly for computer use. Do not start Moonlight, RDP, VNC, Sunshine, compositor automation, or another display/input runtime.
5. During bridge-driven computer use, preserve the running VM and its device topology. Do not restart or shut down the VM, hotplug devices, or replace its QMP configuration for a test, except through the documented shutdown-and-cleanup sequence below. Restart only the bridge service when repairing bridge transport, then repeat the health preflight.
6. The bridge's QMP input target is the QEMU display console `device=vga0`, `head=0`. `kbd0` and `tablet0` are USB device IDs, not display-console targets. The USB tablet is absolute-coordinate input; send absolute framebuffer coordinates and do not substitute relative homing sweeps.
7. After a QMP transport error such as `BlockingIOError(11)`, a partial frame, or a decode failure, discard the affected connection and re-establish the bridge before sending more input. Never continue on a possibly desynchronized stream.
8. For GUI claims, require visible state change in `windows_observe` and, where available, independent read-only guest process evidence. Do not infer that Run, an application launch, or focus changed from accepted input alone.

When a frame source is `qmp-screendump`, report it as such. Do not call a `1280x800` QMP frame a Looking Glass frame or use it as proof that a `1920x1080` Looking Glass surface is active.

## Build and package

From `vosulay/windows`:

```sh
npm run build
npm run typecheck
npm test
npx electron-builder --win dir
npm run verify:package
```

Archive `dist/win-unpacked` for transfer and record its SHA-256. A Linux host without Wine can validate `win-unpacked`, but cannot claim that the NSIS installer was tested.

## Backend compatibility

Deploy the current Windows `vosu.exe` beside the guest's existing counter directory and start it with that directory as its working directory. Probe:

- `http://127.0.0.1:24050/api/counters/search/`
- HTTP status
- `article.counter-card[data-overlay-id]` count
- presence of `data-counter-name`

A stale vosu can expose older markup. The manager should report the explicit unrecognized-markup/version-mismatch error rather than silently showing an empty catalog.

## Launch in the interactive desktop

SSH sessions are non-interactive. Register temporary scheduled tasks with the logged-in Windows user and `LogonType Interactive` to launch:

1. osu!lazer
2. vosulay
3. synthetic hotkeys or screenshot helpers

Verify:

- osu is responsive in session 1.
- `asdf_overlay-x64.dll` appears in osu's modules.
- `Ctrl+Shift+Space` opens and closes edit mode.

## Deep manager inspection

For a diagnostic build only, append an Electron remote-debugging port before `app.ready` using a temporary environment-controlled source change. Choose a free guest port—Discord may already use 9222. Forward it over SSH and attach Chromium to the `vosulay://manager/index.html` target.

Exercise observable behavior:

- catalog count and parser result
- add/remove installed counters
- child iframe URL, HTML, and rendered text
- selected-counter refresh without manager reload
- geometry, rotation, opacity, title, and save completion
- profile create/select/delete
- invalid and valid custom URL handling
- settings save, restart restoration, and config JSON
- SharedArrayBuffer disabled/enabled behavior across restart
- Done/reopen lifecycle

Use a real guest screenshot for visual proof. A catalog sidebar that scrolls and clips the next row at its viewport edge is normal, not a rendering defect.

## Shutdown and cleanup

1. Restore the guest config's pre-test semantics.
2. Exercise graceful `quit` through the manager bridge in the diagnostic run.
3. Confirm zero vosulay processes, a responsive game, flushed config, and a ready task.
4. Do not require the injected DLL to unload: upstream asdf-overlay keeps its IPC server resident for reconnection and cleans backends when the controller disconnects.
5. Rebuild without CDP instrumentation and repeat the core launch/injection/catalog screenshot smoke.
6. Stop scheduled tasks and the `vosulay`, `vosu-current`, and osu processes; wait for them to exit before deleting package files.
7. Remove temporary guest tasks, binaries, archives, and test directories.
8. Request guest shutdown. If Windows `shutdown.exe` from SSH lacks privilege, send QMP `system_powerdown` and wait until QEMU exits.
9. Restore the previously active VM service and verify no test server, tunnel, streamer, or QEMU process remains.

## Evidence to report

Report exact package hash, guest/backend versions, catalog count, workflows exercised, screenshots, process/module state, config before/after semantics, error-log scan, automated verification, installer limitations, and any physical-input path not proven by synthetic input.
