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

## Packaging and automation

[`extra/com.vosu.vosulay.desktop`](../../extra/com.vosu.vosulay.desktop) supplies a desktop entry, while [`extra/attach.sh`](../../extra/attach.sh) is a small geometry-control helper. No installer or release runbook is present yet.

The scheduled [OpenWiki workflow](../../.github/workflows/openwiki-update.yml) runs daily at 08:00 UTC and on demand, installs OpenWiki, updates repository docs, and opens a pull request. It is documentation automation, not an application build pipeline.
