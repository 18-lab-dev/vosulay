---
name: vosulay-cross-platform-dev
description: "Build, test, and verify the vosulay overlay manager (Rust+Qt Linux app and Electron Windows host in one repo) including the Dawn-Qt env and cross-language parity checks"
---

# vosulay cross-platform dev workflow

Repo: `~/gh/vosu-ecosystem/vosulay` (submodule of the vosu-ecosystem umbrella). Linux app = Rust + Qt6 WebEngine (`src/`, `src/qt/`); Windows host = Electron TypeScript (`windows/`); shared frontend = `assets/manager.{html,js,css}` (loaded by BOTH shells — never fork it).

## Rust (Linux) build/test

`build.rs` self-bootstraps: if pkg-config doesn't resolve a Dawn WebGPU-enabled Qt WebEngine, it builds one automatically into `$XDG_CACHE_HOME/vosulay/qt-webgpu` (`~/.cache/vosulay/qt-webgpu` by default) via `extra/build-webgpu-qt.sh`, then uses it for the rest of the build. Plain `cargo build`/`cargo test`/`cargo install --path .` work with **zero manual env vars** as long as `git`, `cmake`, `ninja`, `pkg-config`, `gperf`, and Python's `html5lib` are installed:

```sh
cargo test && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings
```

That auto-build is a full Qt WebEngine/Chromium source compile: ~30 minutes on a 32-thread workstation, **once per cache location** (`~/.cache/vosulay/qt-webgpu` does NOT get wiped on reboot — only `/tmp` does). If it's missing, the very first `cargo build`/`test` after checkout or a reboot triggers it automatically; run that first build via `hub` (`op:"start"`, `persist:true`) in the background and keep working on non-Rust parts (Windows/TS, shared `assets/manager.js` frontend, docs) while it compiles — do not block the whole task on it. `cargo:warning=` lines from build.rs announce when the auto-build starts/finishes; they show up live even without `-vv`.

Point `VOSULAY_WEBGPU_QT_PREFIX=<prefix>` at an already-built prefix (e.g. a leftover one under `/tmp`, or one built by hand with `extra/build-webgpu-qt.sh <prefix>`) to skip the auto-build and reuse it instantly — this also works for a prefix outside the default cache dir. Set `VOSULAY_SKIP_AUTO_WEBGPU_BUILD=1` to disable the auto-build entirely and get the old fail-fast behavior (useful to confirm a specific pre-provisioned Qt is actually being picked up, or in CI that wants to control the Dawn Qt build itself). Manually exporting `PKG_CONFIG_PATH`/`CMAKE_PREFIX_PATH`/`LD_LIBRARY_PATH`/`QTWEBENGINE*` still works exactly as before and takes priority over the auto-detected prefix.


## Windows host on a Linux workstation

`npm ci` fails EBADPLATFORM (win32-only natives). Use:

```sh
cd windows && npm ci --force --ignore-scripts --no-audit
npm run typecheck && npm test   # tsc + node:test via tsx; no Electron binary needed
```

Real installer builds happen in CI (`.github/workflows/windows-build.yml` on windows-latest; tag `v*` triggers `windows-release.yml` which publishes setup+portable exes). Bump `windows/package.json` version (`npm version X.Y.Z --no-git-tag-version`) before reshipping so artifact names change.

## Cross-language contract rules

- Rust is authoritative: `src/api.rs` (catalog parser), `src/config.rs` (config schema/normalization). TS mirrors live in `windows/src/main/{counters,config,types,urls}.ts`.
- Any change to those Rust files must update the TS mirror AND the golden fixtures `windows/test/fixtures/config-parity-*.json` (asserted by `tests/config_parity.rs` and `windows/test/config-parity.test.ts`). CI triggers on `src/api.rs`/`src/config.rs` for this reason.
- Catalog markup contract: `article.counter-card[data-overlay-id]` + `data-counter-name/-author/-version`; zero cards without a `.empty-state` element must throw (guards in both parsers).
- URL identity: TS `encodePathSegment` in `urls.ts` must keep matching the Rust url crate's path-segment encode set (leaves `$&+,:;=@` bare) so persisted counter URLs are byte-identical across platforms.

## End-to-end parser smoke (Linux)

Start the prebuilt daemon (`~/gh/vosu-ecosystem/vosu/target/release/vosu`, port 24050), fetch `/api/counters/search/?query=`, and run the TS parser over the captured HTML with `npx tsx` using an absolute import of `windows/src/main/counters`. Expect real installed counters, not `[]`.
