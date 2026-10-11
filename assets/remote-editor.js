function (profileId, localUrls) {
  // Host bridge for a remote profile page opened on vosulay's own machine. The
  // manager script runs after this and talks to vosulay through these calls
  // instead of the in-game native bridge.
  document.documentElement.dataset.vosulayScope = 'profile';
  const routeBackend = window.__VOSULAY_BACKEND_URL__;
  const local = new Map(Object.entries(localUrls || {}));
  // Local HTML overlays use vosulay-overlay: URLs; this page loads them from the server's /local/ route.
  Object.defineProperty(window, '__VOSULAY_BACKEND_URL__', {
    value: (url) => {
      const key = String(url);
      if (local.has(key)) return local.get(key);
      return typeof routeBackend === 'function' ? routeBackend(url) : key;
    },
    configurable: true,
  });

  const endpoint = (action) => `/__vosulay/edit/${action}?id=${encodeURIComponent(profileId)}`;
  const deliver = (envelope) => {
    if (typeof window.__VOSULAY_RECEIVE__ === 'function') window.__VOSULAY_RECEIVE__(envelope);
  };
  const fail = (error) => deliver({ type: 'error', message: error instanceof Error ? error.message : String(error) });

  async function call(action, body) {
    const init = { cache: 'no-store', credentials: 'same-origin' };
    if (body !== undefined) {
      init.method = 'POST';
      init.headers = { 'Content-Type': 'application/json' };
      init.body = JSON.stringify(body);
    }
    const response = await fetch(endpoint(action), init);
    const text = await response.text();
    if (!response.ok) throw new Error(text.trim() || `vosulay returned ${response.status}`);
    return text ? JSON.parse(text) : null;
  }

  let revision = null;
  let saving = 0;
  // Bumped when a save starts or ends. A state poll that overlapped a save may
  // carry configuration older than the save's reply; revision order cannot tell,
  // because revisions restart when vosulay restarts.
  let saveEpoch = 0;
  async function loadState(force) {
    const epoch = saveEpoch;
    const state = await call('state');
    for (const [key, value] of Object.entries(state.local_urls || {})) local.set(key, value);
    if (saving || epoch !== saveEpoch) return;
    if (!force && state.revision === revision) return;
    revision = state.revision;
    deliver({ type: 'config', config: state.config, revision: state.revision });
  }

  async function save(message) {
    const profile = message.config && Array.isArray(message.config.profiles)
      ? message.config.profiles.find((candidate) => candidate && candidate.id === profileId)
      : null;
    saving += 1;
    saveEpoch += 1;
    try {
      const reply = await call('save', { save_id: message.save_id, base_revision: message.base_revision, profile });
      if (reply && Number.isSafeInteger(reply.revision)) revision = reply.revision;
      deliver(reply);
    } catch (error) {
      deliver({ type: 'save-error', save_id: message.save_id, message: error instanceof Error ? error.message : String(error), code: 'save' });
    } finally {
      saving -= 1;
      saveEpoch += 1;
    }
  }

  window.__VOSULAY_POST_MESSAGE__ = (payload) => {
    const message = typeof payload === 'string' ? JSON.parse(payload) : payload;
    switch (message && message.type) {
      case 'ready':
        // Chromium's own clipboard serves this page.
        deliver({ type: 'clipboard-support', available: false });
        loadState(true).catch(fail);
        call('counters').then(deliver, fail);
        break;
      case 'refresh-counters':
        call('counters').then(deliver, fail);
        break;
      case 'save-config':
        save(message);
        break;
      case 'open-settings':
        call('open-settings', { counter_id: message.counter_id }).catch(fail);
        break;
      default:
        // Edit state is local to this page; native dialogs do not exist here.
        break;
    }
  };

  // Follow edits made in game or in other pages while no save is in flight.
  setInterval(() => {
    if (!saving && revision !== null) loadState(false).catch(() => {});
  }, 2000);
}
