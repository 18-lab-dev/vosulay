---
type: Reference
title: vosulay code wiki quickstart
description: Entry point for the vosulay native Linux in-game overlay manager. Summarizes the Rust and Qt architecture, configuration and overlay workflows, build prerequisites, integrations, tests, and source navigation.
tags: [vosulay, quickstart, rust, qt, overlays]
---

# vosulay code wiki quickstart

## What this repository is

`vosulay` is a cross-platform overlay manager for `vosu`: Linux uses a Rust process with a Qt 6/C++ shell, while Windows uses an Electron host that injects overlays into the game client. Both hosts share versioned profiles and placement state. The Linux process also starts a bounded HTTP server for view-only remote profile pages.

Use this OpenWiki as the engineering entry point: start with the [architecture overview](architecture/overview.md) for runtime boundaries, then use the [source map](source-map.md) when changing code.

## Build and run at a glance

The project is Cargo-based (`Cargo.toml`, Rust 2024, Rust 1.96.1). A native build also requires Qt 6.8+ development modules—including `Qt6DBus`—plus `moc`, CMake, and LayerShellQt; [`build.rs`](../build.rs) probes these dependencies and compiles the C++ shell. See [build and run operations](operations/build-and-run.md) for dependency discovery, CLI options, platform behavior, and failure modes.

Typical checks are:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

`cargo test` still invokes the native build script, so the Qt/LayerShellQt toolchain must be available even for Rust unit tests. The [testing guidance](testing/guidance.md) identifies what is actually covered.

## Core concepts and workflows

- [Architecture overview](architecture/overview.md) explains the Rust, CXX, Qt, WebEngine, and embedded frontend layers.
- [Configuration and overlay model](domain/configuration.md) documents versioned profiles, placement geometry, normalization, and atomic JSON persistence.
- [vosu and overlay integrations](integrations/vosu-and-overlays.md) documents the counter catalog HTML contract, URL validation, local HTML overlays, remote profile pages, IPC, and security boundaries.
- [Build and run operations](operations/build-and-run.md) documents Linux/Windows prerequisites, startup flags, runtime controls, packaging helpers, and manual OpenWiki maintenance.
- [Testing guidance](testing/guidance.md) maps Rust and Windows tests to behavior and highlights areas needing manual or future coverage.
- [Source map](source-map.md) maps Linux and Windows responsibilities to files and gives change-oriented starting points.

## Change navigation

For a UI or bridge change, begin with [`src/qt/qt_shell.cpp`](../src/qt/qt_shell.cpp), [`src/ffi.rs`](../src/ffi.rs), [`src/ui.rs`](../src/ui.rs), and [`assets/manager.js`](../assets/manager.js). For persistence changes, read [`src/config.rs`](../src/config.rs) and its tests before touching the frontend normalization. For vosu catalog behavior, read [`src/api.rs`](../src/api.rs) and the URL helpers in `src/ffi.rs` together.

Preserve the documented constraints: custom overlay paths must remain within their authorized roots; IPC is local, bounded, and user-scoped; configuration writes are atomic; and embedded frontend content must remain safely encoded.

## Repository history

Recent changes extend the product beyond the original Linux shell: the repository now includes a Windows Electron host, first-party Vosulay branding, WebGPU-enabled native build support, and remote profile pages served on the local network. The [operations runbook](operations/build-and-run.md) records the supported manual maintenance path for this wiki. OpenWiki updates must be run manually and locally; no scheduled or manually dispatched GitHub Actions workflow should be restored.

## Backlog

- **Qt/C++ and frontend integration tests — `src/qt/qt_shell.cpp`, `assets/manager.js`:** no dedicated C++ or browser test suite was found; behavior beyond Rust unit tests needs manual or future automated coverage.
- **Release and distribution runbook — `extra/com.vosu.vosulay.desktop`, `LICENSE`:** packaging metadata is present, but no release process or installation documentation exists in the repository.
