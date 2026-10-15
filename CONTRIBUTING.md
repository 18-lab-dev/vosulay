# Contributing to vosulay

You can contribute with one x86_64 Linux or Windows machine. A game install is useful only for in-game checks; no private tools, second machine, or preprovisioned cache is required. Work in this repository, not the ecosystem umbrella. Read the [source map](docs/source-map.md) and [engineering docs](docs/README.md) for the area you change.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md). For a larger change or a new feature, open a [feature request](https://github.com/shiruviaa/vosulay/issues/new?template=feature_request.yml) or start a [discussion](https://github.com/shiruviaa/vosulay/discussions) first, so the approach can be agreed before you write the code. Issues labelled [`good first issue`](https://github.com/shiruviaa/vosulay/labels/good%20first%20issue) are a good place to start.

## Prerequisites

Install [rustup](https://rustup.rs/). Run commands from the checkout; [rust-toolchain.toml](rust-toolchain.toml) selects Rust 1.96.1 with rustfmt and Clippy. To install it explicitly:

```sh
rustup toolchain install 1.96.1 --profile minimal --component clippy,rustfmt
```

### Linux

[Linux CI](.github/workflows/linux-ci.yml) uses Ubuntu 26.04 because its archive supplies both SDL development packages. Its package list is:

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential pkg-config \
  libwayland-dev libegl-dev libsdl2-dev libsdl3-dev \
  libasound2t64 libatk-bridge2.0-0t64 libatk1.0-0t64 libatspi2.0-0t64 \
  libcairo2 libcups2t64 libdbus-1-3 libexpat1 libgbm1 libglib2.0-0t64 \
  libnspr4 libnss3 libpango-1.0-0 libudev1 libx11-6 libxcb1 \
  libxcomposite1 libxdamage1 libxext6 libxfixes3 libxkbcommon0 libxrandr2
pkg-config --cflags --libs sdl2
pkg-config --cflags --libs sdl3
```

Use your distribution's equivalent packages on other Linux systems. Older Ubuntu releases do not supply `libsdl3-dev`; a missing SDL ABI can make the SDL fixture skip coverage while tests still pass. Report that gap rather than claiming both ABIs were tested.

Cargo automatically downloads and verifies the pinned precompiled CEF SDK when `CEF_PATH` is unset; no Chromium source build is needed. For workspace tests, prepare the library path as CI does:

```sh
cef="$(cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform linux-x86_64)"
ldd "$cef/Release/libcef.so"
mkdir -p target/cef-lib
ln -sf "$cef/Release/libcef.so" target/cef-lib/libcef.so
export LD_LIBRARY_PATH="$PWD/target/cef-lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

Resolve any `not found` dependencies first. Expose only `libcef.so`, not the entire `Release` directory: bundled graphics libraries can shadow system EGL/GLES/Vulkan and invalidate native fixtures.

### Native Windows

Install rustup's `x86_64-pc-windows-msvc` toolchain plus the `i686-pc-windows-msvc` target (`rustup target add i686-pc-windows-msvc`), and Visual Studio Build Tools with **Desktop development with C++**, the MSVC x64 and x86 compilers/linkers, and Windows SDK. Use a developer PowerShell/command prompt with those tools available. Native packaging acquires verified CEF assets, builds both asdf-overlay DLLs from pinned source, builds the client DLL, and assembles a complete portable ZIP:

```powershell
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
```

The output is `dist/vosulay-<version>-windows-x86_64.zip`. Keep all runtime files together. CEF is also automatically acquired by the browser crate's build script when `CEF_PATH` is unset. Before native workspace tests, make its DLLs discoverable in the current PowerShell session:

```powershell
$cef = (cargo run --quiet --locked -p vosulay-cef-tools -- acquire --platform windows-x86_64).Trim()
$env:PATH = "$cef\Release;$env:PATH"
```

### Download and storage costs

The current [pins](tools/cef/pins.json) specify approximately 322 MB for the Linux CEF archive and 172 MB for Windows. Extraction, retained runtime assets, Cargo outputs, and optional cross-toolchain files require substantially more disk space; allow several GB. First acquisition and compilation can take minutes or longer depending on network/CPU. Subsequent builds reuse verified caches. Only acquisition needs network access; startup downloads nothing. `CEF_PATH` is an optional override for an already verified, matching pinned SDK, not a prerequisite. See [operations](docs/operations.md) for cache and package details.

## Verification ladder

Run the tiers relevant to your change and report what you actually exercised. Source checks cannot prove native input, GPU composition, or capture behavior.

### Tier 1 — everyone: native workspace checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
python3 tools/notices/generate.py --check
```

Use `--workspace`; Cargo's root-only default misses browser, transport, compositor, and tooling tests. Clippy, tests and the notices check are exactly the commands in Linux CI. After changing dependencies, regenerate the shipped `THIRD_PARTY_NOTICES.md` with `python3 tools/notices/generate.py`. Formatting is a recommended local check, not currently a workflow step. On Windows these commands check native Windows code; on Linux they check native Linux code and shared platform-independent tests.

### Tier 2 — another platform without another machine

Linux contributors can optionally check Windows source using [cargo-xwin](https://github.com/rust-cross/cargo-xwin). Install Clang and LLVM first (a full LLVM installation is recommended by cargo-xwin; on Ubuntu the packages are `clang`, `llvm`, and `lld`). Read cargo-xwin's notice about the Microsoft SDK/CRT license before installing/using it:

```sh
cargo install cargo-xwin --locked
rustup target add x86_64-pc-windows-msvc
rustup component add llvm-tools
unset C_INCLUDE_PATH
cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
```

`C_INCLUDE_PATH` must be unset so host headers do not contaminate the Windows build. cargo-xwin acquires its Windows SDK/CRT inputs; follow its installation prerequisites for your distro. Unset a Linux-only `CEF_PATH` override too, so the target's Windows CEF distribution can be acquired. Cross-Clippy is not Windows execution or portable-package verification.

Windows contributors should use Linux CI for Linux coverage. WSL provides a Linux environment in which the Linux prerequisites and commands can be attempted; this repository does not promise WSL GUI/game attachment support. Windows-native checks alone do not compile Linux-only code.

Hosted safety net: [linux-ci](.github/workflows/linux-ci.yml) runs on pull requests and pushes to `main`, as well as manual dispatch. Pushing a feature branch with an open PR triggers PR checks; pushing a branch alone does not run both workflows. [Packages](.github/workflows/release.yml) is **manual dispatch only**: ask a maintainer to run it for the branch, or run it in your fork from the Actions tab with `publish=false`. It uploads `vosulay-linux-x86_64` and `vosulay-windows-x86_64` artifacts containing Linux packages and the Windows ZIP/installer. Linux CI currently uploads no package artifact. Release publication is not a contributor verification step.

Maintainers changing packaging should follow [operations](docs/operations.md#packaging-verification-and-tested-on-list): build in the Ubuntu 22.04 container and install-test DEB/RPM/Arch packages in disposable distro containers, checking version output, libcef dependencies, setuid sandbox ownership/mode, sustained startup and no duplicate user launcher. Windows installer changes require separate authorized native install/upgrade/uninstall checks.

For Windows release-verifier changes, run the isolated policy regression tests
(PowerShell 7, also run by the Windows packaging job):

```powershell
./packaging/windows/test-verify-release.ps1
```

These tests parse the verifier and mock Authenticode results to exercise policy
acceptance/rejection; they do not validate Windows trust, actual timestamps or
installation. Real signed packages must separately pass the workflow's
`verify-release.ps1 -RequireSignature` check.

### Tier 3 — editor UI without the game

The shared frontend is `assets/manager.html`, `assets/manager.css`, and `assets/manager.js`. Any Chromium-based browser on either platform can run it without CEF or a game:

```sh
python3 tools/manager-preview.py --serve 8765
```

Open `http://127.0.0.1:8765/` (or the printed `file://` URL without `--serve`). The script assembles the page exactly as [src/ui.rs::manager_document](src/ui.rs) does and loads two demo profiles, a counter catalog, and edit mode. Pass `--config ~/.config/vosulay/config.json` to load your own profiles; it is only read, never written. Output goes to `target/manager-preview/`.

The stub host logs every outbound message to the DevTools console (also kept in `window.__VOSULAY_SENT__`) and acknowledges saves so status and undo flows behave normally; nothing is persisted. Demo counters are a local placeholder page, not real counters. Send further host messages from DevTools with `window.__VOSULAY_RECEIVE__({...})` (see `receive()` in `assets/manager.js` for the message types). Native file pickers, host shortcuts, game focus, and compositor input are outside this tier.

To exercise the overlay settings dialog, add `--vosu-url http://127.0.0.1:<port>/` pointing at a vosu with overlay settings profiles (run a test vosu with its own configuration and data directories, not your everyday one). The preview then marks every counter as having settings and snapshots that vosu's profile registry at build time, so profiles created later appear only after rebuilding. Use counter IDs that the vosu has installed (`--config`) for its settings page to show them.

Off-screen CEF does **not** render native popup surfaces: avoid `window.confirm`, native `<select>` popup menus, and browser validation bubbles in the product UI. A normal Chromium preview can show them and therefore cannot prove off-screen compatibility.

For a real isolated CEF smoke on Linux, install Python 3, Xvfb and bubblewrap (`xvfb` and `bubblewrap` on Ubuntu), with permission to create the namespaces used by `bwrap`. Then:

```sh
cargo build --locked -p vosulay-cef-smoke
# The build retains CEF in the executable-adjacent runtime layout the smoke needs;
# the raw SDK splits icudtl.dat from libcef.so and CEF's children then fail.
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
runtime="$(echo "${XDG_CACHE_HOME:-$HOME/.cache}/vosulay/runtime/$version"-*)"
python3 tools/cef/private-smoke.py \
  --binary target/debug/vosulay-cef-smoke --cef-dir "$runtime" \
  --xvfb /usr/bin/Xvfb --output target/private-cef-smoke
```

Use a fresh output directory every time (the wrapper rejects an existing one). It creates a private display and isolates PID/IPC/network namespaces; do not substitute a GUI smoke on your working desktop. Logs and evidence go under the output directory. Namespace restrictions can block this optional tier. Software smoke does not prove game/GPU behavior; WebGPU depends on adapter availability.

### Tier 4 — manual in-game checks on your own machine

Use a test profile/configuration and preserve existing settings. Runtime startup can register launcher entries and refresh Linux osu shortcuts; only perform this tier when you intend those changes.

- **Linux:** run vosu or tosu independently, start vosulay (for example `cargo run --locked`), then launch osu through its refreshed desktop shortcut. If needed, use the [README's manual LD_PRELOAD or Vulkan-layer launch](README.md#linux) with your actual package paths; do not change ptrace policy or elevate to bypass attachment restrictions.
- **Windows:** extract the complete portable package, start vosu or tosu, run `vosulay.exe`, and launch/focus osu!stable or osu!lazer. Stop your test game before replacing injected DLLs.

For UI/input changes, check:

- Ctrl+Shift+Space enters/exits edit mode; Escape releases editing and game input.
- Add a counter; move it, resize/Ctrl-scale/Shift-crop it by its edges, wheel a properties field, save, and reopen it; verify expected placement.
- Ctrl+I toggles counter interaction without leaking editor gestures into the game.
- Lose/regain game focus; editing exits by default, and the configurable keep-editing setting behaves as documented.
- Tray **Edit mode**, **Hide vosulay**, and **Exit vosulay** work and show the expected checked state.
- Open `http://127.0.0.1:5050/overlay` and verify the active profile; do not widen listener access just for a local test.
- With **Allow editing on this computer** on, open `http://manage.localhost:5050/overlay`; edit a placement and a setting, then confirm the in-game editor and config file follow and the plain `/overlay` address stays view-only.
- `vosulay --toggle-edit` toggles in-game editing like the tray item; `vosulay --quit` exits gracefully; both fail with status 1 when nothing is running. `vosulay --server-only` serves pages without touching the game.

Document OS/game/backend versions and any skipped checks. See [testing](docs/testing.md) for coverage boundaries.

## Maintainer environment (optional)

Reviewers may additionally run a native game/OBS matrix, authorized GPU guest tests, heartbeat-recovery checks, and delivered-pixel benchmarks. These are not prerequisites for a contributor PR. The specialized VM/benchmark environment and its safety rules are separated in [testing](docs/testing.md#maintainer-environment-optional); do not attempt to recreate private commands or offline caches to contribute.

## Pull requests

- Add/update tests for behavior changes; explain native/manual coverage and remaining gaps.
- Update hand-maintained `docs/` with code changes. There is no generated wiki to rebuild.
- Follow the existing concise commit style: `feat: ...`, `fix: ...`, `docs: ...` (recent subjects use a lowercase type and an imperative summary).
- Include a clear change summary and exact checks/results. Do not claim hosted CI or runtime behavior you did not observe. No game install is required for a docs-only or source-only contribution. The pull request template lists the checks; tick only what you ran.
- Linux CI runs on every pull request. For a first-time contributor, a maintainer approves the CI run before it starts.
