function (configuredBackend, brokerBase, managerPage, brokerSecret) {
  if (typeof window.__VOSULAY_BACKEND_URL__ === 'function') return;
  const backend = new URL(configuredBackend);
  const broker = brokerBase ? new URL(brokerBase) : null;
  if (!['http:', 'https:'].includes(backend.protocol) || backend.username || backend.password ||
      (broker && (!['http:', 'https:'].includes(broker.protocol) || broker.username || broker.password))) {
    throw new TypeError('Invalid Vosulay backend routing URL');
  }
  // The manage page, like the in-game manager, routes the counters it frames.
  const manager = managerPage === true || (location.protocol === 'vosulay:' && location.host === 'manager');
  if (!manager && location.origin !== backend.origin && location.origin !== broker?.origin) return;
  // The editor embeds vosu's own settings page. vosu accepts its profile and
  // settings writes only from its exact origin, so they must not go via the broker.
  if (!manager && location.origin === backend.origin && location.pathname === '/settings') return;
  const loopback = host => host === 'localhost' || host === '127.0.0.1' || host === '[::1]' || host === '::1';
  const equivalent = candidate => {
    if (!broker || !['http:', 'https:', 'ws:', 'wss:'].includes(candidate.protocol)) return false;
    const backendPort = backend.port || (backend.protocol === 'https:' ? '443' : '80');
    const candidatePort = candidate.port || (candidate.protocol === 'https:' || candidate.protocol === 'wss:' ? '443' : '80');
    const exact = candidate.hostname.toLowerCase() === backend.hostname.toLowerCase();
    const alias = loopback(candidate.hostname.toLowerCase()) && loopback(backend.hostname.toLowerCase());
    return (exact || alias) && candidatePort === backendPort &&
      ((backend.protocol === 'http:' && (candidate.protocol === 'http:' || candidate.protocol === 'ws:')) ||
       (backend.protocol === 'https:' && (candidate.protocol === 'https:' || candidate.protocol === 'wss:')));
  };
  const route = value => {
    try {
      const original = value instanceof URL ? value : new URL(String(value), document.baseURI);
      if (!equivalent(original)) return value instanceof URL ? value.href : String(value);
      const routed = new URL(original.href);
      routed.protocol = original.protocol === 'ws:' || original.protocol === 'wss:'
        ? (broker.protocol === 'https:' ? 'wss:' : 'ws:') : broker.protocol;
      routed.host = broker.host;
      return routed.href;
    } catch (_) { return value; }
  };
  if (!manager && typeof window.COUNTER_PATH !== 'string') {
    try {
      const pathname = location.pathname.endsWith('/')
        ? location.pathname : location.pathname.slice(0, location.pathname.lastIndexOf('/') + 1);
      const counterPath = pathname.split('/').filter(Boolean).map(segment => decodeURIComponent(segment)).join('/');
      if (counterPath) window.COUNTER_PATH = counterPath;
    } catch (_) {}
  }
  Object.defineProperty(window, '__VOSULAY_BACKEND_URL__', { value: route, configurable: true });
  if (broker) {
    const nativeFetch = window.fetch;
    if (typeof nativeFetch === 'function') window.fetch = function(input, init) {
      if (input instanceof Request) {
        const target = route(input.url);
        if (target !== input.url) input = new Request(target, input);
      } else input = route(input);
      return Reflect.apply(nativeFetch, this, [input, init]);
    };
    const nativeOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function(method, url, ...args) {
      return Reflect.apply(nativeOpen, this, [method, route(url), ...args]);
    };
    // Only the in-game browser passes the broker's per-run secret (out of band, never over
    // HTTP). Counter frames present it on their sockets so the broker may use vosu's broker
    // token for them, which lets a counter save its own settings.
    const present = value => {
      const routed = route(value);
      if (typeof brokerSecret !== 'string' || !/^[0-9a-f]{64}$/.test(brokerSecret)) return routed;
      try {
        const url = new URL(routed, document.baseURI);
        if ((url.protocol !== 'ws:' && url.protocol !== 'wss:') || url.host !== broker.host || url.hash) return routed;
        const separator = url.search ? '&' : (url.href.endsWith('?') ? '' : '?');
        return `${url.href}${separator}__vosulay_broker=${brokerSecret}`;
      } catch (_) { return routed; }
    };
    const NativeWebSocket = window.WebSocket;
    if (typeof NativeWebSocket === 'function') {
      window.WebSocket = new Proxy(NativeWebSocket, {
        construct(target, args, newTarget) {
          if (args.length === 0) return Reflect.construct(target, args, newTarget);
          return Reflect.construct(target, [present(args[0]), ...args.slice(1)], newTarget);
        }
      });
    }
  }
}
