# lib-image architecture

One Cargo workspace, one crate per format. Owned code forbids `unsafe`
(workspace lint) and the crates depend on nothing beyond std and each other
(`kinogaki-png` → `kinogaki-deflate`, `kinogaki-image`; `kinogaki-jpeg` →
`kinogaki-image`; `kinogaki-exr` → `kinogaki-deflate`, `kinogaki-image`).

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

## kinogaki-jpeg (crates/jpeg)

| File | Responsibility |
| --- | --- |
| `lib.rs` | `info`, `decode`, `decode_rgba8`, `DecodeOptions`, `Info`; re-exports |
| `error.rs` | `Error`: `Corrupt { what, at }` (byte offset), `Unsupported`, `Invalid`, `TooLarge` |
| `headers.rs` | byte reader, markers, frame header, DQT, DHT, scan headers with progressive order checks |
| `huffman.rs` | decoding tables: 11-bit lookahead; a second table giving run, value and bits used for code plus magnitude together (end of block and zero runs too); the bit reader (64-bit, eight bytes a refill while none is 0xFF, the byte path out of line and by value so the state stays in registers) |
| `coefficients.rs` | coefficient layouts: MCU order for one-scan baseline frames (any run of MCUs is one slice a thread owns), planes per component otherwise, with each block's nonzero mask for progressive frames |
| `scan.rs` | entropy decoding: baseline blocks, DC first/refine, AC first/refine (refinement walks the nonzero mask, never every position), restart markers |
| `idct.rs` | exact float IDCT (even/odd halves over eight lanes); reduced 1/2–1/8 IDCTs as means of the full-size points |
| `color.rs` | bilinear upsampling at pixel centres (libjpeg's triangle filter at 2:1), JFIF/Adobe color conversion |
| `render.rs` | the plan (IDCT size, planes, sampling ratios per component) and the band scheduler: IDCT and color tasks on every processor, coefficient bands arriving while workers run |
| `decode.rs` | the marker walk and the three routes: restart intervals in parallel; one-scan baseline rendered behind its entropy decoding; whole-plane scans then a parallel render |
| `icc.rs` | APP2 ICC profile joining with libjpeg-turbo's rules |
| `encode/mod.rs` | encoder API, frame, quantization tables (Annex K scaled as IJG), headers, rows on workers joined in order |
| `encode/transform.rs` | RGB → YCbCr, edge repetition, chroma averaging, exact float forward DCT, quantization |
| `encode/entropy.rs` | Annex K Huffman tables, optimal tables (K.2), bit writer, the joiner (word-wide bit shifting, byte stuffing) |

Decoding, in three routes that give the same pixels:

- **One scan with restart intervals** (several threads): the intervals are
  found by their markers and entropy-decoded in parallel, each into its own
  slice of MCUs; then the bands render in parallel.
- **One scan, no restarts**: the calling thread entropy-decodes MCU rows
  while the other threads run the IDCT and color conversion of finished
  rows; the coefficients of a band are handed over when its row is done.
- **Progressive, or several baseline scans**: each scan decodes into whole
  component planes, then the bands render in parallel.

A color band reads plane rows of the bands either side (vertical
interpolation), so it waits for their IDCT; workers take the lowest ready
color band first, else the next IDCT. Encoding: each MCU row is converted,
transformed and Huffman-coded by a worker into its own bit stream, starting
from the DC predictors of the row before (it transforms that row's last MCU
again rather than wait); the calling thread joins the streams in order.

Accuracy: the IDCT is the exact transform to float rounding, upsampling
keeps fractions (no rounding between upsampling and color conversion), and
color conversion rounds once. Against libjpeg-turbo (islow, fancy
upsampling) every fixture is within 4 (mean under 0.4), except 4:1 and 3:2
chroma which libjpeg replicates and this decoder interpolates. Against
luce-jpeg's float decoder: 99.97% of 4.5 M fixture samples identical over
all scales, never more than 2 apart (ties at exact halves, which this
decoder keeps exact).

## kinogaki-exr (crates/exr)

| File | Responsibility |
| --- | --- |
| `lib.rs` | `Exr`, `Part`, `Header`, `Channel`, `ChannelSamples` (U32, F16 bits, F32 planes), `Window`, `Tiles`, `Compression`, options |
| `error.rs` | `Error`: `Corrupt { what, at }`, `Unsupported`, `Invalid`, `TooLarge`, `Io` |
| `header.rs` | magic and version flags, attributes, the channel list, the interpreted attributes (everything else is kept raw, in order), header writing |
| `read.rs` | headers of every part, offset tables, chunk validation (part number, y or tile coordinates), decoding in bands of rows on every processor straight into the channel planes |
| `write.rs` | validation, headers, offset tables, chunks built and compressed on every processor, written in increasing y |
| `image.rs` | `Part::to_image` / `from_image` (RGB(A), Y(A) or a lone channel), sample conversions |
| `parallel.rs` | thread count and an ordered parallel map |
| `compression/mod.rs` | chunk codec dispatch; raw-size chunks are stored raw |
| `compression/zip.rs` | the byte predictor ZIP, ZIPS and RLE share (split halves, deltas); the running sum eight bytes at a time |
| `compression/rle.rs` | run-length coding |
| `compression/piz.rs` | PIZ: value bitmap and dense renumbering, the 14/16-bit Haar wavelet, canonical Huffman over up to 58-bit codes (14-bit lookup table) with a run symbol; the encoder builds real Huffman codes, as OpenEXR does |
| `compression/pxr24.rs` | PXR24: floats to 24 bits as OpenEXR rounds them, horizontal deltas, byte planes, zlib |

A part's channels are planes over the data window. Reading splits every
plane into bands (a scanline chunk, or a row of tiles), and each band's
task owns its rows of every plane: chunks decode in parallel with no copy
after. Writing does the inverse per chunk. PIZ and PXR24 follow OpenEXR's
algorithms (BSD-3-Clause, `crates/exr/LICENSE-OpenEXR`); every other part
is from the OpenEXR file layout documentation.

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
- `crates/jpeg/tests/decode.rs`: the 64 fixtures of luce-jpeg (Pillow, cjpeg
  and TurboJPEG over synthetic pictures: every sampling, baseline and
  progressive, grey, RGB, CMYK, YCCK, restarts, multi-thread sizes) decode
  within 4 of libjpeg-turbo's decodes (`tests/fixtures/reference`, written by
  `tools/jpeg_oracle.py references`), full size and 1/2, 1/4, 1/8 (180
  references); thread counts and routes agree byte for byte; every
  truncation of five files fails as `Corrupt`; 2 400 mutated files never
  panic; 18 ICC files join as libjpeg-turbo joined them (expected.txt).
- `crates/jpeg/tests/encode.rs`: sizes 1×1 to 300×200, 1–4 channels, six
  quality/subsampling cases, restart and optimize on and off decode above
  PSNR floors; thread counts never change the bytes; optimized tables are
  smaller; ICC profiles over 64 KB embed and read back.
- `tools/jpeg_oracle.py check DIR` decodes the encoder tests' files with
  libjpeg-turbo (`KINOGAKI_JPEG_ORACLE_DIR=DIR cargo test -p kinogaki-jpeg
  --test encode`): 384/384 decode, PSNR against the source as above.
- `crates/exr/tests/openexr.rs`: 12 files written by OpenEXR 3.5 itself
  (`tools/exr_oracle.py fixtures`: every compression scanline, four tiled,
  PIZ with over 16 384 distinct words, a multi-part file) read to exactly
  the pattern the oracle wrote (PXR24 floats to 24 bits); every
  compression round-trips scanline and tiled at 1×1, 67×45 and 160×120 on
  one thread and on all; multi-part, negative data windows, string
  attributes and names past 31 bytes round-trip; every truncation of six
  files fails and 1 800 mutated files never panic.
- `tools/exr_oracle.py check DIR` reads the files those tests write
  (`KINOGAKI_EXR_ORACLE_DIR=DIR cargo test -p kinogaki-exr --test
  openexr`) with OpenEXR: 56 files, 348 channels, no mismatch.
