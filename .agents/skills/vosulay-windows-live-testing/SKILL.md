---
name: vosulay-windows-live-testing
description: Build, deploy, and verify the native Rust/CEF host in the already-running Windows VM without changing VM lifecycle or the host desktop.
---

# Windows live testing

## Safety and guest selection

Read the current `windows-vm-computer-use` skill before guest GUI work. Use active-guest Cua for verified framebuffer/UI actions and the canonical `windows-vm` host command for PowerShell and file transfer. Never use the Linux desktop as a guest-input proxy.

```sh
windows-vm status --json
windows-vm exec 'Get-CimInstance Win32_Process | Select-Object ProcessId,Name,SessionId'
windows-vm put LOCAL_ARCHIVE 'V:\TASK_WORKSPACE\package.zip'
windows-vm get 'V:\TASK_WORKSPACE\receipt.json' LOCAL_RECEIPT
```

Replace uppercase operands with explicit task-owned paths. Require a verified already-running Windows guest and same-account interactive session. If the guest is absent or another VM owns the GPU, report that prerequisite; never start, stop, switch, restart, shut down, hotplug, or modify VM topology without explicit user authorization. Never open VM disk/firmware files with a second emulator.

## Build and deploy

Use pinned Rust/MSVC and verified CEF/asdf assets. On native Windows:

```powershell
cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo test --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
```

Record the portable ZIP hash. Extract the complete package: bootstrap `vosulay.exe`, client `vosulay.dll`, CEF runtime/resources/locales, and both asdf-overlay DLLs. Use only an approved task workspace; do not overwrite user installations. Stop task-owned game processes before replacing DLLs, since injected files can remain locked.

## Interactive launch and evidence

Remote `exec` sessions may be non-interactive. GUI launches must use the verified logged-in session (for example an explicitly task-owned scheduled task with Interactive logon), never Session 0. Use current guest health and observations before input, and fresh screenshot/semantic evidence after meaningful changes. Accepted input or launch transport is not proof of visible behavior.

Use an independent reachable vosu/tosu-compatible backend; probe `http://127.0.0.1:24050/api/counters/search/` and verify supported markup/counter metadata. Do not silently accept an unrecognized catalog as empty.

Exercise:

- focused osu!stable/lazer attachment and loaded asdf-overlay module;
- actual counter pixels, catalog discovery, add/remove, drag/resize/save/reopen, profiles, rotation/opacity;
- Ctrl+Shift+Space default edit toggle, configurable native edit/hide chords, unbound visibility default, interaction and page shortcuts, Escape game-input release;
- tray **Edit mode**, **Hide vosulay**, **Exit vosulay**, and checked state;
- Start Menu shortcut target and second-instance rejection;
- game focus transitions/restart, FPS settings persistence, and heartbeat-failure recovery using only task-owned renderer processes;
- remote profile allowlist behavior when explicitly in scope.

Do not add persistent remote-debugging instrumentation or infer GPU/capture correctness from source compilation. Screenshot evidence must come from the actual guest; producer counters and DXGI Present timings are not delivered frames or display/input latency.

## Cleanup and report

Restore pre-test configuration semantics. Exit vosulay through the tray, verify its process exit and responsive game input, and remove only task-owned tasks/artifacts after their processes exit. Do not require injected asdf-overlay to unload: it can remain resident for reconnection. Do not stop or restore VM services as cleanup.

Report package hash, guest/backend versions, exact checks, screenshot paths, process/module state, configuration restoration, native verification results, and any input/GPU/capture/performance boundary not exercised. Do not claim an installer was tested from portable-package evidence.
