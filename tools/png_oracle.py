#!/usr/bin/env python3
"""Independent PNG oracle for kinogaki-png.

`fixtures`: writes crates/png/tests/fixtures: for every color type, bit
depth and interlace method, a PNG written by the small encoder below
(Python's zlib, every filter type in turn, tRNS where the type allows it)
with the RGBA8 the PNG specification says it decodes to (`.rgba`); and a
few PNGs written by Pillow, with Pillow's own RGBA reading.
`check DIR`: reads every `*.png` in DIR with Pillow and compares it with
the `.rgba` beside it (the Rust tests write those with
KINOGAKI_PNG_ORACLE_DIR set)."""
import io
import pathlib
import struct
import sys
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "crates/png/tests/fixtures"
W, H = 13, 11

# (color type, samples per pixel, allowed depths)
TYPES = [(0, 1, (1, 2, 4, 8, 16)), (2, 3, (8, 16)), (3, 1, (1, 2, 4, 8)), (4, 2, (8, 16)), (6, 4, (8, 16))]


def value(x, y, k, depth):
    """A deterministic sample covering the whole range."""
    return ((x * 7 + y * 13 + k * 29) * 2654435761 >> 7) % (1 << depth)


def to8(v, depth):
    if depth == 16:
        return v >> 8
    if depth == 8:
        return v
    return v * 255 // ((1 << depth) - 1)


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def filter_row(kind, row, prev, bpp):
    out = bytearray(len(row))
    for i in range(len(row)):
        a = row[i - bpp] if i >= bpp else 0
        b = prev[i]
        c = prev[i - bpp] if i >= bpp else 0
        pred = [0, a, b, (a + b) // 2, paeth(a, b, c)][kind]
        out[i] = (row[i] - pred) & 0xFF
    return bytes(out)


def pack(samples, depth):
    """One row of samples at `depth` bits, big-endian, padded to a byte."""
    if depth == 16:
        return b"".join(struct.pack(">H", s) for s in samples)
    if depth == 8:
        return bytes(samples)
    out, acc, bits = bytearray(), 0, 0
    for s in samples:
        acc = (acc << depth) | s
        bits += depth
        if bits == 8:
            out.append(acc)
            acc, bits = 0, 0
    if bits:
        out.append(acc << (8 - bits))
    return bytes(out)


def make(color, n, depth, interlace):
    palette = [((i * 40) % 256, (i * 90) % 256, (i * 200) % 256) for i in range(1 << min(depth, 8))]
    trns = b""
    transparent = None
    if color == 0:
        transparent = value(3, 4, 0, depth)
        trns = struct.pack(">H", transparent)
    elif color == 2:
        transparent = tuple(value(3, 4, k, depth) for k in range(3))
        trns = struct.pack(">HHH", *transparent)
    elif color == 3:
        alphas = bytes((i * 77) % 256 for i in range(min(len(palette), 5)))
        trns = alphas
    expected = bytearray()
    for y in range(H):
        for x in range(W):
            s = [value(x, y, k, depth) for k in range(n)]
            if color == 3:
                r, g, b = palette[s[0]]
                a = trns[s[0]] if s[0] < len(trns) else 255
                expected += bytes([r, g, b, a])
            elif color == 0:
                g = to8(s[0], depth)
                expected += bytes([g, g, g, 0 if s[0] == transparent else 255])
            elif color == 2:
                expected += bytes([to8(v, depth) for v in s]) + bytes([0 if tuple(s) == transparent else 255])
            elif color == 4:
                g = to8(s[0], depth)
                expected += bytes([g, g, g, to8(s[1], depth)])
            else:
                expected += bytes(to8(v, depth) for v in s)
    passes = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)] if interlace else [(0, 0, 1, 1)]
    bpp = max(1, n * depth // 8)
    raw = bytearray()
    kind = 0
    for x0, y0, dx, dy in passes:
        xs, ys = range(x0, W, dx), range(y0, H, dy)
        if not xs or not ys:
            continue
        prev = bytes(len(pack([0] * len(xs) * n, depth)))
        for y in ys:
            row = pack([value(x, y, k, depth) for x in xs for k in range(n)], depth)
            raw += bytes([kind]) + filter_row(kind, row, prev, bpp)
            prev = row
            kind = (kind + 1) % 5
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, depth, color, 0, 0, interlace))
    png += chunk(b"tEXt", b"Comment\x00ancillary chunks are skipped")
    if color == 3:
        png += chunk(b"PLTE", b"".join(bytes(p) for p in palette))
    if trns:
        png += chunk(b"tRNS", trns)
    data = zlib.compress(bytes(raw), 9)
    # Image data split over several IDAT chunks.
    for i in range(0, len(data), 50):
        png += chunk(b"IDAT", data[i:i + 50])
    png += chunk(b"IEND", b"")
    return png, bytes(expected)


def fixtures():
    from PIL import Image

    OUT.mkdir(parents=True, exist_ok=True)
    count = 0
    for color, n, depths in TYPES:
        for depth in depths:
            for interlace in (0, 1):
                png, rgba = make(color, n, depth, interlace)
                stem = OUT / f"c{color}-d{depth}-i{interlace}"
                stem.with_suffix(".png").write_bytes(png)
                stem.with_suffix(".rgba").write_bytes(rgba)
                count += 1
    # Pillow's own writer, read back by Pillow.
    gradient = Image.new("RGBA", (64, 48))
    gradient.putdata([(x * 4, y * 5, (x * y) % 256, 255 - x) for y in range(48) for x in range(64)])
    for mode in ("RGBA", "RGB", "L", "LA", "P"):
        image = gradient.convert(mode)
        buffer = io.BytesIO()
        image.save(buffer, format="PNG", optimize=True)
        stem = OUT / f"pillow-{mode}"
        stem.with_suffix(".png").write_bytes(buffer.getvalue())
        stem.with_suffix(".rgba").write_bytes(Image.open(io.BytesIO(buffer.getvalue())).convert("RGBA").tobytes())
        count += 1
    print(f"{count} fixtures")


def check(directory):
    from PIL import Image

    files = sorted(pathlib.Path(directory).glob("*.png"))
    failures = 0
    for path in files:
        image = Image.open(path)
        image.load()
        if image.convert("RGBA").tobytes() != path.with_suffix(".rgba").read_bytes():
            print(f"MISMATCH {path.name}")
            failures += 1
    print(f"{len(files) - failures}/{len(files)} PNGs read identically with Pillow")
    return 1 if failures or not files else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["fixtures"]:
        fixtures()
    elif len(sys.argv) == 3 and sys.argv[1] == "check":
        sys.exit(check(sys.argv[2]))
    else:
        sys.exit(__doc__)
