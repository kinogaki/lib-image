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
| `checksum.rs` | CRC-32 (slicing by eight), Adler-32 (sixteen lanes per block, summed without overflow checks so they vectorize) |
| `tables.rs` | RFC 1951 constants and the length/distance symbol tables |
| `inflate/mod.rs` | decoder: blocks, an output kept a match and a chunk longer than written, up to three literals per refill, the next symbol looked up before the refill when the bits left hold its index; matches copied in 16- or 8-byte chunks, periods under eight bytes as 16-byte patterns |
| `inflate/bits.rs` | the bit reader: a 64-bit buffer refilled a word at a time, zeros counted past the end so a truncated stream fails |
| `inflate/table.rs` | two-level Huffman tables (11-bit first level for literals/lengths, 10 for distances) whose entries carry a literal, or a base, and the bits they take with their extra bits, so one shift consumes a length or a distance whole; a dynamic block's built in place, the first level doubled a code length at a time; the fixed block's |
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
  window, many small dynamic blocks) decompress; round trips at every level
  over edge inputs (empty, 258/259-byte runs, a match at exactly 32 768
  bytes, incompressible data) and seeded random data up to 400 000 bytes
  (exact limits, an unknown size, appended to a caller's bytes); hand-written
  streams with a match of every distance 1–40 and others up to 32 768, of
  every length 3–40 and around each chunk size, last in the stream (at
  exactly its limit, and one byte short) or followed by literals; stored
  blocks between fixed blocks, matches reaching across them; output within
  10% of zlib's size at levels 1/6/9; every truncation and 20 000 bit flips
  fail cleanly.
- `inflate/table.rs`'s unit tests: every 15-bit input decodes, through both table
  levels, to the canonical code it starts with, for the fixed codes, a lone
  one-bit code and random complete codes up to 15 bits.
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
