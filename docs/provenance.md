# Distribution provenance and licensing

This is the release inventory, not an assertion that every upstream prebuilt has a reproducible build attestation. Open maintainer items below must be settled before describing the project as fully eligible for a signing programme. System effects are disclosed in [security.md](security.md).

## Distributed artifact inventory

The exact CEF file lists, versions, sizes and digests are in [tools/cef/pins.json](../tools/cef/pins.json). The following table covers the portable Linux tarball, DEB/RPM/Arch packages, Windows ZIP and Inno Setup installer.

| Artifact | Origin / transformation | License and notice | Signing ownership |
| --- | --- | --- | --- |
| Linux `vosulay` | Built from this checkout and locked Cargo dependencies; release build in CI | Repository MIT (`LICENSE-vosulay.txt`) plus Rust dependency notices | vosulay source-built binary |
| Windows `vosulay.dll` | Root crate's `cdylib`, built for `x86_64-pc-windows-msvc` from this checkout | MIT plus Rust dependency notices | vosulay source-built binary; signing target |
| Linux `libvosulay_overlay.so`, `libvosulay_lookup.so` | `crates/overlay-linux` and `crates/overlay-lookup`, built from this checkout | MIT plus Rust dependency notices | vosulay source-built injected libraries |
| `VkLayer_vosulay_overlay.json` | Repository layer manifest, copied unchanged | MIT | Data, not a binary |
| Windows `vosulay.exe` | Official pinned CEF `Release/bootstrap.exe`, renamed to the client basename; icon/version resources edited, CEF copyright retained | CEF BSD-style terms in `LICENSE.txt`; Chromium terms in `CREDITS.html` | **Upstream prebuilt, not vosulay code; ships unsigned** |
| Linux CEF `chrome-sandbox`, `libcef.so`, `libvk_swiftshader.so`, `libvulkan.so.1` | Official minimal CEF archive; Linux packager strips ELF debug/unneeded symbols | CEF `LICENSE.txt` and Chromium component `CREDITS.html` | Third-party prebuilt; not an own-binary signing target |
| Windows CEF `libcef.dll`, `chrome_elf.dll`, `d3dcompiler_47.dll`, `dxcompiler.dll`, `dxil.dll`, `vk_swiftshader.dll`, `vulkan-1.dll` | Official minimal CEF archive | `LICENSE.txt` and `CREDITS.html` (not all component code is CEF's BSD license) | Upstream DLLs ship unsigned by vosulay; preserve upstream signatures if any |
| CEF `v8_context_snapshot.bin`, `vk_swiftshader_icd.json`, `chrome_100_percent.pak`, `chrome_200_percent.pak`, `icudtl.dat`, `resources.pak`, entire `locales/` tree | Copied from pinned archive | `LICENSE.txt` and `CREDITS.html` | Third-party runtime data |
| Windows `asdf_overlay-x64.dll`, `asdf_overlay-x86.dll` | Built by vosulay's Packages workflow (`vosulay-cef build-asdf`) from asdf-overlay source `c925244c8d14ca36d82585834cdfebf13d6ce86b`, linking Frida's stock Gum 17.17.0 devkits; source archive and devkits pinned by SHA-256 | Upstream `MIT OR Apache-2.0`; dependency notices enumerated from the pinned lockfile (205 crates per x64/x86 graph) plus pinned native Frida Gum 17.17.0 SDK component sources; GLib LGPL static-link route described below | Third-party code ships unsigned by vosulay |
| Embedded `assets/manager.html`, `.css`, `.js`, `assets/backend.js` | Repository frontend/broker source compiled into host | Repository MIT, retaining original copyright (see history below) | Part of source-built host |
| `assets/branding/*` (SVG masters; PNG and ICO rendered from them by `tools/branding/render.py`), editor toolbar brand mark | First-party branding by sxlvia: earlier marks in UI commit `0164a66` and Windows ICO commit `53d83db`, replaced by the 18-LAB product art after 1.4.0. The lockups carry the 18-LAB logotype; wordmark text is outlined Barlow Condensed (SIL OFL 1.1). Icon embedded in the Windows bootstrap and tray and in Linux desktop integration | © 2026 sxlvia, [CC BY 4.0 with an extra no-credit permission for showing vosulay](../assets/branding/LICENSE.md), shipped as `LICENSE-branding.txt` | Artwork, not independently signed code |
| Embedded editor fonts | Unmodified Fontsource Latin WOFF2 subsets of Barlow (400/500/600) and IBM Plex Mono (400), in `assets/fonts/`; embedded as data URLs in both CEF/browser documents | SIL Open Font License 1.1; copyright and complete texts in `assets/fonts/fonts-OFL.txt` and generated `THIRD_PARTY_NOTICES.md` | Font data, not independently signed code |
| Linux desktop entry and scalable application icon | `packaging/linux/com.vosu.vosulay.desktop`, `assets/branding/vosulay-app-icon.svg` | Desktop entry MIT; icon CC BY 4.0 (branding) | Integration data |
| Windows setup executable | Inno Setup 6.7.3, pinned by SHA-256 in the release workflow, compiles `packaging/windows/vosulay.iss`, embedding the complete staged package | vosulay MIT plus payload notices; [Inno Setup license](https://github.com/jrsoftware/issrc/blob/is-6_7_3/license.txt), reproduced in notices, permits binary redistribution retaining copyright/site notices | Installer signing target; embedded upstream binaries remain unsigned |
| `vosulay-cef.json` | Generated package receipt; SHA-256 of staged files, including notices | Repository-generated metadata | Records packaged bytes, not source-build attestation |
| `LICENSE-vosulay.txt`, `LICENSE-branding.txt`, `THIRD_PARTY_NOTICES.md`, CEF `LICENSE.txt`, `CREDITS.html` | Repository license, branding license, generated Cargo/upstream notices and original CEF attribution files | Preserve with every distribution | Distribution obligations |

There is no separate vendored production-source tree in this checkout. Registry crate sources may themselves contain vendored C/assembly, certificates or other data: the generator includes nested license/notice files, not only top-level SPDX labels. CEF and asdf-overlay contain additional upstream components; do not describe their complete licenses as simply MIT. The tosu/vosu backend and user-selected local/remote counter HTML are not bundled or licensed by vosulay. Qt/WebEngine legacy sources were removed in CEF cutover `9e17828`; Qt is not shipped by current packages.

## Pins and integrity

CEF is pinned to `154.0.34+g14c5a08+chromium-154.0.8037.98`. Official [Spotify-hosted CEF catalog](https://cef-builds.spotifycdn.com/index.json) size and SHA-1 were matched before calculating archive SHA-256. Since CEF 154 the minimal archives no longer ship ANGLE's separate `libEGL`/`libGLESv2` libraries on either platform:

| Download | Pinned SHA-256 |
| --- | --- |
| Linux minimal CEF archive | `e87e2c76304372dd28b899ceb6efa4b353f52700594ddc358a8237ec07ec6492` |
| Windows minimal CEF archive | `c81c34d048d87276f4a18f0b000206710a1709bb67dd658948b87100e1a538da` |
| asdf-overlay `c925244` source archive (codeload) | `ae413f0ed484f268a9237573a7eb1bafdde4e1d5a6cabd600ebd986818c6573b` |
| Frida Gum devkit 17.17.0 windows-x86_64 | `7c0166afe681395acb523fc3044a8b9434c9166997f6fdba43fd3631abaa6ca0` |
| Frida Gum devkit 17.17.0 windows-x86 | `ee208077ac38d28cd8f262374fc4ff11ebfce138c4b0d782d9f767b9ff49711e` |

The Frida devkit digests match GitHub's release asset metadata for `frida/frida` 17.17.0 (release ID `365221638`) and were recomputed from downloads. Acquisition validates expected sizes and SHA-256 before use. An interrupted download resumes with an HTTP byte range, appending only a `206` whose `Content-Range` continues the partial file; a `200` or any other range starts over. A download gives up after 4 consecutive requests that add no bytes, or 64 requests in all. The finished file is verified the same way and published only then. CEF cache receipts bind the platform/version/archive digest and retained file digests; `package_runtime` verifies the runtime before copying. The package manifest hashes the resulting files after branding/stripping, so transformed binaries need not equal archive members. Hashes detect different bytes; they do **not** establish which source/dependencies produced a prebuilt. Cargo uses `Cargo.lock` registry checksums for source archives.

The asdf-overlay DLLs have no fixed digest: they are compiled per release. `build-asdf` extracts the pinned source archive, serves only the two pinned devkits to upstream's unmodified `frida-build` script over loopback (`FRIDA_DOWNLOAD_CDN`), runs `cargo build --release --locked -p asdf-overlay-dll` for `x86_64-pc-windows-msvc` and `i686-pc-windows-msvc` with the repository's Rust toolchain, and logs each served devkit request and each output DLL's SHA-256 in the Packages run. Upstream's own v2.1.0 release DLLs are no longer shipped.

## Reproducing notices

From the repository root, with Python 3.11+ and the pinned Rust toolchain:

```sh
python3 tools/notices/generate.py
```

For a non-writing freshness check:

```sh
python3 tools/notices/generate.py --check
```

Cargo metadata may fetch locked sources on first use; no CEF archive or full compilation is needed. The generator follows default-feature metadata graphs for `x86_64-unknown-linux-gnu` (`vosulay`, `vosulay-overlay`, `vosulay-lookup`) and `x86_64-pc-windows-msvc` (`vosulay` host/client DLL). It traverses normal and build dependency edges, excludes dev-only edges, includes proc macros/build tooling conservatively and sorts package names, versions and relative paths. This is deliberately a superset of linked crate code, not a compiler/linker reachability claim. Current inventory: **333 registry crate versions**, plus the pinned Rust standard-library attribution document and asdf-overlay's own MIT notice.

All discovered license/notice text is read from Cargo's registry source cache, including nested vendored notices. A crate without license text fails generation before replacing the output. Fifteen crates omit license texts from their published tarballs: version-specific, digest-checked exceptions in `tools/notices/overrides.json` identify the preserved upstream text and source URL. Most are fetched at the `.cargo_vcs_info.json` source commit. `fxhash` and `mac` lack VCS metadata and upstream license files: their explicit source-header/README grants are retained as attributions, and their offered Apache-2.0 option uses the canonical Apache text. `selectors`' MPL-2.0 text comes from Mozilla's canonical license. These are explicit reviewed exceptions, not an SPDX-to-template fallback. New crate versions require new source evidence. The standard-library notice is copied from the pinned toolchain's `share/doc/rust/COPYRIGHT-library.html`; a toolchain change fails until it is refreshed.

`package_runtime` copies the repository `LICENSE` as `LICENSE-vosulay.txt`, `assets/branding/LICENSE.md` as `LICENSE-branding.txt`, and the generated notices next to binaries for both platforms. Linux `build.sh` and the AUR recipe use this shared staging function; all Linux formats copy the whole tree. Windows ZIP creation and Inno Setup's recursive `[Files]` entry preserve these files. Existing distro-specific copyright/license locations remain in addition to the executable-adjacent copies. CEF's `LICENSE.txt` is not overwritten.

## License findings and obligations

The repository MIT license is OSI-approved. Dependency declarations include MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, Unlicense and MPL-2.0; there is no identified requirement to relicense vosulay itself. This is an engineering inventory, not legal advice. Keep attribution, copyright, Apache notices and bundled component terms intact.

The following MPL-2.0 crates ship **unmodified from crates.io**. Their complete source archives are publicly available at the linked version-specific URLs, also included in the shipped notices. This supplies the source-availability reference for the covered files; if modified later, publish the corresponding modified source, not only these links.

| Crate | Version | Unmodified source |
| --- | --- | --- |
| cssparser | 0.35.0 | https://crates.io/api/v1/crates/cssparser/0.35.0/download |
| cssparser-macros | 0.6.1 | https://crates.io/api/v1/crates/cssparser-macros/0.6.1/download |
| dtoa-short | 0.3.5 | https://crates.io/api/v1/crates/dtoa-short/0.3.5/download |
| option-ext | 0.2.0 | https://crates.io/api/v1/crates/option-ext/0.2.0/download |
| selectors | 0.31.0 | https://crates.io/api/v1/crates/selectors/0.31.0/download |

## K4zoku copyright and source history

`LICENSE` already contains `Copyright © 2025 K4zoku` in vosulay's root commit `e438c3c` (2026-07-19, author qt715), together with the 2026 contributor line. It was not introduced by recent packaging work. That root commit already contains the Rust API/configuration/UI and a C++ Qt shell; there is **no earlier imported upstream history or merge ancestry** establishing copied-file boundaries.

[K4zoku/tosu-overlay-qt](https://github.com/K4zoku/tosu-overlay-qt) is a relevant predecessor candidate: its [MIT license at commit `72c55b7`](https://github.com/K4zoku/tosu-overlay-qt/blob/72c55b7a60239cb008803eed69bb5cc187228e0f/LICENSE) has the same 2025 K4zoku copyright and license wording (minus vosulay's additional contributor line). Its source is C++/Qt, whereas vosulay's initial application logic is Rust plus a Qt wrapper. Shared product behavior/license wording is not proof of particular copied code. The archived [tosu-overlay-gtk](https://github.com/K4zoku/tosu-overlay-gtk) is another project by the same author, not established ancestry here.

The initial `src/api.rs`, `src/config.rs`, `src/ui.rs`, `assets/manager.*` belong to the historical application lineage; this is **not** a claim each descends from K4zoku-authored code. The Qt wrapper was retired in `9e17828`, and the frontend branding/UI changed in `0164a66`. Keep K4zoku's notice across all distributions rather than deleting it based on a rewrite.

**Maintainer statement (qt715, 2026-10-10).** vosulay began from K4zoku's working prototypes, which qt715 adapted into the vosu and vosulay codebases; K4zoku's copyright line in `LICENSE` covers that origin. The Qt implementation and tosu-overlay-qt's architecture have since been replaced: no Qt code, Qt dependency or QML remains in this repository (checked against `Cargo.lock` and all tracked sources on 2026-10-10). Every file under `assets/branding/` was committed by qt715 and is first-party artwork by sxlvia (qt715's maintainer name). Since 2026-10-11 it is licensed separately from the MIT code under [CC BY 4.0](../assets/branding/LICENSE.md); copies already distributed under MIT keep that grant.

## Signing boundary and open maintainer items

[SignPath's terms, “Sign your own binaries only”](https://signpath.org/terms.html), distinguish own builds from upstream binaries. The signing policy is **`vosulay.dll` and the Inno Setup installer only** on Windows. `vosulay.exe` is CEF's prebuilt bootstrap with resource-only edits and ships unsigned; renaming/rebranding does not make it an own-source binary. CEF's upstream DLLs and the asdf-overlay DLLs (built by vosulay from third-party source) ship unsigned inside the signed installer, as permitted for embedded third-party code. This policy does not claim that an upstream file never carried its upstream publisher's signature. The full policy is in [code signing](code-signing.md).

Before applying with a claim that *all* distributed provenance/licensing is resolved, maintainers must:

1. **asdf-overlay DLLs: correspondence resolved by building from source; native devkit rebuild unverified.**

   vosulay no longer ships upstream's v2.1.0 release DLLs. The Packages workflow
   builds both from the asdf-overlay archive at commit
   `c925244c8d14ca36d82585834cdfebf13d6ce86b` (the same archive, by SHA-256, that
   the third-party source bundle contains) with
   `cargo build --release --locked -p asdf-overlay-dll`, default features,
   targets `x86_64-pc-windows-msvc` and `i686-pc-windows-msvc`
   ([pins and integrity](#pins-and-integrity)). Each graph contains 205 crates
   (8 upstream workspace and 197 external); `.gitmodules` is empty. The notices
   generator verifies the source archive's pinned SHA-256, checks that the build
   pin and notices pin agree, walks both non-dev graphs and deduplicates their
   license texts with vosulay's notices.

   Verified 2026-10-11: Packages run `38166976289` (branch `main`, `publish=false`)
   built both DLLs on a hosted Windows runner; its log records both pinned devkit
   requests. Exported names (`DllMain`, `vkNegotiateLoaderLayerInterfaceVersion`)
   and imported DLL sets match upstream's v2.1.0 prebuilts for both
   architectures. In a Windows 11 guest, the packaged DLLs injected into x64 and
   x86 Direct3D 11 test windows and drew a tosu counter, and the x86 DLL loaded
   into osu!stable. osu!lazer and osu!stable frames were not checked in that
   guest (lazer exited in its startup auth module before injection; stable's
   captured frame did not update).

   `crates/hook/build.rs` statically links Frida Gum devkit 17.17.0 outside Cargo.
   Gum source is `ddc10c5559cbb41a3dd72866bfba6ff3945ffa5c`; the pinned
   [releng dependency manifest](https://github.com/frida/releng/blob/c9cc18138b364f691573c695e1fe47ddc4c56f26/deps.toml)
   specifies GLib, Capstone, libffi, PCRE2, zlib and SDK liblzma sources.
   Component revisions/source links and license texts are preserved under
   `tools/notices/` and shipped in notices. Gum's wxWindows-3.1 binary exception
   does **not** replace GLib's plain LGPL-2.1-or-later terms. LGPL-2.1 §6 requires
   an applicable static-link compliance route: normally complete corresponding
   library source (including modifications), application object/source material
   and build/link instructions allowing library modification and relinking,
   notices, and terms permitting debugging those modifications; a compliant
   written offer is an alternative under the section's conditions.

   Generate the corresponding-source release asset with:

   ```sh
   python3 tools/notices/source_bundle.py dist/vosulay-<version>-third-party-sources.tar
   ```

   The versioned asset is linked in shipped notices and published alongside
   release packages. It contains 14 original source archives pinned by commit
   and SHA-256, a README with offline source-tree preparation, native-devkit
   build and DLL relink commands, and SHA256SUMS. Native/system compilers, SDKs
   and Rust dependencies are not embedded; Cargo uses the bundled upstream
   lockfile's registry checksums. Repeating bundle generation produced identical
   bytes. The documented source-tree preparation was exercised successfully on
   Linux, including preservation of GLib's symbolic license aliases. Its relink
   route (b) is the route the release build itself runs for both architectures.
   A Linux cargo-xwin build is not a substitute: upstream winres selects the
   nonexistent `/bin\x64\rc.exe` resource compiler.

   **Unverified:** building the native Frida devkit from the bundled sources
   (route (a)) and behavior with a modified library. The GLib object code in the
   shipped DLLs comes from Frida's stock devkits, whose component revisions are
   the bundled sources. **Notices and source links alone do not establish
   compliance.**

   The Cargo graph has no GPL/LGPL/MPL declarations; `frida-build` carries
   wxWindows terms. Native GLib is LGPL. XZ GPL scripts/build-system files and
   LGPL command-line helpers are not asserted to be linked into the public-domain
   liblzma runtime. Native notices conservatively preserve ancillary component
   terms without proving archive-level reachability.
2. Resolved by the maintainer statement in [K4zoku copyright and source history](#k4zoku-copyright-and-source-history): K4zoku's prototypes are the acknowledged origin, the MIT notice is kept, and the branding assets are first-party.

Do not interpret this document, notice generation success or archive checksums as approval from SignPath Foundation.
