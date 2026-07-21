---
type: Reference
title: vosulay code wiki quickstart
description: Entry point for the vosulay native Linux in-game overlay manager. Summarizes the Rust and Qt architecture, configuration and overlay workflows, build prerequisites, integrations, tests, and source navigation.
tags: [vosulay, quickstart, rust, qt, overlays]
---

# vosulay code wiki quickstart

## What this repository is

`vosulay` is a Rust application with a Qt 6/C++ shell for managing `vosu` in-game overlays. The process starts in [`src/main.rs`](../src/main.rs), crosses a generated CXX boundary, and runs a frameless Qt WebEngine window whose manager HTML/CSS/JavaScript is compiled into the binary. The product’s central state is a set of profiles containing positioned overlay placements.

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
- [vosu and overlay integrations](integrations/vosu-and-overlays.md) documents the counter catalog HTML contract, URL validation, local HTML overlays, IPC, and security boundaries.
- [Build and run operations](operations/build-and-run.md) documents native prerequisites, startup flags, runtime controls, and packaging helpers.
- [Testing guidance](testing/guidance.md) maps unit tests to behavior and highlights areas needing manual or future coverage.
- [Source map](source-map.md) maps major responsibilities to files and gives change-oriented starting points.

## Change navigation

For a UI or bridge change, begin with [`src/qt/qt_shell.cpp`](../src/qt/qt_shell.cpp), [`src/ffi.rs`](../src/ffi.rs), [`src/ui.rs`](../src/ui.rs), and [`assets/manager.js`](../assets/manager.js). For persistence changes, read [`src/config.rs`](../src/config.rs) and its tests before touching the frontend normalization. For vosu catalog behavior, read [`src/api.rs`](../src/api.rs) and the URL helpers in `src/ffi.rs` together.

Preserve the documented constraints: custom overlay paths must remain within their authorized roots; IPC is local, bounded, and user-scoped; configuration writes are atomic; and embedded frontend content must remain safely encoded.

## Repository history

The current history begins with the initial repository commit (`e438c3c`) and project README (`7b2e59a`), then records scheduler tuning (`ddae95f`), save-before-idle persistence (`6f45961`), FFI test formatting (`74d25c7`), and profile-switching fixes (`d476f2f`). More recent commits improve the running-instance edit command (`7515875`), inject the counter path into vosu frames (`435cc1d`), refine overlay editor controls (`7301d19`), and move overlay file selection to the desktop portal (`e8d9040`). The scheduled [OpenWiki workflow](../.github/workflows/openwiki-update.yml) keeps this engineering documentation updated through pull requests.

## Backlog

- **Qt/C++ and frontend integration tests — `src/qt/qt_shell.cpp`, `assets/manager.js`:** no dedicated C++ or browser test suite was found; behavior beyond Rust unit tests needs manual or future automated coverage.
- **Release and distribution runbook — `extra/com.vosu.vosulay.desktop`, `LICENSE`:** packaging metadata is present, but no release process or installation documentation exists in the repository.
