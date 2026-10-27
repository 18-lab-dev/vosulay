#!/usr/bin/env python3
"""Run the real CEF OSR smoke on a private X server, never the user's desktop."""
import argparse
import os
import json
import time
from pathlib import Path
import selectors
import subprocess
import sys

def process_sample(root):
    processes = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdecimal():
            continue
        try:
            text = (path / "stat").read_text()
            fields = text[text.rfind(")") + 2:].split()
            command = (path / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
            memory = {}
            try:
                for line in (path / "smaps_rollup").read_text().splitlines():
                    key, _, value = line.partition(":")
                    if key in ("Pss", "Private_Clean", "Private_Dirty"):
                        memory[key] = int(value.split()[0])
            except OSError:
                pass
            processes[int(path.name)] = (
                int(fields[1]), int(fields[11]) + int(fields[12]),
                int(fields[21]) * os.sysconf("SC_PAGE_SIZE") // 1024,
                "--type=renderer" in command or "--type renderer" in command,
                memory.get("Pss", 0), memory.get("Private_Clean", 0) + memory.get("Private_Dirty", 0),
                command[:180],
            )
        except (OSError, ValueError, IndexError):
            continue
    selected = {root}
    while True:
        children = {pid for pid, values in processes.items() if values[0] in selected}
        if children <= selected:
            break
        selected |= children
    rows = [values for pid, values in processes.items() if pid in selected]
    return {"time": time.monotonic(), "wall_time": time.time(), "processes": len(rows),
            "renderers": sum(row[3] for row in rows),
            "rss_kib": sum(row[2] for row in rows),
            "pss_kib": sum(row[4] for row in rows), "private_kib": sum(row[5] for row in rows),
            "commands": [row[6] for row in rows],
            "cpu_seconds": sum(row[1] for row in rows) / os.sysconf("SC_CLK_TCK")}



def inside(args):
    read_fd, write_fd = os.pipe()
    with (args.output / "xvfb.log").open("wb") as log:
        server = subprocess.Popen([str(args.xvfb), "-displayfd", str(write_fd), "-screen", "0", "2560x1440x24" if args.site_pacing else "640x480x24", "-nolisten", "tcp", "-noreset"], pass_fds=(write_fd,), stdout=log, stderr=log)
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
            for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME",
                        "XDG_STATE_HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_DIRS", "XDG_DATA_DIRS"):
                path = args.output / key.lower()
                path.mkdir(mode=0o700)
                environment[key] = str(path)
            with (args.output / "smoke.log").open("wb") as log:
                command = [str(args.binary), "--ozone-platform=x11", "--cef-dir", str(args.cef_dir), "--output", str(args.output)]
                if args.site_pacing:
                    command += ["--site-pacing", "--fps", str(args.fps)]
                    if args.shared_site:
                        command += ["--shared-site"]
                    if args.site_security:
                        command += ["--site-security"]
                process = subprocess.Popen(command, env=environment, stdout=log, stderr=subprocess.STDOUT)
                try:
                    if not args.site_pacing:
                        return process.wait(timeout=35)
                    samples = []
                    deadline = time.monotonic() + 35
                    while process.poll() is None and time.monotonic() < deadline:
                        samples.append(process_sample(process.pid))
                        try:
                            process.wait(timeout=0.1)
                        except subprocess.TimeoutExpired:
                            pass
                    (args.output / "processes.json").write_text(json.dumps(samples, indent=2))
                    return process.wait(timeout=max(0.1, deadline - time.monotonic()))
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
    parser.add_argument("--site-pacing", action="store_true", help="check seven isolated overlay sites and native bootstrap authorization")
    parser.add_argument("--fps", type=int, default=60)
    parser.add_argument("--shared-site", action="store_true", help="measure the deliberately broken sibling-OOPIF baseline")
    parser.add_argument("--site-security", action="store_true", help="also crash/restart one private renderer and test retired origin grants (requires Bun)")
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
    if args.site_pacing:
        command += ["--site-pacing", "--fps", str(args.fps)]
        if args.shared_site:
            command += ["--shared-site"]
        if args.site_security:
            command += ["--site-security"]
    return subprocess.run(command, timeout=55).returncode


if __name__ == "__main__":
    raise SystemExit(main())
