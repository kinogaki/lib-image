# Format coverage

What each crate reads and writes, and what it leaves out.

## PNG (kinogaki-png)

See README.md: every color type and bit depth, Adam7, `tRNS`; 8/16-bit
writing. Not handled: color management chunks (samples are taken as sRGB),
APNG frames (only the default image is read).

## JPEG (kinogaki-jpeg)

Ported from luce-jpeg 0.10.1.

Decoding:
- 8-bit Huffman JPEG: baseline (SOF0), extended sequential (SOF1),
  progressive (SOF2) with spectral selection and successive approximation.
- Restart intervals (decoded in parallel when the picture is one scan).
- Any sampling factors 1–4 (up to ten blocks an MCU); chroma upsampled by
  bilinear interpolation at pixel centres (libjpeg's triangle filter at 2:1).
- Greyscale, YCbCr, RGB (component IDs `R`, `G`, `B` or Adobe transform 0),
  CMYK, Adobe-inverted CMYK and YCCK (Adobe transform 2), converted to RGB.
- Output as the picture's channels (1 or 3) or RGBA; reduced decodes at
  1/2, 1/4 and 1/8 straight from the DCT.
- ICC profiles from APP2 segments, joined as libjpeg-turbo joins them.
- Truncated or corrupt files fail with the byte offset of the problem.

Encoding: baseline from 8-bit grey, grey+alpha, RGB or RGBA (alpha dropped);
quality 1–100 (Annex K tables scaled as IJG does); 4:4:4, 4:2:2, 4:2:0;
restart marker after every MCU row; optimized Huffman tables; embedded ICC
profile.

Not ported from luce-jpeg (no caller needs them yet): row-callback decoding
(`decode_rows`), decoding a file read in pieces (`decode_rows_at`), the
progressive coarse preview callback, the streaming `Encoder` that takes
rows in bands (encoding takes a whole image; workers still hold only a few
MCU rows at a time), and the luce-raster f64 route.

Not supported (as in luce-jpeg): arithmetic coding, lossless and
hierarchical JPEG, 12-bit samples, DNL-defined heights.

## OpenEXR (kinogaki-exr)

Ported from luce-exr 0.6.2, then extended.

- Scanline and tiled parts (one level), multi-part files, read and
  written; increasing, decreasing or random line order read (writes are
  increasing).
- Compression, read and written: none, RLE, ZIPS, ZIP, PIZ, PXR24 (luce-exr
  did not write PXR24).
- UINT, HALF and FLOAT channels at any data window (negative origins
  too), the `pLinear` flag, long names.
- Every header attribute kept: the structural ones typed, the rest raw in
  file order, string attributes readable and settable.
- Conversion to and from the shared interleaved `Image`.

Cryptomatte 1.2 (`kinogaki_exr::cryptomatte`), everything luce-exr had:
layer discovery, layers of FLOAT quadruples (R/G/B/A in three spellings),
mattes of names or exact IDs across every rank with inversion, picking by
ID and name, manifests embedded or in sidecar files (lexically checked
relative paths, 64 MiB limit), manifest writing, and the weighted-sample
builder writing a layer (manifest embedded or sidecar). Renderer-side
sample generation and DCC behaviour stay outside an image library, as in
luce-exr.

Not supported (as in luce-exr): deep data, mipmap and ripmap levels,
subsampled channels (x/y sampling other than 1), B44/B44A, DWAA/DWAB and
the newer HTJ2K and ZSTD compressions.
