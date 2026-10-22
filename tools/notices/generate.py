#!/usr/bin/env python3
"""Generate distribution notices from locked, target-filtered Cargo metadata."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import io
import os
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
TARGETS = {
    "x86_64-unknown-linux-gnu": ("vosulay", "vosulay-overlay", "vosulay-lookup"),
    "x86_64-pc-windows-msvc": ("vosulay",),
}


def collect(root=ROOT, targets=TARGETS, include_workspace=False):
    packages = {}
    for target, roots in targets.items():
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1",
             "--filter-platform", target], cwd=root,
            env={**os.environ, "RUSTUP_TOOLCHAIN": "1.96.1"}))
        by_id = {p["id"]: p for p in metadata["packages"]}
        nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
        pending = [p["id"] for p in metadata["packages"]
                   if p["name"] in roots and p["source"] is None]
        seen = set()
        while pending:
            package_id = pending.pop()
            if package_id in seen:
                continue
            seen.add(package_id)
            package = by_id[package_id]
            if package["source"] is not None or include_workspace:
                packages[package_id] = package
            # Include build dependencies/proc macros conservatively; omit dev-only edges.
            pending.extend(d["pkg"] for d in nodes[package_id]["deps"]
                           if any(k["kind"] != "dev" for k in d["dep_kinds"]))
    return sorted(packages.values(), key=lambda p: (p["name"], p["version"], p["source"] or ""))


def collect_asdf():
    pin = json.loads((ROOT / "tools/notices/overrides.json").read_text())["_asdf_source"]
    data = urllib.request.urlopen(pin["url"], timeout=120).read()
    if hashlib.sha256(data).hexdigest() != pin["sha256"]:
        raise ValueError("asdf-overlay source archive SHA-256 mismatch")
    temporary = tempfile.TemporaryDirectory()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        # Reject archive links and unsafe paths before extraction (Python 3.11+).
        for member in archive.getmembers():
            path = Path(member.name)
            if path.is_absolute() or ".." in path.parts or not (member.isfile() or member.isdir()):
                raise ValueError(f"unsafe upstream archive member: {member.name}")
        archive.extractall(temporary.name)
    root = Path(temporary.name) / ("asdf-overlay-" + pin["commit"])
    if (root / ".gitmodules").read_text().strip():
        raise ValueError("asdf-overlay submodules changed; audit their compiled code and licenses")
    native = json.loads((ROOT / "tools/notices/overrides.json").read_text())["_frida_native"]
    if (root / "crates/hook/FRIDA_VERSION").read_text().strip() != native["version"]:
        raise ValueError("asdf-overlay Frida devkit version changed; refresh native license inventory")
    graphs = {}
    union = {}
    for target in ("x86_64-pc-windows-msvc", "i686-pc-windows-msvc"):
        graph = collect(root, {target: ("asdf-overlay-dll",)}, include_workspace=True)
        graphs[target] = len(graph)
        for package in graph:
            if package["source"] is None:
                package["source"] = "git+https://github.com/storycraft/asdf-overlay#" + pin["commit"]
                package["id"] = package["source"] + ":" + package["name"] + "@" + package["version"]
                package["notice_license_root"] = str(root)
            union[package["id"]] = package
    return temporary, sorted(union.values(), key=lambda p: (p["name"], p["version"], p["source"])), graphs


def source_url(package):
    if package["source"].startswith("registry+"):
        return f"https://crates.io/api/v1/crates/{package['name']}/{package['version']}/download"
    repository, commit = package["source"].removeprefix("git+").split("#", 1)
    return repository + "/tree/" + commit


def license_files(package):
    if package.get("notice_license_root"):
        directory = Path(package["notice_license_root"])
        files = {directory / "LICENSE-MIT", directory / "LICENSE-APACHE"}
    else:
        directory = (ROOT / "tools/notices" if package["id"].startswith("native:")
                     else Path(package["manifest_path"]).parent)
        files = set()
    for file in (() if package.get("notice_license_root") or package["id"].startswith("native:")
                 else directory.rglob("*")):
        name = file.name.upper()
        if file.is_file() and (name.startswith(("LICENSE", "LICENCE", "COPYING"))
                               or name in ("NOTICE", "NOTICE.TXT", "COPYRIGHT")):
            files.add(file)
    if package["license_file"]:
        files.add(directory / package["license_file"])
    if not files:
        overrides = json.loads((ROOT / "tools/notices/overrides.json").read_text())
        entries = overrides.get(package["name"] + "@" + package["version"], [])
        directory = ROOT / "tools/notices"
        for entry in entries:
            file = directory / entry["file"]
            if hashlib.sha256(file.read_bytes()).hexdigest() != entry["sha256"]:
                raise ValueError(f"vendored license digest mismatch: {file}")
            files.add(file)
    if not files:
        raise ValueError(f"{package['name']} {package['version']}: no license text in {directory}")
    return directory, sorted(files, key=lambda f: f.relative_to(directory).as_posix())


def render():
    pins = json.loads((ROOT / "tools/cef/pins.json").read_text())
    if pins["asdf_overlay"]["version"] != "2.1.0":
        raise ValueError("asdf-overlay pin changed; update its vendored license and provenance")
    overrides = json.loads((ROOT / "tools/notices/overrides.json").read_text())
    build_source = pins["asdf_overlay"]["source"]
    if (build_source["commit"], build_source["sha256"]) != (
            overrides["_asdf_source"]["commit"], overrides["_asdf_source"]["sha256"]):
        raise ValueError("asdf-overlay build source differs from the notices/source-bundle pin")
    if pins["asdf_overlay"]["frida_version"] != overrides["_frida_native"]["version"]:
        raise ValueError("asdf-overlay Frida devkit pin differs from the native notices inventory")
    chunks = ["# Third-party notices\n\n"
              "Generated by `python3 tools/notices/generate.py`; do not edit manually.\n\n"
              "Covers the union of the locked Linux host/compositor/lookup and Windows "
              "host/client dependency graphs (default release features), including build "
              "dependencies and proc macros conservatively, excluding dev-only dependencies. "
              "License expressions are upstream declarations; all discovered license and "
              "notice texts are reproduced, not replaced by SPDX labels.\n\n"
              "## CEF and Chromium\n\n"
              f"Pinned CEF: `{pins['cef_version']}`. The accompanying `LICENSE.txt` "
              "and `CREDITS.html` carry CEF and Chromium component terms, including "
              "the runtime libraries, resources, locales, and Windows bootstrap. "
              "Keep those files with the runtime.\n\n"
              "## asdf-overlay 2.1.0\n\n"
              "Source: https://github.com/storycraft/asdf-overlay/tree/"
              "c925244c8d14ca36d82585834cdfebf13d6ce86b\n\n"
              "Upstream offers MIT OR Apache-2.0; the MIT option is reproduced below. "
              "vosulay builds the shipped DLLs from this source; DLL dependency licenses "
              "below are enumerated from its lockfile. Native Frida devkit obligations are "
              "recorded separately in docs/provenance.md.\n\n"]
    overlay = ROOT / "tools/notices/asdf-overlay-2.1.0-LICENSE-MIT.txt"
    chunks.append("```text\n" + overlay.read_text().rstrip() + "\n```\n\n")
    chunks.append("## Editor fonts\n\n"
                  "Barlow and Barlow Condensed © 2017 The Barlow Project Authors "
                  "(https://github.com/jpt/barlow); IBM Plex Mono © 2017 IBM Corp. "
                  "(https://github.com/IBM/plex). Bundled WOFF2 files are unmodified "
                  "Latin subsets distributed by Fontsource, under SIL Open Font License 1.1.\n\n")
    chunks.append("```text\n" + (ROOT / "assets/fonts/fonts-OFL.txt").read_text().rstrip() + "\n```\n\n")
    own_packages = collect()
    upstream_temporary, asdf_packages, asdf_counts = collect_asdf()
    union = {p["id"]: p for p in own_packages + asdf_packages}
    packages = sorted(union.values(), key=lambda p: (p["name"], p["version"], p["source"]))
    native_pin = overrides["_frida_native"]
    manifest = ROOT / "tools/notices/Frida-17.17.0-deps.toml"
    if hashlib.sha256(manifest.read_bytes()).hexdigest() != native_pin["manifest_sha256"]:
        raise ValueError("Frida pinned dependency manifest digest mismatch")
    native_packages = native_pin["components"]
    all_components = packages + native_packages
    texts = {}
    package_texts = {}
    for package in all_components:
        directory, files = license_files(package)
        key = package["id"]
        package_texts[key] = set()
        for file in files:
            # Only line endings and trailing whitespace are normalized. Preserve
            # indentation, internal blank lines and every differing copyright.
            text = file.read_bytes().decode("utf-8").replace("\r\n", "\n").replace("\r", "\n")
            text = "\n".join(line.rstrip() for line in text.split("\n")).rstrip() + "\n"
            group = texts.setdefault(text, {})
            group.setdefault(key, set()).add(file.relative_to(directory).as_posix())
            package_texts[key].add(text)
    ordered_texts = sorted(texts, key=lambda text: (hashlib.sha256(text.encode()).hexdigest(), text))
    text_ids = {text: number for number, text in enumerate(ordered_texts, 1)}
    chunks.append(f"## Rust dependencies ({len(packages)} crates)\n\n"
                  f"Each crate has one inventory entry below, referencing its license/notice "
                  f"texts. The {len(texts)} distinct normalized texts are reproduced once; "
                  "per-text usage lists retain every crate and contributing file.\n\n")
    for package in packages:
        links = ", ".join(f"[Text {text_ids[text]}](#license-text-{text_ids[text]})"
                          for text in sorted(package_texts[package["id"]], key=text_ids.get))
        chunks.append(f"### {package['name']} {package['version']}\n\n"
                      f"Declared license: `{package['license'] or 'see license file'}`\n\n"
                      f"Source: `{package['source']}`\n\n"
                      f"Unmodified crate source: {source_url(package)}\n\n"
                      f"Repository: {package.get('repository') or '(not declared)'}\n\n"
                      f"License/notice texts: {links}\n\n")
        key = package["name"] + "@" + package["version"]
        for entry in overrides.get(key, []):
            chunks.append(f"License text provenance: {entry['url']}\n\n")
        if package["name"] == "fxhash":
            chunks.append("Copyright 2015 The Rust Project Developers. "
                          "Source header attributes the algorithm to rustc and offers "
                          "Apache-2.0 OR MIT; Apache-2.0 terms are reproduced here.\n\n")
        if package["name"] == "mac":
            chunks.append("Author: Jonathan Reem. Upstream README declares MIT/Apache-2.0; "
                          "Apache-2.0 terms are reproduced here.\n\n")
    chunks.append("## asdf-overlay DLL dependencies\n\n"
                  "Release construction: vosulay's Packages workflow runs "
                  "`cargo build --release --locked -p asdf-overlay-dll` on the pinned source "
                  "for x86_64 and i686 MSVC with default features and no extra feature flags, "
                  "linking hash-pinned stock Frida Gum devkits (see tools/cef/pins.json). "
                  "The pinned .gitmodules is empty: no compiled submodules.\n\n"
                  "Graph counts (including upstream workspace crates, build dependencies "
                  "and proc macros; excluding dev-only edges):\n\n")
    for target, count in sorted(asdf_counts.items()):
        chunks.append(f"- `{target}`: {count} crates\n")
    chunks.append("\n")
    for package in asdf_packages:
        links = ", ".join(f"[Text {text_ids[text]}](#license-text-{text_ids[text]})"
                          for text in sorted(package_texts[package["id"]], key=text_ids.get))
        chunks.append(f"- **{package['name']} {package['version']}** — "
                      f"`{package['license'] or 'see license file'}`; "
                      f"[source]({source_url(package)}); "
                      f"repository: {package.get('repository') or '(not declared)'}; {links}\n")
    chunks.append("\n")
    chunks.append("## asdf-overlay native Frida Gum devkit components\n\n"
                  "crates/hook/build.rs statically links gum devkit 17.17.0. "
                  "Component revisions below come from Frida 17.17.0's releng commit "
                  "c9cc18138b364f691573c695e1fe47ddc4c56f26/deps.toml and Gum's "
                  "ddc10c5559cbb41a3dd72866bfba6ff3945ffa5c/meson.build. "
                  "This conservatively covers Gum/GLib's Windows dependency closure, "
                  "including SDK liblzma; it is not an archive-level reachability attestation. "
                  "GLib's plain LGPL static-link obligations are not discharged by notices "
                  "alone. XZ GPL scripts/build tooling and GLib test/documentation terms "
                  "are preserved conservatively, not asserted to be shipped runtime code.\n\n")
    chunks.append(f"Pinned dependency manifest: {native_pin['manifest_url']}\n\n")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    source_asset = f"vosulay-{version}-third-party-sources.tar"
    chunks.append("Corresponding source for the LGPL components and the application's "
                  f"rebuild/relink route: [{source_asset}]"
                  f"(https://github.com/shiruviaa/vosulay/releases/download/v{version}/{source_asset}), "
                  "published alongside every GitHub release. Extract the plain tar and read "
                  "its README.md and SHA256SUMS before rebuilding. It preserves original "
                  "pinned source archives for asdf-overlay, Gum, releng, native components "
                  "and non-system build tooling. Rust crates resolve via the bundled "
                  "Cargo.lock checksums. vosulay builds the shipped asdf-overlay DLLs from "
                  "the bundled asdf-overlay archive with Frida's stock Gum devkits; rebuilding "
                  "those devkits from the bundled native sources remains unverified, and the "
                  "source bundle is not a compliance attestation.\n\n")
    for package in native_packages:
        links = ", ".join(f"[Text {text_ids[text]}](#license-text-{text_ids[text]})"
                          for text in sorted(package_texts[package["id"]], key=text_ids.get))
        chunks.append(f"- **{package['name']}** revision `{package['version']}` — "
                      f"{package['license']}; [source]({source_url(package)}); {links}\n")
    chunks.append("\n")
    chunks.append("## Shared dependency license and notice texts\n\n")
    for text in ordered_texts:
        number = text_ids[text]
        chunks.append(f"### License text {number}\n\n"
                      f"Normalized SHA-256: `{hashlib.sha256(text.encode()).hexdigest()}`\n\n"
                      "Used by:\n\n")
        for package in all_components:
            filenames = texts[text].get(package["id"])
            if filenames is None:
                continue
            paths = ", ".join(f"`{name}`" for name in sorted(filenames))
            chunks.append(f"- **{package['name']} {package['version']}** — "
                          f"`{package['license'] or 'see license file'}`; "
                          f"[source]({source_url(package)}); repository: "
                          f"{package.get('repository') or '(not declared)'}; files: {paths}\n")
        chunks.append("\n````text\n" + text + "````\n\n")
    toolchain = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())
    if toolchain["toolchain"]["channel"] != "1.96.1":
        raise ValueError("Rust toolchain changed; refresh standard-library copyright notices")
    runtime_notices = ROOT / "tools/notices/Rust-1.96.1-COPYRIGHT-library.html"
    chunks.append("## Rust 1.96.1 standard library\n\n"
                  "Source: https://github.com/rust-lang/rust/tree/1.96.1/library\n\n"
                  "The following is the toolchain-distributed standard-library attribution "
                  "document, including its embedded dependency licenses.\n\n````html\n"
                  + runtime_notices.read_text().rstrip() + "\n````\n")
    workflow = (ROOT / ".github/workflows/release.yml").read_text()
    inno = re.search(r"^\s*INNO_SETUP_VERSION:\s*([0-9.]+)\s*$", workflow, re.M)
    if not inno:
        raise SystemExit("INNO_SETUP_VERSION pin missing from .github/workflows/release.yml")
    inno_tag = "is-" + inno.group(1).replace(".", "_")
    chunks.append("\n## Inno Setup installer engine\n\n"
                  f"Inno Setup {inno.group(1)} (pinned in the release workflow) compiles the "
                  "Windows installer and contributes its setup runtime. Source: "
                  f"https://github.com/jrsoftware/issrc/tree/{inno_tag}; license: "
                  f"https://github.com/jrsoftware/issrc/blob/{inno_tag}/license.txt\n\n````text\n"
                  + (ROOT / "tools/notices/Inno-Setup-LICENSE.txt").read_text().rstrip()
                  + "\n````\n")
    upstream_temporary.cleanup()
    return "".join(chunks).encode("utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if committed notices differ")
    args = parser.parse_args()
    data = render()  # Validate all licenses before replacing the destination.
    destination = ROOT / "THIRD_PARTY_NOTICES.md"
    if args.check:
        if not destination.is_file() or destination.read_bytes() != data:
            sys.exit("THIRD_PARTY_NOTICES.md is stale; regenerate it")
    else:
        destination.write_bytes(data)


if __name__ == "__main__":
    main()
