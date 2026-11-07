# lib-image architecture

One Cargo workspace. Owned code forbids `unsafe` (workspace lint) and the
crates depend on nothing beyond std and each other (`kinogaki-png` →
`kinogaki-deflate`, `kinogaki-image`). JPEG and OpenEXR are Kinogaki
Studio's format plugins, each with its codec, over `kinogaki-image` (and
`kinogaki-deflate` for OpenEXR's zlib).

## kinogaki-image (crates/image)

| File | Responsibility |
| --- | --- |
| `lib.rs` | `Image`, `Samples` (U8, U16, U32, F16 bits, F32), `Image::new` (checked), `to_rgba8` |
| `half.rs` | binary16 ↔ binary32, exact: every half round-trips, floats round to nearest even (tested over all 65 536 halves and every halfway point) |
| `srgb.rs` | linear float → sRGB byte by binary search over the 255 code boundaries: exact rounding without a power per sample |

## kinogaki-deflate (crates/deflate)

| File | Responsibility |
| --- | --- |
| `lib.rs` | public API: `deflate`/`inflate` (raw), `zlib_compress`/`zlib_decompress`, `Level`, `Error` |
| `checksum.rs` | CRC-32 (slicing by eight), Adler-32 (sixteen lanes per block) |
| `tables.rs` | RFC 1951 constants and the length/distance symbol tables |
| `inflate.rs` | decoder: 64-bit bit buffer refilled a word at a time; two-level Huffman tables (11-bit first level for literals/lengths, 10 for distances) whose entries carry one or two literals or a base with its extra-bit count; up to four literals per refill; eight-byte match copies into an output kept a match longer than written |
| `huffman.rs` | encoder code lengths: two-queue Huffman construction, length limiting by Kraft rebalancing, canonical codes |
| `deflate.rs` | encoder: hash chains over 4-byte prefixes in a 32 KiB window, lazy matching, blocks of 32 768 symbols written as the cheapest of dynamic, fixed or stored |

Levels: `Fast` (chain 4, no lazy matching), `Balanced` (chain 128, lazy
below 16, nice 128: zlib level 6's parameters), `Small` (chain 2048, full
lazy matching).

Decoding is bounded: every call takes an output limit, and a stream that
reads past its end fails as `Truncated` rather than decoding zero bits.

## kinogaki-png (crates/png)

| File | Responsibility |
| --- | --- |
| `lib.rs` | `Image`, `Samples`, `Error`, `Image::to_rgba8` |
| `encode.rs` | `encode`: one IDAT, adaptive per-row filter (libpng's minimum sum of magnitudes) |
| `decode.rs` | `decode`: chunk walk with CRC checks, IHDR/PLTE/tRNS/IDAT/IEND, Adam7, sample unpacking; 8-bit RGBA unfilters in the decompressed buffer itself |
| `filter.rs` | the five filters, Paeth in its threshold form for decoding |

## Tests and oracles

- `crates/deflate/tests/streams.rs`: zlib-made streams (levels 0–9, a 512-byte
  window) decompress; round trips at every level over edge inputs (empty,
  258/259-byte runs, a match at exactly 32 768 bytes, incompressible data);
  output within 10% of zlib's size at levels 1/6/9; every truncation and
  20 000 bit flips fail cleanly.
- `tools/zlib_oracle.py check DIR` decompresses this library's zlib output
  with Python's zlib (`KINOGAKI_DEFLATE_ORACLE_DIR=DIR cargo test -p
  kinogaki-deflate --test streams`): 42/42 identical.
- `crates/png/tests/images.rs`: every color type × bit depth × interlace
  method (30 files from the independent Python encoder in
  `tools/png_oracle.py`, filters cycling through all five types, IDAT split
  over chunks, tRNS where allowed) decodes to the RGBA the specification
  gives; 5 Pillow-written files decode as Pillow reads them; round trips;
  truncations and corruption fail cleanly.
- `tools/png_oracle.py check DIR` reads this library's PNGs with Pillow
  (`KINOGAKI_PNG_ORACLE_DIR=DIR cargo test -p kinogaki-png --test images`).
