import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../assets/backend.js', import.meta.url), 'utf8');
function manager(tool) {
  const nativeFetch = input => input;
  const XMLHttpRequest = function () {};
  XMLHttpRequest.prototype.open = function () {};
  const window = { fetch: nativeFetch, XMLHttpRequest };
  if (tool) window.__VOSULAY_TOOL_FETCH__ = () => {};
  const context = vm.createContext({ window, XMLHttpRequest, URL, Request,
    document: { baseURI: 'http://127.0.0.1:25950/tools/vosulay/' },
    location: { protocol: 'http:', host: '127.0.0.1:25950', origin: 'http://127.0.0.1:25950' } });
  vm.runInContext(`(${source})`, context)('http://127.0.0.1:25950/', 'http://127.0.0.1:25949/', true);
  return { window, nativeFetch };
}

test('tool manager routes counter URLs but never rewrites authenticated host fetch', () => {
  const { window, nativeFetch } = manager(true);
  assert.equal(window.fetch, nativeFetch);
  assert.equal(window.fetch('http://127.0.0.1:25950/tools/vosulay/api/v1/manage/state'), 'http://127.0.0.1:25950/tools/vosulay/api/v1/manage/state');
  assert.equal(window.__VOSULAY_BACKEND_URL__('http://127.0.0.1:25950/counter/'), 'http://127.0.0.1:25949/counter/');
});

test('standalone manager keeps its existing backend fetch rewriting', () => {
  const { window, nativeFetch } = manager(false);
  assert.notEqual(window.fetch, nativeFetch);
  assert.equal(window.fetch('http://127.0.0.1:25950/json/v2'), 'http://127.0.0.1:25949/json/v2');
});

test('tool frames keep an opaque origin for catalog and custom counters', () => {
  const managerSource = readFileSync(new URL('../assets/manager.js', import.meta.url), 'utf8');
  const elementSource = managerSource.match(/  function overlayElement\([\s\S]*?\r?\n  }\r?\n/);
  assert.ok(elementSource, 'exercise the production frame constructor');
  for (const tool of [true, false]) {
    for (const custom of [true, false]) {
      const frames = [];
      const context = vm.createContext({
        document: { documentElement: { dataset: { vosulayTool: tool ? '1' : '0' } } },
        window: { __VOSULAY_BACKEND_URL__: () => 'http://127.0.0.1:25949/counter/' },
        state: { selected: new Set() },
        applyOverlayStyle() {},
        makeElement(tag) {
          const element = { dataset: {}, attributes: {}, append() {},
            setAttribute(name, value) { this.attributes[name] = value; } };
          if (tag === 'iframe') frames.push(element);
          return element;
        }
      });
      vm.runInContext(`${elementSource[0]}; globalThis.createFrame = overlayElement;`, context);
      context.createFrame({ counter_id: 'counter', ...(custom ? { url: 'http://127.0.0.1:25950/counter/' } : {}) },
        { available: true, url: 'http://127.0.0.1:25950/counter/', name: 'Counter', tag: 'Counter' });
      assert.equal(frames.length, 1);
      assert.equal(frames[0].src, 'http://127.0.0.1:25949/counter/');
      assert.equal(frames[0].attributes.sandbox,
        tool ? 'allow-scripts' : custom ? 'allow-scripts allow-same-origin' : undefined);
    }
  }
});

test('native frames wait for renderer acknowledgement and renew tokens after a process swap', () => {
  const source = readFileSync(new URL('../assets/manager.js', import.meta.url), 'utf8');
  const helpers = source.match(/  const overlaySiteFrames = new Map\(\);[\s\S]*?(?=  function overlayElement)/)[0];
  const constructor = source.match(/  function overlayElement\([\s\S]*?\r?\n  }\r?\n/)[0];
  const receiver = source.match(/  function receive\(message\) \{[\s\S]*?\r?\n  }\r?\n/)[0];
  const frames = [], messages = [];
  let sequence = 0;
  const context = vm.createContext({
    document: { documentElement: { dataset: {} } },
    window: { __VOSULAY_NATIVE_SITES__: true },
    crypto: { randomUUID: () => `request-${++sequence}` },
    state: { selected: new Set() },
    ipc: message => messages.push(message),
    applyOverlayStyle() {},
    makeElement(tag) {
      const element = { dataset: {}, append() {}, setAttribute() {},
        removeAttribute(name) { delete this[name]; },
        replaceWith() {},
        cloneNode() { const clone = { ...this, dataset: { ...this.dataset } }; frames.push(clone); return clone; }
      };
      if (tag === 'iframe') frames.push(element);
      return element;
    }
  });
  vm.runInContext(`${helpers}\n${constructor}\n${receiver}\nglobalThis.createFrame=overlayElement;globalThis.deliver=receive;globalThis.release=releaseOverlaySite;`, context);
  context.createFrame({ counter_id: 'counter' },
    { available: true, url: 'http://127.0.0.1:24050/counter/', name: 'Counter', tag: 'Counter' });
  let frame = frames[0];
  assert.equal(frame.src, undefined, 'no overlay navigation before native site allocation');
  assert.equal(messages[0].type, 'prepare-overlay-site');
  const allocated = { type: 'overlay-site', request_id: 'request-1',
    url: 'http://127.0.0.2:41203/counter/', prepare: 'http://127.0.0.2:41203/__vosulay_prepare', direct: false };
  context.deliver(allocated);
  assert.equal(frame.src, allocated.prepare);
  context.deliver({ type: 'overlay-site-ready', request_id: 'request-1', ready: true });
  assert.equal(frame.src, allocated.url);
  context.deliver({ type: 'overlay-site-ready', request_id: 'request-1', ready: false });
  frame = frames.at(-1);
  assert.equal(frame.name, 'vosulay-site-request-2');
  assert.equal(messages.at(-2).type, 'release-overlay-site');
  assert.equal(messages.at(-1).type, 'prepare-overlay-site');
  context.deliver({ type: 'overlay-site-ready', request_id: 'request-1', ready: true });
  context.deliver({ ...allocated, request_id: 'request-2' });
  assert.equal(frame.src, allocated.prepare, 'old acknowledgement cannot navigate the replacement cycle');
  context.deliver({ type: 'overlay-site-ready', request_id: 'request-2', ready: true });
  assert.equal(frame.src, allocated.url);
  context.release({ querySelector: () => frame });
  assert.equal(messages.at(-1).type, 'release-overlay-site');
  frame.src = 'about:blank';
  context.deliver({ type: 'overlay-site-ready', request_id: 'request-2', ready: true });
  assert.equal(frame.src, 'about:blank', 'removed frames ignore late acknowledgements');
});

test('large native profiles bound allocation requests without losing shared-site fallbacks', () => {
  const source = readFileSync(new URL('../assets/manager.js', import.meta.url), 'utf8');
  const helpers = source.match(/  const overlaySiteFrames = new Map\(\);[\s\S]*?(?=  function overlayElement)/)[0];
  const receiver = source.match(/  function receive\(message\) \{[\s\S]*?\r?\n  }\r?\n/)[0];
  const messages = [];
  let sequence = 0;
  const context = vm.createContext({ crypto: { randomUUID: () => `request-${++sequence}` }, ipc: message => messages.push(message) });
  vm.runInContext(`${helpers}\n${receiver}\nglobalThis.prepare=prepareOverlaySite;globalThis.deliver=receive;`, context);
  for (let index = 0; index < 70; index++) context.prepare({ dataset: {} }, `http://127.0.0.1:24050/counter-${index}/`);
  assert.equal(messages.length, 8);
  for (let index = 1; index <= 70; index++) {
    context.deliver({ type: 'overlay-site', request_id: `request-${index}`, direct: true,
      url: `http://127.0.0.1:41203/counter-${index - 1}/`, prepare: 'unused' });
    assert.equal(messages.length, Math.min(70, index + 8));
  }
  assert.equal(new Set(messages.map(message => message.request_id)).size, 70);
});
