---
type: Domain Model
title: Configuration and overlay placement model
description: Documents vosulay's versioned JSON configuration, profile and placement concepts, normalization invariants, geometry limits, and atomic persistence behavior shared by Rust and the manager frontend.
resource: src/config.rs
tags: [domain, configuration, profiles, overlays, persistence]
---

# Configuration and overlay placement model

## Concepts

`AppConfig` contains a schema `version`, an `active_profile` ID, and a non-empty list of `Profile` values. Each profile has an ID, display name, and overlay `Placement` values. A placement identifies a counter or custom source and stores position (`top`, `left`), size, scale, opacity, rotation, and z-order. Optional `url` and `name` fields preserve custom overlay metadata.

The default state is version 1, one profile named `Default`, and no placements. The config file is located at the platform user configuration directory under `vosulay/config.json`; [`src/config.rs`](../../src/config.rs) uses the `directories` crate to resolve that location.

## Invariants and normalization

`AppConfig::normalize` repairs data at load/save boundaries:

- Version is reset to the supported `CONFIG_VERSION` (currently `1`); other versions are rejected during load.
- At least one profile exists, profile IDs are unique, and blank names receive generated names.
- Duplicate placement `counter_id` values within a profile are removed.
- The active profile falls back to the first profile if its ID is missing.
- Placement URLs are restricted to `http`, `https`, and `vosulay-overlay`.
- Non-finite geometry is replaced with defaults; width/height are at least `10`, scale is at least `0.1`, opacity is clamped to `0..1`, rotation to `-180..180`, and z-index to `0..999`.

[`assets/manager.js`](../../assets/manager.js) mirrors these rules in browser state so edits behave predictably before Rust persists them. Keep the two implementations aligned when adding fields or changing limits.

## Persistence safety

Loads treat a missing file as default configuration, but malformed JSON, unreadable files, and unsupported versions produce contextual errors. Saves serialize pretty JSON to a temporary file in the destination directory, flush and sync it, atomically replace the target, then sync the parent directory. This is deliberate crash-safety behavior, not incidental formatting.

The Qt bridge in [`src/ffi.rs`](../../src/ffi.rs) exposes load/save as JSON envelopes for the frontend. A load failure is returned alongside a default config so the UI can remain usable while surfacing the error; a save failure is propagated. Changes to the persistence format should update the [integration boundary](../integrations/vosu-and-overlays.md) only if they affect custom source URLs or runtime loading.

## Change guidance

Start with `src/config.rs` tests when changing schema or normalization, then inspect the corresponding `normalizeConfig` and `normalizeOverlay` functions in `assets/manager.js`. Preserve duplicate prevention and active-profile repair, and add round-trip/error tests for any new serialized field. See the [testing guidance](../testing/guidance.md) for the existing coverage map.
