# vosulay

Native Linux in-game overlay manager for vosu, built with Rust and Qt 6.

vosulay displays vosu counters in a frameless overlay and provides an edit mode for arranging them. Layouts are stored as profiles, and overlays can come from the vosu counter catalog, HTTP(S) URLs, or local HTML files.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Maintain multiple overlay profiles.
- Discover installed counters from a local vosu instance.
- Add remote or local custom HTML overlays.
- Run click-through overlays on Wayland and X11.
- Control visibility, edit mode, geometry, and shutdown through local IPC.

## Requirements

- Linux with Wayland or X11
- Rust 1.96.1 (installed automatically by `rustup` from `rust-toolchain.toml`)
- A C++17 compiler
- CMake and pkg-config
- Qt 6.8 or newer with:
  - Core
  - Widgets
  - WebEngineWidgets
  - WebChannel
  - Network
- LayerShellQt development files
- A running vosu instance for discovering and using vosu counters; the default URL is `http://127.0.0.1:24050`

Package names vary by distribution. The build also accepts `Qt6_DIR`, `Qt6_ROOT`, `LayerShellQt_DIR`, `LayerShellQt_ROOT`, `CMAKE_PREFIX_PATH`, `CMAKE_TOOLCHAIN_FILE`, and `QT_MOC` for non-system installations.

## Build and run

```sh
cargo build --release
./target/release/vosulay
```

Start directly in edit mode:

```sh
./target/release/vosulay --edit
```

Use a different vosu endpoint:

```sh
./target/release/vosulay --vosu-url http://127.0.0.1:24050
```

Run `./target/release/vosulay --help` for monitor selection, frame limiting, attachment mode, and IPC controls.

## Development

The native Qt dependencies are required even for Rust tests because `build.rs` compiles the C++ shell.

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Start with the [OpenWiki quickstart](openwiki/quickstart.md) for architecture, workflows, integrations, testing guidance, and the source map.

## Security

Overlays are active web content. Remote URLs, vosu counters, and local HTML files can execute JavaScript inside Qt WebEngine and may make network requests. Only add overlays from sources you trust.

Selecting a local HTML file authorizes that overlay to load readable files from the selected file's directory. vosulay blocks access outside that directory, but the restriction does not make untrusted HTML safe. Avoid selecting files from directories that contain sensitive data.

## Configuration

Configuration is stored in the platform user configuration directory under `vosulay/config.json`. Writes are atomic. A missing configuration starts with an empty `Default` profile.

## License

[MIT](LICENSE)
