---
type: Domain Model
title: Configuration and overlay placement model
description: Documents vosulay's canonical TypeScript configuration, profiles, placement normalization, hotkeys, remote settings, revisions, and atomic persistence.
resource: desktop/src/main/config.ts
tags: [domain, configuration, profiles, overlays, persistence]
---

# Configuration and overlay placement model

## Canonical model

[`desktop/src/main/config.ts`](../../desktop/src/main/config.ts) is the single runtime implementation of configuration on Linux and Windows. `AppConfig` contains schema `version`, `active_profile`, a non-empty `profiles` array, and `settings`. Each profile has an ID, display name, and ordered `overlays` array.

A placement references an installed counter through `counter_id` or carries custom `url` and `name` metadata. `counter_id` is not a unique placement identifier: one profile may contain multiple ordered instances of the same counter. Each placement stores top/left position, width/height, scale, opacity, rotation, and z-index.

Settings contain:

- `allowed_ips` for non-loopback remote-profile clients;
- persistent `remote_access_token` and optional `remote_access_host`;
- overlay viewport behavior and the unrestricted `SharedArrayBuffer` compatibility opt-in;
- configurable profile/copy/paste hotkeys.

The default is schema version `1`, profile `default` named `Default`, no placements, and default hotkeys. The persistent remote credential is generated from 32 cryptographically random bytes.

## Normalization invariants

`normalizeConfig` and `normalizeOverlay` repair values at load/save boundaries:

- The supported schema version remains `1`; unsupported files are not silently treated as a different schema.
- At least one profile exists, profile IDs are unique, blank names receive generated names, and a missing active profile falls back to the first profile.
- Placements remain in list order. Repeated `counter_id` values are preserved and normalized independently.
- Source URLs are limited to `http:`, `https:`, and `vosulay-overlay:`.
- Non-finite geometry becomes a default; width/height are at least `10`, scale is at least `0.1`, opacity is clamped to `0..1`, rotation to `-180..180`, and z-index to `0..999`.
- `remote_access_token` is exactly 64 lowercase hexadecimal characters. A missing, malformed, or legacy value is replaced and persisted.
- `remote_access_host` accepts a bare hostname or IP address, removes surrounding IPv6 brackets and a trailing DNS dot, lowercases hostnames, and clears invalid values.
- Hotkeys are normalized against the supported key set and reserved runtime shortcuts.

[`assets/manager.js`](../../assets/manager.js) performs corresponding client-side normalization so edits behave predictably before persistence. `desktop/test/config-parity.test.ts` protects the intended observable result against the retained Rust-derived golden fixture; Rust is no longer a second runtime configuration implementation.

## Persistence and revisions

[`ConfigStore`](../../desktop/src/main/config.ts) owns the in-memory normalized snapshot and monotonically increasing revision. Callers may save with an expected revision; a stale writer receives `ConfigConflictError` rather than overwriting newer state. Subscriptions carry the normalized config, revision, and optional source token so active manager instances and the remote server can reconcile external changes without feedback loops.

A missing file produces defaults. Malformed JSON, unreadable files, and unsupported versions produce contextual load errors while the manager can remain usable with a default configuration. Saves create the data directory, serialize pretty JSON to a temporary sibling, sync the file, atomically rename it over `config.json`, and clean up temporary state on failure.

[`desktop/src/main/storage-path.ts`](../../desktop/src/main/storage-path.ts) resolves storage:

- Installed Linux and Windows builds: the platform application-data directory plus `vosulay/config.json`.
- Windows portable build: `<executable directory>/data/config.json`, selected through `PORTABLE_EXECUTABLE_DIR`.

The Electron host sets its `userData` directory to the same data directory before constructing services.

## Consumers

[`desktop/src/main/host-bridge.ts`](../../desktop/src/main/host-bridge.ts) exposes revision-aware load/save and manager commands. [`desktop/src/main/remote-server.ts`](../../desktop/src/main/remote-server.ts) receives normalized snapshots through `ConfigStore.subscribe`; replacing the credential immediately revokes previously shared remote URLs. Platform overlay providers consume the same store and do not persist separate native state.

## Change guidance

Start with `desktop/src/main/config.ts`, shared types, and the matching tests. Preserve duplicate placement order, active-profile repair, revision conflicts, atomic replacement, remote credential generation, and hotkey reservations. Any new serialized field needs missing/invalid/round-trip tests and matching manager handling. Update [the integration boundary](../integrations/vosu-and-overlays.md) when a change affects accepted source URLs, local capabilities, remote authorization, or browser security behavior.
