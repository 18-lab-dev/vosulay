#!/usr/bin/env python3
"""Wrap the already verified/signed portable runtime; never rebuild its binaries."""
import argparse
import hashlib
import json
from pathlib import Path
import zipfile
try:
    import tomllib
except ModuleNotFoundError:  # Ubuntu 22.04's Python 3.10
    import tomli as tomllib
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[2]
TARGETS = ('x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc')


def package(runtime, output, target):
    manifest = tomllib.loads((ROOT / 'tool.toml').read_text())
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    if manifest['version'] != version:
        raise ValueError('tool.toml version must match Cargo.toml')
    files = [(ROOT / 'tool.toml', 'tool.toml')]
    files += [(p, 'assets/tool/' + p.relative_to(ROOT / 'assets/tool').as_posix()) for p in (ROOT / 'assets/tool').rglob('*') if p.is_file()]
    files += [(p, 'bin/' + p.relative_to(runtime).as_posix()) for p in runtime.rglob('*') if p.is_file()]
    files += [(ROOT / name, 'licenses/' + name) for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md')]
    if not (runtime / ('vosulay.exe' if target == TARGETS[1] else 'vosulay')).is_file():
        raise ValueError('runtime is missing vosulay executable')
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f'vosu-tool-vosulay-{version}-{target}.zip'
    with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
        for source, name in sorted(files, key=lambda item: item[1]):
            # Dereference packaged runtime links: vosu accepts regular files only.
            info = zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0))
            info.external_attr = (0o100755 if name.startswith('bin/') else 0o100644) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            bundle.writestr(info, source.read_bytes())
    print(json.dumps({'archive': str(archive), 'entries': len(files), 'expanded_bytes': sum(p.stat().st_size for p, _ in files)}))


def catalog(directory, repository, tag):
    manifest = tomllib.loads((ROOT / 'tool.toml').read_text())
    packages = {}
    for target in TARGETS:
        path = directory / f'vosu-tool-vosulay-{manifest["version"]}-{target}.zip'
        packages[target] = {'url': f'https://github.com/{repository}/releases/download/{tag}/{path.name}', 'sha256': hashlib.file_digest(path.open('rb'), 'sha256').hexdigest(), 'size': path.stat().st_size}
    entry = {key: manifest[key] for key in ('id', 'name', 'version', 'description', 'author', 'homepage')}
    entry.update(protocol=1, interfaces=['ui', 'service'], requires=[], packages=packages)
    (directory / 'catalog.json').write_text(json.dumps({'catalog_version': 1, 'name': 'vosulay', 'published': datetime.now(timezone.utc).isoformat(), 'tools': [entry]}, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    pack = sub.add_parser('package')
    pack.add_argument('--runtime', type=Path, required=True)
    pack.add_argument('--output', type=Path, required=True)
    pack.add_argument('--target', choices=TARGETS, required=True)
    cat = sub.add_parser('catalog')
    cat.add_argument('--directory', type=Path, required=True)
    cat.add_argument('--repository', default='shiruviaa/vosulay')
    cat.add_argument('--tag', required=True)
    args = parser.parse_args()
    if args.command == 'package':
        package(args.runtime, args.output, args.target)
    else:
        catalog(args.directory, args.repository, args.tag)
