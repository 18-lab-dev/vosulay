#!/usr/bin/env python3
"""Transfer cached inputs and execute the frozen workload only inside Linux KVM."""
from __future__ import annotations

import gzip
import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import tarfile
import tempfile
from offline import locked_registry, stable_member

ROOT = Path(__file__).resolve().parents[2]
ALIAS = "vosu@127.0.0.1"
SDK_NAME = "cef_binary_152.0.8+g1ce985c+chromium-152.0.7977.134_linux64_minimal"
SSH = ["ssh", "-p", "2222", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=10", ALIAS]
SFTP = ["sftp", "-P", "2222", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=10"]


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def quote_sftp(path):
    return '"' + str(path).replace("\\", "\\\\").replace('"', '\\"') + '"'


def cached_inputs(state):
    """Copy locked crates, one Python install and the cached Pillow distribution."""
    home = Path.home()
    sdk = home / ".cache/vosulay/cef" / SDK_NAME
    weston = ROOT / "target/linux-native-proof/weston/usr"
    python = home / ".local/share/uv/python/cpython-3.13.13-linux-x86_64-gnu"
    wheel = (home / ".cache/uv/wheels-v6/pypi/pillow" /
             "12.3.0-cp313-cp313-manylinux_2_27_x86_64.manylinux_2_28_x86_64").resolve(strict=True)
    for path in (sdk / ".vosulay-cef-verified.json", weston / "bin/weston", python / "bin/python3.13", wheel / "PIL"):
        if not path.exists():
            raise RuntimeError(f"offline staging prerequisite missing: {path}")


    archive = state / "linux-inputs.tar.gz"
    with archive.open("wb") as destination, gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0, compresslevel=1) as compressed, tarfile.open(fileobj=compressed, mode="w") as package:
        for path, name in ((sdk, "cef/" + SDK_NAME), (weston, "weston/usr"),
                           (python, "python/" + python.name), (wheel, "pillow")):
            package.add(path, arcname=name, filter=stable_member)
        locked_registry(package, "cargo/")
    return archive


def run_linux(args, status, state, archive):
    linux = status.get("linux_service", {}).get("ActiveState")
    windows = status.get("windows_service", {}).get("ActiveState")
    if linux != "active" or windows not in {"inactive", "failed"}:
        raise RuntimeError(f"refusing Linux dispatch (Linux={linux}, Windows={windows})")
    root = args.linux_root
    if not root.startswith("~/") or ".." in Path(root[2:]).parts:
        raise ValueError("linux_root must be a task-owned path below the canonical guest home")
    remote = root[2:]
    state, archive = Path(state).resolve(), Path(archive).resolve(strict=True)
    digest = sha256(archive)
    inputs = cached_inputs(state)
    inputs_digest = sha256(inputs)
    args.output.mkdir(parents=True, exist_ok=True)
    subprocess.run([*SSH, "mkdir -p " + shlex.quote(remote)], check=True)
    with tempfile.TemporaryDirectory(prefix="vosulay-linux-transfer-") as temp:
        batch = Path(temp) / "put.sftp"
        batch.write_text("\n".join((
            f"put {quote_sftp(archive)} {quote_sftp(remote + '/source.tar.gz')}",
            f"put {quote_sftp(inputs)} {quote_sftp(remote + '/inputs.tar.gz')}",
            f"put {quote_sftp(Path(__file__).with_name('linux_guest.sh'))} {quote_sftp(remote + '/linux_guest.sh')}", "")))
        subprocess.run([*SFTP, "-b", str(batch), ALIAS], check=True)
        command = f"cd {shlex.quote(remote)} && bash linux_guest.sh {digest} {inputs_digest} {args.cpu_seconds} {args.delivery_seconds} {args.warmup} {args.repeats}"
        result = subprocess.run([*SSH, command])
        batch.write_text(f"get -r {quote_sftp(remote + '/reports')} {quote_sftp(args.output)}\n")
        pulled = subprocess.run([*SFTP, "-b", str(batch), ALIAS])
        receipt = {"guest": ALIAS, "source_sha256": digest, "inputs_sha256": inputs_digest,
                   "linux_service": status["linux_service"], "guest_exit_code": result.returncode,
                   "reports_pulled": pulled.returncode == 0}
        (args.output / "guest-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        if result.returncode:
            return result.returncode
        return pulled.returncode
