#!/usr/bin/env python3
"""Run the real CEF OSR smoke on a private X server, never the user's desktop."""
import argparse
import os
from pathlib import Path
import selectors
import subprocess
import sys


def inside(args):
    read_fd, write_fd = os.pipe()
    with (args.output / "xvfb.log").open("wb") as log:
        server = subprocess.Popen([str(args.xvfb), "-displayfd", str(write_fd), "-screen", "0", "640x480x24", "-nolisten", "tcp", "-noreset"], pass_fds=(write_fd,), stdout=log, stderr=log)
        os.close(write_fd)
        try:
            with selectors.DefaultSelector() as ready:
                ready.register(read_fd, selectors.EVENT_READ)
                if not ready.select(10):
                    raise RuntimeError("private Xvfb did not become ready")
                with os.fdopen(read_fd, "r", closefd=False) as readiness:
                    display = readiness.readline(32).strip()
            if not display.isdecimal():
                raise RuntimeError("private Xvfb exited without a display")
            environment = dict(os.environ, DISPLAY=":" + display)
            for key in ("WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "SESSION_MANAGER", "XAUTHORITY"):
                environment.pop(key, None)
            environment["LD_LIBRARY_PATH"] = str(args.cef_dir / "Release" if (args.cef_dir / "Release").is_dir() else args.cef_dir)
            environment["XDG_SESSION_TYPE"] = "x11"
            environment["XDG_RUNTIME_DIR"] = "/tmp/runtime"
            Path("/tmp/runtime").mkdir(mode=0o700)
            with (args.output / "smoke.log").open("wb") as log:
                process = subprocess.Popen([str(args.binary), "--ozone-platform=x11", "--cef-dir", str(args.cef_dir), "--output", str(args.output)], env=environment, stdout=log, stderr=subprocess.STDOUT)
                try:
                    return process.wait(timeout=35)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()
        finally:
            os.close(read_fd)
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--cef-dir", type=Path, required=True)
    parser.add_argument("--xvfb", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--inside", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.cef_dir = args.cef_dir.resolve(strict=True)
    args.xvfb = args.xvfb.resolve(strict=True)
    if args.inside:
        return inside(args)
    args.output.mkdir(parents=True, exist_ok=False)
    args.output = args.output.resolve(strict=True)
    command = ["bwrap", "--die-with-parent", "--unshare-pid", "--unshare-ipc", "--unshare-net", "--ro-bind", "/", "/", "--bind", str(args.output), str(args.output), "--tmpfs", "/tmp", "--proc", "/proc", "--dev-bind", "/dev", "/dev", "--ro-bind", str(args.xvfb), "/tmp/proof-Xvfb"]
    command += [sys.executable, str(Path(__file__).resolve()), "--inside", "--binary", str(args.binary), "--cef-dir", str(args.cef_dir), "--xvfb", "/tmp/proof-Xvfb", "--output", str(args.output)]
    return subprocess.run(command, timeout=55).returncode


if __name__ == "__main__":
    raise SystemExit(main())
