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

## Local IPC and attachment

The shell creates a per-user Unix local socket in the runtime directory, with a temporary-directory fallback. New invocations can send newline-delimited JSON commands to toggle editing, toggle overlay visibility, quit, or set geometry. In particular, `--edit`/`-e` sends `toggle-edit`, so it toggles editing in the already-running instance. Payloads are capped at 4096 bytes and connections/read operations have short timeouts. Geometry is accepted only when the running instance was started with `--attach`.

[`extra/attach.sh`](../../extra/attach.sh) validates four non-negative numeric arguments and delegates to `vosulay --ipc-geometry`. The [operations page](../operations/build-and-run.md) lists the complete CLI surface and the single-instance failure behavior.

## Testing and change guidance

`src/api.rs` tests cover escaped metadata, ordering, fallback dimensions, URL construction, HTTP status/content limits, and deadlines. [`src/ffi.rs`](../../src/ffi.rs) tests cover settings encoding and base URL rejection. The C++ overlay handler and IPC server have no separate test suite in the repository; changes there need careful manual testing and should preserve the bounds and authorization checks.
