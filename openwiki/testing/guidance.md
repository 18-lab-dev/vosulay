---
type: Testing Guide
title: vosulay testing guidance
description: Exact Linux and Windows verification commands for the Rust/CEF workspace, native prerequisites, CI workflows and their trigger policy, the C-backed loader fixtures, a test-to-behavior map, and what automated checks do not cover.
resource: .github/workflows/linux-ci.yml
tags: [testing, rust, cef, ci, linux, windows, regression]
---

# vosulay testing guidance

vosulay is a Rust 2024 Cargo workspace with one pinned, precompiled CEF runtime.
All automated tests are Rust tests run by Cargo; the only non-Rust test sources
are the C fixtures that `vosulay-overlay` integration tests compile and run.
There is no Qt, C++ shell, Electron, Node.js, or TypeScript test suite.

## Commands

Run from the repository root.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Always pass `--workspace`. [`Cargo.toml`](../../Cargo.toml) sets
`default-members = ["."]`, so plain `cargo test` or `cargo clippy` checks only
the root `vosulay` package and skips every crate under `crates/`, including the
transport, compositor, loader, and CEF tooling tests.

`--all-features` currently enables only the root `cef-host` feature, which is
also the default. A build without `cef-host` is not exercised by any command
here.

To run only the C-backed loader fixtures and see their skip messages:

```sh
cargo test --locked -p vosulay-overlay --test loader_scope --test sdl_filter -- --nocapture
```

These must run through Cargo: each test locates `libvosulay_overlay.so` and
`libvosulay_lookup.so` beside its own test executable in `target/<profile>/deps`.

## Linux prerequisites

- x86_64 glibc Linux. The CEF build script rejects other architectures, and the
  loader fixtures compile only for `x86_64`/`gnu`.
- Rust 1.96.1 with `clippy` and `rustfmt`, as pinned by
  [`rust-toolchain.toml`](../../rust-toolchain.toml).
- A C compiler (`cc`) and `pkg-config`: the `ring` dependency, both loader
  fixtures, and pkg-config probes use them.
- `wayland-client` development files: `rfd`'s `xdg-portal` backend probes and
  links it through `wayland-sys`.
- `libEGL` development files: the `gl_surface` example of `vosulay-overlay`
  links `libEGL`, and Cargo builds examples during `cargo test`.
- SDL2 and SDL3 development files reachable via `pkg-config` for
  `sdl_filter` (see below).
- The runtime libraries `libcef.so` needs (NSS/NSPR, GLib, ATK/AT-SPI, CUPS,
  D-Bus, X11/XCB extensions, xkbcommon, GBM, Cairo, Pango, udev, ALSA). Test
  and example executables that link CEF fail to start without them. After
  acquisition, `ldd "$cef/Release/libcef.so" | grep 'not found'` must print
  nothing.
- A loader search path for `libcef.so`. Only the root package's
  [`build.rs`](../../build.rs) adds an RPATH to the retained CEF runtime.
  `vosulay-browser` and `vosulay-cef-smoke` targets get only `$ORIGIN`, and
  Cargo drops link-search paths outside `target/` from `LD_LIBRARY_PATH`.
  CI therefore exposes a directory that contains only a `libcef.so` symlink.
  Do not expose the whole `Release/` directory, because it would also shadow
  the system `libEGL`, `libGLESv2`, and `libvulkan` for the C fixtures:

  ```sh
  mkdir -p target/cef-lib
  ln -sf "$cef/Release/libcef.so" target/cef-lib/libcef.so
  export LD_LIBRARY_PATH="$PWD/target/cef-lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  ```

  `$cef` is the path printed by the `acquire` command below.

On Ubuntu 26.04 the package list in
[`.github/workflows/linux-ci.yml`](../../.github/workflows/linux-ci.yml) covers
all of the above. Ubuntu 24.04 lacks `libsdl3-dev`, so `sdl_filter` there tests
SDL2 only. Sources checked on 2026-10-09:

- [packages.ubuntu.com: `libsdl3-dev`](https://packages.ubuntu.com/search?keywords=libsdl3-dev&searchon=names&suite=all&section=all)
  lists 3.4.2+ds-1ubuntu1 (universe) in resolute (26.04) and no noble (24.04) package.
  [`libsdl2-dev`](https://packages.ubuntu.com/search?keywords=libsdl2-dev&searchon=names&suite=all&section=all)
  is 2.32.10+dfsg-6 in resolute.
- [GitHub changelog, 2026-09-17](https://github.blog/changelog/2026-09-17-ubuntu-26-generally-available-and-latest-migration/):
  the `ubuntu-26.04` runner label is generally available, and `ubuntu-latest`
  migrates to it between October 19 and November 19, 2026.
- [`Ubuntu2604-Readme.md`](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2604-Readme.md)
  (image 20260927.149.1) lists Rustup 1.29.1, pkg-config 2.5.1-4, and installed
  universe packages such as `sshpass`. That universe is enabled on the image is
  inferred from those packages, not stated.

### CEF acquisition and caches

[`crates/browser-cef/build.rs`](../../crates/browser-cef/build.rs) acquires the
pinned Linux CEF distribution from [`tools/cef/pins.json`](../../tools/cef/pins.json)
when `CEF_PATH` is unset. It downloads the archive (about 321 MB; network needed
only the first time), checks its size, SHA1, and SHA256, extracts it, and
verifies the runtime file manifest. An explicit `CEF_PATH` must point at an
already verified pinned distribution and needs no network. To acquire
explicitly and print the verified SDK root:

```sh
cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64
```

Build scripts keep state under `$XDG_CACHE_HOME/vosulay` (`~/.cache/vosulay`
when unset): `cef/` holds the archive and extracted SDK, `runtime/` the retained
executable-adjacent runtime, and `overlay/` the retained compositor and lookup
libraries that the root [`build.rs`](../../build.rs) embeds by path.

## CI workflows

Linux tests and Clippy run automatically; the Windows package and release
workflows stay manual-dispatch only to preserve GitHub Actions minutes. No
workflow has run as part of changing this page.

| Workflow | Triggers | Runner | What it checks |
|---|---|---|---|
| [`linux-ci.yml`](../../.github/workflows/linux-ci.yml) | every `pull_request`, `push` to `main`, `workflow_dispatch` | `ubuntu-26.04` | Installs the prerequisites above, fails if `pkg-config` cannot resolve both SDL2 and SDL3, acquires and verifies CEF, fails on unresolved `libcef.so` dependencies, puts a directory containing only a `libcef.so` link on `LD_LIBRARY_PATH`, then runs the workspace Clippy and test commands above. A newer push cancels an open PR's earlier run; `main` and manual runs are never cancelled. |
| [`windows-build.yml`](../../.github/workflows/windows-build.yml) | `workflow_dispatch` only | `windows-latest` | Acquires CEF/asdf-overlay, builds the release client, and uploads the portable ZIP. No Clippy or tests. |
| [`windows-release.yml`](../../.github/workflows/windows-release.yml) | `workflow_dispatch` only | `windows-latest` | Same package build, then publishes a GitHub release for an existing tag. |

Linux CI caches only the pinned CEF archive (keyed by `tools/cef/pins.json`) and
the Cargo registry plus `target/` (keyed by toolchain and `Cargo.lock`).
Extraction and CEF verification run in every job. Linux CI does not run
`cargo fmt`.

## Windows native checks

Windows tests run only on a native Windows host with Rust 1.96.1 and MSVC. The
maintained runner in
[`tools/autoresearch/windows_benchmark.ps1`](../../tools/autoresearch/windows_benchmark.ps1)
uses:

```powershell
cargo test --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc
cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo fmt --all --check
```

`vosulay-windows` target-selection tests are compiled under
`cfg(any(windows, test))`, so Linux runs also execute them. D3D11 composition,
asdf-overlay injection, tray, and foreground tracking have no automated tests.

## C-backed loader fixtures

[`crates/overlay-linux/tests/loader_scope.rs`](../../crates/overlay-linux/tests/loader_scope.rs)
compiles [`loader_provider.c`](../../crates/overlay-linux/tests/fixtures/loader_provider.c),
[`loader_caller.c`](../../crates/overlay-linux/tests/fixtures/loader_caller.c), and
[`loader_driver.c`](../../crates/overlay-linux/tests/fixtures/loader_driver.c)
with `cc -std=c11 -Wall -Wextra -Werror`, then runs the driver with
`LD_PRELOAD=libvosulay_lookup.so:libvosulay_overlay.so`. In `next`, `local`, and
`explicit` modes it checks that the interposed `dlsym` preserves the native
caller's `RTLD_NEXT` and `RTLD_DEFAULT` scope and returns symbols from the
exact library handle requested. It always runs; it does not skip.

[`crates/overlay-linux/tests/sdl_filter.rs`](../../crates/overlay-linux/tests/sdl_filter.rs)
compiles [`sdl_filter_driver.c`](../../crates/overlay-linux/tests/fixtures/sdl_filter_driver.c)
once against SDL2 and once against SDL3 (`-DVOSULAY_TEST_SDL3`) and runs each
binary under the same preload with `SDL_VIDEODRIVER=dummy`,
`SDL_AUDIODRIVER=dummy`, and an unused `VOSULAY_ENDPOINT`. It checks that the
compositor's SDL hooks keep the game's event-filter callback and userdata
correct across set, replace, and clear. **If `pkg-config` cannot resolve an SDL
package, that ABI is skipped with a stderr message and the test still passes**;
if neither resolves, nothing native runs. Use `--nocapture` locally, and rely on
the CI pre-check, to confirm both ABIs executed.

Neither fixture needs a display, GPU, or running vosulay host.

## Coverage map

| Area | Tests | Behavior protected |
|---|---|---|
| Counter catalog | [`src/api.rs`](../../src/api.rs), fixtures in [`tests/fixtures/backend`](../../tests/fixtures/backend) | vosu and tosu 4.26 counter-search HTML parsing, escaped metadata, source order, legacy/empty markup, invalid resolution defaults, global fetch deadline. |
| Configuration | [`src/config.rs`](../../src/config.rs) | Defaults, profile/geometry normalization, repeated and disabled placements, legacy JSON defaults, FPS-limit bounds, remote listen/access settings, atomic save/load round trips. |
| tosu migration | [`tests/tosu_migration.rs`](../../tests/tosu_migration.rs), fixtures in [`tests/fixtures/migration`](../../tests/fixtures/migration) | Layout conversion, malformed/unsafe input, idempotent read-only automatic import, no overwrite of existing profiles, manual import collision repair. |
| Backend telemetry broker | [`src/backend/tests.rs`](../../src/backend/tests.rs) | One shared upstream per schema, client-local filters, disconnect/recovery, selected-subscription deltas. |
| Remote profile server and proxy | [`src/remote.rs`](../../src/remote.rs), [`src/remote/tests_broker.rs`](../../src/remote/tests_broker.rs) | Listen address, peer allowlists, profile selection and rendering, local capability/traversal rejection, authority checks, proxy bootstrap injection, origin rerouting, listener shutdown. |
| Local overlay authorization | [`src/shell_policy.rs`](../../src/shell_policy.rs) | Asset resolution, token rebinding, traversal and symlink escape, malformed capability URLs. |
| CEF host runtime (`cef-host`) | [`src/runtime.rs`](../../src/runtime.rs), [`src/runtime/editor.rs`](../../src/runtime/editor.rs), [`src/runtime/launchers.rs`](../../src/runtime/launchers.rs) | FPS policy precedence, save under a stalled counter response, backend discovery, osu shortcut preload/Vulkan-layer rewriting without touching system entries. |
| CEF browser wrapper | [`crates/browser-cef/src/lib.rs`](../../crates/browser-cef/src/lib.rs) | Message-pump fallback-clock scheduling and manager-only local/loopback network permission. These do not initialize CEF. |
| CEF acquisition and packaging | [`crates/cef-tools/src/tests.rs`](../../crates/cef-tools/src/tests.rs) | Archive extraction confinement, link/special-node rejection, unpacked-size bounds, receipt verification. |
| Linux frame transport | [`crates/frame-transport/src/ring.rs`](../../crates/frame-transport/src/ring.rs), [`protocol.rs`](../../crates/frame-transport/src/protocol.rs), [`backend.rs`](../../crates/frame-transport/src/backend.rs) | Newest-complete-frame ring semantics, read-lease stability, damage accumulation, saturation, publish/consume races, focus timestamps, generation release. |
| Linux focus and injection | [`crates/platform-linux/src/lib.rs`](../../crates/platform-linux/src/lib.rs) | Focus-driven game selection, stale-surface ordering, exit handling, game discovery, launcher/Wine target filtering. |
| Injected compositor | [`crates/overlay-linux/src/sdl.rs`](../../crates/overlay-linux/src/sdl.rs), [`client.rs`](../../crates/overlay-linux/src/client.rs), loader fixtures above | SDL input/cursor state restoration, focus observation retry, native loader scope, SDL filter state. |
| Windows target selection | [`crates/platform-windows/src/target_selection.rs`](../../crates/platform-windows/src/target_selection.rs) | Focus-based attach, rapid switches, exit and PID reuse. |

`vosulay-frame`, `vosulay-lookup` (apart from the loader fixture),
`vosulay-cef-smoke`, [`src/ui.rs`](../../src/ui.rs), and
[`src/migration.rs`](../../src/migration.rs) internals have no unit tests of
their own; migration is covered through `tests/tosu_migration.rs`.

## Not covered by automated checks

Passing Clippy and the workspace tests does not establish any of these:

- **Real CEF rendering.** No automated test initializes CEF. The
  `vosulay-cef-smoke` binary checks transparent OSR, JavaScript, the manager
  bridge, iframe pixels, WebGL, resize, and shutdown.
  [`tools/cef/private-smoke.py`](../../tools/cef/private-smoke.py) runs it on a
  private Xvfb display inside bubblewrap. It needs `bwrap`, `Xvfb`, and Python 3.

  CEF loads ICU and GPU assets beside the executable (see
  [`src/main.rs`](../../src/main.rs)). The smoke binary does no staging of its
  own, so first package it with the existing CEF package command into a new
  task-owned directory, then pass that packaged binary and directory to the
  runner.

  For a flat directory, the runner puts `--cef-dir` itself on `LD_LIBRARY_PATH`;
  it uses `--cef-dir/Release` only when that subdirectory exists. CEF resolves
  resources from the same flat directory. Neither `package --output` nor the
  runner's `--output` may exist beforehand.

  ```sh
  cargo build --locked -p vosulay-cef-smoke
  cef="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
  run="target/cef-smoke-$(date +%Y%m%d%H%M%S)"
  cargo run --quiet --locked -p vosulay-cef-tools -- package --platform linux-x86_64 \
    --cef-path "$cef" --output "$run/runtime" --exe target/debug/vosulay-cef-smoke
  python3 tools/cef/private-smoke.py --binary "$run/runtime/vosulay-cef-smoke" \
    --cef-dir "$run/runtime" --xvfb "$(command -v Xvfb)" --output "$run/proof"
  ```

  CI does not run this smoke test, and the command above was not executed
  when this page was revised.
- **In-game composition.** Vulkan, EGL/GLX, and SDL swap hooks against real
  drivers and games; CPU-frame upload to game GPU textures; OBS capture order; input capture/release
  timing. The README records past manual and VM evidence for these.
- **Performance.** [`autoresearch.sh`](../../autoresearch.sh) runs only inside an
  already-running GPU VM and is never part of CI.
- **Windows runtime.** D3D11 copies, asdf-overlay injection, and packaged-app
  behavior need native Windows verification.
- **Packaging.** Linux portable packaging (`vosulay-cef-tools package`) and
  `cargo install` runtime retention have no automated test.

## Regression priorities

- Counter HTML contract: add fixtures under `tests/fixtures/backend` for any new
  selector or malformed metadata expected from vosu or tosu.
- URL, proxy, and local-resource handling: test credentials, authority/origin
  rebinding, traversal, symlinks, and scheme authorization.
- Config fields: test missing, duplicate, and invalid values and persistence
  compatibility, and extend the tosu migration fixtures when imports change.
- Loader and hook changes in `vosulay-overlay` or `vosulay-lookup`: extend the C
  fixtures rather than replacing them with Rust-only checks, and keep both SDL
  ABIs installed wherever they run.
- Frame transport: the shared memfd header is writable by both processes.
  Frame geometry used to size or index pixel copies should come from values
  each side validated and retained locally, not from a later re-read of the
  shared header. A separate ring-safety change delivers that validation and its
  regression tests. On 2026-10-09 the coordinator observed it on a local
  integrated checkout:
  - 15 frame-transport tests passed;
  - consumer header corruption returned `InvalidData`;
  - leased pixels were unchanged;
  - publishing after restoration produced sequence 2.

  This is local evidence only, not hosted CI or live-game verification.
