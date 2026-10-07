#!/usr/bin/env python3
"""Dispatch benchmarks to the single running GPU guest; never execute locally."""
from __future__ import annotations

import argparse
import fcntl
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tarfile
from offline import locked_registry

ROOT = Path(__file__).resolve().parents[2]
STATE = ROOT / "target/autoresearch-vm"
WINDOWS_VM = Path("/home/user/.local/bin/windows-vm")
CUA_WINDOWS = Path("/home/user/workspaces/manager/cua-integration/windows.py")
WINDOWS_ROOT = r"V:\agent-workspaces\vosulay-autoresearch-20261007"
EXCLUDED = {".git", ".claude", ".agents", ".omp", ".herdr", "target", "node_modules", ".venv", "__pycache__"}


def run(command, **kwargs):
    return subprocess.run(list(map(str, command)), check=True, **kwargs)


def ps_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def select_guest(status):
    windows = status.get("windows_service", {}).get("ActiveState")
    linux = status.get("linux_service", {}).get("ActiveState")
    if windows == "active" and linux in {"inactive", "failed"}:
        return "windows"
    if linux == "active" and windows in {"inactive", "failed"}:
        return "linux"
    raise RuntimeError(f"no unambiguous running guest (Windows={windows}, Linux={linux}); no VM was started or stopped")


def snapshot():
    """Package current component sources, with stable tar/gzip metadata."""
    archive = STATE / "source.tar.gz"
    with archive.open("wb") as destination:
        with gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as package:
                for directory, directories, files in ROOT.walk():
                    directories[:] = sorted(name for name in directories if name not in EXCLUDED)
                    for name in sorted(files):
                        if name in EXCLUDED or name.endswith((".pyc", ".pyo")) or (name.startswith(".env") and name != ".env.example"):
                            continue
                        path = directory / name
                        if path.is_symlink():
                            raise RuntimeError(f"source snapshot refuses symlink: {path.relative_to(ROOT)}")
                        info = package.gettarinfo(str(path), arcname=path.relative_to(ROOT).as_posix())
                        info.uid = info.gid = info.mtime = 0
                        info.uname = info.gname = ""
                        with path.open("rb") as stream:
                            package.addfile(info, stream)
    return archive


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def launched_pid(result):
    if isinstance(result, dict):
        if isinstance(result.get("pid"), int):
            return result["pid"]
        for value in result.values():
            found = launched_pid(value)
            if found:
                return found
    elif isinstance(result, list):
        for value in result:
            found = launched_pid(value)
            if found:
                return found
    return None


def run_windows(args, status, state, archive):
    if select_guest(status) != "windows":
        raise RuntimeError("Windows guest is not selected")
    run([WINDOWS_VM, "doctor"])
    digest = sha256(archive)
    task = WINDOWS_ROOT
    source = task + "\\source-" + digest[:16]
    remote_archive = task + "\\source.tar.gz"
    run([WINDOWS_VM, "exec", f"New-Item -ItemType Directory -Force -Path {ps_quote(task)} | Out-Null"])
    run([WINDOWS_VM, "put", archive, remote_archive])
    run([WINDOWS_VM, "exec", f"""
$archive = {ps_quote(remote_archive)}; $source = {ps_quote(source)}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ine {ps_quote(digest)}) {{ throw 'Source archive digest mismatch' }}
if (-not (Test-Path -LiteralPath (Join-Path $source '.complete'))) {{
 New-Item -ItemType Directory -Force -Path $source | Out-Null
 & tar.exe -xzf $archive -C $source
 if ($LASTEXITCODE -ne 0) {{ throw 'Source snapshot extraction failed' }}
 New-Item -ItemType File -Path (Join-Path $source '.complete') | Out-Null
}}
"""])
    # Stable archive mtimes must not make changed sources look older than the
    # shared Cargo target's dep-info. Refresh source mtimes on every dispatch,
    # including when returning to an already extracted snapshot.
    run([WINDOWS_VM, "exec", f"""
Get-ChildItem -LiteralPath {ps_quote(source)} -Recurse -File | ForEach-Object {{
 $_.LastWriteTimeUtc = [DateTime]::UtcNow
}}
"""])
    cargo_archive = state / "cargo-inputs.tar.gz"
    with cargo_archive.open("wb") as destination, gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0, compresslevel=1) as compressed, tarfile.open(fileobj=compressed, mode="w") as package:
        locked_registry(package)
    remote_cargo = task + "\\cargo-inputs.tar.gz"
    run([WINDOWS_VM, "put", cargo_archive, remote_cargo])
    run([WINDOWS_VM, "exec", f"""
$archive = {ps_quote(remote_cargo)}; $cargo = Join-Path {ps_quote(task)} 'cargo-home'
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ine {ps_quote(sha256(cargo_archive))}) {{ throw 'Cargo archive digest mismatch' }}
New-Item -ItemType Directory -Force -Path $cargo | Out-Null
& tar.exe -xzf $archive -C $cargo
if ($LASTEXITCODE -ne 0) {{ throw 'Offline Cargo cache extraction failed' }}
"""])
    script = source + r"\tools\autoresearch\windows_benchmark.ps1"
    arguments = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File", script,
                 "-Root", source, "-TaskRoot", task, "-CpuSeconds", str(args.cpu_seconds),
                 "-DeliverySeconds", str(args.delivery_seconds), "-Warmup", str(args.warmup),
                 "-Repeats", str(args.repeats)]
    build = "& powershell.exe " + " ".join(map(ps_quote, arguments + ["-Mode", "Build"]))
    run([WINDOWS_VM, "exec", build])
    run([WINDOWS_VM, "exec", f"""
foreach ($name in @('measurement-result.json','measurement.log')) {{
 $path = Join-Path {ps_quote(task)} $name
 if (Test-Path -LiteralPath $path) {{ Remove-Item -LiteralPath $path }}
}}
"""])
    # The adapter checks console/SSH ownership and uses the existing interactive
    # Cua daemon. A Session-0 Start-Process is not a GUI launch.
    launch = run([sys.executable, CUA_WINDOWS, "call", "launch_app", json.dumps({
        "path": r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        "additional_arguments": arguments + ["-Mode", "Measure"],
    })], capture_output=True, text=True)
    pid = launched_pid(json.loads(launch.stdout))
    if not pid:
        raise RuntimeError("guest Cua launch did not return an owned process ID")
    timeout = 120 + (args.cpu_seconds + args.delivery_seconds + args.warmup + 20) * args.repeats * 2
    result = subprocess.run([str(WINDOWS_VM), "exec", f"""
$p = Get-CimInstance Win32_Process -Filter 'ProcessId={pid}'
if ($p) {{
 if ($p.SessionId -eq 0 -or $p.CommandLine -notlike ('*' + {ps_quote(script)} + '*')) {{ throw 'Benchmark launcher ownership/session mismatch' }}
 Wait-Process -Id {pid} -Timeout {timeout} -ErrorAction Stop
}}
$receipt = {ps_quote(task + r'\measurement-result.json')}
$log = {ps_quote(task + r'\measurement.log')}
if (-not (Test-Path -LiteralPath $receipt)) {{ throw 'No guest measurement result; no metric accepted' }}
Get-Content -LiteralPath $log
$r = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json
if ($r.exit_code -ne 0) {{ exit $r.exit_code }}
"""], capture_output=True, text=True)
    print(result.stdout, end="", flush=True)
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)
    args.output.mkdir(parents=True, exist_ok=True)
    run([WINDOWS_VM, "get", task + r"\measurement-result.json", args.output / "measurement-result.json"])
    run([WINDOWS_VM, "get", task + r"\measurement.log", args.output / "measurement.log"])
    receipt = json.loads((args.output / "measurement-result.json").read_text(encoding="utf-8-sig"))
    report = receipt.get("report_path")
    if not report or not report.lower().startswith(task.lower() + "\\"):
        raise RuntimeError("guest receipt does not name a task-owned report directory")
    evidence = args.output / "reports.tar.gz"
    run([WINDOWS_VM, "get", report + ".tar.gz", evidence])
    reports = args.output / "reports"
    reports.mkdir(parents=True, exist_ok=True)
    with tarfile.open(evidence) as package:
        package.extractall(reports, filter="data")
    (args.output / "guest-receipt.json").write_text(json.dumps({
        "guest": "windows-vm", "source_sha256": digest, "launcher_pid": pid,
        "windows_service": status["windows_service"], "report_path": report,
        "guest_exit_code": receipt["exit_code"],
    }, indent=2) + "\n")
    if result.returncode and not receipt["exit_code"]:
        raise RuntimeError("guest command failed despite a successful measurement receipt")
    return receipt["exit_code"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--guest", choices=("auto", "linux", "windows"), default="auto")
    parser.add_argument("--cpu-seconds", type=int, default=20)
    parser.add_argument("--delivery-seconds", type=int, default=8)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--output", type=Path, default=STATE / "runs")
    parser.add_argument("--linux-root", default="~/vosulay-autoresearch-20261007")
    args = parser.parse_args()
    if args.cpu_seconds <= 0 or args.delivery_seconds <= 0 or args.warmup < 0 or args.repeats < 1:
        parser.error("measurement durations/repeats must be positive; warmup must be nonnegative")
    STATE.mkdir(parents=True, exist_ok=True)
    with (STATE / "dispatch.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        status = json.loads(run([WINDOWS_VM, "status", "--json"], capture_output=True, text=True).stdout)
        guest = select_guest(status)
        if args.guest != "auto" and args.guest != guest:
            raise RuntimeError(f"requested {args.guest} guest is not running; selected {guest}; no lifecycle action taken")
        archive = snapshot()
        print(f"BENCH guest={guest} source_sha256={sha256(archive)}", flush=True)
        if guest == "linux":
            from linux_vm import run_linux
            return run_linux(args, status, STATE, archive)
        return run_windows(args, status, STATE, archive)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"autoresearch guest dispatch: {error}", file=sys.stderr)
        raise SystemExit(1)
