---
type: Runbook
title: Build, launch, and operate vosulay
description: Practical runbook for building vosulay's Rust and Qt stack, satisfying native dependencies, using command-line and IPC controls, selecting displays, and understanding packaging and automation files.
resource: build.rs
tags: [operations, build, runbook, qt, wayland, x11]
---

# Build, launch, and operate vosulay

## Native build prerequisites

[`build.rs`](../../build.rs) requires Qt 6.8 or newer with `Qt6Core`, `Qt6Widgets`, `Qt6WebEngineWidgets`, `Qt6WebChannel`, `Qt6Network`, and `Qt6DBus`, plus `moc`, CMake, and a LayerShellQt development package exposing `LayerShellQt::Interface`. It uses pkg-config for Qt and a generated CMake project for LayerShellQt, then compiles the bridge and shell as C++17.

For non-system installations, the build script recognizes CMake/Qt/LayerShellQt environment variables such as `Qt6_DIR`, `Qt6_ROOT`, `LayerShellQt_DIR`, `LayerShellQt_ROOT`, `CMAKE_PREFIX_PATH`, `CMAKE_TOOLCHAIN_FILE`, and `QT_MOC`. [`Cargo.toml`](../../Cargo.toml) records the Rust, HTTP, parser, serialization, CXX, and build dependencies; [`rust-toolchain.toml`](../../rust-toolchain.toml) pins the Rust toolchain.

If Cargo fails before Rust tests run, inspect the native dependency diagnostic from `build.rs` first. The generated MOC and CMake probe outputs live under Cargo `OUT_DIR`, not in the repository.

The Dawn-enabled WebEngine selected at build time is also the runtime
dependency. `build.rs` records its library directory as the executable
RUNPATH and embeds its subprocess, resource, and locale paths as defaults.
Consequently, binaries produced by `cargo install --path .` do not depend on
the build shell retaining the helper's environment exports. Non-empty runtime
`QTWEBENGINE*` values remain explicit overrides.

## Launch modes and controls

The Qt command-line parser in [`src/qt/qt_shell.cpp`](../../src/qt/qt_shell.cpp) supports:

- `--vosu-url`/`-t`: vosu base URL, default `http://127.0.0.1:24050`.
- `--monitor`/`-m`: monitor index or exact screen name when not attached.
- `--fps-limit`/`-f`: `auto`, `0`, or a positive integer.
- `--attach`/`-a`: accept geometry over IPC.
- `--edit`/`-e`: toggle editing in an already-running instance, then exit; normal startup creates the instance in display mode.
- `--ipc-toggle-overlay`/`-i` and `--ipc-quit`/`-q`: control a running instance.
- `--ipc-geometry x,y,w,h`: set geometry on an attached instance.

A second normal instance exits with an error if the user-scoped IPC endpoint is already owned. IPC behavior and security limits are detailed in the [integration boundary](../integrations/vosu-and-overlays.md).

## Runtime behavior

The shell configures WebEngine frame-rate behavior before creating `QApplication`. `auto`/`0` follows the detected screen refresh rate; explicit limits use the frontend limiter, and high/native rates may add `--disable-frame-rate-limit` to `QTWEBENGINE_CHROMIUM_FLAGS`. Tray actions support editing, show/hide, reload, and quit.

On Wayland, LayerShellQt is used for overlay-layer placement; on X11, input transparency and masks implement click-through display mode. When debugging “invisible” or non-interactive overlays, check edit mode, selected monitor/attachment, visibility, and platform-specific shell behavior before changing frontend code.

## Remote profile server operations

Both platform hosts serve credential-protected, view-only profile pages on `0.0.0.0:5050`; see the [remote integration boundary](../integrations/vosu-and-overlays.md) for routes and authorization. Copy the complete **Remote access URL** from manager settings rather than sharing `/overlay` directly. Configure **Allowed client IPs** before exposing the service beyond loopback, and remember that rendered counter frames may need the same client network allowed by vosu. Regenerating or replacing the credential in `config.json` revokes prior URLs. A bind failure is non-fatal to application startup: the host logs that remote hosting is unavailable and continues without the server.

## Packaging and documentation maintenance

[`extra/com.vosu.vosulay.desktop`](../../extra/com.vosu.vosulay.desktop) supplies a desktop entry, while [`extra/attach.sh`](../../extra/attach.sh) is a small geometry-control helper. Windows packaging is defined by `windows/package.json`; the setup and portable distributions use different configuration locations described in the [configuration model](../domain/configuration.md). No installer or release runbook is present yet.

OpenWiki maintenance is intentionally manual and local. From the repository root, run `openwiki code --update --print` (or `openwiki --update`) after reviewing source changes; inspect the generated diff and commit only the affected `openwiki/` and OpenWiki-managed guidance files. Do not create or restore `.github/workflows/openwiki-update.yml`, or add scheduled/manually dispatched GitHub Actions automation for OpenWiki.
