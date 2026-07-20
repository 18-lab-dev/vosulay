---
type: Source Map
title: vosulay source map
description: Change-oriented map from major vosulay responsibilities to Rust, C++, frontend, build, packaging, and automation files, with pointers to the canonical wiki concepts.
tags: [source-map, navigation, maintenance, vosulay]
---

# vosulay source map

Use the [architecture overview](architecture/overview.md) for relationships and this page for the first file to open when making a change.

| Area | Start here | Responsibility |
|---|---|---|
| Process entrypoint | [`src/main.rs`](../src/main.rs) | Serializes arguments and enters the Qt application through FFI. |
| Rust/C++ boundary | [`src/ffi.rs`](../src/ffi.rs), [`src/qt/bridge.h`](../src/qt/bridge.h) | CXX declarations, Qt-facing Rust operations, URL validation, JSON envelopes. |
| Native shell | [`src/qt/qt_shell.cpp`](../src/qt/qt_shell.cpp) | Qt window, WebEngine, tray, IPC, workers, command-line parsing, platform behavior, local overlay scheme. |
| Configuration | [`src/config.rs`](../src/config.rs) | Version 1 model, normalization, config path, atomic load/save. See [configuration](domain/configuration.md). |
| vosu catalog | [`src/api.rs`](../src/api.rs) | HTTP fetch and HTML parsing for installed counters. See [integrations](integrations/vosu-and-overlays.md). |
| Embedded document | [`src/ui.rs`](../src/ui.rs) | Compile-time asset inclusion and safe manager bootstrap. |
| Frontend state | [`assets/manager.js`](../assets/manager.js) | Profiles, overlay rendering/editing, bridge messages, debounced saves. |
| Frontend structure/style | [`assets/manager.html`](../assets/manager.html), [`assets/manager.css`](../assets/manager.css) | Manager controls and visual presentation. |
| Native build | [`build.rs`](../build.rs), [`Cargo.toml`](../Cargo.toml) | Qt/pkg-config/MOC/CMake discovery and Rust/CXX compilation. See [operations](operations/build-and-run.md). |
| User integration | [`extra/attach.sh`](../extra/attach.sh), [`extra/com.vosu.vosulay.desktop`](../extra/com.vosu.vosulay.desktop) | Geometry IPC helper and desktop launcher metadata. |
| Documentation automation | [`.github/workflows/openwiki-update.yml`](../.github/workflows/openwiki-update.yml) | Scheduled and manual OpenWiki update pull requests. |

## Practical change paths

- **Add or change an overlay property:** update `src/config.rs`, its tests, and the mirrored normalization/rendering in `assets/manager.js`; then inspect bridge serialization in `src/ffi.rs`.
- **Change counter discovery:** update `src/api.rs` fixtures and the catalog handling in `assets/manager.js`; preserve response limits and source order.
- **Change startup or platform behavior:** update `src/qt/qt_shell.cpp`, check `build.rs` and `src/qt/bridge.h`, then manually exercise tray, IPC, edit/display, and target platform paths.
- **Change embedded UI security:** update `src/ui.rs` tests and verify that generated HTML remains self-contained and safely encoded.
