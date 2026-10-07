#!/usr/bin/env python3
"""Build and package the pinned Windows CEF runtime for Vosulay."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ACQUIRE = ROOT / "tools" / "cef" / "acquire.py"


def acquire(*args: str) -> str:
    return subprocess.check_output([sys.executable, str(ACQUIRE), *args], cwd=ROOT, text=True).strip()


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: package_windows.py OUTPUT_DIRECTORY")
    output = Path(sys.argv[1]).resolve()
    platform = "windows-x86_64"
    cef_cache = Path.home() / ".cache" / "vosulay" / "cef"
    cef_root = Path(acquire("acquire", "--platform", platform, "--cache", str(cef_cache)))
    os.environ["CEF_PATH"] = str(cef_root)
    github_env = os.environ.get("GITHUB_ENV")
    if github_env:
        with open(github_env, "a", encoding="utf-8") as stream:
            stream.write(f"CEF_PATH={cef_root}\n")

    pins = json.loads((ROOT / "tools/cef/pins.json").read_text(encoding="utf-8"))
    overlay_cache = Path.home() / ".cache" / "vosulay" / "asdf"
    asdf_root = Path(acquire("acquire-asdf", "--cache", str(overlay_cache)))
    subprocess.run(
        ["cargo", "build", "--release", "--lib", "--locked", "--target", "x86_64-pc-windows-msvc"],
        cwd=ROOT, env=os.environ, check=True,
    )

    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    target = ROOT / os.environ.get("CARGO_TARGET_DIR", "target")
    product = target / "x86_64-pc-windows-msvc" / "release" / "vosulay.dll"
    if not product.is_file():
        raise SystemExit(f"Windows client DLL not found: {product}")
    acquire("package", "--platform", platform, "--cef-path", str(cef_root), "--output", str(output), "--client", str(product),
            "--overlay", str(asdf_root / pins["asdf_overlay"]["assets"][0]["name"]),
            "--overlay", str(asdf_root / pins["asdf_overlay"]["assets"][1]["name"]))
    archive = output.parent / f"vosulay-{version}-windows-x86_64.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as package:
        for path in sorted(output.rglob("*")):
            if path.is_file():
                package.write(path, Path("vosulay") / path.relative_to(output))
    print(archive)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
