---
type: Testing Guide
title: vosulay testing guidance
description: Maps the repository's Rust unit tests to parser, configuration, bridge, and embedded-UI behavior, and explains native build prerequisites and uncovered Qt/frontend areas.
resource: src/api.rs
tags: [testing, rust, qt, security, regression]
---

# vosulay testing guidance

## Available checks

The repository has no separate C++ or browser test tree. Tests are embedded in Rust implementation modules and are exercised through Cargo. Run formatting and unit checks from the repository root:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Because [`build.rs`](../../build.rs) always performs Qt discovery, MOC, CXX compilation, and LayerShellQt probing, a full native dependency stack is required even when the test assertions are Rust-only. See [build and run operations](../operations/build-and-run.md) for environment diagnostics.

## Test coverage by module

- [`src/api.rs`](../../src/api.rs): HTML counter-card parsing, escaped attributes, source ordering, dimensions and defaults, HTTP status/content-type/size handling, and global request deadlines.
- [`src/config.rs`](../../src/config.rs): defaults, profile and placement normalization, duplicate IDs, URL scheme filtering, malformed/unsupported files, atomic persistence, and round trips.
- [`src/ffi.rs`](../../src/ffi.rs): settings URL encoding, base-path behavior, credential rejection, and contextual invalid-JSON errors.
- [`src/ui.rs`](../../src/ui.rs): self-contained document generation, FPS bootstrap values, and escaping of script-closing characters.

These tests directly protect the [configuration model](../domain/configuration.md) and the [vosu/overlay integration contracts](../integrations/vosu-and-overlays.md).

## Regression priorities

When changing the counter HTML contract, add fixtures for the exact selectors and malformed metadata expected from vosu. When changing URL handling or local overlay serving, test credentials, query/fragment rejection, path encoding, traversal, and scheme authorization. When changing config fields, test missing/duplicate/invalid values and persistence compatibility.

The most important uncovered areas are the Qt event loop, QWebChannel message routing, platform click-through behavior, LayerShellQt geometry, IPC socket lifecycle, local-file overlay authorization in C++, and interactive browser behavior in `assets/manager.js`. Until dedicated tests exist, verify these manually in a real Qt/Wayland or X11 environment and preserve the bounded worker/shutdown behavior described in the [architecture overview](../architecture/overview.md).
