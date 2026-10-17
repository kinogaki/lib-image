# lib-image

Image formats and the compression under them for Kinogaki Studio, in Rust
with no dependencies beyond std and no unsafe code. One crate per format,
all reading into owned images and writing from them.

## Crates

| Crate | Path | What |
| --- | --- | --- |
| `kinogaki-image` | crates/image | The shared `Image` (interleaved samples: `U8`, `U16`, `U32`, `F16`, `F32`), exact half-float conversion, `to_rgba8` for display. |
| `kinogaki-deflate` | crates/deflate | DEFLATE and zlib compression and decompression, CRC-32, Adler-32. Shared by PNG, EXR's ZIP compression and lib-usd's usdz packages. |
| `kinogaki-png` | crates/png | PNG decode (every color type, 1–16 bits, interlaced, `tRNS`) and encode (8/16-bit grey, grey+alpha, RGB, RGBA). |
| `kinogaki-jpeg` | crates/jpeg | JPEG decode (baseline and progressive, restart markers, any sampling, grey/YCbCr/RGB/CMYK/YCCK, 1/2–1/8 scaled, ICC profiles) and baseline encode (quality, 4:4:4/4:2:2/4:2:0, restart markers, optimized tables), on every processor. |
| `kinogaki-exr` | crates/exr | OpenEXR read and write: scanline and tiled (one level), multi-part, none/RLE/ZIPS/ZIP/PIZ/PXR24, UINT/HALF/FLOAT channels, every attribute kept; chunks on every processor. |

## The image shape

Every format decodes to an owned image and encodes from one
(`kinogaki-image`, re-exported by each format crate):

```rust
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub channels: u8,       // interleaved samples per pixel
    pub samples: Samples,   // U8, U16, U32, F16 (binary16 bits) or F32
}
```

Samples are interleaved, rows top to bottom, straight (not premultiplied)
alpha. Integer samples are display-referred, float samples scene-linear.
A format keeps the file's own channel count and sample type where it can;
`Image::to_rgba8` gives 8-bit RGBA for display (floats through the sRGB
curve).

```rust
use kinogaki_jpeg::{DecodeOptions, EncodeOptions};
let image = kinogaki_jpeg::decode_rgba8(&bytes, &DecodeOptions::default())?;
let thumbnail = kinogaki_jpeg::decode(&bytes, &DecodeOptions { scale: 8, ..Default::default() })?;
let jpeg = kinogaki_jpeg::encode(&image, &EncodeOptions { quality: 92, ..Default::default() })?;

let exr = kinogaki_exr::read(&bytes, &kinogaki_exr::ReadOptions::default())?;
let beauty = exr.parts[0].to_image()?;            // RGBA half or float, interleaved
let depth = exr.parts[0].channel("Z");            // or any channel's own plane
```

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Independent oracles (local only, never dependencies): `tools/zlib_oracle.py`
(Python's zlib), `tools/png_oracle.py` and `tools/jpeg_oracle.py` (Pillow,
so libjpeg-turbo for JPEG) and `tools/exr_oracle.py` (OpenEXR's Python
bindings) write the test fixtures and check this library's
output; see docs/ARCHITECTURE.md. What each format leaves out is listed in
docs/FORMATS.md.
