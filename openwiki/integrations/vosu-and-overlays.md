---
type: Integration Boundary
title: vosu catalog, overlay sources, and local IPC
description: Describes vosulay's HTTP/HTML integration with a vosu instance, safe URL construction, local HTML overlay serving, single-instance IPC controls, and the security constraints at each boundary.
resource: src/api.rs
tags: [integrations, vosu, http, ipc, security, overlays]
---

# vosu catalog, overlay sources, and local IPC

## vosu counter catalog

[`src/api.rs`](../../src/api.rs) requests `<base>/api/counters/search/` with a 10-second deadline and a 2 MiB response limit. It requires an HTML/XHTML response and parses result cards matching `.result-item[data-overlay-id]`. Metadata comes from `.calu[n][a][v]`; an optional `.settings-button` marks counters with settings. Width and height metadata are read from `[nft="width"]` and `[nft="height"]` using numeric `nfv` values, with a `500x300` fallback.

The parser preserves source order and returns `InstalledCounter` records containing ID, name, author, version, dimensions, settings availability, and a counter URL. The frontend uses these records to populate the catalog and place known counters. The settings and available-counter links are constructed by Rust in [`src/ffi.rs`](../../src/ffi.rs): `/settings?overlay=<id>` and `/available`.

Base URLs must be absolute HTTP(S) URLs with a host, no credentials, and no query or fragment. The helper normalizes a trailing slash before joining paths. Counter IDs are query-encoded for settings URLs and path-segment encoded for counter URLs.

Some counter bundles expect `window.COUNTER_PATH` before their own scripts execute. [`counterMetadataScript`](../../src/qt/qt_shell.cpp) is installed in WebEngine's MainWorld at DocumentCreation with subframe execution enabled. It returns without changing anything unless the frame's origin exactly equals the configured vosu origin, and it never overwrites an existing string-valued `window.COUNTER_PATH`. Otherwise it takes the frame pathname's containing folder (or the pathname itself when it ends in `/`), splits out empty components, decodes every segment, joins them with `/`, and assigns the non-empty result. Invalid URLs or path decoding failures leave the global unset.

## Custom and local HTML overlays

The manager accepts HTTP(S) and capability-bearing `vosulay-overlay` sources. Local filesystem paths are never sent directly to WebEngine.

Local HTML selection goes through the XDG Desktop Portal `OpenFile` request, restricted to one HTML/HTM selection. Before opening the chooser, the Qt shell hides the overlay view and releases input capture; on cancellation, failure, or completion it restores the previous visibility and reapplies capture according to the current editing state. The returned URI must be a valid local `file://` URL. Its target must be a regular readable file with a case-insensitive `.html` or `.htm` suffix and must open successfully for read-only access.

For an accepted file, the shell canonicalizes both the file and its containing directory, verifies that the file is inside that directory, and creates a `vosulay-overlay` URL with a fresh 128-bit hexadecimal host token. The scheme handler authorizes that token for exactly the canonical root encoded in the URL; a token cannot later be rebound to another root. Each request must carry a valid authorized token and the identical canonical root. The requested path is canonicalized again, rejected if it escapes the root (including through traversal or symlinks), and served only when it is a readable regular file that opens read-only. The handler replies with the MIME type detected for that file.

This boundary is security-sensitive: do not replace canonical path checks with string prefix checks, broaden the allowed schemes without reviewing WebEngine settings, or expose arbitrary local filesystem paths. The frontend’s accepted schemes and the C++ handler’s authorization rules must remain consistent with the [configuration model](../domain/configuration.md).

The Qt manager's native `QWebChannel` is attached only to WebEngine's `ApplicationWorld`. The bridge bootstrap also runs only in that isolated world and only in the main frame; `assets/manager.js` relays messages through a same-document custom DOM event. Overlay frames run in `MainWorld`, receive no `qt.webChannelTransport` or native object, and cannot invoke configuration writes or native dialogs through the manager bridge. Preserve both the isolated script world and `runsOnSubFrames=false`.

## Remote profile pages

Linux [`src/remote.rs`](../../src/remote.rs) and Windows [`windows/src/main/remote-server.ts`](../../windows/src/main/remote-server.ts) expose a view-only HTTP service that binds `AppSettings.remote_listen_host:remote_listen_port`, defaulting to `0.0.0.0:5050` when unset or invalid; the bind address is read once at process startup, so listener changes take effect after restarting the host. A generated 256-bit credential prefixes every protected route: `/access/<credential>/overlay` renders the active profile, with the single `profile=<positive 1-based index>` query selecting another profile. `/access/<credential>/local/<capability>/...` forwards local custom-overlay resources. The remote form intentionally uses the capability as an opaque server-side registry key and never discloses the encoded absolute root carried by the internal `vosulay-overlay` URL. HTTP(S) custom overlays load directly in the client, while counter URLs are built against the validated request authority when the configured vosu URL is loopback so remote clients can reach the same machine.

The credential is generated on first config load and persists in `AppSettings.remote_access_token`; the settings dialog asks for the LAN IP or hostname that clients use, persists it as `remote_access_host`, and displays a ready-to-copy URL. Protected requests must use either the literal listener destination or this exact configured host in `Host`, preventing any other attacker-controlled DNS authority from being accepted after rebinding to loopback. Profile-document requests reject cross-origin `Origin` and `Sec-Fetch-Site: cross-site`. Local-resource routes additionally accept the `Origin: null` and cross-site metadata emitted by an opaque sandbox, but only after the same credential, peer allowlist, and local capability checks succeed. Requests also check the peer against `AppSettings.allowed_ips`; loopback is always allowed, and configured entries may be exact IPs or four-part IPv4 wildcard patterns. The shared frontend settings dialog exposes the credential directly as a read-only field with a Regenerate control that replaces it with a fresh cryptographically random value; the new value is only persisted once the dialog is saved.

The Linux implementation uses three-second read/write timeouts, caps requests at 16 KiB and active connections at 32, and shuts down only after request workers drain. Windows keeps a normalized configuration snapshot in memory, rejects overload before route or filesystem work, caps active handlers at 32 and header bytes at 16 KiB, applies three-second request deadlines, and limits connection admission. Remote profile pages are view-only: unprotected routes return 404, bad credentials or browser metadata return 403, unsupported routes return 404, and non-GET/HEAD methods return 405.

Forwarded local overlay frames are sandboxed with `allow-scripts` only. Omitting `allow-same-origin` gives each local frame an opaque origin even though its resource URL shares the profile page's HTTP origin, preventing its scripts from reading the parent DOM or other capability URLs. Local files therefore use `Cross-Origin-Resource-Policy: cross-origin` so classic scripts, styles, and images load, plus the narrowly scoped `Access-Control-Allow-Origin: null` needed for relative `fetch` and module scripts from that opaque origin. HTTP(S) custom overlays retain `allow-scripts allow-same-origin` because they are genuinely cross-origin from the profile page.

This is a network-facing boundary, so preserve the credential, authority and route-specific browser-metadata checks, request bounds, HTML escaping, no-store responses, local capability confinement, opaque remote local-resource paths, and the local-frame CORP/CORS policy when changing either implementation. The server is started by the platform entrypoint and shares the [configuration model](../domain/configuration.md).

## Local IPC and attachment

The shell creates a per-user Unix local socket in the runtime directory, with a temporary-directory fallback. New invocations can send newline-delimited JSON commands to toggle editing, toggle overlay visibility, quit, or set geometry. In particular, `--edit`/`-e` sends `toggle-edit`, so it toggles editing in the already-running instance. Payloads are capped at 4096 bytes and connections/read operations have short timeouts. Geometry is accepted only when the running instance was started with `--attach`.

[`extra/attach.sh`](../../extra/attach.sh) validates four non-negative numeric arguments and delegates to `vosulay --ipc-geometry`. The [operations page](../operations/build-and-run.md) lists the complete CLI surface and the single-instance failure behavior.

## Testing and change guidance

`src/api.rs` tests cover escaped metadata, ordering, fallback dimensions, URL construction, HTTP status/content limits, and deadlines. [`src/ffi.rs`](../../src/ffi.rs) tests cover settings encoding and base URL rejection. The C++ overlay handler and IPC server have no separate test suite in the repository; changes there need careful manual testing and should preserve the bounds and authorization checks.
