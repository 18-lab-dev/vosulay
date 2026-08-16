---
type: Domain Model
title: Configuration and overlay placement model
description: Documents vosulay's versioned JSON configuration, profile and placement concepts, normalization invariants, geometry limits, and atomic persistence behavior shared by Rust and the manager frontend.
resource: src/config.rs
tags: [domain, configuration, profiles, overlays, persistence]
---

# Configuration and overlay placement model

## Concepts

`AppConfig` contains a schema `version`, an `active_profile` ID, a non-empty list of `Profile` values, and `AppSettings`. Each profile has an ID, display name, and an ordered list of overlay `Placement` values. A placement references an installed counter through `counter_id` or describes a custom source; `counter_id` is not a unique placement identifier. A profile may contain multiple placements for the same installed counter, and the placement object together with its position in the list constitutes that runtime instance's identity. Each placement stores position (`top`, `left`), size, scale, opacity, rotation, and z-order. Optional `url` and `name` fields preserve custom overlay metadata.

`AppSettings.allowed_ips` controls which non-loopback clients may use the [remote profile servers](../integrations/vosu-and-overlays.md); loopback is always accepted by the hosts, while entries support exact IPs and IPv4 wildcard patterns. `remote_access_token` is a persistent 256-bit credential generated when missing or invalid and embedded in every protected remote route; changing it revokes previously shared URLs. `remote_access_host` is the optional LAN IP or hostname shown in the ready-to-copy URL and accepted as the request `Host`; blank keeps authority restricted to the literal listener destination. `remote_listen_host` and `remote_listen_port` select the TCP bind address and port for the remote profile server itself, defaulting to `0.0.0.0:5050`; both are read once at process startup, so changes take effect after restarting the host. Settings also carry overlay visibility, SharedArrayBuffer compatibility, and configurable hotkeys. The Rust model in [`src/config.rs`](../../src/config.rs) and the Windows model in [`windows/src/main/config.ts`](../../windows/src/main/config.ts) intentionally normalize the same schema; [`tests/config_parity.rs`](../../tests/config_parity.rs) checks that contract against a shared fixture.

The default state is version 1, one profile named `Default`, and no placements. The config file is located at the platform user configuration directory under `vosulay/config.json`; [`src/config.rs`](../../src/config.rs) uses the `directories` crate to resolve that location. Windows uses `%APPDATA%\\vosulay\\config.json` for the installed build and a `data/config.json` beside the executable for the portable build.

## Invariants and normalization

`AppConfig::normalize` repairs data at load/save boundaries:

- Version is reset to the supported `CONFIG_VERSION` (currently `1`); other versions are rejected during load.
- At least one profile exists, profile IDs are unique, and blank names receive generated names.
- Placements are normalized independently in list order. Repeated `counter_id` values are preserved, allowing multiple ordered instances of the same installed counter.
- The active profile falls back to the first profile if its ID is missing.
- Placement URLs are restricted to `http`, `https`, and `vosulay-overlay`.
- Non-finite geometry is replaced with defaults; width/height are at least `10`, scale is at least `0.1`, opacity is clamped to `0..1`, rotation to `-180..180`, and z-index to `0..999`.
- `remote_access_token` is normalized to exactly 64 lowercase hexadecimal characters; missing, malformed, or legacy values are replaced with a cryptographically random credential and persisted during load. `remote_access_host` accepts a bare hostname or IP address, strips surrounding IPv6 brackets and a trailing DNS dot, lowercases hostnames, and clears invalid values. `remote_listen_host` must parse as an IPv4 or IPv6 address (brackets are stripped first); any other value falls back to `0.0.0.0`. `remote_listen_port` falls back to `5050` when zero.

[`assets/manager.js`](../../assets/manager.js) mirrors these rules in browser state so edits behave predictably before Rust persists them. Keep the two implementations aligned when adding fields or changing limits.

## Persistence safety

Loads treat a missing file as default configuration, but malformed JSON, unreadable files, and unsupported versions produce contextual errors. Saves serialize pretty JSON to a temporary file in the destination directory, flush and sync it, atomically replace the target, then sync the parent directory. This is deliberate crash-safety behavior, not incidental formatting.

The Qt bridge in [`src/ffi.rs`](../../src/ffi.rs) exposes load/save as JSON envelopes for the frontend. A load failure is returned alongside a default config so the UI can remain usable while surfacing the error; a save failure is propagated. Changes to the persistence format should update the [integration boundary](../integrations/vosu-and-overlays.md) only if they affect custom source URLs or runtime loading.

## Change guidance

Start with `src/config.rs` tests when changing schema or normalization, then inspect the corresponding `normalizeConfig` and `normalizeOverlay` functions in `assets/manager.js`. `AppConfig::normalize` iterates every placement without deduplicating, and `normalize_preserves_duplicate_counter_placements_in_order_and_normalizes_each` is the regression test for preserving repeated counters while repairing each placement independently. Preserve this behavior and active-profile repair, and add round-trip/error tests for any new serialized field. See the [testing guidance](../testing/guidance.md) for the existing coverage map.
