import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const backend = readFileSync(new URL('../assets/backend.js', import.meta.url), 'utf8');
const bridge = readFileSync(new URL('../assets/manage-bridge.js', import.meta.url), 'utf8');
function editor(tool = true) {
  const h = { failure: 0, requests: [], frames: [], envelopes: [], intervals: [], navigations: [],
    state: { revision: 0, config: { profiles: [] }, routing: {
      generation: 'first', backend: 'http://127.0.0.1:24050/', view: 'http://127.0.0.1:37100/',
    } } };
  class Element {
    children = [];
    style = {};
    inert = false;
    append(...children) { this.children.push(...children); }
    replaceChildren() { this.children = []; }
    setAttribute() {}
    remove() { body.children = body.children.filter(child => child !== this); }
    contains(target) { return this === target || this.children.includes(target); }
    focus() { h.focused = this; }
  }
  const body = new Element();
  h.stage = new Element();
  h.controls = new Element();
  body.append(h.controls);
  const document = { body, baseURI: 'http://manage.localhost:49000/overlay',
    documentElement: { dataset: {} }, createElement: () => new Element(),
    querySelectorAll: () => h.frames, getElementById: () => h.stage };
  const fetch = async path => {
    h.requests.push(path);
    if (path === '/api/dashboard') return { ok: true, json: async () => ({ origin: 'http://127.0.0.1:24052/' }) };
    return { ok: !h.failure, status: h.failure || 200, text: async () => h.failure ? 'expired' :
      JSON.stringify(path.endsWith('state') ? h.state : { type: 'counters', counters: [] }) };
  };
  const XMLHttpRequest = function () {};
  XMLHttpRequest.prototype.open = () => {};
  const window = { fetch, addEventListener() {}, postMessage() {},
    __VOSULAY_RECEIVE__: envelope => h.envelopes.push(envelope) };
  if (tool) window.__VOSULAY_TOOL_FETCH__ = fetch;
  const location = { protocol: 'http:', host: 'manage.localhost:49000', origin: 'http://manage.localhost:49000',
    assign: url => h.navigations.push(url), reload: () => h.navigations.push('reload') };
  const context = vm.createContext({ window, document, location, XMLHttpRequest, URL, Request,
    fetch: (...args) => window.fetch(...args), setInterval: callback => h.intervals.push(callback) });
  vm.runInContext(`(${backend})`, context)(h.state.routing.backend, h.state.routing.view, true);
  vm.runInContext(`(${bridge})`, context)(h.state.routing.view);
  h.window = window;
  h.body = body;
  h.flush = async () => { await new Promise(setImmediate); await new Promise(setImmediate); };
  h.poll = async () => { h.intervals[0](); await h.flush(); };
  h.addFrame = source => {
    const frame = { dataset: { vosulaySourceUrl: source }, src: window.__VOSULAY_BACKEND_URL__(source), hidden: false,
      addEventListener: (_, loaded) => { frame.loaded = loaded; } };
    h.frames.push(frame);
    return frame;
  };
  window.__VOSULAY_POST_MESSAGE__({ type: 'ready' });
  return h;
}

for (const tool of [true, false]) test(`${tool ? 'tool' : 'standalone'} refreshes broker generation even when revisions repeat`, async () => {
  const h = editor(tool);
  await h.flush();
  const frame = h.addFrame('http://127.0.0.1:24050/counter/');
  const local = h.addFrame('vosulay-overlay://public-id/root/sub/index.html');
  const configs = h.envelopes.filter(e => e.type === 'config').length;
  h.state = { ...h.state, routing: { ...h.state.routing, generation: 'second', view: 'http://127.0.0.1:37101/' } };
  await h.poll();
  assert.equal(frame.src, 'http://127.0.0.1:37101/counter/');
  assert.equal(local.src, 'http://127.0.0.1:37101/local/public-id/sub/index.html');
  assert.equal(h.envelopes.filter(e => e.type === 'config').length, configs + 1);
  assert.equal(h.controls.inert, false);
  assert.equal(h.window.__VOSULAY_UPDATE_ROUTING__, undefined);
});

test('expired tool authority freezes editing, hides stale frames and offers dashboard reopening', async () => {
  const h = editor();
  await h.flush();
  const frame = h.addFrame('http://127.0.0.1:24050/counter/');
  h.failure = 403;
  await h.poll();
  assert.equal(frame.hidden, true);
  assert.equal(h.controls.inert, true);
  const notice = h.body.children.at(-1);
  assert.match(notice.children[0].textContent, /read-only/);
  const count = h.requests.length;
  h.window.__VOSULAY_POST_MESSAGE__({ type: 'save-config', config: {} });
  h.failure = 0;
  await h.poll();
  assert.equal(h.requests.length, count, 'never renews or reuses an expired write session');
  assert.equal(notice.children[1].textContent, 'Open from vosu');
  await notice.children[1].onclick();
  assert.deepEqual(h.navigations, ['http://127.0.0.1:24052/tools']);
});

test('temporary outage pauses editing and recovers the same authority without stale frames', async () => {
  const h = editor(false);
  await h.flush();
  const frame = h.addFrame('http://127.0.0.1:24050/counter/');
  h.failure = 503;
  await h.poll();
  assert.equal(h.controls.inert, true);
  assert.equal(frame.hidden, true);
  h.failure = 0;
  h.state.routing = { ...h.state.routing, generation: 'replacement', view: 'http://127.0.0.1:37102/' };
  await h.poll();
  assert.equal(h.controls.inert, false);
  assert.equal(frame.hidden, true, 'stale pixels stay hidden until replacement loads');
  frame.loaded();
  assert.equal(frame.hidden, false);
  assert.equal(frame.src, 'http://127.0.0.1:37102/counter/');
  assert.equal(h.body.children.length, 1);
});

test('initial tool document rejection offers the same explicit dashboard action', async () => {
  const html = readFileSync(new URL('../assets/tool/index.html', import.meta.url), 'utf8');
  const script = html.split('<script type="module">')[1].split('</script>')[0]
    .replace(/^import .*;$/m, '');
  const status = { children: [], append(child) { this.children.push(child); } };
  const navigations = [];
  await vm.runInNewContext(`(async () => { ${script} })()`, {
    vosu: { ready: Promise.resolve(), canWrite: true, fetch: async () => ({ ok: false, status: 403 }) },
    window: {}, document: { getElementById: () => status, createElement: () => ({}) }, URL,
    fetch: async () => ({ ok: true, json: async () => ({ origin: 'http://127.0.0.1:24052/' }) }),
    location: { assign: url => navigations.push(url) },
  });
  assert.match(status.textContent, /read-only or unavailable/);
  assert.equal(status.children[0].textContent, 'Open from vosu');
  await status.children[0].onclick();
  assert.deepEqual(navigations, ['http://127.0.0.1:24052/tools']);
});
