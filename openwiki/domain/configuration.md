---
type: Domain Model
title: Configuration and overlay placement model
description: Documents vosulay's versioned JSON configuration, profiles and placements, settings, normalization invariants, file location and portable mode, tosu import marker, and atomic persistence, as implemented in src/config.rs and mirrored by the manager page.
resource: src/config.rs
tags: [domain, configuration, profiles, overlays, persistence]
---

# Configuration and overlay placement model

## Concepts

[`src/config.rs`](../../src/config.rs) defines the model:

- **`AppConfig`** has a schema `version`, an `active_profile` ID, a non-empty list
  of `Profile`s, `settings`, and a `tosu_migration_completed` marker. The marker
  is written only when it is true.
- **`Profile`** has an `id`, a display `name`, and an ordered list of
  `Placement`s.
- **`Placement`** refers to an installed counter through `counter_id`, or to a
  custom source through `url` (plus an optional `name`). It stores `top`, `left`,
  `width`, `height`, `scale`, `opacity`, `rotation`, `enabled`, and `z_index`.
  - `counter_id` is not a unique placement identity. A profile may hold several
    placements of the same counter; the placement and its position in the list
    together identify a runtime instance.
  - A disabled placement keeps its geometry and order.
- **`AppSettings`** contains:

  | Field | Default | Meaning |
  |---|---|---|
  | `always_keep_overlays_in_view` | `false` | Constrains placement while editing. |
  | `fps_limit` | `0` | `0` means follow the display; otherwise a fixed cap up to `MAX_FPS_LIMIT` = 1,000,000. |
  | `allow_unrestricted_shared_array_buffer` | `false` | Opt-in that applies after a restart. |
  | `allowed_ips` | `127.0.0.1,localhost,absolute` | Remote-page peer allowlist. |
  | `remote_access_host` | empty | Host name shown in, and accepted for, the remote URL. |
  | `remote_listen_host` | `0.0.0.0` | Remote server bind address. |
  | `remote_listen_port` | `5050` | Remote server port. |
  | `hotkeys` | see below | Profile, copy, and paste shortcuts. |

  Default hotkeys are `Ctrl+0`–`Ctrl+9` for profiles 0–9, and `Ctrl+C`/`Ctrl+V`
  for copy and paste.

The default configuration is version `1` with one profile, `default`/`Default`,
and no placements.

Fields missing from older files receive defaults: opacity `1.0`, rotation `0.0`,
enabled `true`, and per-field settings defaults. Unknown fields are ignored on
load and dropped on the next save. This includes the retired remote credential
fields: the remote server has no access token.

## File location

- **Default:** `config_path()` resolves `<user config dir>/vosulay/config.json`
  through the `directories` crate.
- **Portable mode:** [`src/runtime.rs`](../../src/runtime.rs) switches to
  `data/config.json` beside the executable when a `portable` marker file or
  that config file exists there.
- **Override:** `--config PATH` replaces both.

## Invariants and normalization

`AppConfig::normalize` runs after every load and before every save made by the
editor worker:

- The version is set to `CONFIG_VERSION` (`1`); files with any other version are
  rejected on load.
- At least one profile exists. Empty or duplicate profile IDs are replaced with
  `profile-N`, and blank names become `Profile N`.
- An `active_profile` that matches no profile falls back to the first profile.
- Each placement is normalized independently and in order; nothing is
  deduplicated.
  - `url` is kept only for `http`, `https`, or `vosulay-overlay` schemes.
  - `name` is trimmed and dropped if empty.
  - Non-finite numbers become their defaults.
  - `width` and `height` are at least `10`, and `scale` at least `0.1`.
  - `opacity` is clamped to `0..1`, `rotation` to `-180..180`, and `z_index` to
    `0..999`.
- Settings:
  - `fps_limit` is capped at `MAX_FPS_LIMIT`, and `allowed_ips` is trimmed.
  - `remote_access_host` keeps only a valid IP or host name: brackets and a
    trailing dot are removed and host names are lowercased.
  - `remote_listen_host` must parse as an IP address; otherwise it becomes
    `0.0.0.0`. A `remote_listen_port` of `0` becomes `5050`.
  - Blank hotkeys return to their defaults.

The `normalizeConfig`, `normalizeOverlay`, and `normalizeHotkeys` functions in
[`assets/manager.js`](../../assets/manager.js) mirror these rules in page state.
Keep both implementations aligned when adding fields or changing limits.

## Persistence safety

- **Load:** a missing file produces the default configuration, which is written
  to disk immediately. Malformed JSON, unreadable files, and unsupported
  versions produce contextual errors.
- **Save:** the configuration is serialized as pretty JSON into a temporary file
  in the target directory. That file is synced and atomically replaces the
  target; on Unix the parent directory is then synced too.
- **Conflicts:** the [editor worker](../architecture/overview.md#editor-worker)
  rejects a save whose revision is stale. Before persisting, it re-authorizes
  every `vosulay-overlay` URL in the saved profiles.

## tosu layout import

[`src/migration.rs`](../../src/migration.rs) parses tosu 4.26.x
`__ingame__.values.json` and legacy layouts into profiles. It appends them with
ID-collision repair and sets `tosu_migration_completed`.

[`src/runtime/tosu.rs`](../../src/runtime/tosu.rs) imports automatically only
when the configuration is still pristine and exactly one source is found; it
never calls tosu's settings API. A manual import (Settings → Import tosu layout)
appends without replacing existing profiles. The README has the user-facing
details.

## Change guidance

Start with the tests in `src/config.rs` and with
[`tests/tosu_migration.rs`](../../tests/tosu_migration.rs). Preserve repeated
placements (`normalize_preserves_duplicate_counter_placements_in_order_and_normalizes_each`),
active-profile repair, and atomic persistence. Add round-trip and error tests
for every new serialized field.

See the [testing guidance](../testing/guidance.md) for commands and the coverage
map.
