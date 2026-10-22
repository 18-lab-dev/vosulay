function (viewOrigin) {
  // Host bridge for the manage page (manage.localhost). The manager script runs
  // after this and talks to vosulay over HTTP instead of the in-game native bridge.
  document.documentElement.dataset.vosulayHost = 'page';
  const toolFetch = window.__VOSULAY_TOOL_FETCH__;
  delete window.__VOSULAY_TOOL_FETCH__;
  if (toolFetch) document.documentElement.dataset.vosulayTool = '1';
  const routeBackend = window.__VOSULAY_BACKEND_URL__;
  const view = new URL(viewOrigin);
  // Local HTML overlays load from the listener's own /local/ route, like on the
  // view-only page: the manage origin serves nothing but the editor.
  const localUrl = (url) => {
    let parsed;
    try {
      parsed = new URL(url);
    } catch (_) {
      return null;
    }
    if (parsed.protocol !== 'vosulay-overlay:' || !parsed.hostname) return null;
    const [, root, ...relative] = parsed.pathname.split('/');
    if (!root || !relative.length) return null;
    return new URL(`/local/${parsed.hostname}/${relative.join('/')}${parsed.search}${parsed.hash}`, view).href;
  };
  Object.defineProperty(window, '__VOSULAY_BACKEND_URL__', {
    value: (url) => localUrl(String(url)) || (typeof routeBackend === 'function' ? routeBackend(url) : String(url)),
    configurable: true,
  });

  const deliver = (envelope) => {
    if (typeof window.__VOSULAY_RECEIVE__ === 'function') window.__VOSULAY_RECEIVE__(envelope);
  };
  const describe = (error) => (error instanceof Error ? error.message : String(error));
  const fail = (error) => deliver({ type: 'error', message: describe(error) });

  async function request(action, body) {
    const init = { cache: 'no-store', credentials: 'same-origin' };
    if (body !== undefined) {
      init.method = 'POST';
      init.headers = { 'Content-Type': 'application/json' };
      init.body = JSON.stringify(body);
    }
    const response = toolFetch
      ? await toolFetch(`v1/manage/${action}`, init)
      : await fetch(`/__vosulay/manage/${action}`, init);
    const text = await response.text();
    if (!response.ok) throw new Error(text.trim() || `vosulay returned ${response.status}`);
    return text ? JSON.parse(text) : null;
  }

  // vosulay serves a local HTML file only once a saved placement uses it, so frames
  // that loaded a newly chosen file before its first save reload after it.
  let served = new Set();
  function noteServed(config) {
    const profiles = config && Array.isArray(config.profiles) ? config.profiles : [];
    const next = new Set(profiles
      .flatMap((profile) => (profile && Array.isArray(profile.overlays) ? profile.overlays : []))
      .map((overlay) => (overlay && typeof overlay.url === 'string' ? localUrl(overlay.url) : null))
      .filter(Boolean));
    for (const frame of document.querySelectorAll('iframe.overlay-frame')) {
      if (next.has(frame.src) && !served.has(frame.src)) frame.src = frame.src;
    }
    served = next;
  }

  let revision = null;
  let saving = 0;
  // Bumped when a save starts or ends. A state poll that overlapped a save may
  // carry configuration older than the save's reply; revision order cannot tell,
  // because revisions restart when vosulay restarts.
  let saveEpoch = 0;
  async function loadState(force) {
    const epoch = saveEpoch;
    const state = await request('state');
    deliver({ type: 'ruleset', ruleset: state.ruleset ?? null });
    if (saving || epoch !== saveEpoch) return;
    if (!force && state.revision === revision) return;
    revision = state.revision;
    noteServed(state.config);
    deliver({ type: 'config', config: state.config, revision: state.revision });
  }

  async function send(message) {
    const save = message.type === 'save-config';
    if (save) {
      saving += 1;
      saveEpoch += 1;
    }
    try {
      for (const envelope of await request('message', message)) {
        if (envelope && Number.isSafeInteger(envelope.revision)) revision = envelope.revision;
        if (save && envelope && envelope.type === 'saved') noteServed(message.config);
        if (envelope && envelope.type === 'config') noteServed(envelope.config);
        deliver(envelope);
      }
    } catch (error) {
      deliver(save
        ? { type: 'save-error', save_id: message.save_id, message: describe(error), code: 'save' }
        : { type: 'error', message: describe(error) });
    } finally {
      if (save) {
        saving -= 1;
        saveEpoch += 1;
      }
    }
  }

  const FORWARDED = new Set([
    'save-config', 'choose-overlay-file', 'import-profile', 'export-profile',
    'import-tosu-layout', 'authorize-profile-import', 'open-settings',
    'vosu-settings-load', 'vosu-settings-save', 'vosu-profile-action', 'vosu-settings-link',
  ]);
  window.__VOSULAY_POST_MESSAGE__ = (payload) => {
    const message = typeof payload === 'string' ? JSON.parse(payload) : payload;
    const type = message && message.type;
    if (type === 'ready') {
      // The browser's own clipboard serves this page.
      deliver({ type: 'clipboard-support', available: false });
      loadState(true).catch(fail);
      request('counters').then(deliver, fail);
      if (toolFetch) deliver({ type: 'editing', editing: true });
    } else if (type === 'refresh-counters') {
      request('counters').then(deliver, fail);
    } else if (type === 'set-editing') {
      // The page left or entered edit mode itself (shortcut, Escape, Done).
      window.postMessage(message.editing ? 'editingStarted' : 'editingEnded', '*');
    } else if (FORWARDED.has(type)) {
      send(message);
    }
    // Other messages (heartbeats, flush acknowledgements) concern the in-game host only.
  };

  // Edit-mode API for overlay hosts that load this page in their own window,
  // in tosu's in-game overlay convention: the host runs
  // `window.postMessage('editingStarted' | 'editingEnded')` in the page, and the
  // page posts the same strings when it changes edit mode itself. Only posts
  // from this window count: counter frames must not switch the editor.
  window.addEventListener('message', (event) => {
    if (event.source !== window) return;
    if (event.data === 'editingStarted') deliver({ type: 'editing', editing: true });
    else if (event.data === 'editingEnded') deliver({ type: 'editing', editing: false });
  });

  // Follow edits made in game or in other pages while no save is in flight.
  setInterval(() => {
    if (!saving && revision !== null) loadState(false).catch(() => {});
  }, 2000);
}
