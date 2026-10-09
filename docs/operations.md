# Build, launch, and package

## Prerequisites and runtime

Use the pinned [Rust toolchain](../rust-toolchain.toml), a C compiler, and `pkg-config`. Linux needs Wayland/EGL development files and CEF's native runtime libraries; the exact Ubuntu packages and SDL fixture prerequisites are in [Linux CI](../.github/workflows/linux-ci.yml). Windows needs the native MSVC toolchain. See [testing](testing.md) for loader-path setup.

Cargo automatically acquires the pinned precompiled CEF distribution when `CEF_PATH` is unset. [crates/cef-tools](../crates/cef-tools/) verifies archive size, SHA1/SHA256, and runtime files using [pins.json](../tools/cef/pins.json). An explicit `CEF_PATH` must point to a verified distribution. Only the initial acquisition needs network access; application startup downloads nothing.

Linux caches under `$XDG_CACHE_HOME/vosulay` (`~/.cache/vosulay` by default): `cef/` stores the SDK, `runtime/` retains executable-adjacent CEF assets, and `overlay/` retains native compositor/lookup libraries. Cargo-installed binaries rely on runtime/overlay caches; reinstall if they are removed. Portable packages use adjacent assets instead.

## Installation and packaging

```sh
cargo install --path . --locked
vosulay
```

Use `--force` to reinstall. To distribute Linux builds, assemble the complete runtime rather than copying the installed executable alone:

```sh
cargo build --release --locked
cargo build --release --locked -p vosulay-overlay -p vosulay-lookup
CEF_PATH="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
cargo run --quiet --locked -p vosulay-cef-tools -- package --platform linux-x86_64 \
  --cef-path "$CEF_PATH" --output dist/linux \
  --exe target/release/vosulay --overlay target/release/libvosulay_overlay.so \
  --overlay target/release/libvosulay_lookup.so \
  --overlay crates/overlay-linux/VkLayer_vosulay_overlay.json
```

On native Windows:

```powershell
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
```

This acquires verified CEF/asdf assets before building the client DLL and assembling `dist/vosulay-<version>-windows-x86_64.zip`. Keep the bootstrap `vosulay.exe`, `vosulay.dll`, CEF files/locales, and both injection DLLs together. [CMakeLists.txt](../CMakeLists.txt) installs Linux desktop/icon metadata only; distro packaging must install the application and complete runtime separately. Its desktop ID and icon name are `com.vosu.vosulay`.

## Launchers and single instance

Startup registers vosulay in app launchers. Linux writes `$XDG_DATA_HOME/applications/com.vosu.vosulay.desktop` (default `~/.local/share/applications/com.vosu.vosulay.desktop`) with an absolute Exec and installs the matching `icons/hicolor/scalable/apps/com.vosu.vosulay.svg`, unless the desktop entry exists in `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`). Windows creates `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Vosulay.lnk`. Registration failures are logged without stopping the host. Registration is separate from refreshing osu shortcuts for injection. A second launch while vosulay runs prints a message and exits rather than creating another instance.

Both platforms expose the tray menu **Edit mode** (checkable), **Hide vosulay** (checkable), and **Exit vosulay**. Linux uses a StatusNotifierItem; a desktop needs compatible tray support to display it. Hiding is not quitting. Configure native edit/visibility chords in Settings → Shortcuts; edit defaults to Ctrl+Shift+Space and visibility starts unbound.

## Command line and storage

[src/runtime.rs](../src/runtime.rs) accepts `--vosu-url`/`-t`, `--fps-limit`/`-f` (`auto`, `0`, or 1–1,000,000), repeatable nonzero `--pid`, repeatable executable-basename `--process-name`, `--config`, and `--cef-dir`. `--help`/`-h` and `--version`/`-v` exit without starting the host. `--benchmark-uncapped` is testing-only and overrides pacing without changing persisted FPS settings. Unknown options are rejected.

On Linux `--cef-dir` also selects adjacent compositor/lookup libraries. Windows uses the executable's runtime directory. A `portable` marker or existing adjacent `data/config.json` selects portable storage; `--config` overrides it. See [configuration](configuration.md).

## Attachment and troubleshooting

Linux refreshes recognized osu shortcuts with absolute injection paths, preserving foreign preloads, arguments, and OBS flags. System shortcuts receive user overrides; changes affect the next game launch. It also attempts live attachment to same-user native games. Where policy prevents injection, restart through the refreshed shortcut or use the manual preload/Vulkan commands in the [README](../README.md).

For a missing overlay, check game focus, backend reachability, loaded compositor/runtime files, hidden state, enabled placement, and viewport position. Selection follows focus, not process order. Do not change ptrace policy or request elevation just to bypass attachment restrictions.

Input failure releases capture within the five-second safety bound; Windows may exit. Restart the host if needed. If a remote bind fails, the local backend broker remains available. Listener/allowlist changes and remote security are described in [integrations](integrations.md).
