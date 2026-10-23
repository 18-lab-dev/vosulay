# vosulay branding

vosulay is an 18-LAB product (stage code 18-02). Its art shares vosu's rules and colours (vosu `docs/branding.md`) and the first-party tool art family (vosu-tools `docs/art.md`). The terms are in [`assets/branding/LICENSE.md`](../assets/branding/LICENSE.md).

## Mark

<img src="../assets/branding/vosulay-mark.svg" alt="" width="96">

osu!'s hit circle in ink with the product's part in accent in front of it; vosu uses the same rule with a V. For vosulay the part is the overlay panel, laid out like its catalog icon (`vosu-tool-icon.svg`).

- Drawn on the 18-LAB mark's 120-unit grid: circle centre (50, 70), radius 28, 12-unit stroke; panel from (58, 16) to (104, 56) with a 12-unit frame, 10-unit outer radius and a 14×6 bar. The circle is cut 6 units around the panel. Outer extents are x and y 16–104.
- Never recolour beyond the two roles below, outline, rotate, add effects, or separate the panel from the circle. No ppy or osu! logos.
- 16 and 24 px have pixel-fitted masters (`vosulay-app-icon-16.svg`, `vosulay-app-icon-24.svg`, 2- and 3-pixel strokes, no bar). From 32 px up the master scales.

| Role | Light (DRAFTING) | Dark (WORKSTATION) |
| --- | --- | --- |
| Circle, wordmark | `#132030` | `#ebe8e1` |
| Panel (accent) | `#175fad` | `#ff963f` |
| Rule, endorsement | `#a7b2bd`, `#3b4858` | `#3b3f45`, `#b8b5ad` |

The app icon is the mark at 0.8 scale on a graphite (`#111214`) tile with 12 % corner radius, in both modes. The editor toolbar draws the mark inline with `--fg` and `--accent`.

## Lockup

`vosulay-logo.svg` (light surfaces) and `vosulay-logo-on-dark.svg`: mark, the lowercase wordmark in Barlow Condensed 600 (x-height 46 units, baseline 90), a hairline rule, and the 18-LAB logotype as endorsement in one ink colour (cap height 20). The 18-LAB logotype's mark stands in for "18"; never set a typed "18" beside it. Keep 16 units of clear space. Use it at 64 px height or more, where the endorsement stays legible; smaller, use the mark alone.

## Files

| File | Use |
| --- | --- |
| `vosulay-app-icon.svg` | Master app icon; Linux `hicolor/scalable` icon (packages and the self-registered desktop entry) |
| `vosulay-app-icon-16.svg`, `vosulay-app-icon-24.svg` | Pixel-fitted small masters |
| `vosulay-mark.svg` | Mark without tile, follows `prefers-color-scheme` |
| `vosulay-logo.svg`, `vosulay-logo-on-dark.svg` | Lockup |
| `vosulay-banner.svg`, `vosulay-banner-light.svg` | README header, 1280×320, chosen with `<picture>` |
| `vosulay-social-preview.svg` / `.png` | GitHub repository social preview, 1280×640; upload the PNG in the repository settings |
| `vosulay.ico` | `vosulay.exe`, shortcuts and the uninstall entry: 16, 20, 24, 32, 40, 48, 64, 128 (DIB) and 256 (PNG) |
| `icons/vosulay-<size>.png` | Windows tray, 16–48 px; the tray picks the size for the display scale |
| `vosu-tool-icon.svg`, `vosu-tool-banner.svg` | vosu catalog card art (tool art family) |

## Rendering

The SVG files are the source; every raster is generated and committed, so builds and packages need no image tools. After changing a master:

```sh
python3 tools/branding/render.py           # needs resvg on PATH (0.48)
python3 tools/branding/render.py --check   # committed rasters match the masters
```

Text in the logos, banners and social preview is outlined, so it renders without fonts on GitHub and in image viewers. It was set in Barlow Condensed 600, Barlow 400 and IBM Plex Mono 500 (Fontsource Latin subsets, as in vosu's `assets/server/`) with HarfBuzz shaping and fontTools outlines. To change a line, outline the new text from the same fonts, keep sizes and positions, and re-render.
