#!/usr/bin/env python3
"""Measure native host overhead with real broker clients and selected-field traffic.

Requires aiohttp. Always re-execs in a private network namespace and creates fresh
HOME/XDG roots. Binaries are copied before launch; no installed runtime is changed.
This server-only measurement excludes CEF, GPU work, and real games.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time

from aiohttp import ClientSession, web


class Fixture:
    def __init__(self, rate, duplicate_modes):
        self.rate = rate
        self.duplicate_modes = duplicate_modes
        self.messages = 0
        self.fields = []
        self.mode = 3

    async def handle(self, request):
        if request.path.startswith('/websocket/'):
            ws = web.WebSocketResponse()
            await ws.prepare(request)
            fields = await ws.receive_json() if request.path.endswith('/subscribe') else None
            self.fields.append(fields)
            mode_only = fields == ['/settings/mode']
            revision = 0
            deadline = asyncio.get_running_loop().time()
            reader = asyncio.create_task(drain(ws))
            try:
                while not ws.closed and not reader.done():
                    revision += 1
                    mode = {'number': self.mode, 'name': 'mania'}
                    play = {'score': revision * 300, 'combo': revision, 'accuracy': 99.51,
                            'hits': {'300': revision, '100': 2, '50': 0, 'miss': 0},
                            'pp': {'current': 242.5, 'fc': 265.2}, 'time': revision * 6.67}
                    beatmap = {'id': 123, 'checksum': 'a' * 32, 'artist': 'Fixture',
                               'title': 'Private benchmark', 'difficulty': 'Another',
                               'stats': {'ar': 9.5, 'od': 8.2, 'cs': 4, 'hp': 6}}
                    if mode_only:
                        if revision == 1 or self.duplicate_modes:
                            await ws.send_json([0 if revision == 1 else 1, revision, [[0, mode]]])
                            self.messages += 1
                    elif fields is not None:
                        pairs = [[index, {'/settings/mode': mode, '/beatmap': beatmap,
                                          '/play': play}[field]] for index, field in enumerate(fields)]
                        await ws.send_json([0 if revision == 1 else 1, revision, pairs])
                        self.messages += 1
                    else:
                        await ws.send_json({'settings': {'mode': mode}, 'beatmap': beatmap, 'play': play})
                        self.messages += 1
                    deadline += 1 / self.rate
                    await asyncio.sleep(max(0, deadline - asyncio.get_running_loop().time()))
            except (ConnectionError, RuntimeError, asyncio.CancelledError):
                pass
            finally:
                reader.cancel()
                await asyncio.gather(reader, return_exceptions=True)
            return ws
        if request.path.startswith('/counter/'):
            return web.Response(text='''<!doctype html><style>
                html,body{margin:0;width:600px;height:200px;overflow:hidden;background:transparent;color:#fff;font:32px sans-serif}
                #bar{height:20px;background:#f80}
                </style><span id=\"score\">0</span><div id=\"bar\"></div><script>
                const socket=new WebSocket('ws://'+location.host+'/websocket/v2/subscribe');
                socket.onopen=()=>socket.send(JSON.stringify(['/settings/mode','/beatmap','/play']));
                let latest=null,queued=false;
                socket.onmessage=e=>{latest=JSON.parse(e.data);if(!queued){queued=true;requestAnimationFrame(()=>{
                    queued=false;const play=latest[2].find(p=>p[0]===2)[1];
                    document.getElementById('score').textContent=play.score;
                    document.getElementById('bar').style.width=(50+play.combo%500)+'px';
                })}};
                </script>''', content_type='text/html')
        if request.path == '/api/counters/search/':
            return web.Response(text='<!doctype html><div class="no-results">No installed counters</div>', content_type='text/html')
        if request.path == '/api/counters/profiles':
            return web.json_response({'active': 'default', 'profiles': ['default'], 'writable': False})
        return web.Response(status=404)


def listener_ports(pid):
    inodes = set()
    for descriptor in Path(f'/proc/{pid}/fd').iterdir():
        try:
            target = descriptor.readlink().as_posix()
        except FileNotFoundError:
            continue
        if target.startswith('socket:['):
            inodes.add(target[8:-1])
    return [int(fields[1].split(':')[1], 16)
            for line in Path(f'/proc/{pid}/net/tcp').read_text().splitlines()[1:]
            if (fields := line.split())[3] == '0A' and fields[9] in inodes]

def cpu(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')


def allocation_counts(directory):
    result = [0, 0, 0]
    for path in directory.glob('*.bin'):
        data = path.read_bytes()
        if len(data) == 24:
            for i, value in enumerate(struct.unpack('QQQ', data)):
                result[i] += value
    return result


async def drain(ws):
    async for _ in ws:
        pass


async def measure(args, label, binary, duplicate_modes, repetition):
    with tempfile.TemporaryDirectory(prefix='vosulay-ruleset-bench-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY',
                    'LD_PRELOAD', 'VOSU_TOOL_PROTOCOL', 'VOSU_TOOL_TOKEN'):
            environment.pop(key, None)
        for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME',
                    'XDG_RUNTIME_DIR', 'XDG_STATE_HOME', 'XDG_DATA_DIRS', 'XDG_CONFIG_DIRS'):
            directory = root / key.lower()
            directory.mkdir(mode=0o700)
            environment[key] = str(directory)
        allocations = root / 'allocations'
        allocations.mkdir()
        if args.allocation_probe:
            environment['LD_PRELOAD'] = str(args.allocation_probe.resolve(strict=True))
            environment['VOSULAY_PERF_ALLOC_DIR'] = str(allocations)
        fixture = Fixture(args.rate, duplicate_modes)
        application = web.Application()
        application.router.add_route('*', '/{path:.*}', fixture.handle)
        runner = web.AppRunner(application)
        await runner.setup()
        site = web.TCPSite(runner, '127.0.0.1', 0)
        await site.start()
        backend = f'http://127.0.0.1:{site._server.sockets[0].getsockname()[1]}/'
        staged = root / 'bin'
        staged.mkdir()
        shutil.copy2(binary, staged / 'vosulay')
        (staged / 'libcef.so').symlink_to(args.cef_library.resolve(strict=True))
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            remote_port = reservation.getsockname()[1]
        config = root / 'config.json'
        config.write_text(json.dumps({'version': 1, 'tosu_migration_completed': True,
            'active_profile': 'default', 'profiles': [{'id': 'default', 'name': 'Default', 'overlays': []}],
            'settings': {'remote_listen_host': '127.0.0.1', 'remote_listen_port': remote_port,
                         'vosu_url': backend, 'allow_local_profile_editing': True}}))
        environment['VOSULAY_ENDPOINT'] = f'@vosulay-ruleset-bench-{root.name}'
        if label not in args.legacy:
            environment.update(VOSU_TOOL_PROTOCOL='1', VOSU_TOOL_TOKEN='b' * 64, VOSU_URL=backend,
                VOSU_TOOL_DATA_DIR=str(root / 'tool-data'), VOSU_TOOL_CACHE_DIR=str(root / 'tool-cache'))
        log_path = args.output / f'{label}-{int(duplicate_modes)}-{repetition}.log'
        clients = []
        tasks = []
        with log_path.open('wb') as log:
            process = await asyncio.create_subprocess_exec(str(staged / 'vosulay'), '--server-only',
                '--config', str(config), '--vosu-url', backend, env=environment, cwd=root,
                stdin=asyncio.subprocess.PIPE, stdout=log, stderr=log)
            try:
                async with ClientSession() as client:
                    until = time.monotonic() + 15
                    broker = f'http://127.0.0.1:{remote_port}/'
                    while True:
                        if process.returncode is not None:
                            raise RuntimeError(f'{label} exited: {log_path.read_text()}')
                        if label not in args.legacy:
                            match = re.search(r'vosu-tool-ready (\d+)', log_path.read_text())
                            if match:
                                other_ports = [port for port in listener_ports(process.pid) if port != int(match[1])]
                                if len(other_ports) == 1:
                                    broker = f'http://127.0.0.1:{other_ports[0]}/'
                                    break
                        else:
                            try:
                                async with client.get(broker + 'overlay') as response:
                                    if response.status == 200:
                                        break
                            except OSError:
                                pass
                        if time.monotonic() > until:
                            raise RuntimeError(f'host readiness timed out: {log_path.read_text()}')
                        await asyncio.sleep(.025)
                    for route in ('websocket/v2', 'websocket/v2/subscribe'):
                        ws = await client.ws_connect(broker.replace('http:', 'ws:') + route)
                        if route.endswith('subscribe'):
                            await ws.send_json(['/settings/mode', '/beatmap', '/play'])
                        clients.append(ws)
                        tasks.append(asyncio.create_task(drain(ws)))
                    await asyncio.sleep(args.warmup)
                    started = time.monotonic()
                    cpu_start = cpu(process.pid)
                    alloc_start = allocation_counts(allocations)
                    messages_start = fixture.messages
                    await asyncio.sleep(args.seconds)
                    elapsed = time.monotonic() - started
                    used = cpu(process.pid) - cpu_start
                    alloc_end = allocation_counts(allocations)
                    record = {'revision': label, 'duplicate_modes': duplicate_modes, 'repetition': repetition,
                              'seconds': elapsed, 'cpu_ms_per_second': used * 1000 / elapsed,
                              'cpu_us_per_tick': used * 1e6 / (elapsed * args.rate),
                              'socket_messages_per_second': (fixture.messages - messages_start) / elapsed,
                              'subscriptions': fixture.fields,
                              'allocations_per_second': (alloc_end[0] - alloc_start[0]) / elapsed,
                              'allocated_bytes_per_second': (alloc_end[1] - alloc_start[1]) / elapsed}
                    print(json.dumps(record), flush=True)
                    with (args.output / 'native.jsonl').open('a') as output:
                        output.write(json.dumps(record) + '\n')
            finally:
                for ws in clients:
                    await ws.close()
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
                if process.returncode is None:
                    process.terminate()
                    try:
                        await asyncio.wait_for(process.wait(), 10)
                    except asyncio.TimeoutError:
                        process.kill()
                        await process.wait()
                await runner.cleanup()


async def main(args):
    for repetition in range(args.reps):
        for duplicate_modes in (False, True):
            for value in args.binary:
                label, path = value.split('=', 1)
                await measure(args, label, Path(path).resolve(strict=True), duplicate_modes, repetition)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', action='append', required=True, help='REVISION=/absolute/path/to/vosulay')
    parser.add_argument('--legacy', action='append', default=[], help='LABEL of a pre-tool-protocol binary; repeatable')
    parser.add_argument('--cef-library', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--rate', type=float, default=150)
    parser.add_argument('--seconds', type=float, default=10)
    parser.add_argument('--warmup', type=float, default=2)
    parser.add_argument('--reps', type=int, default=2)
    parser.add_argument('--allocation-probe', type=Path)
    parser.add_argument('--outer-netns', help=argparse.SUPPRESS)
    args = parser.parse_args()
    namespace = os.readlink('/proc/self/ns/net')
    if args.outer_netns is None:
        subprocess.run(['unshare', '-rn', 'sh', '-c', 'ip link set lo up && exec "$@"',
                        'sh', sys.executable, str(Path(__file__).resolve()), *sys.argv[1:], '--outer-netns', namespace], check=True)
    else:
        if namespace == args.outer_netns:
            parser.error('private network namespace was not created')
        args.output = args.output.resolve()
        args.output.mkdir(parents=True, exist_ok=True)
        asyncio.run(main(args))
