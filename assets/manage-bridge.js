function (viewOrigin) {
  // Host bridge for the manage page (manage.localhost). The manager script runs
  // after this and talks to vosulay over HTTP instead of the in-game native bridge.
  document.documentElement.dataset.vosulayHost = 'page';
  const toolFetch = window.__VOSULAY_TOOL_FETCH__;
  delete window.__VOSULAY_TOOL_FETCH__;
  if (toolFetch) document.documentElement.dataset.vosulayTool = '1';
  const routeBackend = window.__VOSULAY_BACKEND_URL__;
  const updateBackend = window.__VOSULAY_UPDATE_ROUTING__;
  delete window.__VOSULAY_UPDATE_ROUTING__;
  let view = new URL(viewOrigin);
  let generation = null;
  let connection = 'ready';
  let notice = null;
  const locked = new Map();
  function connectionFailed(error) {
    if (connection === 'readonly') return;
    const next = error.status === 401 || error.status === 403 ? 'readonly' : 'reconnecting';
    if (connection === next && notice) return;
    connection = next;
    if (!notice) {
      notice = document.createElement('aside');
      notice.setAttribute('role', 'status');
      notice.style.cssText = 'position:fixed;bottom:12px;left:12px;z-index:10000;padding:12px;background:var(--panel,#222);color:var(--text,#fff);border:1px solid currentColor';
      document.body.append(notice);
    }
    notice.replaceChildren();
    const line = document.createElement('span');
    line.textContent = connection === 'readonly'
      ? 'Vosulay restarted or editing permission changed. This editor is read-only. '
      : 'Vosulay is unavailable. Reconnecting; editing is paused. ';
    const open = document.createElement('button');
    open.textContent = toolFetch ? 'Open from vosu' : 'Reload editor';
    open.onclick = async () => {
      if (!toolFetch) { location.reload(); return; }
      try {
        const response = await fetch('/api/dashboard', { cache: 'no-store' });
        if (!response.ok) throw new Error('dashboard unavailable');
        const url = new URL((await response.json()).origin);
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password)
          throw new Error('invalid dashboard');
        location.assign(new URL('/tools', url).href);
      } catch (_) {
        line.textContent = 'This editor is read-only. Open vosu’s Tools page and choose Open for vosulay. ';
      }
    };
    notice.append(line, open);
    for (const child of document.body.children) {
      if (child === notice) continue;
      if (!locked.has(child)) locked.set(child, child.inert);
      child.inert = true;
    }
    document.getElementById('stage').hidden = true;
    for (const frame of document.querySelectorAll('iframe.overlay-frame')) frame.hidden = true;
    open.focus();
  }
  function connected(routing) {
    if (connection === 'readonly') throw new Error('Editing authority expired; open again from vosu');
    const changed = routing && ((generation !== null && routing.generation !== generation) || routing.view !== view.href);
    if (routing) {
      if (typeof updateBackend !== 'function') throw new Error('Routing update unavailable');
      updateBackend(routing.backend, routing.view);
      view = new URL(routing.view);
      generation = routing.generation;
    }
    const recovering = connection !== 'ready';
    connection = 'ready';
    if (notice) { notice.remove(); notice = null; }
    for (const [child, inert] of locked) child.inert = inert;
    locked.clear();
    for (const frame of document.querySelectorAll('iframe.overlay-frame')) {
      if (changed || recovering) {
        frame.hidden = true;
        frame.addEventListener('load', () => {
          if (connection === 'ready') frame.hidden = false;
        }, { once: true });
        frame.src = window.__VOSULAY_BACKEND_URL__(frame.dataset.vosulaySourceUrl || frame.src);
      }
    }
    document.getElementById('stage').hidden = false;
    return changed || recovering;
  }
  window.addEventListener('keydown', event => {
    if (connection === 'ready') return;
    event.stopImmediatePropagation();
    if (!notice?.contains(event.target)) event.preventDefault();
  }, true);
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
    if (connection !== 'ready' && !['error', 'save-error'].includes(envelope.type)) return;
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
    try {
      const response = toolFetch
        ? await toolFetch(`v1/manage/${action}`, init)
        : await fetch(`/__vosulay/manage/${action}`, init);
      const text = await response.text();
      if (!response.ok) throw Object.assign(new Error(text.trim() || `vosulay returned ${response.status}`), { status: response.status });
      return text ? JSON.parse(text) : null;
    } catch (error) {
      connectionFailed(error);
      throw error;
    }
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
  let polling = false;
  async function loadState(force) {
    if (polling || connection === 'readonly') return;
    polling = true;
    try {
      const epoch = saveEpoch;
      const state = await request('state');
      const changed = connected(state.routing);
      if (changed) request('counters').then(deliver, fail);
      deliver({ type: 'ruleset', ruleset: state.ruleset ?? null });
      if (saving || epoch !== saveEpoch) return;
      if (!force && !changed && state.revision === revision) return;
      revision = state.revision;
      noteServed(state.config);
      deliver({ type: 'config', config: state.config, revision: state.revision });
    } finally {
      polling = false;
    }
  }

  async function send(message) {
    const save = message.type === 'save-config';
    if (save) {
      saving += 1;
      saveEpoch += 1;
    }
    try {
      const envelopes = await request('message', message);
      if (connection !== 'ready') throw new Error('Connection changed while saving; check the reconnected state');
      for (const envelope of envelopes) {
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
    if (connection !== 'ready') {
      if (type === 'save-config') deliver({
        type: 'save-error', save_id: message.save_id, code: 'connection',
        message: 'Editing is paused; reopen from vosu if this editor is read-only.',
      });
      return;
    }
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
    if (connection !== 'ready') return;
    if (event.data === 'editingStarted') deliver({ type: 'editing', editing: true });
    else if (event.data === 'editingEnded') deliver({ type: 'editing', editing: false });
  });

  // Follow edits made in game or in other pages while no save is in flight.
  setInterval(() => {
    if (!saving && connection !== 'readonly') loadState(false).catch(fail);
  }, 2000);
}
