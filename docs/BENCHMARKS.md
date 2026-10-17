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
