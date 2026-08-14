import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { chmodSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';

const addonPath = process.env.VOSULAY_NATIVE_ADDON;
const fakeAgentPath = process.env.VOSULAY_FAKE_AGENT;

function exited(child) {
  return new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

function nextEvent(handle, name) {
  return new Promise((resolve) => {
    handle.on(name, resolve);
  });
}


test('overlay-node fake-agent lifecycle when addon and agent are available', { timeout: 5000 }, async (t) => {
  if (!addonPath || !fakeAgentPath) {
    t.skip('set VOSULAY_NATIVE_ADDON and VOSULAY_FAKE_AGENT to built artifacts');
    return;
  }
  const addon = createRequire(import.meta.url)(addonPath);
  assert.equal(typeof addon.attach, 'function');
  const temporary = await mkdtemp(path.join(tmpdir(), 'vosulay-node-attach-'));
  const attachTool = path.join(temporary, 'attach-tool');
  await writeFile(attachTool, '#!/bin/sh\ncredentials="/tmp/vosulay-node-attach-$VOSULAY_ATTACH_TARGET_PID"\nprintf "%s %s\\n" "$4" "$5" > "$credentials"\n');
  chmodSync(attachTool, 0o700);
  // The attach tool publishes the credentials while attach waits. The target
  // then execs the fake agent without changing PID, satisfying SO_PEERCRED.
  const child = spawn('/bin/sh', ['-c',
    'credentials="/tmp/vosulay-node-attach-$$"; while [ ! -s "$credentials" ]; do sleep 0.01; done; read -r name cookie < "$credentials"; rm -f "$credentials"; exec "$VOSULAY_FAKE_AGENT" "$name" "$cookie"'
  ], {
    stdio: ['ignore', 'ignore', 'inherit'],
    env: {
      ...process.env,
      VOSULAY_FAKE_AGENT: fakeAgentPath,
    },
  });
  const exitResult = exited(child);
  let handle;
  try {
    handle = await addon.attach(child.pid, {
      attachToolPath: attachTool,
      profilerLibraryPath: '/unused/profiler',
      agentLibraryPath: fakeAgentPath,
      timeout: 5,
    });
    assert.equal(handle.pid, child.pid);
    assert.match(handle.socketName(), /^[0-9a-f]+$/);
    assert.equal(handle.cookie().length, 64);
    // The fake agent advertises shared memory only, so zero-copy stays off.
    assert.equal(handle.supportsDmabuf(), false);
    assert.equal(handle.supportsBgraShm(), true);
    await handle.setVisible(true);
    await new Promise((resolve) => setTimeout(resolve, 100));
    const target = await nextEvent(handle, 'target');
    assert.equal(target, 'created:1:1280:720');
    await handle.captureInput(true);
    assert.equal(child.exitCode, null);
    await handle.destroy();
    const result = await exitResult;
    assert.equal(result.code, 0);
  } finally {
    if (handle) {
      await handle.destroy().catch(() => {});
    }
    child.kill();
    await rm(temporary, { recursive: true, force: true });
  }
});
