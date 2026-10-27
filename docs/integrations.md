# Integrations

## Backend catalog and broker

[src/api.rs](../src/api.rs) fetches `/api/counters/search/` with a 10-second deadline and 2 MiB body limit. It recognizes vosu, tosu 4.26, legacy, and empty-state markup rather than trusting a product name; missing/invalid resolutions fall back to 500×300. [Fixtures](../tests/fixtures/backend/) protect this contract. The backend defaults to `http://127.0.0.1:24050/`; `--vosu-url` accepts HTTP(S) without credentials, query, or fragment.

### Ruleset selection

`src/runtime/editor/rulesets.rs` owns one bounded
`/websocket/v2/subscribe` connection selecting only `["/settings/mode"]`.
It decodes v2 initial/update/removal frames, reconnects without credentials, and
passes availability/changes through the editor worker's queue. No browser
editor needs to be open. An unavailable field disables binding controls (tosu's
other APIs and existing manual profiles continue working). See
[configuration](configuration.md) for manual-override and duplicate-binding rules.

Unpinned view pages poll a read-only active-profile ID once per second and reload
only after it changes: `/__vosulay/view-state` standalone, or
`/tools/vosulay/api/v1/view/state` through vosu. Explicit `?profile=N` pages install
no follower. The state route exposes no manager revision, bridge or write authority.


### vosu overlay settings profiles

After a successful catalog fetch, [src/api.rs](../src/api.rs) reads vosu's `GET /api/counters/profiles` within the same deadline (64 KiB limit, no redirects) and adds `vosu: {active, profiles, writable, reason}` to the manager's `counters` envelope, both in game and on the manage page. `writable` says whether vosulay holds a broker token (below) that vosu has not refused; otherwise `reason` says why in plain words (vosu on another computer, no token file, a file for another listener, an invalid file, or a token vosu refused even after a reread). Any failure, or a registry that is not self-consistent (invalid names, no `default`, active profile not listed), yields `vosu: null`: tosu and older vosu keep the profile-less editor and the browser settings page.

The overlay settings modal is vosulay's own UI, built from vosu's HTTP API by [src/api/vosu.rs](../src/api/vosu.rs) in the host process; no vosu page is framed. The manager sends `vosu-settings-load` (`GET /api/counters/settings/<overlay>[?profile=<p>]` plus the registry), `vosu-settings-save` (`POST` of the changed `[{uniqueID, value}]`), `vosu-profile-action` (`create` with optional `copy_from`, `rename`, `delete`, `activate`: `POST /api/counters/profiles`) and `vosu-settings-link`, each with a `request_id`; replies are `vosu-settings`, `vosu-settings-saved`, `vosu-profiles` or `vosu-settings-error`. Overlay and profile names are validated before any request. Writes carry `Authorization: Bearer <broker token>`; a 401 rereads the token once, and vosu's HTTP 200 `{"error": ...}` replies become errors. A `button` setting opens only the http(s) link in vosu's schema (or its saved value), never a URL from the page. In game these requests run on their own worker thread beside the manager task thread; on the manage page they are answered on the page's request thread. Neither path makes saves of vosulay's configuration wait for vosu.

Without a usable token (`writable: false`) the modal still opens: values load through the open `GET`s and the editor shows `reason` with a Retry instead of Save. A write refused for lack of a token replies `vosu-settings-error` with `read_only: <reason>`, which switches an open modal to the same state. Retry sends `refresh-counters`; each catalog refresh reads `broker.json` again when there is no usable token, and a token vosu refused stays off until the file holds another. Only tosu and vosu releases without settings profiles (`vosu: null`) still open the settings page in the browser, as does the modal's **Open in vosu dashboard**. vosu serves that page on its dashboard origin, which it reports at `GET /api/dashboard` on the overlay origin (default `http://127.0.0.1:24051`); `api::settings_page` resolves it at click time (two-second bound) and falls back to `<vosu_url>/settings`. The broker never serves dashboard pages: requests for them on the overlay origin get vosu's 302 to the dashboard origin, whose `Location` the proxy passes through unchanged because only the backend's own origin (and its loopback alias on the same port) is rerouted.

Pinned placements load as `<counter>/?vosuProfile=<pin>` in game, on the view-only page and on the manage page; vosu then injects the qualified `COUNTER_PATH`, which overrides the path-derived value backend.js supplies. Every registry the editor receives (catalog refreshes and the replies above) updates the pin choices. When it removes or restores the profile of a pinned placement in the active vosulay profile, the editor reloads that counter: vosu resolves a pin only at page load and the counter's command socket keeps its `l` identity.

Through the broker, each instance keeps its own upstream `/websocket/commands` connection with its original `l` query, so vosu's per-identity routing applies unchanged: a pinned instance reads and is refreshed with its profile only, and activating a profile refreshes only unpinned instances.

### vosu broker token

The broker's upstream sockets carry no browser `Origin`, and vosulay never forges one, so on its own vosu treats a counter's `saveSettings` over the broker like a file-loaded overlay and refuses it. vosu instead writes a per-start token to `broker.json` (Linux `$XDG_RUNTIME_DIR/vosu/`, else `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/`; Windows `%LOCALAPPDATA%\vosu\`). [src/backend/credential.rs](../src/backend/credential.rs) reads it only when `vosu_url` is loopback (`127.0.0.0/8`, `::1`, `localhost`) and the file's `origin` matches its scheme, port and host (`localhost` and `127.0.0.1` count as one; a vosu bound to every interface publishes `0.0.0.0` or `[::]`, which matches any loopback `vosu_url` on the same scheme and port, since such a listener serves loopback too), and requires a 64-character lowercase hex token. The broker then sends `Authorization: Bearer <token>` on upstream `/websocket/commands` upgrades, both relayed ones and the command socket a telemetry connection opens for its command frames, and on no other socket: telemetry sockets and `/websocket/v2/subscribe` relays never carry it. Its only HTTP use is the host's own settings and profile writes described above, which no page can trigger with a URL of its choosing. The token is cached; a 401 rereads the file once and retries, and a second 401 (or no file) connects without it, which keeps vosu's read-only behaviour.

Only downstream sockets that `broker_upgrade_target` in [src/remote.rs](../src/remote.rs) accepts use the token. Each run, the remote server generates a 256-bit broker secret that reaches only the in-game browser: [src/runtime/host.rs](../src/runtime/host.rs) passes it to CEF's renderer extra info with the broker URL, and the renderer gives it to [assets/backend.js](../assets/backend.js) for frames at the broker's origin, which appends `__vosulay_broker=<secret>` to their WebSocket URLs that target the broker. It is never served over HTTP (`/__vosulay/backend.js` has none), so reaching the port is not enough. A socket qualifies when it presents exactly one matching secret (compared in constant time), its peer is loopback, `Origin` is present and exactly the listener's origin, and `Sec-Fetch-Site` is absent or `same-origin`, on top of the listener's usual Host checks. The parameter is removed before forwarding, so vosu never sees it and the counter's own query (`l`) is unchanged byte for byte. Counters in OBS, a browser or the manage page load through the remote listener without the secret, so their own saves stay refused; edit their settings in game or on vosu's settings page. Counter frames can read the secret from their own socket URLs; they are trusted code under vosu ADR 0013 and could already save through those sockets. The vosu token itself never reaches page JavaScript and is not logged.

Native CEF placements additionally use bound, per-source loopback sites to keep
their animation widgets in separate renderer processes. The same secret is
available only after the native prepare/renderer-acknowledgement handshake for
that exact owned origin; arbitrary `127.0.0.N` hosts are not trusted. The
WebSocket's `Origin` must still equal its listener's own origin. Backend paths,
precise subscriptions, settings-profile queries and upstream broker sharing are
unchanged. Same-source instances retain shared localStorage/IndexedDB within a
run; different source folders are isolated. The broker port was already
ephemeral, so storage origins still change across runs. Browser/remote viewers
and opaque capability previews keep their previous routing and storage behavior.


Counters keep public backend API paths. [assets/backend.js](../assets/backend.js) routes matching fetch, XMLHttpRequest, and WebSocket calls through vosulay. [src/backend.rs](../src/backend.rs) shares one upstream telemetry socket per active schema (`/ws`, `/tokens`, `/websocket/v2`, `/websocket/v2/precise`). Filters stay client-local; projections keep the backend's object key order (serde_json `preserve_order`), because counters that iterate keys render rows in that order. Matching HTTP JSON can reuse the newest snapshot, with backend HTTP fallback when unavailable. Slow consumers skip stale telemetry instead of queueing it.

`/websocket/v2/subscribe` is relayed per client to preserve backend precise cadence; it is not projected from the slower shared V2 stream. Settings and command channels remain separate with original query/identity because authorization and replies are client-specific. The loopback broker is independent of the remote listener, so a busy remote port does not break in-game counters.

tosu serves the shared schemas, commands, settings pushes, and file/JSON routes through the broker as it does directly. It has no `/websocket/v2/subscribe`; the relay's upstream handshake times out and vosu-only counters reconnect. On Windows, tosu's optional in-game overlay (`ENABLE_INGAME_OVERLAY`, `tosu-ingame-overlay.exe`) injects a second asdf-overlay into the game; users must disable it (see the [README](../README.md#windows-1)).

## Local overlay capabilities

The editor's HTML picker creates a fresh 128-bit capability and a `vosulay-overlay://` URL containing the canonical parent root. [src/shell_policy.rs](../src/shell_policy.rs) binds each capability to exactly one canonical root; rebinding conflicts are rejected. Every request is reauthorized, percent-decoded into normal path components, canonicalized, and confined beneath the root, including symlink resolution. Files must be readable regular files and fit the 32 MiB limit. Never replace these checks with a string-prefix path check.

Only the exact trusted manager main frame receives the native bridge. Counter frames cannot write configuration or invoke native dialogs. Local resources are secure contexts but not cross-origin isolated. SharedArrayBuffer is unavailable by default; the unrestricted compatibility opt-in affects all overlays after restart and should be used only with fully trusted content.

## Remote profile pages

[src/remote.rs](../src/remote.rs) defaults to `127.0.0.1:5050`. Settings → Remote overlay server controls the listener address/port and allowed clients; listener changes require restart. LAN clients use the host's actual interface IP, not the wildcard bind address.

- `GET /overlay` renders the active profile; `?profile=N` selects by canonical positive 1-based position.
- `/local/<capability>/<path>` resolves saved authorized local roots without exposing the filesystem root.
- `/__vosulay/backend.js` serves the browser routing bootstrap.
- Other routes proxy the backend.

There is no access token or configurable remote hostname. Loopback is always allowed; other peers must match comma-separated exact IPs or four-part IPv4 wildcard patterns. Host authority must match the listener's accepted IP authority and port (localhost is supported for loopback); Origin and Sec-Fetch-Site checks prevent unrelated cross-origin access. Local resources/frame navigations also support opaque-origin embedding. Canonical-path confinement applies to remote local files too.

Only GET/HEAD are accepted, except the local-editing POST routes below. Requests are capped at 16 KiB of headers, 32 concurrent connections, and three-second I/O timeouts. A remote bind failure is logged without stopping the local broker. Allowed clients can view every profile and use the proxied backend: restrict listener and allowlist to trusted networks. The backend must allow the vosulay host, not each viewing device. Unrelated custom HTTP(S) origins load directly.

### Manage page (local editing)

With `allow_local_profile_editing` on (default off), loopback peers that name the Host `manage.localhost:<listener port>` get the full manager at `/overlay`, with [assets/manage-bridge.js](../assets/manage-bridge.js) as its HTTP host bridge ([src/remote/manage.rs](../src/remote/manage.rs)). `request_authority` accepts `manage.localhost`, like `localhost`, only when the listener address is loopback; on that host every route is denied unless editing is enabled for the peer, and only `/overlay` and `/__vosulay/manage/state|counters` (GET) and `/__vosulay/manage/message` (POST) exist: no backend proxy, local files or WebSockets. The plain listener address always serves the static view-only page. The bridge routes the counters it frames (via [assets/backend.js](../assets/backend.js) with its manager flag) and local HTML files (`/local/...`) to the listener's own `IP:port`, a different origin, so counter scripts cannot reach the manage routes, which require `Sec-Fetch-Site: same-origin` (POSTs also the manage Origin and `application/json`, up to 1 MiB). The edit and interaction shortcuts, native in game, are page keydown handlers here (`data-vosulay-host="page"`); counter frames ignore pointer input outside edit mode ([assets/manager.css](../assets/manager.css)) so a click cannot move keyboard focus into a frame, where the page never sees the edit shortcut. While interacting, a focused frame keeps the keys until a click on the editor chrome or empty stage.

`message` forwards an allowlist of manager messages (`save-config`, native file dialogs, profile import/export, tosu import, counter settings) to the editor worker ([src/runtime/editor.rs](../src/runtime/editor.rs)) and returns the envelopes they produce; clipboard and in-game host messages stay with the in-game manager. Revisions are shared with the in-game editor, which receives each change the page makes (including its settings: hotkeys, FPS limit). Page saves and profile-import authorizations may only carry `vosulay-overlay:` URLs that are already configured or were chosen in a native dialog during this run, so a page script cannot grant itself a filesystem root. Dialog requests wait for the user; others time out after five seconds. The bridge polls `state` every two seconds to follow edits made elsewhere, and reloads frames of a newly chosen HTML file once its first save makes `/local/` serve it.

External renderers (idk-overlay, tosu-overlay-qt style windows) use the manage page as their frontend: they load it top-level, sized to the game; outside edit mode it renders only counters on a transparent background. They toggle edit mode with tosu's in-game convention: the bridge maps `window.postMessage('editingStarted' | 'editingEnded')` to the manager's `editing` envelope and posts the same strings when the manager reports `set-editing` (shortcut, Escape, **Done**), which hosts use to capture input or restore click-through. It accepts only messages whose `source` is the page's own window, so counter frames cannot switch the editor. The page cannot be framed (`frame-ancestors 'none'`, `X-Frame-Options: DENY`, and a 403 for `/overlay` with a non-`document` `Sec-Fetch-Dest`), because a framing counter or website could otherwise trick clicks into same-origin edits.

### vosu dashboard tool sessions

The optional `tool.toml` UI loads vosu's browser bridge. Dashboard Open exchanges
its one-use launch code for an in-memory session; the service token never enters
the browser. `/v1/manage` is write-only in the manifest, including document,
state and catalog GETs. The session therefore replaces standalone
`manage.localhost` plus the local-editing opt-in as the authorization gate.
The same editor worker still checks message allowlists, revisions, local-file
capabilities and native-dialog selections. Tool mode disables the standalone
manage origin even if an imported config enabled it.

The HTTP bridge captures the session-aware fetch closure and removes its global.
Tool-manager counter frames are sandboxed with scripts but without same-origin
authority, including custom URLs; they cannot reach the manager or session.
Counter data comes from the independent loopback broker. This opaque-origin
sandbox can limit counters requiring browser storage or privileged browser APIs.
Standalone browser/CEF frames retain their existing behavior.

Tool previews use a separate per-run capability authority on the loopback broker:
two random 32-hex DNS labels before `.preview.localhost`, with the broker's exact
port. The 256-bit value is independent of the native broker secret. Only an exact
authority match from a loopback peer admits opaque-origin counter resources and
public data sockets. Those responses allow `Origin: null` without credentials and
cross-origin resource embedding. Plain/guessed authorities do not gain that access.
`/overlay`, `__vosulay/*` (including backend.js/view-state/manage), and tool/config
routes remain closed on the capability authority. Preview HTML carries its public
backend bootstrap inline; the reserved backend.js route stays closed.

Relative and root-absolute asset paths retain the capability hostname without
rewriting counter CSS. Existing local-file URLs also need their independently
unguessable configured root capability; the preview hostname does not enumerate
or authorize other roots. No manager or credentialed commands are exposed.
Standalone and native CEF normal-origin routing retain their existing policies.
Restrictive counter CSP may prevent the injected inline bootstrap; it is not
weakened. Opaque-origin browser-storage/API restrictions still apply.

Only authenticated manager document/state routes disclose the preview origin,
never the public view/view-state routes. It stays in memory, changes on restart,
and is not persisted or logged. The reconnect path picks up the replacement origin.
Preview frames and responses use no-referrer. Because `.localhost` resolves on the
viewer's computer, a tool page opened through a non-loopback vosu URL shows a
local-only preview explanation without loading a broker or falling back to a
broader origin policy. Use vosu through localhost/127.0.0.1 on that computer.

The authorized manager state includes a public process-generation ID, backend URL,
and the private preview URL. The HTTP bridge polls that identity independently of config
revision numbers (which can repeat after restart), updates its routing closure,
refreshes the catalog and reloads affected counter/local-file frames when the
existing editor authority still works. Standalone manage pages use the same path.
Temporary outages pause editing and hide stale frames until that authority can
be checked again. A 401/403 instead freezes the editor read-only with an explicit
**Open from vosu** action (standalone: **Reload editor**); no session is renewed
or silently replaced. The tool bootstrap offers the same dashboard action if its
initial document request fails. The generation ID is not the broker secret and
confers no authority.

The UI refuses framed/opener-bearing launches. Direct/reloaded tool pages fetch
only `/v1/view`, an open read route rendering the active or selected profile.

### Native lifecycle hand-off

Standalone startup reads the existing bounded `broker.json` credential, accepts
only its validated literal-loopback HTTP origin, and sends authenticated
`GET /tools/_host/v1/cli`. An enabled `vosulay` UI delegates through
`POST /tools/_host/v1/cli/vosulay/open`, exactly the `vosux --open` path.
Discovery has a 750 ms deadline; launch has a three-second deadline, no proxy,
and no redirects. No broker, stale authorization, unreachable vosu, or a disabled
tool leaves standalone behavior unchanged. Once an enabled tool is confirmed,
launch failure is reported rather than starting a competing host. Launch-code
fragments are never printed. `--server-only` delegates without opening a browser.
The tool tray resolves vosu's actual dashboard via existing `GET /api/dashboard`
and opens its Tools page; it does not infer dashboard ports.

The existing local control channel adds `yield-to-vosu`: only standalone accepts
it, follows the same flush/close path as standalone Exit, and releases ownership.
Tool instances reject both this request and legacy `quit`; vosu stops them by
closing stdin. Takeover waits at most five seconds after the bounded control
reply, never sends signals or kills a peer, and never falls back to blindly
quitting an unidentified/older peer. An older standalone without this command
must be exited once before starting the tool. If takeover fails, stderr explains
why and the tool exits 0: vosu shows `stopped`, detail `exited`, with no restart.



## OBS capture boundary

Linux launch order is **lookup helper → foreign preloads → compositor**, and OBS's Vulkan layer stays ahead of vosulay's. When OBS's GL capture library is loaded, vosulay's GLX/EGL swap hooks call OBS's swap wrapper first, so OBS captures the clean frame and chains back for composition regardless of preload order; this covers games such as Wine that resolve swaps through `dlsym`. Display Capture still includes the overlay. Vosulay does not modify OBS scenes, capture sources, or export flags.

On Windows, asdf-overlay draws before forwarding DXGI Present/Present1; exclusion depends on hook order. OBS's Capture third-party overlays option is not a guaranteed exclusion switch. Do not claim display/input latency from CPU callback timings or present timings, or capture correctness from an export that failed.
