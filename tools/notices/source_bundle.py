#!/usr/bin/env python3
"""Download verified source archives and assemble a deterministic, uncompressed tar."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def readme(pins):
    table = "\n".join(f"| {p['name']} | `{p['commit']}` | {p['repository']} |" for p in pins)
    names = [p['name'] for p in pins]
    return f'''# Corresponding sources for vosulay's asdf-overlay DLLs

This bundle supplies the source inputs and a rebuild/relink route for the
LGPL-covered GLib component statically linked through Frida Gum into the
upstream asdf-overlay 2.1.0 DLLs. It is not a claim that legal compliance is
fully established: the native devkit rebuild below is NOT verified by vosulay,
and correspondence between these sources and the upstream prebuilt DLLs
vosulay ships remains UNVERIFIED. The local-HTTP replacement-devkit route was
verified natively on Windows for x64 with a stock devkit (section (c)); this
does not verify rebuilding the native devkit from source.

Verify the original compressed archives and this README using SHA256SUMS.
The outer tar is plain/uncompressed; its members retain downloaded archive
bytes unchanged. Source licenses and notices are inside the source archives
and in the distribution's THIRD_PARTY_NOTICES.md.

## Inputs and boundary

| Source | Exact revision | Repository |
| --- | --- | --- |
{table}

Frida Gum 17.17.0 uses releng's pinned deps.toml and the Gum subproject wraps.
The Windows Gum-only closure includes GLib/GObject, Capstone, libffi, PCRE2 and
zlib; SDK liblzma (XZ) is included conservatively. GumJS/Core-only dependencies
(V8, QuickJS, TinyCC, TLS/network stacks, etc.) are not needed for this Gum-only
route and are not asserted to be embedded in the Gum devkit. Meson and tomlkit
are releng's exact submodule revisions. Frida's patched Ninja, pkg-config and
Vala sources are included at the deps.toml revisions as non-system build tools.

General system tools are outside this source bundle: Python, Git, a C/C++
compiler/linker, platform SDK/CRT (e.g. Microsoft Visual Studio Build Tools),
Rust/Cargo and archive tools. Provision appropriately licensed tools yourself.
Using Frida's optional prebuilt bootstrap toolchain is NOT necessary in the
route below: --without-prebuilds=toolchain,sdk selects PATH tools and source
fallbacks instead. You may build the bundled Ninja/pkg-config/Vala sources
using their own build instructions if the PATH equivalents are unsuitable.
The complete releng/deps.py SDK builder is also bundled, but its default
bootstrap-toolchain download is not the source-only route documented here.

Rust registry crates are intentionally not vendored. Cargo resolves them from
crates.io using the bundled asdf-overlay Cargo.lock source checksums; crates.io
does not delete published crate versions. Use cargo --locked. If long-term
offline availability is needed, cargo vendor is a separate local operation.

## (a) Rebuild the native Gum devkits from bundled source (unverified)

Use a Windows x64 developer environment with MSVC x64 AND x86 compiler/SDK
support, Python 3.12+, and suitable Ninja/pkg-config on PATH. This route uses
Frida's actual configure/releng/Meson devkit target, not an invented build API.
Extract the outer tar to an empty directory. Save the following as prepare.py
there and run `python prepare.py`. It uses only bundled archives and seeds
all required subprojects, preventing Git clones with --wrap-mode=nodownload.
The source archives contain symbolic links (including GLib license aliases).
Preparation therefore needs an environment supporting symlink extraction and
copying; preparing on Linux and transferring the tree to Windows is another
option. Preserve links rather than dereferencing possibly dangling aliases.

```python
import pathlib, shutil, tarfile
names = {names!r}
work = pathlib.Path('work')
work.mkdir()  # Use a fresh directory, never overwrite an existing checkout.
for name in names:
    archive = next(pathlib.Path('.').glob(name + '-*.tar.gz'))
    scratch = work / ('extract-' + name)
    scratch.mkdir()
    with tarfile.open(archive, 'r:gz') as source:
        source.extractall(scratch, filter='data')
    top, = scratch.iterdir()
    shutil.move(str(top), work / name)
    scratch.rmdir()
def install(source, destination):
    if destination.exists():
        if any(destination.iterdir()):
            raise RuntimeError('refusing to replace nonempty ' + str(destination))
        destination.rmdir()
    shutil.copytree(source, destination, symlinks=True)
gum = work / 'frida-gum'
install(work / 'releng', gum / 'releng')
install(work / 'meson', gum / 'releng' / 'meson')
install(work / 'tomlkit', gum / 'releng' / 'tomlkit')
for name in ('glib', 'capstone', 'libffi', 'pcre2', 'zlib', 'xz'):
    install(work / name, gum / 'subprojects' / name)
# GLib's own fallback wraps may resolve in its nested subprojects directory.
for name in ('libffi', 'pcre2', 'zlib'):
    install(work / name, gum / 'subprojects' / 'glib' / 'subprojects' / name)
```

Make desired GLib/library modifications in the seeded subproject sources before
configuring. In PowerShell from `work/frida-gum`, repeat for each architecture:

```powershell
foreach ($arch in @('x86_64', 'x86')) {{
    $env:MESON_BUILD_ROOT = Join-Path $PWD "build-$arch"
    .\\configure.bat --host="windows-$arch" --without-prebuilds=toolchain,sdk -- -Dfrida_version=17.17.0 -Dgumjs=disabled -Dgumpp=disabled -Dtests=disabled -Ddevkits=gum --wrap-mode=nodownload
    if ($LASTEXITCODE -ne 0) {{ throw 'Gum configure failed' }}
    python releng/meson/meson.py compile -C "build-$arch" gum-devkit
    if ($LASTEXITCODE -ne 0) {{ throw 'Gum devkit build failed' }}
}}
```

`gum/devkit/meson.build` writes frida-gum.h, frida-gum.lib and example files to
`build-<arch>/gum/devkit/`. Archive the CONTENTS of each directory (no parent
folder) as tar.xz, e.g. using tar with xz support:

```powershell
New-Item -ItemType Directory -Force ../../http/17.17.0
foreach ($arch in @('x86_64', 'x86')) {{
    tar -cJf "../../http/17.17.0/frida-gum-devkit-17.17.0-windows-$arch.tar.xz" -C "build-$arch/gum/devkit" .
    if ($LASTEXITCODE -ne 0) {{ throw 'Devkit archive creation failed' }}
}}
Remove-Item Env:MESON_BUILD_ROOT
```

This route deliberately disables optional GumJS/Gum++/tests and does not clone
unbundled optional subprojects. If an unexpected dependency is required,
--wrap-mode=nodownload fails rather than silently fetching an unpinned input.
No native-source devkit rebuild was exercised by vosulay; adapt toolchain/PATH
setup to Frida's bundled releng/env_windows.py as needed. Alternatively, using
upstream releng/deps.py without seeding its cache clones the exact commits in
its deps.toml; inspect that file before permitting network access. Those
unmodified scripts are included for review, not claimed to have been tested.

## (b) Feed a replacement devkit to the UNMODIFIED asdf hook build

In a separate Windows PowerShell terminal at the extracted bundle root,
serve the `http` directory using the same HttpListener mechanism tested by
vosulay (keep this terminal open; stop the listener when finished):

```powershell
$bundle = $PWD.Path
$listener = New-Object System.Net.HttpListener
$listener.Prefixes.Add('http://127.0.0.1:18765/')
$listener.Start()
try {{
    while ($listener.IsListening) {{
        $context = $listener.GetContext()
        $path = $context.Request.Url.AbsolutePath
        Write-Output $path
        if ($path -match '^/17\\.17\\.0/frida-gum-devkit-17\\.17\\.0-windows-(x86_64|x86)\\.tar\\.xz$') {{
            $bytes = [IO.File]::ReadAllBytes((Join-Path $bundle ('http' + $path.Replace('/', '\\'))))
            $context.Response.ContentLength64 = $bytes.Length
            $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
        }} else {{ $context.Response.StatusCode = 404 }}
        $context.Response.Close()
    }}
}} finally {{ $listener.Close() }}
```

In a Windows Rust 1.96.1/MSVC developer terminal in `work/asdf-overlay`
(ensure the compiler, SDK `rc.exe` and standalone Rust installation are on
PATH; standalone Cargo ignores upstream's `rust-toolchain.toml`):

```powershell
$env:FRIDA_DOWNLOAD_CDN = 'http://127.0.0.1:18765'
$env:CARGO_TARGET_DIR = Join-Path $PWD 'target-relinked'
cargo build --release --locked -p asdf-overlay-dll --target x86_64-pc-windows-msvc
```

The x64 command above was verified. For the separately unverified x86 route,
provide an x86 devkit and the i686 compiler/target, then run:

```powershell
cargo build --release --locked -p asdf-overlay-dll --target i686-pc-windows-msvc
```

When redirecting native-command output in Windows PowerShell 5.1, temporarily
set `$ErrorActionPreference = 'Continue'` and explicitly check `$LASTEXITCODE`.

frida-build 0.17.2 uses reqwest and requests
`$FRIDA_DOWNLOAD_CDN/17.17.0/frida-gum-devkit-17.17.0-windows-<arch>.tar.xz`.
file:// is unsupported. A fresh CARGO_TARGET_DIR is important because cached
build-script downloads would otherwise bypass the replacement server. No
source modification to asdf's hook or frida-build is needed. Use native Windows:
the attempted Linux cargo-xwin build failed at upstream's resource compilation
before reaching the devkit download (section (c)).

Rename `target-relinked/x86_64-pc-windows-msvc/release/asdf_overlay_dll.dll`
to `asdf_overlay-x64.dll`, and the i686 output to `asdf_overlay-x86.dll`.
With vosulay and the game stopped, substitute these files beside vosulay.exe
in an extracted portable package. Preserve a backup of the original DLLs.
Modified DLLs are not signed by vosulay and will not match original package
receipt hashes. You are permitted to modify/rebuild the library and relink;
no assertion of compatible runtime behavior follows from compilation alone.

## (c) Verification and remaining correspondence gap

The stock-devkit local HTTP override plus Linux cargo-xwin x64 DLL build was
attempted with cargo-xwin 0.23.0 and Rust 1.96.1. It failed (exit 101) before
any HTTP request or DLL output: upstream crates/dll/build.rs invokes winres
0.1.12, which on Linux selects the literal path /bin\\\\x64\\\\rc.exe and returns
ENOENT. winres has no environment override for that compiler path, and the
unmodified upstream build script does not configure one. No resource-build
bypass, upstream patch, or system-path changes were used.

The same route then succeeded natively on Windows using Rust/Cargo 1.96.1
and MSVC BuildTools (COFF/PE Dumper 14.44.35228.0), building unmodified bundled
asdf source with `cargo build --release --locked -p asdf-overlay-dll --target
x86_64-pc-windows-msvc`, a fresh target directory, and a loopback HttpListener
serving the stock 17.17.0 x64 devkit. Cargo exited 0; the listener recorded
`/17.17.0/frida-gum-devkit-17.17.0-windows-x86_64.tar.xz`.
The resulting DLL was 3,734,528 bytes with SHA-256
`a49a3b06e9c1b918bcdc5ea54593e78a6a1d3ea6e809fb32b6244bd24d081706`.
Both it and pinned asdf_overlay-x64.dll exported exactly `DllMain` and
`vkNegotiateLoaderLayerInterfaceVersion`; exported names matched, not bytes.
This verifies replacement-devkit downloading, static relinking and x64 DLL
generation with a stock devkit, not native-source devkit generation or behavior
after modifying GLib. The task-owned VM build directory was removed afterward.

Step (a), the x86 relink route, modified-library behavior and a complete native
source-to-upstream-prebuilt equivalence check remain unverified. This bundle
contains source and rebuild instructions, not precompiled application object
files or an attestation that upstream's release inputs were identical.
'''.encode('utf-8')


def download(pin):
    url = pin['repository'].replace('https://github.com/', 'https://codeload.github.com/') + '/tar.gz/' + pin['commit']
    with urllib.request.urlopen(url, timeout=180) as response:
        data = response.read()
    actual = hashlib.sha256(data).hexdigest()
    if actual != pin['sha256']:
        raise ValueError(f"source archive SHA-256 mismatch for {pin['name']}: expected {pin['sha256']}, got {actual} ({url})")
    return pin['name'] + '-' + pin['commit'] + '.tar.gz', data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    overrides = json.loads((ROOT / 'tools/notices/overrides.json').read_text())
    pins = sorted(overrides['_source_bundle'], key=lambda pin: pin['name'])
    by_name = {pin['name']: pin for pin in pins}
    if len(by_name) != len(pins):
        raise ValueError('duplicate source-bundle component name')
    if by_name['asdf-overlay']['commit'] != overrides['_asdf_source']['commit']:
        raise ValueError('asdf-overlay source bundle pin differs from notices pin')
    for component in overrides['_frida_native']['components']:
        if by_name[component['name']]['commit'] != component['version']:
            raise ValueError('native source bundle revision differs from notices: ' + component['name'])
    if args.output.exists():
        raise FileExistsError('refusing to overwrite ' + str(args.output))
    with ThreadPoolExecutor(max_workers=4) as pool:
        members = dict(pool.map(download, pins))
    members['README.md'] = readme(pins)
    members['SHA256SUMS'] = ''.join(f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in sorted(members.items())).encode()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=args.output.parent, prefix='.third-party-sources-', delete=False) as scratch:
        temporary = Path(scratch.name)
    try:
        with tarfile.open(temporary, 'w', format=tarfile.USTAR_FORMAT) as bundle:
            for name, data in sorted(members.items()):
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mtime = info.uid = info.gid = 0
                info.uname = info.gname = ''
                info.mode = 0o644
                bundle.addfile(info, io.BytesIO(data))
        temporary.replace(args.output)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
