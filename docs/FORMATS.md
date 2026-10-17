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
