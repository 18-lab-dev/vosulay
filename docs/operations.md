# Build, launch, and package

Start with [CONTRIBUTING.md](../CONTRIBUTING.md) for single-machine prerequisites and verification tiers. The commands below work on a contributor's own Linux or native Windows machine; no private host tooling or offline cache is required.

## Prerequisites and runtime

Install rustup and use the pinned [Rust toolchain](../rust-toolchain.toml). Linux needs a C compiler, `pkg-config`, Wayland/EGL development files, both SDL2/SDL3 development packages for fixtures, and CEF's runtime libraries; [CONTRIBUTING.md](../CONTRIBUTING.md) includes the exact [Linux CI](../.github/workflows/linux-ci.yml) package list and distribution caveats. Native Windows needs MSVC Build Tools with Desktop development with C++, an x64 compiler/linker and Windows SDK. See [testing](testing.md) for library-path preparation.

Cargo automatically acquires the pinned precompiled CEF distribution when `CEF_PATH` is unset. [crates/cef-tools](../crates/cef-tools/) verifies archive size, SHA1/SHA256, and runtime files using [pins.json](../tools/cef/pins.json). An explicit `CEF_PATH` must point to a verified distribution. Only the initial acquisition needs network access; application startup downloads nothing.

The first CEF acquisition requires internet access and time to download/extract approximately 322 MB on Linux or 172 MB on Windows; allow several GB for extracted assets and Cargo outputs. An empty cache is supported. These are precompiled distributions, not Chromium source builds.

Linux caches under `$XDG_CACHE_HOME/vosulay` (`~/.cache/vosulay` by default): `cef/` stores the SDK, `runtime/` retains executable-adjacent CEF assets, and `overlay/` retains native compositor/lookup libraries. Cargo-installed binaries rely on runtime/overlay caches; reinstall if they are removed. Portable packages use adjacent assets instead.

## Installation and packaging

For release installation and removal, see the [README](../README.md#install).
For a source installation, run `cargo install --path . --locked` (`--force` to
reinstall). Cargo-installed Linux binaries depend on the retained caches above,
not a redistributable executable alone.

### Linux release packages

[`packaging/linux/build.sh`](../packaging/linux/build.sh) builds the tarball and
uses [nfpm metadata](../packaging/linux/nfpm.yaml) to produce DEB, RPM and Arch
binary packages. Version comes from `Cargo.toml`'s workspace version:

- `vosulay_<v>-1_amd64.deb` — Ubuntu 22.04+ / Debian 12+.
- `vosulay-<v>-1.x86_64.rpm` — Fedora.
- `vosulay-<v>-1-x86_64.pkg.tar.zst` — Arch (`pacman -U`).
- `vosulay-<v>-linux-x86_64.tar.gz` — other distributions with compatible
  glibc and CEF dependencies; extract and run `./vosulay`.

Packages install the executable, CEF runtime/resources/locales, overlay/lookup
libraries and Vulkan layer manifest in `/usr/lib/vosulay`, with a
`/usr/bin/vosulay` symlink. They install
`/usr/share/applications/com.vosu.vosulay.desktop`, the scalable
`com.vosu.vosulay.svg` icon and license. `chrome-sandbox` is root-owned mode
`4755`, allowing Chromium's sandbox where unprivileged user namespaces are
restricted (Ubuntu 24.04+). With the system launcher installed, startup does not
write a per-user entry. Tarballs instead register a user entry pointing at the
extracted location; see the README for their sandbox setup.

The Ubuntu 22.04 build environment preserves a glibc 2.35 floor. The cef-tools
`package` command strips debug/static symbols from Linux ELF files; compressed
distro packages are approximately 155 MB. Package builds set
`VOSULAY_PACKAGE_BUILD=1`, so the executable's only library search path is
`$ORIGIN` rather than the build machine's CEF cache.

From the repository root, download nfpm v2.47.0 from
[GoReleaser's nfpm releases](https://github.com/goreleaser/nfpm/releases/tag/v2.47.0),
verify the Linux x86_64 archive against the checksum pinned in
[`release.yml`](../.github/workflows/release.yml), and extract its binary:

```sh
podman build -t vosulay-package-build -f packaging/linux/Containerfile packaging/linux
podman run --rm -v "$PWD:/src" \
  -v /path/to/nfpm:/usr/local/bin/nfpm:ro \
  -e CARGO_TARGET_DIR=/src/target/package-linux \
  vosulay-package-build packaging/linux/build.sh target/package-dist
```

Replace `/path/to/nfpm` with the extracted binary's absolute path. The
[Containerfile](../packaging/linux/Containerfile) uses
[`install-build-deps.sh`](../packaging/linux/install-build-deps.sh) for build
prerequisites; nfpm must also be available inside the container. Docker works
with the same commands in place of Podman. Optional persistent mounts for
`~/.cache/vosulay/cef` at `/root/.cache/vosulay/cef` and the Cargo registry at
`/root/.cargo/registry` avoid repeat downloads. Use a fresh output directory
(or remove only prior task-owned staging directories before rebuilding).
Running `packaging/linux/build.sh` directly on a newer host works, but raises
the binaries' glibc floor to that host's version.

### Arch source package / AUR

[`packaging/arch/PKGBUILD`](../packaging/arch/PKGBUILD) builds from source, unlike
the prebuilt Arch release artifact. It uses `rustup` as a make dependency and
the pinned CEF archive as a source, installs the setuid sandbox, and specifies
`!lto !strip !debug`. After tagging, bump `pkgver` and run `updpkgsums`; the
source tarball checksum is `SKIP` until then. Keep the CEF source and checksum
aligned with `tools/cef/pins.json`.

### Windows packages

On native Windows, stage the complete runtime ZIP and compile the installer
with Inno Setup 6.7.3, the version the release workflow pins (replace `<v>` with
the workspace version and use absolute
source/output paths if required by your installation):

```powershell
cargo run --locked -p vosulay-cef-tools -- package-windows dist/package
ISCC /DAppVersion=<v> /DSourceDir=<dist/package> /DOutputDir=<dist> packaging/windows/vosulay.iss
```

`package-windows` builds `vosulay.dll` itself; pass `--client <path>` to package
an already built (for example, signed) DLL instead. See the
[code signing policy](code-signing.md) for the release signing flow.

Outputs are `vosulay-<v>-windows-x86_64.zip` and
`vosulay-<v>-windows-x86_64-setup.exe`. Keep the bootstrap `vosulay.exe`,
`vosulay.dll`, CEF files/locales and both injection DLLs together.
The [installer](../packaging/windows/vosulay.iss) is per-user, needs no admin
rights and defaults to `%LOCALAPPDATA%\Programs\Vosulay`. Its Start Menu
shortcut matches the app's self-registration path. Desktop and Windows-startup
shortcuts are unchecked options; the final page offers launch. Exit through
**tray → Exit vosulay** before upgrade/uninstall; the installer checks for a
running instance. Uninstall preserves `%APPDATA%\vosulay\config.json`. The ZIP
uses the same user config, not portable storage. Unsigned installers may
trigger SmartScreen.

### Release process

1. Bump `[workspace.package].version` in `Cargo.toml` and keep the lockfile's
   workspace package versions consistent. Update `pkgver` in
   `packaging/arch/PKGBUILD`, and regenerate the notices with
   `python3 tools/notices/generate.py`: they link the versioned source asset,
   and Linux CI fails while they are stale.
2. Commit the release changes.
3. Create `v<v>` for that version: `git tag v<v>`.
4. Push the release commit and tag: `git push` and `git push origin v<v>`.
5. In Actions, manually run [**Packages**](../.github/workflows/release.yml) on
   that tag with `publish=true`. The Linux job builds in `ubuntu:22.04`; the
   Windows job produces the ZIP and installer. Every run uploads
   `vosulay-linux-x86_64` and `vosulay-windows-x86_64` artifacts.
6. Publication verifies the tag matches `Cargo.toml`, then creates the GitHub
   release with all six packages, `vosulay-<v>-third-party-sources.tar` and
   `SHA256SUMS`. The source archive is the corresponding source for the LGPL
   components inside the asdf-overlay DLLs
   ([provenance](provenance.md)); it is assembled from pinned, hash-checked
   upstream archives, and a changed upstream archive fails publication. A
   branch run with `publish=false` builds artifacts only; never publish for
   routine verification.
7. Run `updpkgsums` for `packaging/arch/PKGBUILD` against the published tag and
   update the AUR package metadata.
8. Update the Windows package-manager manifests with the new version, release
   date and the hashes from `SHA256SUMS`:
   [`packaging/winget/`](../packaging/winget/) (`PackageVersion`, `ReleaseDate`,
   `InstallerUrl`, `InstallerSha256`, `ReleaseNotesUrl`) and
   [`packaging/scoop/vosulay.json`](../packaging/scoop/vosulay.json) (`version`,
   `url`, `hash`). They are prepared but not yet submitted to winget-pkgs or a
   Scoop bucket: both need publicly downloadable release assets.

### Packaging verification and tested-on list

For packaging changes, build in the Ubuntu 22.04 container and install-test the
DEB/RPM/Arch packages in disposable target distro containers. Check
`vosulay --version`, all `libcef.so` dependencies, root ownership and mode
`4755` of `chrome-sandbox`, sustained app startup and absence of a duplicate
user desktop entry. Runtime launch checks need an isolated display; container
checks do not establish native game/GPU behavior.

Locally tested on:

- Ubuntu 22.04.
- Ubuntu 24.04.
- Debian 12.
- Fedora latest at test time.
- Arch Linux latest at test time.

Packages were built in the Ubuntu 22.04 container. On each listed distro,
`vosulay --version` ran, all libcef dependencies resolved, the sandbox was
root-owned mode `4755`, the app started and kept running, and no duplicate user
desktop entry was written.

On 2026-10-10 the release workflow's Windows steps ran natively on Windows 11
with the real 1.1.0 runtime: client DLL build, packaging with `--client`,
installer compilation (Inno Setup 7.1.0) and
[`verify-release.ps1`](../packaging/windows/verify-release.ps1). The pinned
Inno Setup 6.7.3 then compiled `vosulay.iss` against a dummy layout with the
expected version metadata; the 1.0.0 and 1.1.0 installers came from the 6.7
line. No installer was run for this check; it is build evidence, not
install/upgrade/uninstall evidence.

## Launchers and single instance

Startup registers vosulay in app launchers. Linux writes `$XDG_DATA_HOME/applications/com.vosu.vosulay.desktop` (default `~/.local/share/applications/com.vosu.vosulay.desktop`) with an absolute Exec and installs the matching `icons/hicolor/scalable/apps/com.vosu.vosulay.svg`, unless the desktop entry exists in `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`). Windows creates `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Vosulay.lnk`. Registration failures are logged without stopping the host. Registration is separate from refreshing osu shortcuts for injection. A second launch while vosulay runs prints a message and exits rather than creating another instance.

Both platforms expose the tray menu **Edit mode** (checkable), **Hide vosulay** (checkable), and **Exit vosulay**. Linux uses a StatusNotifierItem; a desktop needs compatible tray support to display it. Hiding is not quitting. Configure native edit/visibility chords in Settings → Shortcuts; edit defaults to Ctrl+Shift+Space and visibility starts unbound.

## Command line and storage

[src/runtime.rs](../src/runtime.rs) accepts `--vosu-url`/`-t`, `--fps-limit`/`-f` (`auto`, `0`, or 1–1,000,000), repeatable nonzero `--pid`, repeatable executable-basename `--process-name`, `--config`, and `--cef-dir`. `--help`/`-h` and `--version`/`-v` exit without starting the host. `--benchmark-uncapped` is testing-only and overrides pacing without changing persisted FPS settings. Unknown options are rejected.

On Linux `--cef-dir` also selects adjacent compositor/lookup libraries. Windows uses the executable's runtime directory. A `portable` marker or existing adjacent `data/config.json` selects portable storage; `--config` overrides it. See [configuration](configuration.md).

## Attachment and troubleshooting

Linux refreshes recognized osu shortcuts with absolute injection paths, preserving foreign preloads, arguments, and OBS flags. System shortcuts receive user overrides; changes affect the next game launch. It also attempts live attachment to same-user native games. Where policy prevents injection, restart through the refreshed shortcut or use the manual preload/Vulkan commands in the [README](../README.md).

For a missing overlay, check game focus, backend reachability, loaded compositor/runtime files, hidden state, enabled placement, and viewport position. Selection follows focus, not process order. Do not change ptrace policy or request elevation just to bypass attachment restrictions.

Input failure releases capture within the five-second safety bound; Windows may exit. Restart the host if needed. If a remote bind fails, the local backend broker remains available. Listener/allowlist changes and remote security are described in [integrations](integrations.md).

## Maintainer environment (optional)

Guest deployments and offline performance caches are additional maintainer tooling, not installation prerequisites. See [testing](testing.md#maintainer-environment-optional) for authorized guest safety and benchmark boundaries. Contributors can package locally and request a manual [Packages](../.github/workflows/release.yml) Actions run with `publish=false` instead; it uploads Linux packages and the Windows ZIP/installer. Never publish a release as part of routine verification.
