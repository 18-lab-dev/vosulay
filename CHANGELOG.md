# Changelog

## Unreleased

- Let vosu own an enabled vosulay tool's lifetime. Standalone app-menu, autostart, and CLI launches hand off through vosu's authenticated native tool-launch API when vosu is running; absent/unreachable vosu or a disabled tool retains standalone operation.
- Ask a compatible standalone host to flush and yield before supervised startup, with bounded waiting and no forced peer termination. Refused takeover exits cleanly and is shown by vosu as stopped instead of crash-restarting.
- Replace the managed tray's Exit action with Open in vosu dashboard. Keep standalone tray behavior, and stop managed hosts through vosu's dashboard or vosu shutdown.
