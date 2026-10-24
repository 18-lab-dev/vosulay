#!/usr/bin/env python3
"""Measure real manager OSR paints on private Xvfb; no game or desktop is used.

Requires aiohttp, bubblewrap and Xvfb. Compare independently built examples with
repeated --binary REV=PATH. Software CEF metrics do not establish GPU performance.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import runpy
import selectors
import shutil
import subprocess
import sys
import tempfile

from aiohttp import web

SUPPORT = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'ruleset-bench.py'))


async def measure(args, label, binary, editing, repetition, display):
    with tempfile.TemporaryDirectory(prefix='case-', dir=args.output) as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for key in ('WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY', 'LD_PRELOAD'):
            environment.pop(key, None)
        for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME',
                    'XDG_RUNTIME_DIR', 'XDG_STATE_HOME', 'XDG_DATA_DIRS', 'XDG_CONFIG_DIRS'):
            directory = root / key.lower()
            directory.mkdir(mode=0o700)
            environment[key] = str(directory)
        environment.update(DISPLAY=display, XDG_SESSION_TYPE='x11', LIBGL_ALWAYS_SOFTWARE='1',
                           GALLIUM_DRIVER='llvmpipe', LP_NUM_THREADS='2')
        environment.update(__EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json',
                           __GLX_VENDOR_LIBRARY_NAME='mesa')
        allocations = root / 'allocations'
        allocations.mkdir()
        if args.allocation_probe:
            environment.update(LD_PRELOAD=str(args.allocation_probe), VOSULAY_PERF_ALLOC_DIR=str(allocations))
        runtime = root / 'runtime'
        runtime.mkdir()
        for directory in (args.cef_dir / 'Release', args.cef_dir / 'Resources'):
            for asset in directory.iterdir():
                if not (runtime / asset.name).exists():
                    (runtime / asset.name).symlink_to(asset)
        shutil.copy2(binary, runtime / 'manager_bench')
        fixture = SUPPORT['Fixture'](args.rate, False)
        application = web.Application()
        application.router.add_route('*', '/{path:.*}', fixture.handle)
        runner = web.AppRunner(application)
        await runner.setup()
        site = web.TCPSite(runner, '127.0.0.1', 0)
        await site.start()
        backend = f'http://127.0.0.1:{site._server.sockets[0].getsockname()[1]}/'
        log_path = args.output / f'{label}-{int(editing)}-{repetition}.log'
        command = [str(runtime / 'manager_bench'), '--cef-dir', str(runtime), '--cache', str(root / 'cache'),
                   '--backend', backend, '--seconds', str(args.seconds), '--ozone-platform=x11',
                   '--use-gl=angle', '--use-angle=swiftshader']
        if editing:
            command.append('--editing')
        if args.check:
            command.append('--check')
        with log_path.open('wb') as log:
            process = await asyncio.create_subprocess_exec(*command, env=environment, cwd=root,
                                                          stdout=log, stderr=log)
            try:
                await asyncio.wait_for(process.wait(), timeout=35)
                if process.returncode:
                    raise RuntimeError(f'manager benchmark failed ({process.returncode}): {log_path.read_text(errors="replace")}')
                record = next(json.loads(line) for line in reversed(log_path.read_text().splitlines()) if line.startswith('{'))
                record.update(revision=label, repetition=repetition, subscriptions=fixture.fields,
                              fixture_socket_messages=fixture.messages)
                print(json.dumps(record), flush=True)
                with (args.output / 'cef.jsonl').open('a') as output:
                    output.write(json.dumps(record) + '\n')
            finally:
                if process.returncode is None:
                    process.terminate()
                    try:
                        await asyncio.wait_for(process.wait(), 10)
                    except asyncio.TimeoutError:
                        process.kill()
                        await process.wait()
                await runner.cleanup()


async def inside(args):
    if os.readlink('/proc/self/ns/net') == args.outer_netns:
        raise RuntimeError('private network namespace was not created')
    display_state = Path(tempfile.mkdtemp(prefix='display-', dir=args.output))
    display_environment = dict(os.environ)
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME',
                'XDG_RUNTIME_DIR', 'XDG_STATE_HOME', 'XDG_DATA_DIRS', 'XDG_CONFIG_DIRS'):
        directory = display_state / key.lower()
        directory.mkdir(mode=0o700)
        display_environment[key] = str(directory)
    for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY', 'LD_PRELOAD'):
        display_environment.pop(key, None)
    display_environment.update(LIBGL_ALWAYS_SOFTWARE='1', GALLIUM_DRIVER='llvmpipe', LP_NUM_THREADS='2',
        __EGL_VENDOR_LIBRARY_FILENAMES='/usr/share/glvnd/egl_vendor.d/50_mesa.json',
        __GLX_VENDOR_LIBRARY_NAME='mesa')
    read_fd, write_fd = os.pipe()
    with (args.output / 'xvfb.log').open('wb') as log:
        server = subprocess.Popen([str(args.xvfb), '-displayfd', str(write_fd), '-screen', '0',
                                   '2560x1440x24', '-nolisten', 'tcp', '-noreset', '-extension', 'GLX'],
                                  pass_fds=(write_fd,), stdout=log, stderr=log, env=display_environment)
        os.close(write_fd)
        try:
            with selectors.DefaultSelector() as ready:
                ready.register(read_fd, selectors.EVENT_READ)
                if not ready.select(10):
                    raise RuntimeError('private Xvfb did not become ready')
                number = os.read(read_fd, 128).decode().strip()
                if not number.isdecimal():
                    raise RuntimeError(f'private Xvfb failed: {(args.output / "xvfb.log").read_text()}')
                display = ':' + number
            os.close(read_fd)
            for repetition in range(args.reps):
                for editing in (False, True):
                    for value in args.binary:
                        label, path = value.split('=', 1)
                        await measure(args, label, Path(path), editing, repetition, display)
        finally:
            server.terminate()
            server.wait(timeout=10)
            shutil.rmtree(display_state)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', action='append', required=True)
    parser.add_argument('--cef-dir', type=Path, required=True, help='Pinned minimal CEF SDK directory')
    parser.add_argument('--xvfb', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--check', action='store_true', help='Also require fewer than 5%% full-frame paints')
    parser.add_argument('--allocation-probe', type=Path)
    parser.add_argument('--rate', type=float, default=150)
    parser.add_argument('--seconds', type=float, default=6)
    parser.add_argument('--reps', type=int, default=2)
    parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--outer-netns', help=argparse.SUPPRESS)
    args = parser.parse_args()
    for key in ('cef_dir', 'xvfb', 'output', 'allocation_probe'):
        if (path := getattr(args, key)) is not None:
            setattr(args, key, path.resolve())
    args.output.mkdir(parents=True, exist_ok=True)
    if args.inside:
        asyncio.run(inside(args))
    else:
        command = ['unshare', '--user', '--map-current-user', '--net', '--keep-caps', 'sh', '-c',
                   'ip link set lo up && exec "$@"', 'sh',
                   'setpriv', '--inh-caps=-all', '--ambient-caps=-all',
                   'bwrap', '--die-with-parent', '--unshare-pid', '--unshare-ipc',
                   '--ro-bind', '/', '/', '--tmpfs', '/tmp', '--bind', str(args.output.parent), str(args.output.parent),
                   '--proc', '/proc', '--dev', '/dev', sys.executable, str(Path(__file__).resolve()),
                   *sys.argv[1:], '--inside', '--outer-netns', os.readlink('/proc/self/ns/net')]
        subprocess.run(command, check=True, timeout=300)
