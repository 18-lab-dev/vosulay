#!/usr/bin/env python3
"""Headless benchmark of the in-game cost of overlay updates (Linux).

Builds the compositor library, the lookup helper and the `upload_bench`
example explicitly, then runs a matrix of overlay scenarios against a
synthetic surfaceless-EGL game with the library preloaded. Repetitions are
interleaved so drift (other work on a shared GPU) spreads over every scenario.
No window, display connection or input is involved.

Scenarios: none (no compositor), static (composite only), small (one 600x200
counter changes), corners (two counters in opposite corners change), full
(every pixel changes). Rates are overlay updates per second.

Output: a Markdown table of medians across repetitions, a frame-pacing table
(distribution of game frame intervals: p50/p99/p99.9/max and frames over 2x
the median or 16.7 ms, the worst repetition of each), and with --json the
raw per-run records. `--caps` also runs the game with a frame limiter.
`--compositor DIR` preloads libvosulay_overlay.so and libvosulay_lookup.so
from DIR (for example another commit's release build) instead of this
checkout's, so compositor versions compare under the same game. `--profile` builds the compositor with its `profile`
feature, so each record also carries the per-call CPU and GPU breakdown of
the upload and composite; a second table summarizes it.
"""
import argparse
import hashlib
import itertools
import json
import os
import statistics
import subprocess
import sys
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NVIDIA_EGL = Path("/usr/share/glvnd/egl_vendor.d/10_nvidia.json")


def build(target: Path, profile: bool) -> Path:
    environment = dict(os.environ, CARGO_TARGET_DIR=str(target))
    features = ["--features", "vosulay-overlay/profile"] if profile else []
    # --lib and --example are separate invocations: building only the example
    # does not rebuild the cdylib that gets preloaded.
    for arguments in (
        ["-p", "vosulay-overlay", "-p", "vosulay-lookup", "--lib", *features],
        ["-p", "vosulay-overlay", "--example", "upload_bench"],
    ):
        subprocess.run(
            ["cargo", "build", "--release", "--locked", *arguments],
            cwd=ROOT,
            env=environment,
            check=True,
        )
    return target / "release"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()[:16]


def run(release: Path, libraries: Path, args, scenario: str, rate: float, draws: int, cap: float) -> dict:
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in ("DISPLAY", "WAYLAND_DISPLAY", "LD_PRELOAD", "VOSULAY_ENDPOINT")
    }
    if args.egl_vendor:
        environment["__EGL_VENDOR_LIBRARY_FILENAMES"] = args.egl_vendor
    if scenario != "none":
        environment["LD_PRELOAD"] = ":".join(
            str(libraries / name) for name in ("libvosulay_lookup.so", "libvosulay_overlay.so")
        )
        environment["VOSULAY_ENDPOINT"] = f"@vosulay-upload-bench-{uuid.uuid4().hex[:12]}"
    command = [
        str(release / "examples" / "upload_bench"),
        "--scenario", scenario,
        "--rate", str(rate),
        "--seconds", str(args.seconds),
        "--draws", str(draws),
        "--api", args.api,
        "--cap-ms", str(cap),
    ]
    result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=120)
    if result.returncode != 0:
        raise SystemExit(f"{' '.join(command)} failed:\n{result.stderr}")
    record = json.loads(result.stdout.strip().splitlines()[-1])
    # The top-left counter reads back as red 128 when the overlay was drawn.
    record["composited"] = record["pixel"][0] == 128 and record["pixel"][3] >= 200
    if (scenario == "none") == record["composited"]:
        raise SystemExit(f"composition proof failed for {scenario}: {record['pixel']}")
    # After updates stop, both counters must show the last published frame.
    if not record["content_ok"]:
        raise SystemExit(f"stale overlay content for {scenario}: {record['pixel']} {record['corner']}")
    return record


def table(records: list) -> str:
    groups = {}
    for record in records:
        key = (record["draws"], record.get("cap_ms", 0), record["scenario"], record["rate"] if record["scenario"] not in ("none", "static") else 0)
        groups.setdefault(key, []).append(record)
    order = {"none": 0, "static": 1, "small": 2, "corners": 3, "full": 4}
    lines = [
        "| game draws | cap ms | scenario | updates/s | fps | frame ms | hooked swap CPU µs p50 / p99 | hook GPU µs p50 / p99 | runs |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for key in sorted(groups, key=lambda k: (k[0], k[1], order[k[2]], k[3])):
        runs = groups[key]
        median = lambda f: statistics.median(f(r) for r in runs)  # noqa: E731
        gpu = runs[0]["gpu"]
        hook = (
            f"{median(lambda r: r['gpu']['hook_us']['p50']):.0f} / {median(lambda r: r['gpu']['hook_us']['p99']):.0f}"
            if gpu
            else "n/a"
        )
        lines.append(
            f"| {key[0]} | {key[1] or '-'} | {key[2]} | {key[3] or '-'} | {median(lambda r: r['fps']):.0f} | "
            f"{median(lambda r: r['frame_ms']):.3f} | "
            f"{median(lambda r: r['swap_cpu_us']['p50']):.0f} / {median(lambda r: r['swap_cpu_us']['p99']):.0f} | "
            f"{hook} | {len(runs)} |"
        )
    return "\n".join(lines)


def pacing_table(records: list) -> str:
    """Frame pacing per configuration: medians of p50/p99, worst of the tail."""
    groups = {}
    for record in records:
        key = (record["draws"], record["cap_ms"], record["scenario"], record["rate"] if record["scenario"] not in ("none", "static") else 0)
        groups.setdefault(key, []).append(record["pacing_ms"])
    order = {"none": 0, "static": 1, "small": 2, "corners": 3, "full": 4}
    lines = [
        "| game draws | cap ms | scenario | updates/s | frame ms p50 | p99 | p99.9 (worst run) | max (worst run) | >2x median (worst run) | >16.7 ms (worst run) | frames/run |",
        "|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for key in sorted(groups, key=lambda k: (k[0], k[1], order[k[2]], k[3])):
        runs = groups[key]
        median = lambda field: statistics.median(run[field] for run in runs)  # noqa: E731
        worst = lambda field: max(run[field] for run in runs)  # noqa: E731
        lines.append(
            f"| {key[0]} | {key[1] or '-'} | {key[2]} | {key[3] or '-'} | {median('p50'):.3f} | {median('p99'):.3f} | "
            f"{worst('p999'):.2f} | {worst('max'):.2f} | {worst('over_2x_median')} | {worst('over_16_7ms')} | {median('frames'):.0f} |"
        )
    return "\n".join(lines)


PROFILE_STAGES = (
    "cpu_capture", "cpu_orphan", "cpu_map", "cpu_copy", "cpu_unmap",
    "cpu_tex_sub_image", "cpu_draw", "cpu_restore", "cpu_compose", "gpu_upload", "gpu_composite",
)


def profile_table(records: list) -> str:
    """Per-update stages (median across repetitions of each run's p50/p99, µs)."""
    groups = {}
    for record in records:
        if record.get("profile") and record["scenario"] != "none":
            key = (record["draws"], record["scenario"], record["rate"])
            groups.setdefault(key, []).append(record["profile"])
    order = {"static": 1, "small": 2, "corners": 3, "full": 4}
    header = " | ".join(stage.replace("cpu_", "").replace("gpu_", "GPU ") for stage in PROFILE_STAGES)
    lines = [f"| game draws | scenario | updates/s | MB/update | {header} |", "|---" * (4 + len(PROFILE_STAGES)) + "|"]
    for key in sorted(groups, key=lambda k: (k[0], order[k[1]], k[2])):
        runs = groups[key]
        cells = []
        for stage in PROFILE_STAGES:
            if not any(run[stage]["count"] for run in runs):
                cells.append("-")
                continue
            p50 = statistics.median(run[stage]["p50"] for run in runs)
            p99 = statistics.median(run[stage]["p99"] for run in runs)
            cells.append(f"{p50:.0f}/{p99:.0f}")
        uploads = sum(run["uploads"] for run in runs)
        megabytes = sum(run["upload_bytes"] for run in runs) / max(uploads, 1) / 1e6
        lines.append(f"| {key[0]} | {key[1]} | {key[2] or '-'} | {megabytes:.2f} | " + " | ".join(cells) + " |")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--scenarios", default="none,static,small,corners,full")
    parser.add_argument("--rates", default="60,120,360", help="overlay updates per second")
    parser.add_argument("--draws", default="4,100", help="game load: blended fullscreen layers per frame")
    parser.add_argument("--reps", type=int, default=2)
    parser.add_argument("--seconds", type=float, default=2.0)
    parser.add_argument("--api", choices=("gl", "es"), default="gl")
    parser.add_argument("--caps", default="0", help="game frame limiter in ms per frame; 0 = uncapped")
    parser.add_argument(
        "--egl-vendor",
        default=str(NVIDIA_EGL) if NVIDIA_EGL.exists() else "",
        help="glvnd EGL vendor JSON (default: NVIDIA when installed, so Mesa's software renderer is never measured)",
    )
    parser.add_argument("--profile", action="store_true", help="build the compositor's profile feature")
    parser.add_argument("--compositor", type=Path, help="preload the compositor libraries from this directory")
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target" / "overlay-upload-bench")
    parser.add_argument("--json", type=Path, help="write raw records here")
    args = parser.parse_args()

    release = build(args.target_dir / ("profile" if args.profile else "plain"), args.profile)
    libraries = args.compositor.resolve(strict=True) if args.compositor else release
    for name in ("libvosulay_overlay.so", "libvosulay_lookup.so"):
        print(f"{name} sha256 {digest(libraries / name)}", file=sys.stderr)

    configurations = []
    for draws in (int(value) for value in args.draws.split(",")):
        for scenario in args.scenarios.split(","):
            rates = [0.0] if scenario in ("none", "static") else [float(v) for v in args.rates.split(",")]
            configurations += [
                (scenario, rate, draws, float(cap)) for rate in rates for cap in args.caps.split(",")
            ]
    records = []
    for repetition, configuration in itertools.product(range(args.reps), configurations):
        record = run(release, libraries, args, *configuration)
        record["repetition"] = repetition
        records.append(record)
        print(
            f"rep {repetition} {configuration[0]:>7} {configuration[1]:>5.0f}/s draws {configuration[2]:>3} "
            f"cap {configuration[3]:g} ms: {record['fps']:.0f} fps, p99.9 {record['pacing_ms']['p999']:.2f} ms",
            file=sys.stderr,
        )
    renderers = {record["renderer"] for record in records}
    print(f"renderer: {', '.join(sorted(renderers))}")
    print(table(records))
    print()
    print(pacing_table(records))
    if args.profile:
        print()
        print(profile_table(records))
    if args.json:
        args.json.write_text("\n".join(json.dumps(record) for record in records) + "\n")


if __name__ == "__main__":
    main()
