# Benchmarks

Apple M4 Max, macOS, release builds (thin LTO, one codegen unit). Other
agents' builds were running, so each figure is the **minimum** of many
runs, all variants interleaved. The harnesses live outside the repository
(they link other libraries); their cases are listed so they can be
rebuilt.

## Inflate (zlib streams, Adler-32 verified)

2026-11-07, against the system zlib (1.2.12, `uncompress` into a fresh
buffer a stream). `kinogaki-deflate` runs `zlib_decompress` with the exact
size as its limit, as PNG, OpenEXR and FBX readers know it. Minimum of 35
runs (five rounds of seven), load average 15–30; every output equals
Python's zlib's byte for byte.

| Stream | Packed → raw | zlib | before | now | speed against zlib |
| --- | --- | ---: | ---: | ---: | ---: |
| FBX normals: the largest array of a 1M-triangle binary FBX exported by Houdini, one stream | 29.1 → 72.0 MB | 72.6 ms | 115.6 ms | 73.1 ms | 0.99× |
| the same array recompressed at zlib level 6 | 27.1 → 72.0 MB | 55.6 ms | 98.8 ms | 59.9 ms | 0.93× |
| PNG: a Studio capture, 2400×1800 RGBA, its IDAT as written | 0.41 → 17.3 MB | 3.65 ms | 6.06 ms | 2.92 ms | 1.25× |
| the same pixels at zlib level 6 | 0.40 → 17.3 MB | 3.41 ms | 5.49 ms | 2.94 ms | 1.16× |
| EXR-like ZIP: 3840×2160 half RGBA in 135 blocks of 16 scanlines, bytes split and delta-coded as OpenEXR does, zlib level 6 | 25.4 → 66.4 MB | 59.9 ms | 92.1 ms | 57.4 ms | 1.04× |
| Text-like: 20 MB of English words, zlib level 6 | 6.8 → 20.0 MB | 15.4 ms | 27.2 ms | 16.0 ms | 0.96× |

Without LTO, as Studio builds its plugins, the figures are the same within
the noise (FBX 76.2 ms, PNG 2.90 ms, EXR 58.1 ms).

What the profile showed and what changed:

- **Adler-32** ran at 4.3 GB/s: its lane sums were checked for overflow
  (release builds keep overflow checks), which kept them from vectorizing.
  Summed wrapping (a block can't overflow them), it runs at 22 GB/s; it was
  more than half of a PNG's time.
- **Entries take their extra bits.** An entry's bit count includes a
  length's or a distance's extra bits and its code's own length says where
  they start, so one shift consumes the symbol and its extra bits; the
  table's first level is a fixed-size array, so lookups carry no bounds
  check.
- **The next symbol is looked up before the refill** when the bits left
  hold its index (after literals, and after most matches), taking the
  refill off the chain from one symbol to the next. The decoder's time is
  that chain and the mispredicted branch between a literal and a match.
- **Dynamic tables are built in place** (no allocation a block), the first
  level doubled a code length at a time from the canonical order, the
  longer codes' second-level tables from consecutive runs of that order.
  A level-6 stream has a dynamic block every 16 000 symbols or so: 1 470 in
  the FBX stream, 4.5 µs each before, 2.7 µs now.
- **Literal pairs are gone**: merging two short literal codes into one
  entry cost 1.2 µs a table and made no stream measurably faster.
- **Copies** go in 16-byte chunks when the source is at least that far
  behind, 8-byte ones from 8, and as a 16-byte pattern for shorter periods
  (no byte loop, no call for a run of one byte); a long match apart from
  what it reads is one move.

## PNG, 8-bit RGBA

Against `png` 0.18.1 at its defaults (flate2/miniz_oxide level 6, adaptive
filter; decoding through fdeflate with the Adler-32 check skipped), the
crate Studio used before. Measured before the inflate round above, which
made inflating a capture's IDAT twice as fast.

| Image | Operation | png 0.18.1 | kinogaki-png |
| --- | --- | --- | --- |
| Studio capture 2800×1800 (20.2 MB) | encode Balanced | 73.8 ms, 553 852 B | 72.2 ms, 561 510 B |
| | encode Fast | — | 42.1 ms, 619 034 B |
| | decode (file written by png) | 14.2 ms | 15.6 ms |
| | decode (file written by png, re-encoded by png) | 13.7 ms | 13.2 ms |
| Studio capture 1200×700 (3.4 MB) | encode Balanced | 25.4 ms, 214 954 B | 22.2 ms, 216 592 B |
| | encode Fast | — | 9.3 ms, 242 268 B |
| | decode | 3.1 ms | 3.0 ms |

Balanced uses zlib level 6's search parameters (chains of 128, lazy
matching below 16) and lands within 1.5% of zlib's size. Decoding is the
png crate's speed or within 10% of it, depending on the filter mix (Paeth
rows dominate the remaining time).
