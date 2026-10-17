#!/usr/bin/env python3
"""JPEG oracle for kinogaki-jpeg, run locally (Pillow, i.e. libjpeg-turbo;
never a dependency of the library).

  jpeg_oracle.py references          write crates/jpeg/tests/fixtures/reference/:
                                     libjpeg-turbo's decode of every fixture as
                                     a PNG (grey or RGB; CMYK and YCCK through
                                     (255 - C)(255 - K) / 255), plus 1/2, 1/4
                                     and 1/8 scaled decodes (`<name>.s<N>.png`)
  jpeg_oracle.py check DIR           decode each DIR/*.jpg that the encoder
                                     tests wrote (KINOGAKI_JPEG_ORACLE_DIR) and
                                     compare it with its DIR/*.png source: PSNR
  jpeg_oracle.py compare A.jpg OUT   decode A.jpg as Pillow does into OUT (PNG)

The fixtures themselves came from luce-jpeg's tests/make_fixtures.py (Pillow,
cjpeg and TurboJPEG over synthetic pictures); see THIRD_PARTY_NOTICES.md for
the two Ladybird ICC files.
"""
import math
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / 'crates/jpeg/tests/fixtures'
REFERENCE = FIXTURES / 'reference'


def to_reference(image):
    """Grey stays grey; CMYK goes to RGB as kinogaki-jpeg converts it."""
    if image.mode == 'L':
        return image
    if image.mode == 'CMYK':
        c, m, y, k = image.split()
        def channel(a):
            return Image.frombytes('L', image.size, bytes(
                min(255, int((255 - p) * (255 - q) / 255 + 0.5)) for p, q in zip(a.tobytes(), k.tobytes())))
        return Image.merge('RGB', (channel(c), channel(m), channel(y)))
    return image.convert('RGB')


def references():
    REFERENCE.mkdir(exist_ok=True)
    count = 0
    for path in sorted(FIXTURES.glob('*.jpg')):
        with Image.open(path) as image:
            image.load()
            to_reference(image).save(REFERENCE / f'{path.stem}.png')
        count += 1
        if path.stem.startswith('mt_') or path.stem.endswith(('_1x1', '_7x9')):
            continue
        for scale in (2, 4, 8):
            with Image.open(path) as image:
                wanted = (math.ceil(image.width / scale), math.ceil(image.height / scale))
                # Pillow picks the largest scale whose floor size covers the request.
                image.draft(image.mode, (max(1, image.width // scale), max(1, image.height // scale)))
                image.load()
                if image.size != wanted:
                    continue
                to_reference(image).save(REFERENCE / f'{path.stem}.s{scale}.png')
            count += 1
    print(f'{count} references')


def psnr(a, b):
    pa, pb = a.tobytes(), b.tobytes()
    error = sum((x - y) ** 2 for x, y in zip(pa, pb)) / len(pa)
    return float('inf') if error == 0 else 10 * math.log10(255 * 255 / error)


def check(directory):
    worst = float('inf')
    count = 0
    for path in sorted(Path(directory).glob('*.jpg')):
        with Image.open(path) as image:
            image.load()
            decoded = to_reference(image)
        with Image.open(path.with_suffix('.png')) as source:
            source = source.convert(decoded.mode)
        value = psnr(decoded, source)
        print(f'{path.name}: {decoded.mode} {decoded.size[0]}x{decoded.size[1]} PSNR {value:.2f} dB')
        worst = min(worst, value)
        count += 1
    print(f'{count} files decoded by libjpeg-turbo; worst PSNR {worst:.2f} dB')


if __name__ == '__main__':
    if sys.argv[1:2] == ['references']:
        references()
    elif sys.argv[1:2] == ['check'] and len(sys.argv) == 3:
        check(sys.argv[2])
    elif sys.argv[1:2] == ['compare'] and len(sys.argv) == 4:
        with Image.open(sys.argv[2]) as image:
            image.load()
            to_reference(image).save(sys.argv[3])
    else:
        raise SystemExit(__doc__)
