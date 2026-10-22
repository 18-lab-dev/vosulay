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
