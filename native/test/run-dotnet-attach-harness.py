#!/usr/bin/env python3
"""Build and prove post-launch CoreCLR attach reaches an active EGL swap loop."""

from __future__ import annotations

import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

NATIVE = Path(__file__).resolve().parents[1]
BOOTSTRAP = NATIVE / "crates/overlay-dotnet-bootstrap"
TARGET_PROJECT = BOOTSTRAP / "dotnet/AttachTarget"
ATTACH_PROJECT = BOOTSTRAP / "dotnet/Vosulay.AttachProfiler"
TARGET_DLL = TARGET_PROJECT / "bin/Release/net8.0/AttachTarget.dll"
ATTACH_DLL = ATTACH_PROJECT / "bin/Release/net8.0/Vosulay.AttachProfiler.dll"
PROFILER_SO = NATIVE / "target/debug/liboverlay_dotnet_bootstrap.so"
AGENT_SO = NATIVE / "target/debug/liboverlay_agent_gl.so"
CHECKER_HOST = NATIVE / "target/debug/examples/shm-checkerboard"


def run(*args: str, cwd: Path = NATIVE) -> None:
    subprocess.run(args, cwd=cwd, check=True)


def wait_file(path: Path, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.is_file() and path.stat().st_size > 0:
            return
        time.sleep(0.01)
    raise TimeoutError(f"timed out waiting for {path}")


def read_line(process: subprocess.Popen[str], timeout: float) -> str:
    assert process.stdout is not None
    readable, _, _ = select.select([process.stdout], [], [], timeout)
    if not readable:
        raise TimeoutError("target did not publish its PID")
    return process.stdout.readline().strip()


def stop(process: subprocess.Popen[str] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=3)


def main() -> int:
    run(
        "cargo", "build",
        "-p", "overlay-agent-gl",
        "-p", "overlay-dotnet-bootstrap",
    )
    run("cargo", "build", "-p", "overlay-host", "--example", "shm-checkerboard")
    run("dotnet", "build", "-c", "Release", cwd=TARGET_PROJECT)
    run("dotnet", "build", "-c", "Release", cwd=ATTACH_PROJECT)

    target: subprocess.Popen[str] | None = None
    host: subprocess.Popen[str] | None = None
    with tempfile.TemporaryDirectory(prefix="vosulay-dotnet-attach-") as temporary:
        directory = Path(temporary)
        ready = directory / "target.ready"
        credentials = directory / "credentials"
        environment = os.environ | {
            "EGL_PLATFORM": "surfaceless",
            "LIBGL_ALWAYS_SOFTWARE": "1",
        }
        try:
            target = subprocess.Popen(
                ["dotnet", str(TARGET_DLL), str(ready)],
                cwd=NATIVE,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            target_pid = int(read_line(target, 10))
            if target_pid != target.pid:
                raise RuntimeError(f"target reported PID {target_pid}, launcher has {target.pid}")
            wait_file(ready, 10)

            host = subprocess.Popen(
                [
                    str(CHECKER_HOST), str(target_pid), str(credentials),
                    "640", "480", "opaque-marker",
                ],
                cwd=NATIVE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            wait_file(credentials, 10)
            fields = credentials.read_bytes().split(b"\0")
            if len(fields) != 3:
                raise RuntimeError("checker host emitted malformed credentials")
            socket_name, cookie_hex, host_pid = fields

            attached = subprocess.run(
                [
                    "dotnet", str(ATTACH_DLL), str(target_pid),
                    str(PROFILER_SO), str(AGENT_SO), socket_name.hex(),
                    cookie_hex.decode("ascii"), host_pid.decode("ascii"),
                ],
                cwd=NATIVE,
                text=True,
                capture_output=True,
                timeout=20,
            )
            if attached.returncode != 0:
                raise RuntimeError(
                    f"profiler attach failed ({attached.returncode}): "
                    f"{attached.stdout}{attached.stderr}"
                )

            result = target.wait(timeout=45)
            target_stderr = target.stderr.read() if target.stderr is not None else ""
            if result != 0:
                raise RuntimeError(
                    f"late-attached EGL target exited {result}: {target_stderr}"
                )
            print("late CoreCLR attach established the authenticated session and composited EGL")
            return 0
        finally:
            stop(host)
            stop(target)


if __name__ == "__main__":
    raise SystemExit(main())
