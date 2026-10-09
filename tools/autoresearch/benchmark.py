#!/usr/bin/env python3
"""Matched Chromium/CEF counter benchmark on a private hardware XWayland display."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

from pixels import MARKER_X, MARKER_Y, WINDOW_TITLE, X11, decode

ROOT = Path(__file__).resolve().parents[2]
TOOLS = Path(__file__).resolve().parent
PINS = json.loads((ROOT / "tools/cef/pins.json").read_text())
PIN = PINS["platforms"]["linux-x86_64"]
CHROMIUM = PINS["cef_version"].split("chromium-", 1)[1]
WESTON = ROOT / "target/linux-native-proof/weston/usr"
STATE = ROOT / "target/autoresearch"
WIDTH, HEIGHT = 640, 480
# Leave room around the test windows; a work-area-sized Alloy window is clamped.
DISPLAY_WIDTH, DISPLAY_HEIGHT = 1024, 768
COUNTER_LEFT, COUNTER_TOP = 64, 64
SEED = 211


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(command, **kwargs):
    print("BENCH " + shlex.join(map(str, command)), flush=True)
    return subprocess.run(list(map(str, command)), check=True, **kwargs)


def prepare(args):
    args.cef_dir = args.cef_dir.resolve(strict=True)
    run(["cargo", "run", "--offline", "--locked", "-p", "vosulay-cef-tools", "--",
         "verify", "--platform", "linux-x86_64", "--cef-path", args.cef_dir], cwd=ROOT)
    if not (args.weston / "bin/weston").is_file():
        raise RuntimeError(f"private headless Weston is missing: {args.weston}; set --weston to the extracted package usr directory")
    args.weston = args.weston.resolve(strict=True)
    for binary in ("bwrap", "cc", "pkg-config", "cargo"):
        if not shutil.which(binary):
            raise RuntimeError(f"required benchmark tool missing: {binary}")
    environment = dict(os.environ, CEF_PATH=str(args.cef_dir))
    # The measured workload and preparation both fail closed without live network.
    run(["cargo", "build", "--offline", "--release", "--locked", "-p", "vosulay", "-p", "vosulay-overlay", "-p", "vosulay-lookup"], cwd=ROOT, env=environment)
    run(["cargo", "build", "--offline", "--release", "--locked", "-p", "vosulay-browser", "--example", "autoresearch_browser"], cwd=ROOT, env=environment)
    runtime = STATE / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    receipt = json.loads((args.cef_dir / ".vosulay-cef-verified.json").read_text())
    # Cache only digest-verified SDK files; never mutate the SDK or reuse stale code.
    for relative, digest in receipt["runtime_sha256"].items():
        parts = Path(relative).parts
        if parts[0] not in ("Release", "Resources"):
            continue
        destination = runtime.joinpath(*parts[1:])
        destination.parent.mkdir(parents=True, exist_ok=True)
        if not destination.is_file() or sha256(destination) != digest:
            shutil.copy2(args.cef_dir / relative, destination)
    for source, name in ((ROOT / "target/release/vosulay", "vosulay"),
                         (ROOT / "target/release/libvosulay_overlay.so", "libvosulay_overlay.so"),
                         (ROOT / "target/release/libvosulay_lookup.so", "libvosulay_lookup.so"),
                         (ROOT / "target/release/examples/autoresearch_browser", "autoresearch_browser")):
        shutil.copy2(source, runtime / name)
    flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "sdl2", "gl"], text=True))
    run(["cc", "-O2", "-Wall", "-Wextra", "-Werror", TOOLS / "game.c", "-o", runtime / "game", *flags])
    flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "x11", "xext"], text=True))
    run(["cc", "-O2", "-Wall", "-Wextra", "-Werror", TOOLS / "observe.c", "-o", runtime / "observe", *flags])
    args.runtime = runtime.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    args.output = Path(tempfile.mkdtemp(prefix="run-", dir=args.output)).resolve()
    return args


class Backend(BaseHTTPRequestHandler):
    capabilities = []
    page = (TOOLS / "counter.html").read_bytes()
    profile = b'''<!doctype html><title>Vosulay autoresearch browser</title><style>html,body{margin:0;width:640px;height:480px;overflow:hidden;background:#091f38}iframe{position:absolute;left:64px;top:64px;width:480px;height:160px;border:0}</style><iframe src="/counter/index.html"></iframe>'''

    def do_GET(self):
        parsed = urlsplit(self.path)
        if parsed.path == "/capabilities":
            try:
                data = parse_qs(parsed.query)["data"][0]
                if len(data) > 4096:
                    raise ValueError("capabilities too large")
                self.capabilities.append(json.loads(data))
                body, content_type = b"ok", "text/plain"
            except (KeyError, ValueError):
                self.send_error(400)
                return
        elif parsed.path.startswith("/api/counters/search/"):
            body, content_type = b'<!doctype html><div class="no-results">No installed counters</div>', "text/html"
        elif parsed.path == "/profile.html":
            body, content_type = self.profile, "text/html"
        elif parsed.path == "/counter/index.html":
            body, content_type = self.page, "text/html"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def wait_until(predicate, processes=(), timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        check_alive(processes)
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise RuntimeError("benchmark readiness timed out")


def check_alive(processes):
    for process in processes:
        if process.poll() is not None:
            raise RuntimeError(f"measured process {process.pid} exited with {process.returncode}")


def process_snapshot(roots):
    rows = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            pid = int(entry.name)
            rows[pid] = {"parent": int(fields[1]), "start": int(fields[19]),
                         "ticks": int(fields[11]) + int(fields[12])}
        except (OSError, ValueError, IndexError):
            continue
    owners = {root: {root} for root in roots}
    for root, owned in owners.items():
        while True:
            descendants = {pid for pid, row in rows.items() if row["parent"] in owned}
            updated = owned | descendants
            if updated == owned:
                break
            owned.update(updated)
    return {root: {(pid, rows[pid]["start"]): rows[pid]["ticks"] for pid in owned if pid in rows}
            for root, owned in owners.items()}


def sample_cpu(host, game, seconds):
    roots = (host.pid, game.pid)
    previous = process_snapshot(roots)
    totals = dict.fromkeys(roots, 0)
    started = time.monotonic()
    baseline_ticks = float(Path("/proc/uptime").read_text().split()[0]) * os.sysconf("SC_CLK_TCK")
    samples = []
    while time.monotonic() - started < seconds:
        time.sleep(min(0.25, max(0, seconds - (time.monotonic() - started))))
        check_alive((host, game))
        current = process_snapshot(roots)
        for root in roots:
            for key, ticks in current[root].items():
                if key in previous[root]:
                    totals[root] += max(0, ticks - previous[root][key])
                elif key[1] >= baseline_ticks:
                    totals[root] += ticks
        samples.append({"elapsed": time.monotonic() - started,
                        "trees": {str(root): {f"{pid}:{identity}": ticks for (pid, identity), ticks in tree.items()}
                                  for root, tree in current.items()}})
        previous = current
    elapsed = time.monotonic() - started
    tick_seconds = 1 / os.sysconf("SC_CLK_TCK")
    return {"elapsed_seconds": elapsed, "host_cpu_percent": totals[host.pid] * tick_seconds / elapsed * 100,
            "game_cpu_percent": totals[game.pid] * tick_seconds / elapsed * 100, "samples": samples}


def sample_delivery(display, window, host, game, seconds, out, runtime, environment):
    origin = (COUNTER_LEFT + MARKER_X, COUNTER_TOP + MARKER_Y)
    check_alive((host, game))
    result = subprocess.run([str(runtime / "observe"), str(window), str(origin[0]), str(origin[1]),
                             str(seconds), str(out / "visible-transitions.csv")], env=environment,
                            text=True, capture_output=True, timeout=seconds + 10)
    (out / "observer.log").write_text(result.stdout + result.stderr)
    result.check_returncode()
    check_alive((host, game))
    delivery = json.loads(result.stdout)
    display.screenshot(window).save(out / "delivered-after.png")
    return delivery


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    # Subprocess sandbox helpers share this task-owned process group.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass


def inside(args):
    Path("/tmp/runtime").mkdir(mode=0o700)
    Path("/tmp/home").mkdir(mode=0o700)
    Path("/tmp/.X11-unix").mkdir(mode=0o1777)
    server = ThreadingHTTPServer(("127.0.0.1", 24050), Backend)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = dict(os.environ, HOME="/tmp/home", XDG_RUNTIME_DIR="/tmp/runtime", XDG_SESSION_TYPE="x11")
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "SESSION_MANAGER", "XAUTHORITY",
                "LD_PRELOAD", "LIBGL_ALWAYS_SOFTWARE", "__GLX_VENDOR_LIBRARY_NAME", "GALLIUM_DRIVER",
                "MESA_LOADER_DRIVER_OVERRIDE", "VK_INSTANCE_LAYERS", "VK_DRIVER_FILES", "LD_LIBRARY_PATH"):
        base.pop(key, None)
    report = {"schema": 1, "platform": "linux-glx-xwayland", "chromium_version": CHROMIUM,
              "reference": "windowed Alloy browser using the exact pinned CEF; not installed Helium/Google Chrome",
              "seed": SEED, "resolution": [WIDTH, HEIGHT], "counter_geometry": [64, 64, 480, 160],
              "display_resolution": [DISPLAY_WIDTH, DISPLAY_HEIGHT],
              "workload_sha256": sha256(TOOLS / "counter.html"), "game_source_sha256": sha256(TOOLS / "game.c"),
              "observer_source_sha256": sha256(TOOLS / "observe.c"),
              "cef_archive_sha256": PIN["sha256"],
              "binary_sha256": {name: sha256(args.runtime / name) for name in ("vosulay", "autoresearch_browser", "libvosulay_overlay.so", "libvosulay_lookup.so", "game", "observe")},
              "cpu_seconds": args.cpu_seconds, "delivery_seconds": args.delivery_seconds, "warmup_seconds": args.warmup,
              "fps_mode": "benchmark: Chromium/vsync disabled, no game limiter, CEF minimum positive OSR interval 1us (1000000 Hz API ceiling)",
              "limits": ["Linux OSR uses CPU BGRA transport; windowed reference uses native Chromium composition.",
                         "Visible changes are checksum-valid transitions in post-present X11 window pixels, not physical scanout/input latency.",
                         "Sampling may miss IDs; observed updates/s is a lower bound. Observer cadence and skipped IDs are reported.",
                         "CPU is host/browser process-tree percentage of one logical core; injected game CPU is reported separately.",
                         "CPU and delivered-pixel sampling use separate intervals to avoid observer work inflating CPU attribution.",
                         "Hardware performance can vary with external machine/GPU activity; workload and case order are fixed."],
              "runs": []}
    cases = [(repeat, name) for repeat in range(args.repeats) for name in ("cef", "browser")]
    random.Random(SEED).shuffle(cases)
    try:
        for repeat, name in cases:
            out = args.output / f"{repeat}-{name}"
            out.mkdir()
            processes, streams = [], []
            display = None

            def start(command, environment, filename):
                stream = (out / filename).open("wb")
                streams.append(stream)
                process = subprocess.Popen(list(map(str, command)), env=environment, stdout=stream,
                                           stderr=subprocess.STDOUT, start_new_session=True)
                processes.append(process)
                return process

            environment = dict(base, XDG_CONFIG_HOME=str(out / "xdg"), XDG_CACHE_HOME=str(out / "cache"))
            weston_env = dict(environment, LD_LIBRARY_PATH=f"{args.weston / 'lib'}:{args.weston / 'lib/weston'}",
                              WESTON_MODULE_MAP=";".join(f"{item}={args.weston / 'lib/libweston-15' / item}"
                                                         for item in ("headless-backend.so", "gl-renderer.so", "xwayland.so")))
            config = out / "weston.ini"
            config.write_text(f"[shell]\nclient={args.weston / 'lib/weston/weston-desktop-shell'}\nlocking=false\npanel-position=none\n[input-method]\npath={args.weston / 'lib/weston/weston-keyboard'}\n")
            try:
                weston = start([args.weston / "bin/weston", "--backend=headless", "--renderer=gl", "--xwayland",
                                "--no-xwm-decorations", "--fake-seat", "--config=" + str(config),
                                "--socket=vosulay-autoresearch", f"--width={DISPLAY_WIDTH}", f"--height={DISPLAY_HEIGHT}", "--idle-time=0",
                                "--shell=" + str(args.weston / "lib/weston/desktop-shell.so")], weston_env, "weston.log")
                match = wait_until(lambda: re.search(r"xserver listening on display.*?(:\d+)", (out / "weston.log").read_text(errors="replace"), re.I), (weston,))
                environment["DISPLAY"] = match.group(1)
                display = X11(environment["DISPLAY"])
                game_env = dict(environment, SDL_VIDEODRIVER="x11")
                if name == "cef":
                    game_env["LD_PRELOAD"] = f"{args.runtime / 'libvosulay_lookup.so'}:{args.runtime / 'libvosulay_overlay.so'}"
                game = start([args.runtime / "game"], game_env, "game.log")
                game_window = wait_until(lambda: display.find_window(game.pid, WINDOW_TITLE), (game,))
                display.screenshot(game_window).save(out / "game-before.png")
                renderer = wait_until(lambda: re.search(r"renderer=(.+)", (out / "game.log").read_text()), (game,))
                if "NVIDIA" not in renderer.group(1):
                    raise RuntimeError(f"hardware game renderer not proven: {renderer.group(1)}")
                Backend.capabilities.clear()
                browser_env = dict(environment, LD_LIBRARY_PATH=str(args.runtime))
                if name == "cef":
                    config_path = out / "config.json"
                    config_path.write_text(json.dumps({"version": 1, "tosu_migration_completed": True,
                        "active_profile": "benchmark", "profiles": [{"id": "benchmark", "name": "Benchmark", "overlays": [{
                            "counter_id": "benchmark", "name": "Benchmark", "url": "http://127.0.0.1:24050/counter/index.html",
                            "left": 64, "top": 64, "width": 480, "height": 160, "scale": 1, "opacity": 1,
                            "rotation": 0, "enabled": True, "z_index": 1}]}],
                        "settings": {"remote_listen_host": "127.0.0.1", "remote_listen_port": 25050}}))
                    command = [args.runtime / "vosulay", "--cef-dir", args.runtime, "--config", config_path,
                               "--vosu-url", "http://127.0.0.1:24050/", "--pid", game.pid,
                               "--process-name", "vosulay-unused-benchmark-name", "--benchmark-uncapped"]
                    host = start(command, browser_env, "host.log")
                    window = game_window
                else:
                    command = [args.runtime / "autoresearch_browser", "--cef-dir=" + str(args.runtime),
                               "--cache=" + str(out / "browser-cache"), "--url=http://127.0.0.1:24050/profile.html",
                               "--force-device-scale-factor=1", "--ozone-platform=x11"]
                    host = start(command, browser_env, "host.log")
                    identity = wait_until(lambda: re.search(r"BROWSER_WINDOW=(\d+)\nCHROMIUM_VERSION=([^\s]+)", (out / "host.log").read_text(errors="replace")), (host, game))
                    if identity.group(2) != CHROMIUM:
                        raise RuntimeError(f"Chromium mismatch: {identity.group(2)} != {CHROMIUM}")
                    window = int(identity.group(1))
                origin = (COUNTER_LEFT + MARKER_X, COUNTER_TOP + MARKER_Y)
                try:
                    wait_until(lambda: (value if (value := decode(display.marker(window, origin))) else None), (host, game))
                    wait_until(lambda: Backend.capabilities, (host, game))
                except RuntimeError:
                    display.screenshot(window).save(out / "readiness-failed.png")
                    (out / "readiness-failed.json").write_text(json.dumps({
                        "marker": decode(display.marker(window, origin)),
                        "capabilities": Backend.capabilities,
                    }, indent=2) + "\n")
                    raise
                capabilities = Backend.capabilities[:]
                if not all("NVIDIA" in str(item.get("webglRenderer")) and f"Chrome/{CHROMIUM.split('.')[0]}." in item.get("userAgent", "") for item in capabilities):
                    raise RuntimeError(f"matching hardware browser not proven: {capabilities}")
                before = display.screenshot(window)
                if before.size != (WIDTH, HEIGHT):
                    raise RuntimeError(f"viewport mismatch: {before.size} != {(WIDTH, HEIGHT)}")
                before.save(out / "delivered-before.png")
                time.sleep(args.warmup)
                check_alive((host, game))
                cpu = sample_cpu(host, game, args.cpu_seconds)
                delivery = sample_delivery(display, window, host, game, args.delivery_seconds, out, args.runtime, environment)
                row = {"runtime": name, "repeat": repeat, "command": list(map(str, command)),
                       "game_renderer": renderer.group(1), "capabilities": capabilities, "cpu": cpu, "delivery": delivery}
                report["runs"].append(row)
                print(f"BENCH {repeat}-{name}: CPU={cpu['host_cpu_percent']:.3f}% changes={delivery['updates_per_second']:.3f}/s observer={delivery['observer_hz']:.1f}Hz", flush=True)
            finally:
                if display:
                    display.close()
                for process in reversed(processes):
                    stop(process)
                for stream in streams:
                    stream.close()
                (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    finally:
        server.shutdown()
        server.server_close()
    summary = {}
    for name in ("cef", "browser"):
        rows = [row for row in report["runs"] if row["runtime"] == name]
        costs = [row["cpu"]["host_cpu_percent"] * 10000 / row["delivery"]["updates_per_second"] for row in rows]
        summary[name] = {"cpu_percent": statistics.median(row["cpu"]["host_cpu_percent"] for row in rows),
                         "updates_per_second": statistics.median(row["delivery"]["updates_per_second"] for row in rows),
                         "game_cpu_percent": statistics.median(row["cpu"]["game_cpu_percent"] for row in rows),
                         "observer_hz": statistics.median(row["delivery"]["observer_hz"] for row in rows),
                         "cpu_us_per_visible_update": statistics.median(costs)}
    metrics = {"cpu_us_per_visible_update": summary["cef"]["cpu_us_per_visible_update"],
               "render_cost_ratio": summary["cef"]["cpu_us_per_visible_update"] / summary["browser"]["cpu_us_per_visible_update"],
               "visible_update_ratio": summary["cef"]["updates_per_second"] / summary["browser"]["updates_per_second"]}
    metrics.update({f"{name}_{key}": value for name, values in summary.items() for key, value in values.items() if key != "cpu_us_per_visible_update"})
    if not all(math.isfinite(value) and value > 0 for value in metrics.values()):
        raise RuntimeError(f"nonfinite or empty benchmark: {metrics}")
    report.update(summary=summary, metrics=metrics, cleanup="task process groups terminated; private PID/network/IPC namespace exits")
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    (args.output / "metrics.json").write_text(json.dumps(metrics, indent=2) + "\n")
    print("BENCH report=" + str(args.output / "report.json"), flush=True)
    print("ASI platform=linux_glx_xwayland", flush=True)
    print("ASI chromium_version=" + CHROMIUM, flush=True)
    print("ASI workload_sha256=" + report["workload_sha256"], flush=True)
    for name, value in metrics.items():
        print(f"METRIC {name}={value:.6f}", flush=True)
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cef-dir", type=Path, default=Path(os.environ.get("CEF_PATH", Path.home() / ".cache/vosulay/cef" / PIN["cef_path_subdir"])))
    parser.add_argument("--weston", type=Path, default=WESTON)
    parser.add_argument("--output", type=Path, default=STATE / "runs")
    parser.add_argument("--cpu-seconds", type=float, default=20)
    parser.add_argument("--delivery-seconds", type=float, default=8)
    parser.add_argument("--warmup", type=float, default=5)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--inside", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--runtime", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.cpu_seconds <= 0 or args.delivery_seconds <= 0 or args.warmup < 0 or args.repeats < 1:
        parser.error("measurement durations/repeats must be positive; warmup must be nonnegative")
    if args.inside:
        return inside(args)
    if sys.platform != "linux":
        raise RuntimeError("this hardware/private-display baseline requires Linux; Windows has a separate composition path")
    if subprocess.run(["systemd-detect-virt", "--vm"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        raise RuntimeError("workload execution requires the Linux VM; use bash autoresearch.sh for guest dispatch")
    args = prepare(args)
    command = ["bwrap", "--die-with-parent", "--unshare-pid", "--unshare-ipc", "--unshare-net", "--ro-bind", "/", "/",
               "--bind", args.output, args.output, "--tmpfs", "/tmp", "--proc", "/proc", "--dev-bind", "/dev", "/dev",
               sys.executable, Path(__file__).resolve(), "--inside", "--output", args.output,
               "--runtime", args.runtime, "--weston", args.weston, "--cef-dir", args.cef_dir,
               "--cpu-seconds", str(args.cpu_seconds), "--delivery-seconds", str(args.delivery_seconds),
               "--warmup", str(args.warmup), "--repeats", str(args.repeats)]
    print("BENCH output=" + str(args.output), flush=True)
    return run(command, timeout=120 + (args.cpu_seconds + args.delivery_seconds + args.warmup + 20) * args.repeats * 2).returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"autoresearch: {error}", file=sys.stderr)
        raise SystemExit(1)
