---
name: vosulay-local-ci
description: "Run vosulay's native, Windows, packaging, and GitHub Actions pre-push verification locally on Linux"
---

# vosulay local CI on Linux

Use this procedure before pushing `vosulay` changes that affect native or Windows code.

## Preconditions

- Work from the intended clean branch/worktree.
- Do not push as part of verification.
- The Dawn-enabled Qt build is available at `/tmp/vosulay-qt-webgpu`.
- `act` and Podman are installed for workflow/package checks.

## Native matrix

Export the matching Qt paths for every Cargo command:

```sh
export PKG_CONFIG_PATH=/tmp/vosulay-qt-webgpu/lib/pkgconfig
export CMAKE_PREFIX_PATH=/tmp/vosulay-qt-webgpu
export LD_LIBRARY_PATH=/tmp/vosulay-qt-webgpu/lib
export QTWEBENGINEPROCESS_PATH=/tmp/vosulay-qt-webgpu/lib/qt6/QtWebEngineProcess
export QTWEBENGINE_RESOURCES_PATH=/tmp/vosulay-qt-webgpu/share/qt6/resources
export QTWEBENGINE_LOCALES_PATH=/tmp/vosulay-qt-webgpu/share/qt6/translations/qtwebengine_locales
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Stock Qt WebEngine is expected to fail the repository's Dawn requirement; do not weaken that check.

## Windows matrix on Linux

`windows/package.json` declares `"os": ["win32"]`, so exact `npm ci` fails with `EBADPLATFORM` on Linux. This is expected; Windows GitHub Actions runs the exact command on Windows. For local source verification:

```sh
cd windows
npm ci --force
npm run typecheck
npm test
```

## Windows distributions

If host Wine is unavailable, package with the Wine builder container from the repository root:

```sh
podman run --rm \
  -v "$PWD:/project" \
  -w /project/windows \
  docker.io/electronuserland/builder:wine \
  npm run dist:win
```

Success must include both NSIS and portable artifacts plus `verify:package` success. Confirm:

```sh
sha256sum windows/dist/vosulay-*-setup.exe windows/dist/vosulay-*-portable.exe
```

Podman rootless volume writes remain owned by the invoking user. Generated `out/`, `dist/`, and `node_modules/` are ignored, but verify Git cleanliness afterward.

## GitHub Actions workflow checks

List all workflows:

```sh
act -l
```

`act` cannot execute a native `windows-latest` job. Use Linux mappings only for parsing/dry-run validation:

```sh
act -n workflow_dispatch -W .github/workflows/windows-build.yml -P windows-latest=catthehacker/ubuntu:act-latest
act -n push -W .github/workflows/windows-release.yml -P windows-latest=catthehacker/ubuntu:act-latest
```

The no-secret OpenWiki path is safe to exercise; guarded generation and PR steps must remain skipped:

```sh
act workflow_dispatch \
  -W .github/workflows/openwiki-update.yml \
  -P ubuntu-latest=node:22-bookworm-slim
```

Never provide OpenWiki or release secrets during local preflight.

## Final checks

```sh
git diff --check
git status --short --branch
```

Report native test count, Windows test count, package verification result, generated artifact names/checksums, workflow dry-run limitations, HEAD, and cleanliness. Treat an uncommitted tracked change as a blocker.
