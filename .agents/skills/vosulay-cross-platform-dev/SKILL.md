---
name: vosulay-cross-platform-dev
description: "Build, test, and verify the vosulay overlay manager (Rust+Qt Linux app and Electron Windows host in one repo) including the Dawn-Qt env and cross-language parity checks"
---

# vosulay cross-platform dev workflow

Repo: `~/gh/vosu-ecosystem/vosulay` (submodule of the vosu-ecosystem umbrella). Linux app = Rust + Qt6 WebEngine (`src/`, `src/qt/`); Windows host = Electron TypeScript (`windows/`); shared frontend = `assets/manager.{html,js,css}` (loaded by BOTH shells — never fork it).

## Rust (Linux) build/test

`build.rs` hard-rejects stock Qt WebEngine (needs Dawn WebGPU). Use the Dawn-enabled prefix (find with `fd Qt6WebEngineCore.pc / -d 6` if moved; last seen `/tmp/vosulay-qt-webgpu`, buildable via `extra/build-webgpu-qt.sh <prefix>`):

```sh
P=/tmp/vosulay-qt-webgpu
export PKG_CONFIG_PATH="$P/lib/pkgconfig" CMAKE_PREFIX_PATH="$P" LD_LIBRARY_PATH="$P/lib"
export QTWEBENGINEPROCESS_PATH="$P/libexec/QtWebEngineProcess" \
       QTWEBENGINE_RESOURCES_PATH="$P/resources" \
       QTWEBENGINE_LOCALES_PATH="$P/translations/qtwebengine_locales"
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

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
