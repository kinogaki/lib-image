#!/usr/bin/env python3
"""Independent zlib oracle for kinogaki-deflate.

`fixtures`: writes zlib streams of the shared test inputs made by Python's
zlib (levels 0 to 9 and raw streams with a 9-bit window) into
crates/deflate/tests/fixtures; the tests decompress them.
`check DIR`: decompresses every `*.zlib` in DIR with Python's zlib and
compares it with the `.raw` beside it (the Rust tests write those with
KINOGAKI_DEFLATE_ORACLE_DIR set)."""
import pathlib
import sys
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[1]


def inputs():
    """The shared inputs; crates/deflate/tests/inputs.rs makes the same bytes."""
    words = [b"mesh", b"polygon", b"edge", b"point", b"Kinogaki", b" ", b"\n", b"vertex"]
    x = 1
    text = bytearray()
    while len(text) < 50_000:
        x = (x * 1103515245 + 12345) % 2**31
        text += words[(x >> 16) % len(words)]
    noise = bytearray()
    x = 7
    for _ in range(70_000):
        x = (x * 1103515245 + 12345) % 2**31
        noise.append((x >> 16) & 0xFF)
    runs = bytearray()
    x = 3
    while len(runs) < 100_000:
        x = (x * 1103515245 + 12345) % 2**31
        runs += bytes([(x >> 16) & 0xFF]) * (1 + (x >> 8) % 600)
    return {"text": bytes(text), "noise": bytes(noise), "runs": bytes(runs)}


def fixtures():
    out = ROOT / "crates/deflate/tests/fixtures"
    out.mkdir(parents=True, exist_ok=True)
    # Incompressible noise is stored at every level: two levels suffice.
    levels = {"text": (0, 1, 6, 9), "noise": (0, 9), "runs": (1, 6, 9)}
    for name, data in inputs().items():
        for level in levels[name]:
            (out / f"{name}-{level}.zlib").write_bytes(zlib.compress(data, level))
        if name != "noise":
            c = zlib.compressobj(9, zlib.DEFLATED, -9, 9, zlib.Z_FILTERED)
            (out / f"{name}-small-window.deflate").write_bytes(c.compress(data) + c.flush())


def check(directory):
    failures = 0
    files = sorted(pathlib.Path(directory).glob("*.zlib"))
    for packed in files:
        raw = packed.with_suffix(".raw").read_bytes()
        if zlib.decompress(packed.read_bytes()) != raw:
            print(f"MISMATCH {packed.name}")
            failures += 1
    print(f"{len(files) - failures}/{len(files)} streams decompress identically with zlib")
    return 1 if failures or not files else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["fixtures"]:
        fixtures()
    elif len(sys.argv) == 3 and sys.argv[1] == "check":
        sys.exit(check(sys.argv[2]))
    else:
        sys.exit(__doc__)
