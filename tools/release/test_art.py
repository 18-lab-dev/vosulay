"""Artwork contract tests shared by the release packaging repositories."""
from pathlib import Path
import struct
import tempfile
import unittest
import zlib

from package_art import artwork, dimensions, MAX_ART_BYTES


def svg(width, height):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"/>'.encode()


def png(width, height):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress((b'\0' + b'\0' * (width * 4)) * height)) + chunk(b'IEND', b''))


class ArtworkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.values = {'icon': 'icon.svg', 'banner': 'banner.svg'}
        (self.root / 'icon.svg').write_bytes(svg(256, 256))
        (self.root / 'banner.svg').write_bytes(svg(1200, 400))

    def test_svg_and_png_and_viewbox(self):
        self.assertEqual(set(artwork(self.root, self.values, required=True)), {'icon', 'banner'})
        self.assertEqual(dimensions(png(256, 256), '.png'), (256, 256))
        self.assertEqual(dimensions(b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 400"/>', '.svg'), (1200, 400))
        (self.root / 'icon.png').write_bytes(png(256, 256))
        self.assertIn('icon', artwork(self.root, {'icon': 'icon.png'}))

    def test_jpeg_and_all_webp_headers(self):
        frame = b'\xff\xd8\xff\xc0\x00\x0b\x08' + struct.pack('>HH', 400, 1200) + b'\x01\x01\x11\x00\xff\xd9'
        self.assertEqual(dimensions(frame, '.jpg'), (1200, 400))
        chunks = [(b'VP8X', b'\0' * 4 + (1199).to_bytes(3, 'little') + (399).to_bytes(3, 'little')),
                  (b'VP8L', b'\x2f' + (1199 | (399 << 14)).to_bytes(4, 'little')),
                  (b'VP8 ', b'\0' * 3 + b'\x9d\x01\x2a' + struct.pack('<HH', 1200, 400))]
        for kind, payload in chunks:
            chunk = kind + len(payload).to_bytes(4, 'little') + payload + b'\0' * (len(payload) % 2)
            data = b'RIFF' + (len(chunk) + 4).to_bytes(4, 'little') + b'WEBP' + chunk
            self.assertEqual(dimensions(data, '.webp'), (1200, 400))

    def test_rejects_missing_unsafe_or_wrong_type(self):
        for value in [None, '', 1, [], '../outside.svg', '/absolute.svg', 'a/../icon.svg', 'a\\icon.svg', 'missing.svg']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                artwork(self.root, {'icon': value})
        for name, data in [('fake.svg', b'not svg'), ('fake.png', svg(256, 256)), ('fake.gif', b'GIF89a')]:
            (self.root / name).write_bytes(data)
            with self.subTest(name=name), self.assertRaises(ValueError):
                artwork(self.root, {'icon': name})
        with self.assertRaises(ValueError):
            artwork(self.root, {}, required=True)
        self.assertEqual(artwork(self.root, {}), {})

    def test_size_and_geometry(self):
        for role, width, height in [('icon', 255, 255), ('icon', 256, 257), ('banner', 1200, 3990), ('banner', 0, 0), ('banner', 'nan', 400)]:
            (self.root / (role + '.svg')).write_bytes(svg(width, height))
            with self.subTest(role=role, width=width, height=height), self.assertRaises(ValueError):
                artwork(self.root, {role: role + '.svg'})
        for width in (1176, 1224):
            (self.root / 'banner.svg').write_bytes(svg(width, 400))
            self.assertIn('banner', artwork(self.root, {'banner': 'banner.svg'}))
        valid = svg(256, 256)
        (self.root / 'icon.svg').write_bytes(valid + b' ' * (MAX_ART_BYTES - len(valid)))
        self.assertIn('icon', artwork(self.root, {'icon': 'icon.svg'}))
        with (self.root / 'icon.svg').open('ab') as output:
            output.write(b' ')
        with self.assertRaises(ValueError):
            artwork(self.root, {'icon': 'icon.svg'})

    def test_symlinks_and_ambiguous_fallback(self):
        (self.root / 'icon.png').write_bytes(png(256, 256))
        with self.assertRaises(ValueError):
            artwork(self.root, {}, fallback=True)
        self.assertIn('icon', artwork(self.root, self.values, fallback=True))
        try:
            (self.root / 'linked.svg').symlink_to(self.root / 'icon.svg')
            (self.root / 'linked-dir').symlink_to(self.root, target_is_directory=True)
        except OSError:
            self.skipTest('symlinks unavailable')
        for value in ('linked.svg', 'linked-dir/icon.svg'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                artwork(self.root, {'icon': value})


if __name__ == '__main__':
    unittest.main()
