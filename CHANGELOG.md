# Changelog

## Unreleased

- Align editor colours, typography, spacing, corner radii and focus treatment with vosu's shared 18-LAB system, preserving vosulay's layout and standalone rendering.
- Let vosu own an enabled vosulay tool's lifetime. Standalone app-menu, autostart, and CLI launches hand off through vosu's authenticated native tool-launch API when vosu is running; absent/unreachable vosu or a disabled tool retains standalone operation.
- Ask a compatible standalone host to flush and yield before supervised startup, with bounded waiting and no forced peer termination. Refused takeover exits cleanly and is shown by vosu as stopped instead of crash-restarting.
- Replace the managed tray's Exit action with Open in vosu dashboard. Keep standalone tray behavior, and stop managed hosts through vosu's dashboard or vosu shutdown.
- Give the vosu catalog card its own icon and banner in the family of vosu's first-party tools, following the dashboard's light and dark modes.
- vosulay is now an 18-LAB product. A new mark (osu!'s hit circle in ink with an accent overlay panel in front of it, matching the catalog icon), a lowercase Barlow Condensed wordmark with the 18-LAB endorsement, light and dark README banners and a 1280×640 social preview replace the violet overlay frame across the editor toolbar, Windows executable, shortcuts and installer, and the Linux desktop icon. 16 and 24 px icons are pixel-fitted, and the Windows tray picks the rendered icon for the display scale instead of shrinking one large bitmap. The installer, executable and packages name 18-LAB as publisher. Rules are in [branding](docs/branding.md).
