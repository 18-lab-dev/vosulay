---
name: vosulay-windows-live-testing
description: Maintainer environment (optional) only when the private windows-vm command is available; deploy and verify in an already-running Windows guest without changing guest lifecycle.
---

# Windows live testing — maintainer environment (optional)

## Availability gate — first step

Run `command -v windows-vm` before following this skill. If it fails, do not try to install private tooling or recreate the maintainer workstation. Fall back to the `vosulay-local-ci`/`vosulay-cross-platform-dev` skills and `CONTRIBUTING.md`: Linux can run optional Windows cross-Clippy; request or run manual GitHub Actions **Packages** (`.github/workflows/release.yml`) in a fork with `publish=false` for `vosulay-linux-x86_64` and `vosulay-windows-x86_64` artifacts (Linux packages and Windows ZIP/installer); use an authorized native Windows machine for manual game testing. With only a Linux machine, report native Windows runtime checks as unexercised. No VM is required to contribute.

Everything below applies only to the authorized maintainer environment with that command and guest GUI tooling available.

## Safety and guest selection

Read the current `windows-vm-computer-use` skill before guest GUI work. Use active-guest Cua for verified framebuffer/UI actions and the canonical `windows-vm` host command for PowerShell and file transfer. Never use the Linux desktop as a guest-input proxy.

```sh
windows-vm status --json
windows-vm exec 'Get-CimInstance Win32_Process | Select-Object ProcessId,Name,SessionId'
windows-vm put LOCAL_ARCHIVE 'C:\TASK_WORKSPACE\package.zip'
windows-vm get 'C:\TASK_WORKSPACE\receipt.json' LOCAL_RECEIPT
```

Replace uppercase operands with explicit approved task-owned paths; the example drive is not a required workspace layout. Require a verified already-running Windows guest and same-account interactive session. If the guest is absent or another VM owns the GPU, use the contributor fallback above; never start, stop, switch, restart, shut down, hotplug, or modify VM topology without explicit user authorization. Never open VM disk/firmware files with a second emulator.

## Build and deploy

Use pinned Rust/MSVC and verified CEF/asdf assets. On native Windows:

```powershell
cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo test --workspace --all-features --locked --target x86_64-pc-windows-msvc
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
```

Record the portable ZIP hash. Extract the complete package: bootstrap `vosulay.exe`, client `vosulay.dll`, CEF runtime/resources/locales, and both asdf-overlay DLLs. Use only an approved task workspace; do not overwrite user installations. Stop task-owned game processes before replacing DLLs, since injected files can remain locked.

For packaging changes, follow `docs/operations.md`: build Linux packages in the Ubuntu 22.04 container and install-test in disposable distro containers (version, libcef dependencies, root-owned `4755` sandbox, sustained startup and no duplicate user launcher). Compile installer changes with Inno Setup 6 and, when authorized, test install/upgrade/uninstall in a task-owned Windows workspace, including tray exit prompts, shortcut options and retained user configuration. Do not overwrite a user installation merely to verify packaging.

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

Do not add persistent hidden remote-debugging instrumentation. The user-facing **Enable DevTools debugging (CDP)** setting is an explicitly approved exception: off by default, loopback only, with a visible indicator, and enabled only for authorized diagnostics. Disable it and restart after testing. Never infer GPU/capture correctness from source compilation. Screenshot evidence must come from the actual guest; producer counters and DXGI Present timings are not delivered frames or display/input latency.

## Cleanup and report

Restore pre-test configuration semantics. Exit vosulay through the tray, verify its process exit and responsive game input, and remove only task-owned tasks/artifacts after their processes exit. Do not require injected asdf-overlay to unload: it can remain resident for reconnection. Do not stop or restore VM services as cleanup.

Report package hash, guest/backend versions, exact checks, screenshot paths, process/module state, configuration restoration, native verification results, and any input/GPU/capture/performance boundary not exercised. Do not claim an installer was tested from portable-package evidence.
