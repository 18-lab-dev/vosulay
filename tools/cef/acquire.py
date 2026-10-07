#!/usr/bin/env python3
"""Acquire pinned upstream CEF binaries and package their runtime; never builds CEF."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import tomllib
import shutil
import sys
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PINS = json.loads((Path(__file__).with_name("pins.json")).read_text(encoding="utf-8"))
MAX_MEMBERS = 20_000
MAX_UNPACKED = 3_000_000_000


def product_version() -> str:
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    package_version = manifest.get("package", {}).get("version", {})
    if package_version != {"workspace": True}:
        raise ValueError("Cargo.toml [package].version must inherit from [workspace.package].version")
    version = manifest.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(version, str):
        raise ValueError("Cargo.toml [workspace.package].version is missing")
    return version


def runtime_manifest(root: Path) -> dict[str, str]:
    files = {}
    for directory in ("Release", "Resources"):
        for path in sorted((root / directory).rglob("*")):
            if path.is_symlink():
                raise ValueError(f"symlink in CEF runtime tree: {path}")
            if path.is_file():
                files[path.relative_to(root).as_posix()] = digest(path, "sha256")
    return files


def write_receipt(root: Path, platform: str, pin: dict) -> None:
    receipt = {
        "platform": platform,
        "cef_version": PINS["cef_version"],
        "archive": pin["archive"],
        "archive_sha256": pin["sha256"],
        "product_version": product_version(),
        "runtime_sha256": runtime_manifest(root),
    }
    (root / ".vosulay-cef-verified.json").write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def verify_receipt(root: Path, platform: str, pin: dict) -> None:
    path = root / ".vosulay-cef-verified.json"
    if path.is_symlink() or not path.is_file():
        raise ValueError("verified CEF receipt is missing")
    receipt = json.loads(path.read_text(encoding="utf-8"))
    expected = {
        "platform": platform,
        "cef_version": PINS["cef_version"],
        "archive": pin["archive"],
        "archive_sha256": pin["sha256"],
        "product_version": product_version(),
    }
    if any(receipt.get(key) != value for key, value in expected.items()):
        raise ValueError("verified CEF receipt does not match this platform/pin/product")
    if receipt.get("runtime_sha256") != runtime_manifest(root):
        raise ValueError("CEF runtime tree digest mismatch")

def digest(path: Path, algorithm: str) -> str:
    h = hashlib.new(algorithm)
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def validate_archive(path: Path, pin: dict) -> None:
    if path.stat().st_size != pin["size"]:
        raise ValueError(f"archive size mismatch: {path}")
    for algorithm in ("sha1", "sha256"):
        actual = digest(path, algorithm)
        if actual != pin[algorithm]:
            raise ValueError(f"archive {algorithm} mismatch: expected {pin[algorithm]}, got {actual}")


def safe_members(package: tarfile.TarFile, root_name: str) -> list[tarfile.TarInfo]:
    members = package.getmembers()
    if len(members) > MAX_MEMBERS:
        raise ValueError("archive member limit exceeded")
    total = 0
    for member in members:
        rel = PurePosixPath(member.name)
        if rel.is_absolute() or ".." in rel.parts or not rel.parts or rel.parts[0] != root_name:
            raise ValueError(f"unsafe archive path: {member.name}")
        if not (member.isfile() or member.isdir()):
            raise ValueError(f"unsupported archive member type: {member.name}")
        if member.isfile():
            total += member.size
            if total > MAX_UNPACKED:
                raise ValueError("archive unpacked size limit exceeded")
    return members


def verify_tree(root: Path, pin: dict) -> None:
    if root.name != pin["cef_path_subdir"] or root.is_symlink():
        raise ValueError("CEF_PATH directory name does not match pinned archive")
    root_real = root.resolve()
    for rel in pin["required_files"]:
        p = root / rel
        if p.is_symlink() or not p.is_file() or not p.resolve().is_relative_to(root_real):
            raise ValueError(f"required CEF runtime file missing or unsafe: {rel}")


def acquire(platform: str, cache: Path) -> Path:
    pin = PINS["platforms"][platform]
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / pin["archive"]
    if archive.exists():
        validate_archive(archive, pin)
    else:
        fd, temporary_name = tempfile.mkstemp(prefix=".cef-download-", dir=cache)
        os.close(fd)
        temporary = Path(temporary_name)
        try:
            with urllib.request.urlopen(pin["url"], timeout=90) as response, temporary.open("wb") as out:
                h1, h256, size = hashlib.sha1(), hashlib.sha256(), 0
                while True:
                    block = response.read(1024 * 1024)
                    if not block:
                        break
                    size += len(block)
                    if size > pin["size"]:
                        raise ValueError("download exceeded pinned archive size")
                    out.write(block)
                    h1.update(block)
                    h256.update(block)
            if size != pin["size"] or h1.hexdigest() != pin["sha1"] or h256.hexdigest() != pin["sha256"]:
                raise ValueError("download size/SHA1/SHA256 mismatch")
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)

    root = cache / pin["cef_path_subdir"]
    extracted_now = not root.exists()
    if extracted_now:
        with tempfile.TemporaryDirectory(prefix=".cef-extract-", dir=cache) as staging_name:
            staging = Path(staging_name)
            with tarfile.open(archive, "r:bz2") as package:
                members = safe_members(package, pin["cef_path_subdir"])
                for member in members:
                    destination = staging.joinpath(*PurePosixPath(member.name).parts)
                    if member.isdir():
                        destination.mkdir(parents=True, exist_ok=True)
                    else:
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        source = package.extractfile(member)
                        if source is None:
                            raise ValueError(f"cannot read archive member: {member.name}")
                        with source, destination.open("xb") as out:
                            shutil.copyfileobj(source, out)
                        destination.chmod(0o755 if member.mode & 0o111 else 0o644)
                extracted = staging / pin["cef_path_subdir"]
                verify_tree(extracted, pin)
                extracted.rename(root)
    verify_tree(root, pin)
    if extracted_now:
        write_receipt(root, platform, pin)
    verify_receipt(root, platform, pin)
    return root


def acquire_asdf(cache: Path) -> Path:
    """Acquire the matching native injection DLLs, never from the application."""
    root = cache / PINS["asdf_overlay"]["version"]
    root.mkdir(parents=True, exist_ok=True)
    for pin in PINS["asdf_overlay"]["assets"]:
        path = root / pin["name"]
        if path.exists() or path.is_symlink():
            if path.is_symlink() or not path.is_file() or path.stat().st_size != pin["size"] or digest(path, "sha256") != pin["sha256"]:
                raise ValueError(f"native injection asset digest/type mismatch: {path}")
            continue
        fd, name = tempfile.mkstemp(prefix=".asdf-download-", dir=root)
        temporary = Path(name)
        try:
            with os.fdopen(fd, "wb") as out, urllib.request.urlopen(pin["url"], timeout=90) as response:
                sha256, size = hashlib.sha256(), 0
                while block := response.read(1024 * 1024):
                    size += len(block)
                    if size > pin["size"]:
                        raise ValueError("native asset download exceeded pinned size")
                    sha256.update(block)
                    out.write(block)
            if size != pin["size"] or sha256.hexdigest() != pin["sha256"]:
                raise ValueError(f"native injection asset digest mismatch: {pin['name']}")
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    return root


def package_runtime(platform: str, cef: Path, output: Path, executables: list[Path], overlays: list[Path], client: Path | None = None) -> None:
    pin = PINS["platforms"][platform]
    verify_tree(cef, pin)
    verify_receipt(cef, platform, pin)
    if client is not None:
        if platform != "windows-x86_64" or client.suffix.lower() != ".dll" or not client.is_file() or client.is_symlink():
            raise ValueError("--client requires a regular Windows CEF client DLL")
    if output.exists():
        raise ValueError(f"output already exists: {output}")
    output.mkdir(parents=True)
    try:
        for name in pin["package_release_files"]:
            shutil.copy2(cef / "Release" / name, output / name)
        resources = output
        for name in pin["package_resources"]:
            source = cef / "Resources" / name
            destination = resources / name
            if source.is_dir():
                shutil.copytree(source, destination, symlinks=False)
            else:
                shutil.copy2(source, destination)
        for name in ("LICENSE.txt", "CREDITS.html"):
            shutil.copy2(cef / name, output / name)
        for item in executables:
            if not item.is_file() or item.is_symlink():
                raise ValueError(f"executable input must be a regular file: {item}")
            dest = output / item.name
            if dest.exists():
                raise ValueError(f"duplicate package basename: {item.name}")
            shutil.copy2(item, dest)
        if client is not None:
            for source, basename in ((client, client.name), (cef / "Release/bootstrap.exe", client.with_suffix(".exe").name)):
                if (output / basename).exists():
                    raise ValueError(f"duplicate package basename: {basename}")
                shutil.copy2(source, output / basename)
        for item in overlays:
            if item.is_symlink() or not item.exists() or not (item.is_file() or item.is_dir()):
                raise ValueError(f"overlay input must be a regular file or directory: {item}")
            dest = output / item.name
            if dest.exists():
                raise ValueError(f"duplicate package basename: {item.name}")
            if item.is_dir():
                if any(p.is_symlink() for p in item.rglob("*")):
                    raise ValueError(f"overlay directory contains symlinks: {item}")
                shutil.copytree(item, dest, symlinks=False)
            else:
                shutil.copy2(item, dest)
        (output / "vosulay-cef.json").write_text(json.dumps({
            "product_version": product_version(),
            "cef_version": PINS["cef_version"],
            "platform": platform,
            "archive": pin["archive"],
            "archive_sha256": pin["sha256"],
            "runtime_sha256": {
                p.relative_to(output).as_posix(): digest(p, "sha256")
                for p in sorted(output.rglob("*")) if p.is_file() and p.name != "vosulay-cef.json"
            },
        }, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    except BaseException:
        shutil.rmtree(output, ignore_errors=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    get = sub.add_parser("acquire", help="download/verify/extract pinned CEF")
    get.add_argument("--platform", choices=PINS["platforms"], required=True)
    get.add_argument("--cache", type=Path, default=Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "vosulay/cef")
    native = sub.add_parser("acquire-asdf", help="download/verify pinned native Windows injection DLLs")
    native.add_argument("--cache", type=Path, default=Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "vosulay/asdf")
    pack = sub.add_parser("package", help="package runtime beside built executables/assets")
    pack.add_argument("--platform", choices=PINS["platforms"], required=True)
    pack.add_argument("--cef-path", type=Path, required=True)
    pack.add_argument("--output", type=Path, required=True)
    pack.add_argument("--exe", type=Path, action="append", default=[])
    pack.add_argument("--overlay", type=Path, action="append", default=[])
    pack.add_argument("--client", type=Path, help="Windows client DLL; package the pinned sandbox bootstrap with its basename")
    args = parser.parse_args()
    if args.command == "acquire":
        print(acquire(args.platform, args.cache).resolve())
    elif args.command == "acquire-asdf":
        print(acquire_asdf(args.cache).resolve())
    else:
        package_runtime(args.platform, args.cef_path.resolve(), args.output.resolve(), args.exe, args.overlay, args.client)
        print(args.output.resolve())
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, tarfile.TarError, urllib.error.URLError) as error:
        print(f"cef-acquire: {error}", file=sys.stderr)
        raise SystemExit(1)
