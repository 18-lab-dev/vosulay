"""Stage digest-checked locked crates without invoking Cargo or the network."""
import hashlib
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def stable_member(info):
    info.uid = info.gid = info.mtime = 0
    info.uname = info.gname = ""
    return info


def locked_registry(package, prefix=""):
    registry = Path.home() / ".cargo/registry"
    indexes = sorted((registry / "index").iterdir())

    def add(path):
        package.add(path, arcname=prefix + path.relative_to(registry.parent).as_posix(), filter=stable_member)

    for index in indexes:
        config = index / "config.json"
        if config.is_file():
            add(config)
    for crate in tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]:
        if not crate.get("source", "").startswith("registry+"):
            continue
        name = crate["name"]
        archives = list((registry / "cache").glob(f"*/{name}-{crate['version']}.crate"))
        if len(archives) != 1:
            raise RuntimeError(f"missing/ambiguous locked crate: {name} {crate['version']}")
        with archives[0].open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if digest != crate["checksum"]:
            raise RuntimeError(f"locked crate digest mismatch: {name} {crate['version']}")
        add(archives[0])
        if len(name) <= 2:
            relative = f"{len(name)}/{name}"
        elif len(name) == 3:
            relative = f"3/{name[0]}/{name}"
        else:
            relative = f"{name[:2]}/{name[2:4]}/{name}"
        entries = [index / ".cache" / relative for index in indexes if (index / ".cache" / relative).is_file()]
        if not entries:
            raise RuntimeError(f"missing offline registry metadata: {name}")
        for entry in entries:
            add(entry)
