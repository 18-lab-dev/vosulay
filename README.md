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

See [Build and run](openwiki/operations/build-and-run.md) for supported platforms, native dependencies, and setup instructions.

## Build and run

```sh
cargo build --release
./target/release/vosulay
```

For command-line options, monitor selection, frame limiting, attachment mode, and IPC controls, see [Build and run](openwiki/operations/build-and-run.md).

## Development

See [Testing guidance](openwiki/testing/guidance.md) for the development and verification workflow.

## Engineering documentation

[OpenWiki](openwiki/quickstart.md) is the canonical source for detailed engineering documentation. Start with the [quickstart](openwiki/quickstart.md) for architecture, workflows, integrations, testing guidance, and the source map.

## Security

Overlays are active web content and may execute JavaScript or make network requests. Only add overlays from sources you trust. See [vosu and overlays](openwiki/integrations/vosu-and-overlays.md) for the security model and local-file authorization boundaries.

## Configuration

See [Configuration](openwiki/domain/configuration.md) for the configuration model, storage, and persistence behavior.

## License

[MIT](LICENSE)
