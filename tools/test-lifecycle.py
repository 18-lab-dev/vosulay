#!/usr/bin/env python3
"""Exercise native lifecycle ownership with a real vosu and packaged Linux runtime.

The launcher creates a private network namespace (see docs/testing.md). All app
state is private; no desktop, game attachment, installed app, or external network is used.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import sys
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vosu', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--outer-netns', help=argparse.SUPPRESS)
    args = parser.parse_args()
    current_netns = os.readlink('/proc/self/ns/net')
    if args.outer_netns is None:
        subprocess.run(['unshare', '-rn', 'sh', '-c',
                        'ip link set lo up && exec "$@"', 'sh', sys.executable,
                        str(Path(__file__).resolve()), *sys.argv[1:],
                        '--outer-netns', current_netns], check=True)
        return
    if current_netns == args.outer_netns:
        parser.error('private network namespace was not created')
    vosu = args.vosu.resolve(strict=True)
    runtime = args.runtime.resolve(strict=True)
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    result = {'isolation': {'outer_netns': args.outer_netns, 'private_netns': current_netns}}
    processes = []
    logs = []
    with tempfile.TemporaryDirectory(prefix='vosulay-lifecycle-') as directory:
        root = Path(directory)
        env = dict(os.environ)
        for key in tuple(env):
            if key.startswith(('XDG_', 'VOSU_TOOL_')) or key in ('DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY'):
                env.pop(key)
        for key, name in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_CACHE_HOME', 'cache'), ('XDG_DATA_HOME', 'data'), ('XDG_STATE_HOME', 'state'), ('XDG_RUNTIME_DIR', 'runtime'), ('XDG_DATA_DIRS', 'system-data'), ('XDG_CONFIG_DIRS', 'system-config')]:
            path = root / name
            path.mkdir(mode=0o700)
            env[key] = str(path)
        # The namespace is the safety boundary for the normal per-uid abstract
        # endpoint too, not only the filesystem instance/control endpoints.
        env.pop('VOSULAY_ENDPOINT', None)
        install = root / 'data/vosu/tools/vosulay'
        install.mkdir(parents=True)
        try:
            shutil.copytree(runtime, install / 'bin', copy_function=os.link, symlinks=True)
        except OSError:
            shutil.rmtree(install / 'bin', ignore_errors=True)
            shutil.copytree(runtime, install / 'bin', symlinks=True)
        shutil.copytree(ROOT / 'assets', install / 'assets')
        manifest = (ROOT / 'tool.toml').read_text()
        assert manifest.count('command = ["bin/vosulay"]') == 1
        # Real service, real package layout; only suppress native GUI/attachment.
        (install / 'tool.toml').write_text(manifest.replace('command = ["bin/vosulay"]', 'command = ["bin/vosulay", "--server-only"]'))
        config = root / 'config/vosu'
        config.mkdir()
        (config / 'vosu.toml').write_text('ENABLE_AUTOUPDATE = false\nOPEN_DASHBOARD_ON_STARTUP = false\nSTART_AT_LOGIN = false\nSERVER_IP = "127.0.0.1"\nSERVER_PORT = 25968\nDASHBOARD_IP = "127.0.0.1"\nDASHBOARD_PORT = 25969\nALLOWED_IPS = "127.0.0.1,localhost,absolute"\n')
        standalone_config = root / 'config/vosulay'
        standalone_config.mkdir()
        (standalone_config / 'config.json').write_text(json.dumps({'version': 1, 'active_profile': 'default', 'profiles': [{'id': 'default', 'name': 'Standalone import', 'overlays': []}], 'custom_migration_completed': True, 'settings': {'vosu_url': 'http://127.0.0.1:9', 'remote_listen_host': '127.0.0.1', 'remote_listen_port': 25967}}))
        control = root / 'runtime/vosulay/control.sock'
        overlay = 'http://127.0.0.1:25968'
        dashboard = 'http://127.0.0.1:25969'
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        csrf = {}

        def http(path, method='GET', data=None, on_dashboard=True):
            headers = dict(csrf) if method != 'GET' else {}
            payload = None if data is None else json.dumps(data).encode()
            if payload is not None:
                headers['Content-Type'] = 'application/json'
            request = urllib.request.Request((dashboard if on_dashboard else overlay) + path, data=payload, method=method, headers=headers)
            with opener.open(request, timeout=5) as response:
                return response.read().decode()

        def state():
            return json.loads(http('/api/tools/vosulay'))

        def until(predicate, seconds=20):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                try:
                    value = predicate()
                    if value:
                        return value
                except (OSError, urllib.error.URLError, KeyError, json.JSONDecodeError):
                    pass
                time.sleep(.05)
            raise AssertionError('bounded lifecycle condition did not settle')

        def spawn(argv, name):
            log = (evidence / (name + '.log')).open('w')
            logs.append(log)
            process = subprocess.Popen([str(arg) for arg in argv], cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            processes.append(process)
            return process

        def boot(name):
            nonlocal csrf
            host = spawn([vosu], name)
            until(lambda: state())
            page = http('/tools')
            context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S)[1])
            csrf = {'Origin': dashboard, 'X-Vosu-CSRF': context['csrf']}
            return host

        def action(name):
            return json.loads(http('/api/tools/vosulay/' + name, 'POST', {}))

        def stop(process):
            process.terminate()
            assert process.wait(timeout=15) == 0

        try:
            # Approve this actual user-root installation through the dashboard,
            # then retain its enabled state for the cold-start scenarios.
            host = boot('approval')
            action('trust')
            action('enable')
            stop(host)

            standalone = spawn([runtime / 'vosulay', '--server-only'], 'standalone-no-vosu')
            until(lambda: control.exists() and standalone.poll() is None)
            result['c_no_vosu_standalone_normal'] = True
            host = boot('takeover')
            until(lambda: state()['status'] == 'running')
            assert standalone.wait(timeout=10) == 0
            running = state()
            assert running['process']['restarts'] == 0
            assert json.loads((root / 'data/vosu/tool-data/vosulay/config.json').read_text())['profiles'][0]['name'] == 'Standalone import'
            result['a_standalone_then_vosu_takeover'] = running

            handoff = spawn([runtime / 'vosulay', '--server-only'], 'handoff')
            assert handoff.wait(timeout=6) == 0
            handoff_text = (evidence / 'handoff.log').read_text()
            assert 'managed by vosu' in handoff_text and '#vosu-launch=' not in handoff_text
            after = state()
            assert after['process']['pid'] == running['process']['pid'] and after['process']['restarts'] == 0
            result['b_handoff_same_host'] = after
            tool_pid = after['process']['pid']
            stop(host)
            until(lambda: not Path('/proc', str(tool_pid)).exists())
            result['e_vosu_exit_stops_tool'] = True

            host = boot('disabled')
            until(lambda: state()['status'] == 'running')
            action('disable')
            until(lambda: state()['status'] == 'disabled')
            standalone = spawn([runtime / 'vosulay', '--server-only'], 'standalone-disabled')
            until(lambda: control.exists() and standalone.poll() is None)
            assert state()['process']['pid'] is None
            stop(host)
            assert standalone.poll() is None
            stop(standalone)
            result['d_disabled_tool_standalone_normal'] = True

            # A foreign lock holder with no matching vosulay control server must
            # remain alive; startup fails cleanly rather than crash-restarting.
            lock_path = root / 'runtime/vosulay/instance.lock'
            with lock_path.open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                host = boot('refused-takeover')
                action('enable')
                try:
                    action('start')
                except urllib.error.HTTPError as error:
                    assert error.code == 400 and 'exited' in error.read().decode()
                def stopped_cleanly():
                    value = state()
                    return value if value['status'] == 'stopped' and value['status_detail'] == 'exited' else None
                refused = until(stopped_cleanly)
                assert refused['process']['restarts'] == 0
                assert 'ownership takeover refused' in http('/api/tools/vosulay/logs')
                result['refused_takeover_no_restart'] = refused
                stop(host)
            (evidence / 'results.json').write_text(json.dumps(result, indent=2))
            print(json.dumps({'passed': list(result), 'evidence': str(evidence)}, indent=2))
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
            for log in logs:
                log.close()


if __name__ == '__main__':
    main()
