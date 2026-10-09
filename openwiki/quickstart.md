---
type: Reference
title: vosulay code wiki quickstart
description: Entry point for the vosulay Rust/CEF in-game overlay manager. Summarizes the workspace architecture, build and verification commands, configuration and overlay workflows, integrations, tests, and source navigation.
tags: [vosulay, quickstart, rust, cef, overlays]
---

# vosulay code wiki quickstart

## What this repository is

`vosulay` is a cross-platform in-game overlay manager for `vosu` or tosu-compatible backends. A single native Rust host renders the overlay profile in one pinned CEF off-screen browser. On Linux an injected compositor (`libvosulay_overlay.so` plus the `libvosulay_lookup.so` helper) draws the frames inside Vulkan, EGL/GLX, and SDL games; on Windows the host copies CEF's D3D11 textures and composes through asdf-overlay. Both platforms share versioned profiles and placement state, and the host serves bounded view-only remote profile pages.

Start with the [README](../README.md) for current runtime, build, and packaging behavior, then use the [source map](source-map.md) when changing code.

## Build and verify at a glance

The project is a Cargo workspace (Rust 2024, Rust 1.96.1). Cargo acquires and verifies the pinned precompiled CEF distribution automatically on the first build; no Qt, Electron, Python, or Chromium source build is involved. Verify with:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

`--workspace` is required because `default-members` contains only the root package. The [testing guidance](testing/guidance.md) lists native prerequisites, the C-backed loader fixtures, CI triggers, coverage, and what remains manual.

## Core concepts and workflows

- [Architecture overview](architecture/overview.md) explains the single Rust/CEF host, manager page and bridge, resource schemes, backend broker, editor worker, and the Linux and Windows frame paths.
- [Configuration and overlay model](domain/configuration.md) documents profiles, placements, settings, normalization, file location and portable mode, tosu import, and atomic JSON persistence.
- [vosu and overlay integrations](integrations/vosu-and-overlays.md) documents the counter catalog contract, backend broker, local HTML overlay capabilities, remote profile pages, the Linux control channel, and their security boundaries.
- [Build and run operations](operations/build-and-run.md) documents build prerequisites and CEF caches, command-line options, runtime/config selection, Linux attachment and troubleshooting, the remote server, packaging, and manual OpenWiki maintenance.
- [Testing guidance](testing/guidance.md) gives exact verification commands, prerequisites, CI policy, coverage, and areas needing manual verification.
- [Source map](source-map.md) maps Linux and Windows responsibilities to files and gives change-oriented starting points.

## Change navigation

For a manager UI or bridge change, begin with [`src/runtime/editor.rs`](../src/runtime/editor.rs), [`crates/browser-cef/src/bridge.rs`](../crates/browser-cef/src/bridge.rs), [`src/ui.rs`](../src/ui.rs), and [`assets/manager.js`](../assets/manager.js). For persistence changes, read [`src/config.rs`](../src/config.rs) and its tests before touching the frontend normalization. For vosu catalog behavior, read [`src/api.rs`](../src/api.rs) and its fixtures under `tests/fixtures/backend`.

Preserve the documented constraints: custom overlay paths must remain within their authorized roots; IPC is local, bounded, and user-scoped; configuration writes are atomic; and embedded frontend content must remain safely encoded.

## Repository history

Recent changes extend the product beyond the original Linux shell: the repository now includes a Windows Electron host, first-party Vosulay branding, WebGPU-enabled native build support, and remote profile pages served on the local network. The [operations runbook](operations/build-and-run.md) records the supported manual maintenance path for this wiki. OpenWiki updates must be run manually and locally; no scheduled or manually dispatched GitHub Actions workflow should be restored.

## Backlog

- **CEF, in-game composition, and frontend integration tests — `crates/cef-smoke`, `crates/overlay-linux`, `assets/manager.js`:** no automated test initializes CEF, drives the manager page, or composes into a real game; these need the manual checks listed in the [testing guidance](testing/guidance.md#not-covered-by-automated-checks).
- **Release and distribution runbook — `extra/com.vosu.vosulay.desktop`, `LICENSE`:** packaging metadata is present, but no release process or installation documentation exists in the repository.
