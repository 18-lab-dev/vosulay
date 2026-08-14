---
type: Reference
title: vosulay code wiki quickstart
description: Entry point for vosulay's shared Electron host and Linux Rust in-game overlay backend, with build, integration, testing, and source-navigation links.
tags: [vosulay, quickstart, electron, rust, overlays]
---

# vosulay code wiki quickstart

## What this repository is

`vosulay` is a cross-platform in-game overlay manager for `vosu`. [`desktop/`](../desktop/) is the canonical Electron host on Linux and Windows. It owns configuration, profiles, counter discovery, local overlay authorization, the remote profile server, tray controls, and the hidden offscreen manager renderer. Platform providers attach that renderer to the game: Windows uses asdf-overlay; Linux uses the Rust implementation under [`native/`](../native/).

The Linux native boundary is intentionally narrow. The Node-API host establishes authenticated local sessions and forwards bounded frame/input messages. The GL agent, profiler bootstrap, and Vulkan layer run only the presentation and input work needed inside the target. No browser, configuration parser, HTTP server, or process supervisor is injected into the game.

Start with the [architecture overview](architecture/overview.md) for runtime boundaries, then use the [source map](source-map.md) when changing code.

## Build and run at a glance

The desktop host requires Node.js 22 or newer:

```sh
cd desktop
npm ci
npm run typecheck
npm test
npm run build
```

The Linux native workspace pins Rust 1.96.1 and also builds a napi-rs addon:

```sh
cd native
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
npm ci
npm run build
npm test
```

A Linux AppImage additionally requires the .NET 8 SDK because native staging publishes the self-contained profiler attach tool:

```sh
cd desktop
npm run dist:linux
```

No Qt, CXX, custom WebEngine, Dawn-Qt, CMake, or LayerShellQt installation is required. See [build and run operations](operations/build-and-run.md) for launch modes, packaging, and diagnostics.

## Core concepts and workflows

- [Architecture overview](architecture/overview.md) explains the shared Electron services, provider boundary, authenticated Linux protocol, offscreen texture ownership, compositor paths, and fail-open lifecycle.
- [Configuration and overlay model](domain/configuration.md) documents versioned profiles, placement normalization, hotkeys, remote settings, and atomic TypeScript persistence.
- [vosu and overlay integrations](integrations/vosu-and-overlays.md) documents counter catalog parsing, custom protocols, local-file capabilities, remote pages, and native IPC security.
- [Build and run operations](operations/build-and-run.md) documents Linux/Windows prerequisites, CLI options, native staging, launch modes, and manual OpenWiki maintenance.
- [Testing guidance](testing/guidance.md) maps TypeScript, Rust, Node-addon, package, and live checks to their contracts.
- [Source map](source-map.md) maps runtime responsibilities to the first files to inspect.

## Change navigation

For shared host lifecycle or controls, begin with [`desktop/src/main/index.ts`](../desktop/src/main/index.ts), [`desktop/src/main/provider-factory.ts`](../desktop/src/main/provider-factory.ts), and [`desktop/src/main/overlay-manager.ts`](../desktop/src/main/overlay-manager.ts). For configuration, counters, local overlays, remote routes, or bridge behavior, change the canonical TypeScript service and its test; do not recreate a second Rust domain implementation.

For Linux attachment and Electron texture forwarding, inspect [`desktop/src/main/linux-overlay-provider.ts`](../desktop/src/main/linux-overlay-provider.ts), [`desktop/src/main/linux-overlay-process.ts`](../desktop/src/main/linux-overlay-process.ts), and the matching crates under [`native/crates/`](../native/crates/). Graphics backend work belongs in `overlay-agent-gl` or `overlay-vulkan-layer`; protocol and authentication changes begin in `overlay-protocol` and `overlay-host`.

Preserve these invariants:

- Chromium shared textures have bounded lifetime; every accepted or dropped frame must eventually release its Electron resource.
- Presentation hooks never wait for host IPC or a new overlay frame and never suppress the original game swap/present after failure.
- Host disconnect clears input capture and disables composition.
- Native packets, descriptors, ancillary FDs, peer PID/UID, and session cookies remain bounded and authenticated.
- Local overlay paths remain inside canonical authorized roots; remote local frames remain opaque sandbox origins.
- Configuration writes remain revision-checked and atomic.

## Known boundary

The Linux GL path provides SDL input capture and redirection for in-game editing. The Vulkan/Wine path currently provides presentation but does not implement that SDL input path. Treat Vulkan editing as incomplete until an authenticated Wine-compatible input backend is added and live-tested.

## Documentation maintenance

OpenWiki updates are manual and local. Review source changes, edit only affected pages, and run the local OpenWiki command if regeneration is wanted. Do not create scheduled or manually dispatched OpenWiki GitHub Actions workflows.
