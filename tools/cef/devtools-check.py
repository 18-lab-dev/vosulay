#!/usr/bin/env python3
"""Exercise the pinned CEF DevTools server on a private display/network.

Requires aiohttp, Xvfb and bubblewrap. Uses the real manager benchmark fixture;
software screenshots and protocol checks do not establish game/GPU behavior.
"""
import argparse
import asyncio
import base64
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys

from aiohttp import ClientSession, ClientError, web


async def check(args, display):
    result = {}
    env = dict(os.environ, DISPLAY=display, XDG_SESSION_TYPE='x11',
               LIBGL_ALWAYS_SOFTWARE='1', GALLIUM_DRIVER='llvmpipe', LP_NUM_THREADS='2')
    for key in ('WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY', 'LD_PRELOAD'):
        env.pop(key, None)
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME',
                'XDG_RUNTIME_DIR', 'XDG_STATE_HOME', 'XDG_DATA_DIRS', 'XDG_CONFIG_DIRS'):
        directory = args.output / key.lower()
        directory.mkdir(mode=0o700)
        env[key] = str(directory)
    env['LD_LIBRARY_PATH'] = str(args.binary.parents[1] / 'cef-lib')
    outcomes = []
    async def page(request):
        ws = request.query.get('ws', '')
        return web.Response(text='<script>const w=new WebSocket(' + json.dumps(ws) + ');w.onopen=()=>fetch("/result?state=open");w.onerror=()=>fetch("/result?state=refused");</script>', content_type='text/html')
    async def outcome(request):
        outcomes.append(request.query['state'])
        return web.Response(text='ok')
    app = web.Application()
    app.router.add_get('/', page)
    app.router.add_get('/result', outcome)
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, '127.0.0.1', 0)
    await site.start()
    origin = f'http://127.0.0.1:{site._server.sockets[0].getsockname()[1]}'
    config_path = args.output / 'config.json'
    config_path.write_text(json.dumps({'version': 1, 'active_profile': 'default', 'profiles': [{'id': 'default', 'name': 'Default', 'overlays': []}], 'settings': {}}))
    async with ClientSession() as client:
        for enabled in (False, True, False):
            label = ('on' if enabled else 'off') + str(len(result))
            log = (args.output / (label + '.log')).open('wb')
            command = [str(args.binary), '--cef-dir', str(args.cef_dir), '--cache', str(args.output / label),
                       '--backend', origin + '/', '--seconds', '15', '--editing', '--ozone-platform=x11',
                       '--use-gl=angle', '--use-angle=swiftshader', '--config', str(config_path)]
            if enabled:
                config = json.loads(config_path.read_text())
                config['settings'].update(devtools_enabled=True, devtools_port=39229)
                config_path.write_text(json.dumps(config))
            process = await asyncio.create_subprocess_exec(*command, env=env, stdout=log, stderr=log)
            try:
                targets = None
                for _ in range(160 if enabled else 50):
                    try:
                        async with client.get('http://127.0.0.1:39229/json/list') as reply:
                            targets = await reply.json()
                        if enabled and targets:
                            break
                        if not enabled:
                            raise AssertionError('debugging listener present while disabled')
                    except (ClientError, OSError):
                        pass
                    await asyncio.sleep(.05)
                assert process.returncode is None, f'CEF fixture exited: {process.returncode}'
                if not enabled:
                    result[label] = {'endpoint_absent': True}
                    continue
                assert targets, 'no overlay targets'
                result['targets'] = targets
                listeners = []
                for table in ('/proc/net/tcp', '/proc/net/tcp6'):
                    for line in Path(table).read_text().splitlines()[1:]:
                        fields = line.split()
                        if fields[3] == '0A' and int(fields[1].split(':')[1], 16) == 39229:
                            listeners.append(fields[1])
                assert listeners == ['0100007F:993D'], listeners
                result['listeners'] = listeners
                async with client.get('http://127.0.0.1:39229/json/list', headers={'Host': 'rebind.attacker.invalid'}) as reply:
                    result['foreign_host'] = {'status': reply.status, 'body': await reply.text()}
                    assert reply.status == 500
                target = next(t for t in targets if t['type'] == 'page' and t['url'].startswith('vosulay://manager'))
                try:
                    await client.ws_connect(target['webSocketDebuggerUrl'], origin=origin)
                    raise AssertionError('Origin-bearing WebSocket accepted')
                except ClientError as error:
                    result['origin_handshake'] = str(error)
                    assert getattr(error, 'status', None) == 403
                async with client.ws_connect(target['webSocketDebuggerUrl']) as ws:
                    sequence = 0
                    async def cdp(method, params):
                        nonlocal sequence
                        sequence += 1
                        await ws.send_json({'id': sequence, 'method': method, 'params': params})
                        while True:
                            message = await ws.receive_json(timeout=5)
                            if message.get('id') == sequence:
                                assert 'error' not in message, message
                                assert 'exceptionDetails' not in message.get('result', {}), message
                                return message.get('result', {})
                    await cdp('Runtime.evaluate', {'expression': '(()=>{const f=document.createElement("iframe");f.src=' + json.dumps(origin + '/?ws=' + target['webSocketDebuggerUrl']) + ';document.body.append(f)})()'})
                    for _ in range(100):
                        if outcomes:
                            break
                        await asyncio.sleep(.05)
                    assert outcomes == ['refused'], outcomes
                    result['page_origin_websocket'] = outcomes
                    for theme in ('light', 'dark'):
                        await cdp('Runtime.evaluate', {'expression': f'document.documentElement.dataset.theme={json.dumps(theme)};document.getElementById("open-settings").click();document.querySelector("[data-section=settings-section-advanced]").click();document.getElementById("settings-section-advanced").scrollIntoView()'})
                        await asyncio.sleep(.6)
                        image = await cdp('Page.captureScreenshot', {'format': 'png'})
                        (args.output / f'{theme}-settings.png').write_bytes(base64.b64decode(image['data']))
                        await cdp('Runtime.evaluate', {'expression': 'document.getElementById("settings-cancel").click()'})
                        await asyncio.sleep(.2)
                        image = await cdp('Page.captureScreenshot', {'format': 'png'})
                        (args.output / f'{theme}-indicator.png').write_bytes(base64.b64decode(image['data']))
                    await cdp('Runtime.evaluate', {'expression': 'document.getElementById("open-settings").click();document.getElementById("devtools-enabled").click();document.getElementById("settings-form").requestSubmit()'})
                    for _ in range(100):
                        if not json.loads(config_path.read_text())['settings']['devtools_enabled']:
                            break
                        await asyncio.sleep(.05)
                    assert not json.loads(config_path.read_text())['settings']['devtools_enabled']
                    result['unchecked_and_saved'] = True
                result[label] = {'endpoint_present': True}
            finally:
                if process.returncode is None:
                    process.terminate()
                    try:
                        await asyncio.wait_for(process.wait(), 10)
                    except asyncio.TimeoutError:
                        process.kill()
                        await process.wait()
                log.close()
    await runner.cleanup()
    (args.output / 'security.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--cef-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--xvfb', type=Path, default=Path('/usr/bin/Xvfb'))
    parser.add_argument('--inside', action='store_true')
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.cef_dir = args.cef_dir.resolve(strict=True)
    args.xvfb = args.xvfb.resolve(strict=True)
    args.output = args.output.resolve()
    if not args.inside:
        args.output.mkdir(parents=True, exist_ok=False)
        return subprocess.call(['bwrap', '--die-with-parent', '--unshare-pid', '--unshare-ipc', '--unshare-net',
                                '--ro-bind', '/', '/', '--bind', str(args.output), str(args.output),
                                '--tmpfs', '/tmp', '--proc', '/proc', '--dev-bind', '/dev', '/dev',
                                sys.executable, str(Path(__file__).resolve()), '--inside', '--binary', str(args.binary),
                                '--cef-dir', str(args.cef_dir), '--output', str(args.output), '--xvfb', str(args.xvfb)])
    read_fd, write_fd = os.pipe()
    with (args.output / 'xvfb.log').open('wb') as log:
        server = subprocess.Popen([str(args.xvfb), '-displayfd', str(write_fd), '-screen', '0', '2560x1440x24', '-nolisten', 'tcp'], pass_fds=(write_fd,), stdout=log, stderr=log)
        os.close(write_fd)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(read_fd, selectors.EVENT_READ)
                assert selector.select(10), 'Xvfb not ready'
                display = ':' + os.read(read_fd, 32).decode().strip()
            asyncio.run(check(args, display))
        finally:
            os.close(read_fd)
            server.terminate()
            server.wait(timeout=5)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
