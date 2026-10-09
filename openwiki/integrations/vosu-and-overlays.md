---
type: Integration Boundary
title: vosu catalog, overlay sources, and remote pages
description: Describes vosulay's HTTP/HTML integration with a vosu or tosu-compatible backend, the in-browser backend broker, local HTML overlay capabilities, the view-only remote profile server, the Linux host-to-game control channel, and the security constraints at each boundary.
resource: src/remote.rs
tags: [integrations, vosu, tosu, http, security, overlays]
---

# vosu catalog, overlay sources, and remote pages

## Counter catalog

[`src/api.rs`](../../src/api.rs) requests `<base>/api/counters/search/`. It
applies a 10-second overall deadline and a 2 MiB body limit, and requires a
`text/html` or `application/xhtml+xml` response.

The parser:

- accepts vosu markup, tosu 4.26 markup, legacy tosu markup, and empty-state
  markup;
- rejects unrecognized markup and counters missing required attributes;
- preserves source order;
- falls back to `500×300` when a resolution is missing or invalid.

Fixtures live in [`tests/fixtures/backend`](../../tests/fixtures/backend).
Compatibility is detected from the counter API markup, not from the server's
product name.

The `--vosu-url` base must be HTTP(S) without credentials, query, or fragment
([`src/runtime.rs`](../../src/runtime.rs)). The editor opens the backend's
`/available` page in the system browser.

## Backend broker inside the browser

Counter pages keep their original public API paths. Every non-manager frame
receives [`assets/backend.js`](../../assets/backend.js), injected by
[`crates/browser-cef/src/bridge.rs`](../../crates/browser-cef/src/bridge.rs).
The script:

- rewrites `fetch`, XMLHttpRequest, and WebSocket URLs that target the backend
  so they reach the loopback broker in [`src/remote.rs`](../../src/remote.rs);
- sets `window.COUNTER_PATH` from the frame's directory path, unless the page
  already defined a string value.

[`src/backend.rs`](../../src/backend.rs) shares one upstream telemetry WebSocket
per schema (`/ws`, `/tokens`, `/websocket/v2`, `/websocket/v2/precise`).

- Filters and selected-subscription state stay client-local.
- Matching HTTP JSON requests reuse the newest snapshot.
- Slow consumers skip stale data instead of queueing it.
- Settings and other command channels are not shared.

The loopback broker is bound separately from the LAN listener. An occupied
remote port therefore never prevents in-game counters from loading.

## Custom and local HTML overlays

Placements accept `http`, `https`, and `vosulay-overlay` URLs (see the
[configuration model](../domain/configuration.md)). Local filesystem paths are
never handed to CEF directly.

**Selecting a local file.** The editor's Choose HTML overlay action
([`src/runtime/editor.rs`](../../src/runtime/editor.rs)):

1. opens an `rfd` file dialog filtered to `.html`/`.htm`;
2. requires the selected path to have one of those extensions;
3. generates a fresh 128-bit hexadecimal capability token with `getrandom`;
4. builds `vosulay-overlay://<token>/<base64url(canonical parent)>/<file>` with
   [`shell_policy::overlay_url`](../../src/shell_policy.rs). The file must
   canonicalize to a regular file.

**Authorization.** `OverlayAuthorization` binds each token to exactly one
canonical root directory. A token can never be rebound to another root, and a
batch containing a conflict is rejected as a whole. The editor re-authorizes
every saved or imported profile's local URLs.

**Each request** through the `vosulay-overlay` scheme
([`src/runtime/resources.rs`](../../src/runtime/resources.rs)) must pass these
checks:

- the URL has no credentials, port, query, or fragment;
- the root segment is canonical base64url for an existing canonical directory;
- every path segment is percent-decoded into a single normal component;
- the token is authorized for that root;
- the canonical target stays under the root, which also rejects symlink
  escapes;
- the target is a readable regular file;
- reads stay within the 32 MiB resource limit.

This boundary is security-sensitive. Never replace canonical-path checks with
string-prefix checks, and never serve paths that bypass `OverlayAuthorization`.

## Manager bridge isolation

Only the main frame whose URL is exactly `vosulay://manager/index.html` receives
the native bridge function. The manager script removes the function from
`window` immediately. Messages are capped at 256 KiB, and at most 64 may be
pending. Counter and overlay frames, including iframes inside the manager, can
neither write configuration nor open native dialogs. See the
[architecture overview](../architecture/overview.md#browser-manager-page-and-bridge).

## Remote profile pages

[`src/remote.rs`](../../src/remote.rs) serves view-only pages on
`settings.remote_listen_host:remote_listen_port` (default `0.0.0.0:5050`).

**Routes**

| Route | Behavior |
|---|---|
| `GET /overlay` | Renders the active profile. `?profile=N` selects by 1-based position; any other query is rejected. |
| `/local/<token>/<path>` | Forwards local overlay resources. The URL carries only the opaque token, never the root. The server maps the token through the saved `vosulay-overlay` placements, which must name exactly one canonical root, and confines the decoded path beneath that root. |
| `/__vosulay/backend.js` | The same routing script, pointed at this server. |
| Anything else | Proxied to the backend. |

**Authorization.** There is no URL credential. Every request must pass all of:

- **Peer:** the client is loopback, or listed in `allowed_ips`. Entries are
  comma-separated exact IPs or four-part IPv4 `*` patterns.
- **Host:** the `Host` header names the listener's own IP, `localhost` on a
  loopback listener, or the configured `remote_access_host`, on the listener
  port. This blocks DNS rebinding.
- **Origin:** optional. When supplied, it must be the `Host` authority's exact
  HTTP origin. `Origin: null` is also accepted for `/local/` routes and frame
  navigations; same-site alone is not sufficient.
- **Sec-Fetch-Site:** optional. When supplied, it may be `none`, `same-origin`,
  or `same-site`. `cross-site` is also accepted for `/local/` routes and frame
  navigations.

**Request handling**

- Only GET and HEAD are accepted; other methods return 405.
- Reads and writes time out after three seconds.
- Requests are capped at 16 KiB, with at most 32 concurrent connections.
- A LAN bind failure is logged and is not fatal.

**Rendered pages**

- Disabled placements are skipped. Profile names and titles are HTML-escaped.
- Local overlay frames use `sandbox="allow-scripts"`, which gives them an opaque
  origin. Their resources are sent with `Cross-Origin-Resource-Policy:
  cross-origin`, `Access-Control-Allow-Origin: null`, `no-store`, and `nosniff`.
- HTTP(S) custom overlays keep `allow-scripts allow-same-origin`.

Anyone on the allowlist can view every profile and use the proxied backend.
Keep the listener and allowlist restricted to trusted networks. Listener changes
take effect after a restart.

## Linux host-to-game control channel

The injected compositor connects to the host over a `SOCK_SEQPACKET` Unix
socket ([`crates/frame-transport/src/ipc.rs`](../../crates/frame-transport/src/ipc.rs)).
It uses `VOSULAY_ENDPOINT` if set, otherwise the abstract address
`@vosulay-<euid>`.

- Peers are authenticated with `SO_PEERCRED`, so only the same user can connect.
- [`protocol.rs`](../../crates/frame-transport/src/protocol.rs) defines a
  versioned packet format and rejects frames larger than 16384 px per side or
  256 MiB.
- Pixels travel only through the sealed memfd ring, never over the socket.
- Host and compositor must come from the same package build.

There is no user-facing IPC or geometry command; attachment follows game focus.

## Testing and change guidance

The relevant tests are:

- `src/api.rs`
- `src/backend/tests.rs`
- `src/remote.rs` and `src/remote/tests_broker.rs`
- `src/shell_policy.rs`
- the frame-transport crate

See the [testing guidance](../testing/guidance.md#coverage-map). Changes to any
boundary above should extend the matching tests. In particular, preserve the
authority, origin, and peer checks, the request bounds, HTML escaping,
capability confinement, and the local-frame CORP/CORS policy.
