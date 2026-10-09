---
name: vosulay-local-ci
description: Verify vosulay's Rust/CEF workspace on Linux, cross-check Windows source, and run private headless smoke safely.
---

# Local CI

Run only verification requested by the task. Do not push, publish releases, launch the manager on the user's desktop, or alter user launcher files/configuration during tests. Use task-owned temporary directories and injectable paths for launch-registration tests.

## Linux preparation

Use Rust 1.96.1 and the prerequisite packages in `.github/workflows/linux-ci.yml`: compiler/pkg-config, Wayland/EGL development files, SDL2 and SDL3 development files, and CEF's runtime shared libraries. Confirm both SDL ABIs are reachable:

```sh
pkg-config --cflags --libs sdl2
pkg-config --cflags --libs sdl3
cef="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
ldd "$cef/Release/libcef.so"
mkdir -p target/cef-lib
ln -sf "$cef/Release/libcef.so" target/cef-lib/libcef.so
export LD_LIBRARY_PATH="$PWD/target/cef-lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

Resolve any `not found` dependencies before tests. Expose only `libcef.so`, not the whole CEF Release directory, to avoid shadowing the system graphics libraries used by native fixtures. CEF acquisition verifies pinned archive/runtime digests and caches the SDK; `CEF_PATH` can select an already verified pinned SDK.

## Workspace checks

```sh
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Always use `--workspace`; root-only defaults miss compositor, transport, browser, and tooling coverage. `cargo fmt --all --check` is an optional explicit formatting check, not a Linux CI step. Loader/SDL fixture details and skip semantics are in `docs/testing.md`.

## Windows source check from Linux

With cargo-xwin installed:

```sh
env -u C_INCLUDE_PATH cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
```

Cross-Clippy is not native Windows execution. On native Windows, package via `cargo run --locked -p vosulay-cef-tools -- package-windows dist/package`; native workspace tests/Clippy use `--target x86_64-pc-windows-msvc`. Do not substitute Linux workflow emulation for Windows package/runtime validation.

## Private headless smoke

Requires Python 3, bubblewrap, and Xvfb. Use a new output directory:

```sh
cargo build --locked -p vosulay-cef-smoke
python3 tools/cef/private-smoke.py \
  --binary target/debug/vosulay-cef-smoke --cef-dir "$cef" \
  --xvfb /usr/bin/Xvfb --output target/private-cef-smoke
```

The wrapper isolates the X display and namespaces; never replace it with a launch on the working desktop. Hardware/game behavior needs separate authorized private or VM verification.

## Report

Report exact commands and observed results, skipped fixture ABIs, smoke output, native versus cross-check boundaries, package artifact paths/checksums when generated, and blockers. Do not claim hosted CI, Windows runtime behavior, or cleared performance gates from local source checks. Windows workflow publication remains manual and requires authorization.
