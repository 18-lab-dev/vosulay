<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/vosulay-logo-on-dark.svg">
  <img src="assets/branding/vosulay-logo.svg" alt="vosulay" width="720">
</picture>

**Native Linux in-game overlay manager for vosu, built with Rust and Qt 6.**

</div>

vosulay displays vosu counters in a frameless overlay and provides an edit mode for arranging them. Layouts are stored as profiles, and overlays can come from the vosu counter catalog, HTTP(S) URLs, or local HTML files.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Maintain multiple overlay profiles.
- Discover installed counters from a local vosu instance.
- Add remote or local custom HTML overlays.
- Render WebGPU overlays through Dawn's native Vulkan backend.
- Run click-through overlays on Wayland and X11.
- Control visibility, edit mode, geometry, and shutdown through local IPC.

## Requirements

vosulay requires Qt 6.11.1 development packages and a Dawn-enabled Qt
WebEngine. Stock Qt WebEngine builds disable Dawn, so `build.rs` rejects them
instead of silently exposing `navigator.gpu` without a working adapter.

Install the normal Qt WebEngine build dependencies, `gperf`, and Python's
`html5lib`, then build the pinned WebEngine variant into a separate prefix:

```sh
extra/build-webgpu-qt.sh "$HOME/.local/opt/vosulay-qt-webgpu"
```

Apply the six environment exports printed by the helper. They select its
pkg-config metadata, libraries, subprocess, resources, and locales.

## Build and run

```sh
cargo build --release
./target/release/vosulay
```

For command-line options, monitor selection, frame limiting, attachment mode, and IPC controls, see [Build and run](openwiki/operations/build-and-run.md).

## Measured overlay overhead

These are machine-specific observations, not performance promises. On 2026-07-26, a Ryzen 9 9950X3D/RTX 5090 host ran sequential 8-vCPU Windows and Linux VMs through three counterbalanced ON-OFF pairs, with 30 seconds of warmup and 120 seconds of measurement per condition. Both products rendered the same HTML at 1280x720. The table reports the median of the three within-pair ON minus OFF deltas.

| Primary full-stack ON-OFF delta | Upstream tosu on Windows | vosulay on Linux |
| --- | ---: | ---: |
| CPU (percentage points of one logical CPU) | +31.114192 (+3.8893% of 8-vCPU capacity) | +14.000311 (+1.7500% of 8-vCPU capacity) |
| Private memory | +384.654894 MiB | +412.209097 MiB |
| Working set / summed RSS | +498.781637 MiB working set | +1121.972011 MiB summed RSS |
| GPU utilization | +12.214876 percentage points | +2.520661 percentage points |
| VRAM | +19.181818 MiB | +107.512397 MiB |
| GPU power | Not captured | +11.568760 W |

ON ran the game, required daemon, and overlay renderer/host; OFF ran the same game and required daemon with only the overlay renderer/host disabled. The primary paired delta therefore captures all work induced by enabling the overlay, including work charged outside renderer processes, rather than measuring the renderer process tree alone. Process-tree memory sums can double-count shared pages.

Windows and Linux use different display stacks and frame policies. The side-by-side values are therefore directional context for these tested configurations, not a platform-neutral comparison or a universal winner.

## Development

See [Testing guidance](openwiki/testing/guidance.md) for the development and verification workflow.

## Engineering documentation

[OpenWiki](openwiki/quickstart.md) is the canonical source for detailed engineering documentation. Start with the [quickstart](openwiki/quickstart.md) for architecture, workflows, integrations, testing guidance, and the source map.

## Security

`vosulay-overlay` local HTML overlays run in a secure context, but overlay pages are not cross-origin isolated. As a result, `SharedArrayBuffer` is unavailable by default.

The **Allow unrestricted SharedArrayBuffer** setting is a compatibility opt-in. Enabling it requires a restart and affects every remote URL and local HTML overlay. It bypasses a Chromium Spectre mitigation, so use it only when every overlay is fully trusted.

## Configuration

See [Configuration](openwiki/domain/configuration.md) for the configuration model, storage, and persistence behavior.

## License

[MIT](LICENSE)
