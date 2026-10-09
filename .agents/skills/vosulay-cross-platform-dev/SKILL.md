---
name: vosulay-cross-platform-dev
description: Build and maintain vosulay's shared Rust/CEF host, native Linux/Windows composition, and manager frontend.
---

# Cross-platform development

Work inside the vosulay repository, not the umbrella or other submodules. Read `docs/README.md`, `docs/source-map.md`, and `docs/testing.md` first. One Rust host and `assets/manager.{html,js,css}` serve both platforms; do not fork the frontend or add a second configuration convention.

## Source contracts

- `src/config.rs` owns serialized defaults/normalization; manager page state mirrors it. Update both and relevant round-trip/migration fixtures when the model changes.
- `src/api.rs` detects backend compatibility from public counter markup, not the product name. Preserve source order, explicit unrecognized-markup errors, and response limits.
- `src/backend.rs` shares telemetry by schema; precise subscriptions and commands are per-client relays, not projected shared snapshots.
- `src/runtime/host.rs` owns CEF/browser lifetime, editing/visibility, frame policy, and input safety. Shared native hotkey parsing is in `crates/frame/src/hotkey.rs`.
- Linux publishes CPU BGRA through a persistent memfd ring; Windows copies borrowed CEF D3D11 textures into application-owned GPU textures before returning. Preserve frame ownership and bounded input-release behavior.

## Build and package

Use the pinned Rust toolchain and prerequisites in `.github/workflows/linux-ci.yml`. Cargo acquires verified precompiled CEF when `CEF_PATH` is unset; explicit paths must match `tools/cef/pins.json`. No runtime startup download is allowed.

```sh
cargo build --locked
cargo install --path . --locked
```

Complete Linux package commands are in `docs/operations.md`; Cargo install depends on retained runtime/overlay caches and is not a portable bundle. Native Windows packaging is:

```powershell
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
```

`Cargo.toml` supplies package versioning. Windows build/release workflows are manual-dispatch only.

## Verification and safety

Use the `vosulay-local-ci` skill for workspace Clippy/tests and Windows cross-Clippy. Use `tools/cef/private-smoke.py` for isolated headless CEF checks; never launch GUI tests on the user's working desktop. Runtime claims require actual pixels and game input evidence, not accepted launch/input commands or producer counters. Use the `vosulay-windows-live-testing` skill and canonical `windows-vm` host for authorized guest testing; never start/stop/switch a VM without explicit user authorization.

Report only exercised checks, exact artifacts, and remaining platform boundaries. Update hand-maintained `docs/` with permanent code changes.
