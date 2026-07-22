# vosulay

Cross-platform in-game overlay manager for vosu. Linux uses Rust and Qt 6; Windows injects an Electron shared-GPU surface directly into the osu! game window.

vosulay displays vosu counters and provides an edit mode for arranging them. Layouts are stored as profiles, and overlays can come from the vosu counter catalog, HTTP(S) URLs, or local HTML files. vosu remains a separate process; the Windows in-game renderer is packaged only with vosulay.

## Features

- Position, resize, scale, rotate, reorder, and adjust the opacity of overlays.
- Maintain multiple overlay profiles.
- Discover installed counters from a local vosu instance.
- Add remote or local custom HTML overlays.
- Run click-through overlays on Wayland and X11.
- Render directly in osu! stable and lazer windows on Windows.
- Control Linux visibility, edit mode, geometry, and shutdown through local IPC.

## Linux

Requirements and native dependencies are documented in [Build and run](openwiki/operations/build-and-run.md).

```sh
cargo build --release
./target/release/vosulay
```

For Linux command-line options, monitor selection, frame limiting, attachment mode, and IPC controls, see [Build and run](openwiki/operations/build-and-run.md).

## Windows

The Windows host requires 64-bit Windows and a separately running vosu instance. It follows the most recently focused discovered `osu!.exe` or `osulazer.exe` client, remains attached while another application has focus, and switches only when another game client receives focus. It attaches only that game client rather than both simultaneously, and renders the manager UI through `@asdf-overlay`'s shared GPU surface inside the game presentation rather than a transparent desktop window. The composed overlay is therefore available to OBS Game Capture.

Press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd> in game to enter or leave edit mode. Reload, edit, and exit controls are also available from the system tray.

If a native input capture or release request stalls for five seconds, vosulay exits immediately so Windows closes the injected connection and restores input to osu!. Restart vosulay to attach again.

To build from source with Node.js 22 or newer:

```powershell
cd windows
npm ci
npm run typecheck
npm test
npm run dist:win
```

Run `windows\dist\win-unpacked\vosulay.exe` during development or install the generated setup executable. The Windows host accepts `--vosu-url`/`-t`, `--fps-limit`/`-f`, repeatable `--pid`, and repeatable `--process-name` options.

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
