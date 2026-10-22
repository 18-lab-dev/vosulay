"""Standard-library manifest artwork validation (kept identical across release repos)."""
import math
from pathlib import Path, PurePosixPath
import re
import struct
import xml.etree.ElementTree as ET

MAX_ART_BYTES = 1024 * 1024
ART_EXTENSIONS = ('.svg', '.png', '.jpg', '.jpeg', '.webp')


def dimensions(data, extension):
    """Read intrinsic image dimensions, rejecting mismatched types and bad headers."""
    try:
        if extension == '.svg':
            if b'<!DOCTYPE' in data.upper() or b'<!ENTITY' in data.upper():
                raise ValueError('SVG entities and doctypes are not supported')
            root = ET.fromstring(data)
            if root.tag != '{http://www.w3.org/2000/svg}svg':
                raise ValueError('expected an SVG root')
            def length(value):
                if not re.fullmatch(r'(?:\d+(?:\.\d*)?|\.\d+)(?:px)?', value or ''):
                    return None
                return float(value.removesuffix('px'))
            width, height = length(root.get('width')), length(root.get('height'))
            if width is not None and height is not None:
                return width, height
            box = [float(v) for v in re.split(r'[\s,]+', root.get('viewBox', '').strip())]
            if len(box) != 4:
                raise ValueError('SVG needs intrinsic dimensions or a viewBox')
            return box[2], box[3]
        if extension == '.png':
            if data[:8] != b'\x89PNG\r\n\x1a\n' or data[8:16] != b'\x00\x00\x00\rIHDR' or len(data) < 33:
                raise ValueError('invalid PNG header')
            return struct.unpack('>II', data[16:24])
        if extension in ('.jpg', '.jpeg'):
            if data[:2] != b'\xff\xd8':
                raise ValueError('invalid JPEG header')
            offset = 2
            while offset < len(data):
                if data[offset] != 255:
                    raise ValueError('invalid JPEG marker')
                while offset < len(data) and data[offset] == 255:
                    offset += 1
                marker = data[offset]
                offset += 1
                if marker in (0xd9, 0xda):
                    break
                if marker == 0x01 or 0xd0 <= marker <= 0xd7:
                    continue
                size = int.from_bytes(data[offset:offset + 2], 'big')
                if size < 2 or offset + size > len(data):
                    raise ValueError('truncated JPEG segment')
                if marker in (0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf):
                    if size < 8:
                        raise ValueError('invalid JPEG frame')
                    height, width = struct.unpack('>HH', data[offset + 3:offset + 7])
                    return width, height
                offset += size
            raise ValueError('JPEG has no image frame')
        if extension == '.webp':
            if data[:4] != b'RIFF' or data[8:12] != b'WEBP' or int.from_bytes(data[4:8], 'little') + 8 != len(data):
                raise ValueError('invalid WebP container')
            offset = 12
            while offset + 8 <= len(data):
                kind, size = data[offset:offset + 4], int.from_bytes(data[offset + 4:offset + 8], 'little')
                payload = data[offset + 8:offset + 8 + size]
                if len(payload) != size:
                    raise ValueError('truncated WebP chunk')
                if kind == b'VP8X' and size == 10:
                    return 1 + int.from_bytes(payload[4:7], 'little'), 1 + int.from_bytes(payload[7:10], 'little')
                if kind == b'VP8L' and size >= 5 and payload[0] == 0x2f:
                    bits = int.from_bytes(payload[1:5], 'little')
                    return (bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1
                if kind == b'VP8 ' and size >= 10 and payload[3:6] == b'\x9d\x01\x2a':
                    width, height = struct.unpack('<HH', payload[6:10])
                    return width & 0x3fff, height & 0x3fff
                offset += 8 + size + (size & 1)
            raise ValueError('WebP has no image dimensions')
    except (ET.ParseError, UnicodeError, IndexError, struct.error) as error:
        raise ValueError('invalid artwork image') from error
    raise ValueError(f'unsupported artwork type: {extension}')


def artwork(directory, metadata, required=False, fallback=False):
    """Return {role: (relative path, bytes)} with confined, validated inputs."""
    directory = Path(directory)
    result = {}
    for role in ('icon', 'banner'):
        value = metadata.get(role)
        if role not in metadata and fallback:
            candidates = sorted(p.name for p in directory.glob(role + '.*') if p.suffix.lower() in ART_EXTENSIONS)
            if len(candidates) > 1:
                raise ValueError(f'ambiguous {role} artwork; declare it in metadata')
            value = candidates[0] if candidates else None
        if value is None and role not in metadata and not required:
            continue
        if not isinstance(value, str) or not value:
            raise ValueError(f'{role} must name an artwork file')
        path = PurePosixPath(value)
        if path.is_absolute() or any(p in ('', '.', '..') for p in value.split('/')) or any(c in value for c in ('\\', ':', '\0', '?', '#')):
            raise ValueError(f'unsafe {role} path: {value}')
        source = directory
        for part in path.parts:
            source = source / part
            if source.is_symlink():
                raise ValueError(f'{role} must not traverse symlinks')
        if not source.is_file() or not source.resolve().is_relative_to(directory.resolve()):
            raise ValueError(f'{role} file is missing or outside its directory')
        if not 0 < source.stat().st_size <= MAX_ART_BYTES:
            raise ValueError(f'{role} exceeds 1 MiB or is empty')
        extension = source.suffix.lower()
        if extension not in (('.svg', '.png') if role == 'icon' else ART_EXTENSIONS):
            raise ValueError(f'unsupported {role} image type: {extension}')
        data = source.read_bytes()
        width, height = dimensions(data, extension)
        if not all(math.isfinite(v) and v > 0 for v in (width, height)):
            raise ValueError(f'{role} dimensions must be positive and finite')
        if role == 'icon' and (width != height or width < 256):
            raise ValueError('icon must be square and at least 256 by 256')
        if role == 'banner' and not 2.94 <= width / height <= 3.06:
            raise ValueError('banner must have a 3:1 aspect ratio (within 2%)')
        result[role] = (value, data)
    return result


def art_urls(art, identity, kind, base_url):
    return {role + '_url': base_url.rstrip('/') + f'/vosu-{kind}-{identity}-{role}' + Path(path).suffix.lower()
            for role, (path, _) in art.items()}


def publish_art(art, identity, kind, output):
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    for role, (path, data) in art.items():
        (output / (f'vosu-{kind}-{identity}-{role}' + Path(path).suffix.lower())).write_bytes(data)
