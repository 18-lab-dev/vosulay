# Changelog

## 1.5.2 - 2026-10-27

- Isolate native backend overlay sources on separate loopback sites, keeping Chromium site isolation, so sibling animation widgets no longer multiply each other's requestAnimationFrame callbacks and canvas flushes. Use an acknowledged native bootstrap before granting broker routing; retain bounded listeners to prevent origin takeover. Same-source instances keep shared storage, and remote viewers/preview capabilities remain unchanged.
- Bound native site allocation to eight in-flight requests so large profiles reach the shared-site fallback without overflowing the bridge inbox.

## 1.5.1 - 2026-10-27

- Add off-by-default, loopback-only DevTools debugging in Advanced settings, with a configurable port, restart notice, active endpoint indicator and port-conflict reporting.
- Stabilize listener-shutdown and ruleset-worker regression tests against parallel port allocation and asynchronous save broadcasts, retaining strict shutdown, rebind and no-redundant-render assertions.

## 1.5.0 - 2026-10-25

- Restore styled/live tool-manager counters using independent, per-run 256-bit loopback preview hostnames, not broad `Origin: null` admission. Keep opaque frames and all manager/config/native-command boundaries; disclose capabilities only through authenticated manager routes, rotate them on restart, and show a local-only explanation for remote viewers. Add hostile-origin, local-root, browser/CEF resolver and standalone-parity coverage.
- Keep the Windows native worker's command receiver alive across connection/display errors and panics, release the failed connection's input, and reconnect with the original failure in the tool log instead of masking it as a closed command channel.
- Make early Windows OpenGL attachment safe across startup context replacement and reentrant resize callbacks: perform native setup outside registry write locks, keep renderers context-owned, and clean up only in the owning context. Add optional native hook diagnostics and include the revision-marked asdf Rust overrides in source bundles.
- Recover browser-editor routing after a vosulay restart even when configuration revisions repeat. Reuse only surviving authority; pause editing during outages and show a read-only explanation plus Open from vosu when a tool session expires, instead of leaving counter frames pointed at a dead broker.
- Move official repository, catalog, support and package URLs to `18-lab-dev`. Existing published package hashes remain paired with their original versions until release artifacts are available.
- Attribute the project's own code to 18-LAB, retaining K4zoku's copyright and sxlvia's separate branding/artwork licences.
- Cap the attached in-game editor at 30 fps without changing the saved/game overlay limit; lower user limits still win and closing edit mode restores the resolved limit immediately. Keep idle painting damage-driven. Extend the private CEF benchmark with hardware-pinned BGRA OSR, idle checks and compositor diagnostics, distinguishing production GPU composition from the no-GPU SwiftShader fallback.
- Add isolated native-traffic and CEF manager benchmarks, optional malloc-family instrumentation, and a queue-barrier regression proving repeated ruleset samples do not save, redraw, or override manual profile selection.
- Align editor colours, typography, spacing, corner radii and focus treatment with vosu's shared 18-LAB system, preserving vosulay's layout and standalone rendering.
- Let vosu own an enabled vosulay tool's lifetime. Standalone app-menu, autostart, and CLI launches hand off through vosu's authenticated native tool-launch API when vosu is running; absent/unreachable vosu or a disabled tool retains standalone operation.
- Ask a compatible standalone host to flush and yield before supervised startup, with bounded waiting and no forced peer termination. Refused takeover exits cleanly and is shown by vosu as stopped instead of crash-restarting.
- Replace the managed tray's Exit action with Open in vosu dashboard. Keep standalone tray behavior, and stop managed hosts through vosu's dashboard or vosu shutdown.
- Give the vosu catalog card its own icon and banner in the family of vosu's first-party tools, following the dashboard's light and dark modes.
- vosulay is now an 18-LAB product. The overlay-frame mark keeps its shape in 18-LAB colours (amber or blueprint frames, V and handles, signal-coloured node) with the 1.x wordmark and an 18-LAB endorsement; light and dark README banners and a 1280×640 social preview are new. 16 and 24 px icons are pixel-fitted, and the Windows tray picks the rendered icon for the display scale instead of shrinking one large bitmap. The installer, executable and packages name 18-LAB as publisher. Rules are in [branding](docs/branding.md).
