# Configuration

[src/config.rs](../src/config.rs) is authoritative; [assets/manager.js](../assets/manager.js) mirrors the model in editor state. Keep their defaults and normalization aligned.

## Model and defaults

`AppConfig` has schema version `1`, an active profile ID, a non-empty profile list, settings, and a tosu migration-completed marker. The default profile is `default` / `Default`, with no placements. Profiles have an ID, name, and ordered placements. Repeated placements of one counter are valid and must not be deduplicated.

A placement identifies an installed `counter_id` or custom `url`, with optional name, geometry, scale, opacity, rotation, crop, enabled state, and z-order. Disabling keeps placement data. Supported custom schemes are `http`, `https`, and `vosulay-overlay`.

`width`/`height` size the counter's frame. `crop_top`, `crop_right`, `crop_bottom` and `crop_left` (default `0`) hide that many unscaled counter pixels from each side; `left`/`top` place the top-left corner of the remaining region, which is what scale and rotation apply to. Renderers ([assets/manager.js](../assets/manager.js), the static page in [src/remote.rs](../src/remote.rs)) clip a full-size frame offset by the left/top crop.

| Setting | Default | Meaning |
| --- | --- | --- |
| `always_keep_overlays_in_view` | `false` | Constrain the selected counter while editing. |
| `exit_edit_mode_on_focus_loss` | `true` | Leave editing when the game loses focus. |
| `fps_limit` | `0` | Follow display refresh; fixed caps range up to 1,000,000. |
| `allow_unrestricted_shared_array_buffer` | `false` | Trusted-overlay compatibility opt-in; restart required. |
| `allowed_ips` | `127.0.0.1,localhost,absolute` | Remote client allowlist; loopback is always allowed. |
| `allow_local_profile_editing` | `false` | Remote profile pages opened from this computer (loopback) can edit the profile they show. |
| `remote_listen_host` | `127.0.0.1` | Listener IP, not a public hostname. |
| `remote_listen_port` | `5050` | Listener port. |
| `vosu_url` | `http://127.0.0.1:24050/` | vosu or tosu-compatible backend origin (scheme, host and port; API paths are absolute, so no path prefix); restart required. `--vosu-url` overrides it for one run. Invalid values fall back to the default. |
| `hotkeys` | below | Settings → Shortcuts. |

Hotkey fields are snake_case; serialized chords use strings such as `Ctrl+Shift+Z`.

| Field | Default |
| --- | --- |
| `toggle_edit` | `Ctrl+Shift+Space` |
| `toggle_visibility` | empty (unbound) |
| `toggle_interaction` | `Ctrl+I` |
| `profile_0` … `profile_9` | `Ctrl+0` … `Ctrl+9` |
| `copy_overlay`, `paste_overlay` | `Ctrl+C`, `Ctrl+V` |
| `undo`, `redo` | `Ctrl+Z`, `Ctrl+Shift+Z` |
| `refresh_selected_overlay`, `refresh_all_overlays` | `Ctrl+R`, `Ctrl+Shift+R` |

Native edit, visibility, and interaction chords require Ctrl, Alt, or Meta plus a natively mappable key. Only visibility can be left empty. Page-level editing shortcuts do not replace text fields' native undo/redo.

To clear the hide/show binding, press unmodified Backspace or Delete while recording it; the UI shows **Not set**. On Windows visibility can toggle regardless of focus, while edit toggle only acts when the game is focused. If another application owns a Windows chord, registration warns without crashing startup; choose another chord.

## Storage and persistence

Default storage is the user configuration directory's `vosulay/config.json` (Windows: `%APPDATA%\vosulay\config.json`). A `portable` marker or existing `data/config.json` beside the executable selects that portable path. `--config PATH` overrides both.

Missing files receive defaults; malformed/unreadable files and unsupported schema versions produce errors. Missing fields receive defaults. Unknown fields, including obsolete remote credential/host fields, are ignored on load and omitted by subsequent saves. There is no remote access token.

Normalization repairs missing/duplicate profile IDs and invalid active-profile selection, preserves placement order and duplicates, trims optional names, and replaces non-finite geometry values. Width/height are at least 10, scale at least 0.1; opacity is clamped to 0–1, rotation to −180–180, and z-order to 0–999. Crops are at least 0 and leave at least 10 px of the frame visible on each axis (left/top take precedence over right/bottom). Invalid listener IPs return to loopback and port zero returns to 5050.

The editor worker revision-checks saves and reauthorizes local-overlay capabilities before persistence. Pretty JSON is written to a temporary file in the target directory, synced, and atomically replaces the target. Unix also syncs the parent directory.

## tosu migration

[src/migration.rs](../src/migration.rs) converts tosu 4.26.x and legacy layouts. [src/runtime/tosu.rs](../src/runtime/tosu.rs) discovers layouts and automatically imports only into an untouched/default configuration when exactly one source exists. It reads the source without modifying it or calling the settings API. A saved marker makes automatic import idempotent.

Settings → Import tosu layout appends profiles with collision repair rather than replacing user profiles. Pending edits must flush successfully first. See [migration fixtures and tests](../tests/tosu_migration.rs).
