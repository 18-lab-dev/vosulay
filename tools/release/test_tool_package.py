"""Exercise both portable targets and the published artwork catalog contract."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('tool_package', Path(__file__).with_name('tool-package.py'))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class ToolPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.runtime = self.root / 'runtime'
        self.runtime.mkdir()
        (self.runtime / 'vosulay').write_bytes(b'fixture executable')
        (self.runtime / 'vosulay.exe').write_bytes(b'fixture executable')
        self.out = self.root / 'out'

    def test_packages_include_manifest_art_and_catalog_publishes_assets(self):
        with contextlib.redirect_stdout(io.StringIO()):
            for target in package.TARGETS:
                package.package(self.runtime, self.out, target)
        manifest = package.tomllib.loads((package.ROOT / 'tool.toml').read_text())
        for archive in self.out.glob('*.zip'):
            with zipfile.ZipFile(archive) as bundle:
                for role in ('icon', 'banner'):
                    self.assertEqual(bundle.read(manifest[role]), (package.ROOT / manifest[role]).read_bytes())
        package.catalog(self.out, 'example/vosulay', 'v' + manifest['version'])
        entry = json.loads((self.out / 'catalog.json').read_text())['tools'][0]
        for role in ('icon', 'banner'):
            asset = f'vosu-tool-vosulay-{role}.svg'
            self.assertEqual(entry[role + '_url'], f'https://github.com/example/vosulay/releases/download/v{manifest["version"]}/{asset}')
            self.assertEqual((self.out / asset).read_bytes(), (package.ROOT / manifest[role]).read_bytes())

    def test_invalid_manifest_art_fails_before_zip(self):
        with patch.object(package, 'artwork', side_effect=ValueError('missing banner')):
            with self.assertRaises(ValueError):
                package.package(self.runtime, self.out, package.TARGETS[0])
        self.assertFalse(self.out.exists())


if __name__ == '__main__':
    unittest.main()
