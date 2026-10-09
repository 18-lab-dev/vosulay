---
type: Runbook
title: Build, launch, and operate vosulay
description: Runbook for building vosulay's Rust/CEF workspace and its pinned CEF caches, the host's command-line options and runtime/config selection, Linux attachment and troubleshooting, the remote profile server, packaging, and manual OpenWiki maintenance.
resource: build.rs
tags: [operations, build, runbook, cef, linux, windows]
---

# Build, launch, and operate vosulay

## Native build prerequisites

vosulay builds with Cargo and Rust 1.96.1 ([`rust-toolchain.toml`](../../rust-toolchain.toml)); [`Cargo.toml`](../../Cargo.toml) defines the workspace. Qt, CMake/MOC, LayerShellQt, Dawn Qt WebEngine, Electron, Python, and Chromium/CEF source builds are no longer part of the build.

[`crates/browser-cef/build.rs`](../../crates/browser-cef/build.rs) acquires the pinned precompiled CEF distribution from [`tools/cef/pins.json`](../../tools/cef/pins.json) through [`crates/cef-tools`](../../crates/cef-tools) when `CEF_PATH` is unset, verifying archive size, SHA1, SHA256, and the runtime file manifest. The first acquisition needs network access; later builds reuse `$XDG_CACHE_HOME/vosulay/cef` (`~/.cache/vosulay/cef` when unset). An explicit `CEF_PATH` must name an already verified pinned distribution. Only x86_64 Linux and Windows targets have pins.

On Linux the root [`build.rs`](../../build.rs) also retains the executable-adjacent CEF runtime in `$XDG_CACHE_HOME/vosulay/runtime` and the built compositor and lookup libraries in `$XDG_CACHE_HOME/vosulay/overlay`, then embeds those paths so a Cargo-installed binary runs without environment setup. Keep both caches while using such an installation.

Linux builds additionally need a C compiler, `pkg-config`, `wayland-client` development files (linked by `rfd`'s portal backend), `libEGL` development files for the compositor example, and the system libraries `libcef.so` loads at runtime. The [testing guidance](../testing/guidance.md#linux-prerequisites) lists the exact Ubuntu packages, the SDL2/SDL3 development packages the loader fixtures need, and the verification commands. Windows builds need the native MSVC toolchain; the `package-windows` helper documented in the [README](../../README.md) acquires CEF and asdf-overlay assets before building.

## Launch modes and controls

[`src/runtime.rs`](../../src/runtime.rs) parses the command line of the single CEF host (`vosulay`, or `vosulay.exe` on Windows):

| Option | Effect |
|---|---|
| `--vosu-url`/`-t URL` | Backend base URL, default `http://127.0.0.1:24050/`. Must be HTTP(S) without credentials, query, or fragment; a trailing `/` is added. |
| `--fps-limit`/`-f FPS` | `auto` or `0` follows the target display; otherwise `1`–`1000000`. Overrides the saved Settings value for this run only. |
| `--pid PID` | Additional supported game process (repeatable, non-zero). |
| `--process-name NAME` | Additional supported executable basename (repeatable; no path separators). |
| `--config PATH` | Configuration file. |
| `--cef-dir PATH` | Runtime directory. On Linux it also selects `libvosulay_overlay.so` and `libvosulay_lookup.so` from that directory. |
| `--benchmark-uncapped` | Testing only: disables Chromium pacing and forces the smallest positive OSR interval. Not a production setting. |
| `--help`/`-h`, `--version`/`-v` | Print usage or version and exit. |

Unknown options are rejected. There are no IPC, monitor-selection, or attach-geometry options; the host follows the focused supported game instead.

Runtime selection:

- **Portable package:** when `libcef.so` (Linux) sits beside the executable, the host uses that directory for CEF, the compositor, and the lookup helper. Windows always uses the executable's directory.
- **Cargo build or install (Linux):** [`src/main.rs`](../../src/main.rs) hard-links (or copies) the executable into the build-time retained runtime directory and re-executes it there, because CEF loads ICU and GPU assets beside the executable. The compositor and lookup paths come from the retained `overlay` cache. Removing either cache breaks an installed binary until it is reinstalled.
- **Configuration:** a `portable` file or an existing `data/config.json` beside the executable selects `data/config.json`; otherwise the user configuration directory is used. `--config` overrides both.

## Runtime behavior

CEF starts lazily when a supported game is selected and stays initialized until exit, because CEF cannot be reinitialized in one process. `Ctrl+Shift+Space` toggles edit mode in game, and Escape leaves it. Input capture and browser-heartbeat failures are bounded to five seconds: Windows terminates the host so input recovers, while Linux releases capture and keeps the host running. The README describes the full editor, focus-following, and frame-rate behavior.

Linux delivers frames through the injected compositor. The host refreshes recognized osu desktop shortcuts on startup with `lookup helper → foreign preloads → compositor` preload order and Vulkan layer settings, and also attempts live attachment to same-user native games. Where injection policy blocks live attachment, launch the game through its refreshed shortcut or with the `LD_PRELOAD` or `VK_LAYER_PATH`/`VK_INSTANCE_LAYERS` commands in the README. Package the host and compositor together: the control protocol is versioned.

When an overlay is missing in game, check in this order:

1. Is the game focused? Selection follows focus, not process order.
2. Is the backend reachable at the `--vosu-url` value?
3. Did the compositor load? Check the preload order and whether the game was launched before the shortcut refresh.
4. Is the overlay enabled and inside the game viewport?

## Remote profile server operations

The host serves view-only profile pages at `/overlay` (`?profile=N` selects by 1-based position) on `settings.remote_listen_host:remote_listen_port`. The default is `0.0.0.0:5050`; changes are made in manager settings and apply after a restart. Clients must use the machine's loopback or LAN address, not the bind address.

There is no access token or credential. Access depends on three checks in [`src/remote.rs`](../../src/remote.rs):

- **Allowed client IPs:** loopback is always allowed. Other entries are exact IPs or four-part IPv4 `*` patterns, separated by commas.
- **Host header:** it must name the listener's own address or the configured **Remote host**.
- **Browser metadata:** same-origin `Origin` and `Sec-Fetch-Site` are required, except for local overlay resources and frame navigations.

**Remote access URL** appears in settings only after **Remote host** is set. Anyone allowed can view every profile and use the proxied backend, so restrict the listener and allowlist to trusted networks. The backend must allow the vosulay machine, not each viewing device.

A LAN bind failure is non-fatal: the host logs `Remote overlay hosting at … is unavailable` and keeps the loopback broker that the in-game browser needs. See [vosu catalog, overlay sources, and remote pages](../integrations/vosu-and-overlays.md#remote-profile-pages) for routes and headers.

## Packaging and documentation maintenance

[`extra/com.vosu.vosulay.desktop`](../../extra/com.vosu.vosulay.desktop) supplies the desktop entry that [`CMakeLists.txt`](../../CMakeLists.txt) installs with the application icon. Portable Linux and Windows packages are assembled by [`crates/cef-tools`](../../crates/cef-tools) (`package` and `package-windows`); the README gives the exact commands. Portable configuration selection is described under "Runtime selection" above. No installer or release runbook is present yet.

OpenWiki maintenance is intentionally manual and local. From the repository root, run `openwiki code --update --print` (or `openwiki --update`) after reviewing source changes; inspect the generated diff and commit only the affected `openwiki/` and OpenWiki-managed guidance files. Do not create or restore `.github/workflows/openwiki-update.yml`, or add scheduled/manually dispatched GitHub Actions automation for OpenWiki.
