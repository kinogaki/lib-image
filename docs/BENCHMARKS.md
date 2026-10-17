# Benchmarks

Apple M4 Max, macOS, release builds (thin LTO, one codegen unit). Other
agents' builds were running (load average 40–100), so each figure is the
**minimum** of 20–40 runs, all variants interleaved in one process. The
comparisons are the crates Studio used before: `png` 0.18.1 at its
defaults (flate2/miniz_oxide level 6, adaptive filter; decoding through
fdeflate with the Adler-32 check skipped), `miniz_oxide` 0.8.9 and
`fdeflate` 0.3.7. The harness lives outside the repository (it needs those
crates); its cases are listed so they can be rebuilt.

## Inflate (zlib streams, Adler-32 verified)

| Payload | kinogaki-deflate | miniz_oxide | fdeflate |
| --- | --- | --- | --- |
| Studio capture IDAT, 1.0 MB → 20.2 MB, exact size as limit (as PNG decodes) | 6.3 ms | 7.2 ms | 4.7 ms |
| same, no size known | 7.0 ms | 7.2 ms | 4.7 ms |
| EXR-like ZIP: 3840×2160 half RGBA in 135 blocks of 16 scanlines, bytes split and delta-coded as OpenEXR does, miniz level 6; 29.7 MB → 66.4 MB | 115 ms | 123 ms | 67 ms |
| Text-like 20 MB, miniz level 6 (1.8 MB) | 9.5 ms | 16.9 ms | 7.1 ms |

2026-10-17, the OpenEXR ZIP chunks of a 3840×2160 eight-channel picture
(RGBA and normals half, depth float; written by OpenEXR 3.5, 135 chunks of
16 scanlines, 86 MB → 149 MB), minimum of 15 runs, quiet machine:

| Payload | kinogaki-deflate | miniz_oxide 0.8.9 | fdeflate 0.3.7 |
| --- | ---: | ---: | ---: |
| 4K 8-channel EXR, ZIP (before this round) | 313.5 ms | 283.8 ms | 197.3 ms |
| same, now | 294.8 ms | 283.4 ms | 197.6 ms |
| same picture, ZIPS (2160 one-scanline chunks) | 302.9 ms | 289.9 ms | 203.3 ms |

The decoding loop keeps the bit reader in registers (its byte-by-byte tail
takes and returns the state by value), drops limit checks while a whole
match still fits below the limit, and looks up the next three literal
codes from one buffer at once. fdeflate is still about 1.5× faster on
these literal-heavy streams; EXR reads hide most of it by decompressing
chunks on every processor.

The decoder resolves up to 11 bits per first-level lookup, merges two
short literal codes into one entry, decodes up to four literals per
refill, copies matches eight bytes at a time (periods under eight bytes
first widen to a multiple of themselves), and takes fresh zero pages for
outputs of 4 MiB and more. fdeflate is still faster on literal-heavy data;
closing that gap is open work.

## PNG, 8-bit RGBA

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

## JPEG against luce-jpeg 0.10.1

Same machine (Apple M4 Max, 16 cores), both release builds (luce-jpeg's
`tests/bench.lucb` and `tests/encode.lucb` built with `luce-base build
--native`; ours `crates/jpeg/examples/bench.rs`), load average 3–15 while
measuring, each figure the minimum over three interleaved rounds of five
runs. The pictures: a 6000×4000 (24 MP) photograph re-encoded by Pillow
(libjpeg-turbo) in each coding; both decoders give RGB bytes.

| Decode | kinogaki, 16 threads | luce, 16 threads | kinogaki, 1 thread | luce, 1 thread |
| --- | ---: | ---: | ---: | ---: |
| baseline 4:2:0 q95 (6.7 MB) | 41.2 ms | 53.1 ms | 141.9 ms | 219.7 ms |
| baseline 4:4:4 q90 (5.8 MB) | 45.3 ms | 58.5 ms | 142.2 ms | 227.5 ms |
| baseline 4:2:0, restart every MCU row | 20.3 ms | 29.3 ms | 140.7 ms | 220.8 ms |
| progressive 4:2:0 q95 (5.5 MB) | 75.8 ms | 106.1 ms | 168.1 ms | 263.1 ms |
| progressive 4:4:4 q90 (5.1 MB) | 95.7 ms | 148.8 ms | 179.9 ms | 306.0 ms |
| greyscale q90 (4.7 MB) | 28.1 ms | 33.8 ms | 43.4 ms | 115.2 ms |

Baseline decoding without restart markers is bound by the one thread that
entropy-decodes (the rendering runs behind it on the others); the gain over
luce there is the Huffman loop (bit-reader state in registers, end of block
and zero runs in the combined table). Progressive refinement walks each
block's nonzero mask instead of every coefficient position.

| Encode the 24 MP picture, q90 | kinogaki | luce | kinogaki size | luce size |
| --- | ---: | ---: | ---: | ---: |
| 4:4:4 | 17.5 ms | 30.0 ms | 5 810 995 B | 5 818 416 B |
| 4:2:2 | 12.8 ms | 25.7 ms | 5 396 590 B | 5 398 351 B |
| 4:2:0 | 11.8 ms | 22.5 ms | 5 188 198 B | 5 191 536 B |
| 4:2:0, optimized tables | 17.7 ms | 26.6 ms | 4 612 587 B | 4 608 115 B |
| 4:2:0, restart markers | 11.3 ms | 22.8 ms | 5 188 877 B | 5 192 220 B |
| 4:2:0, one thread | 143.7 ms | 183.3 ms | | |

Encoded files are within 0.1% of luce's sizes and decode in libjpeg-turbo
to the PSNR libjpeg-turbo's own encoder reaches on the same source (the
encoder tests' oracle check).
