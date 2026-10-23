#!/usr/bin/env python3
"""Render vosulay's brand rasters (PNG, ICO) from the SVG masters in assets/branding/.

The masters are the source; every raster here is generated and committed, so builds
and packaging need no image tools. Requires `resvg` on PATH (rules in
docs/branding.md). `--check` renders to a temporary directory and fails if a
committed raster differs.
"""

import argparse
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BRANDING = Path("assets/branding")


# Pixel-fitted masters draw the smallest sizes on whole pixels; the rest scale the master.
def icon_source(size):
    if size <= 16:
        return BRANDING / "vosulay-app-icon-16.svg"
    if size <= 24:
        return BRANDING / "vosulay-app-icon-24.svg"
    return BRANDING / "vosulay-app-icon.svg"


# Windows tray: the small-icon size at 100-300 % display scaling (SM_CXSMICON).
ICON_PNGS = [16, 20, 24, 32, 40, 48]
ICOS = {
    # vosulay.exe (the branded CEF bootstrap), its shortcuts and the installer's uninstall entry.
    BRANDING / "vosulay.ico": [16, 20, 24, 32, 40, 48, 64, 128, 256],
}
PREVIEWS = {BRANDING / "vosulay-social-preview.png": (BRANDING / "vosulay-social-preview.svg", 1280, 640)}


def render(svg, png, width, height):
    subprocess.run(["resvg", "--width", str(width), "--height", str(height), str(ROOT / svg), str(png)],
                   check=True)


def decode_rgba(data):
    """Decode resvg's 8-bit RGBA, non-interlaced PNG output."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, header = 8, b"", None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    width, height, depth, color, _, _, interlace = header
    if (depth, color, interlace) != (8, 6, 0):
        raise ValueError(f"unsupported PNG layout {header}")
    raw, stride, rows, prev = zlib.decompress(idat), width * 4, [], bytearray(width * 4)
    for y in range(height):
        kind, line = raw[y * (stride + 1)], bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            left = line[i - 4] if i >= 4 else 0
            up, corner = prev[i], prev[i - 4] if i >= 4 else 0
            if kind == 1:
                line[i] = (line[i] + left) & 255
            elif kind == 2:
                line[i] = (line[i] + up) & 255
            elif kind == 3:
                line[i] = (line[i] + (left + up) // 2) & 255
            elif kind == 4:
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                line[i] = (line[i] + (left if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 255
        rows.append(bytes(line))
        prev = line
    return width, height, rows


def bmp_entry(png):
    """32-bit BGRA DIB with an AND mask, the classic ICO frame every Windows API reads."""
    width, height, rows = decode_rgba(png)
    pixels = b"".join(bytes(row[x * 4 + c] for x in range(width) for c in (2, 1, 0, 3))
                      for row in reversed(rows))
    mask_stride = (width + 31) // 32 * 4
    mask = bytearray()
    for row in reversed(rows):
        bits = bytearray(mask_stride)
        for x in range(width):
            if row[x * 4 + 3] == 0:
                bits[x // 8] |= 0x80 >> (x % 8)
        mask += bits
    header = struct.pack("<IiiHHIIiiII", 40, width, height * 2, 1, 32, 0, len(pixels) + len(mask), 0, 0, 0, 0)
    return header + pixels + bytes(mask)


def write_ico(path, frames):
    """frames: {size: png bytes}. 256 px stays PNG-compressed, smaller frames are DIBs."""
    images = [(size, png if size >= 256 else bmp_entry(png)) for size, png in sorted(frames.items())]
    out = bytearray(struct.pack("<HHH", 0, 1, len(images)))
    offset = 6 + 16 * len(images)
    for size, data in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in images:
        out += data
    path.write_bytes(bytes(out))


def build(out):
    """Render every output under `out`, mirroring repository paths. Returns the relative paths."""
    written = []
    cache = out / ".sizes"
    cache.mkdir(parents=True)
    sizes = sorted(set(ICON_PNGS) | {s for v in ICOS.values() for s in v})
    for size in sizes:
        render(icon_source(size), cache / f"{size}.png", size, size)
    icons = out / BRANDING / "icons"
    icons.mkdir(parents=True)
    for size in ICON_PNGS:
        target = BRANDING / "icons" / f"vosulay-{size}.png"
        shutil.copyfile(cache / f"{size}.png", out / target)
        written.append(target)
    for ico, ico_sizes in ICOS.items():
        (out / ico).parent.mkdir(parents=True, exist_ok=True)
        write_ico(out / ico, {s: (cache / f"{s}.png").read_bytes() for s in ico_sizes})
        written.append(ico)
    for png, (svg, width, height) in PREVIEWS.items():
        render(svg, out / png, width, height)
        written.append(png)
    shutil.rmtree(cache)
    return written


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if a committed raster differs from a fresh render")
    args = parser.parse_args()
    if shutil.which("resvg") is None:
        print("error: resvg is required (see docs/branding.md)", file=sys.stderr)
        return 1
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp)
        written = build(out)
        if args.check:
            stale = [str(p) for p in written
                     if not (ROOT / p).is_file() or (ROOT / p).read_bytes() != (out / p).read_bytes()]
            if stale:
                print("stale rasters: " + ", ".join(stale) + "; run tools/branding/render.py", file=sys.stderr)
                return 1
            print(f"All {len(written)} brand rasters match their masters")
            return 0
        stale_icons = set((ROOT / BRANDING / "icons").glob("*.png")) - {ROOT / p for p in written}
        for path in stale_icons:
            path.unlink()
        for path in written:
            (ROOT / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(out / path, ROOT / path)
    print(f"Rendered {len(written)} brand rasters")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
