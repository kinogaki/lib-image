# lib-image

The images, PNG and the compression under it for Kinogaki Studio, in Rust
with no dependencies beyond std and no unsafe code. Studio's tools and
captures write and read PNGs through it, and its JPEG and OpenEXR format
plugins build their codecs on its image and DEFLATE.

## Crates

| Crate | Path | What |
| --- | --- | --- |
| `kinogaki-image` | crates/image | The shared `Image` (interleaved samples: `U8`, `U16`, `U32`, `F16`, `F32`), exact half-float conversion, `to_rgba8` for display. |
| `kinogaki-deflate` | crates/deflate | DEFLATE and zlib compression and decompression, CRC-32, Adler-32. Shared by PNG and OpenEXR's ZIP compression. |
| `kinogaki-png` | crates/png | PNG decode (every color type, 1–16 bits, interlaced, `tRNS`) and encode (8/16-bit grey, grey+alpha, RGB, RGBA), with the `sRGB` or `iCCP` chunk saying a display. |

## The image shape

Every codec decodes to an owned image and encodes from one
(`kinogaki-image`, re-exported by `kinogaki-png`):

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
`Image::to_rgba8` gives 8-bit RGBA for display (floats through the sRGB
curve).

```rust
use kinogaki_png::{Image, Level};
let image = Image::rgba8(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]);
let png = kinogaki_png::encode(&image, Level::Balanced)?;
let back = kinogaki_png::decode(&png, 1 << 20)?;
```

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Independent oracles (local only, never dependencies): `tools/zlib_oracle.py`
(Python's zlib) and `tools/png_oracle.py` (Pillow) write the test fixtures
and check this library's output; see docs/ARCHITECTURE.md. What PNG leaves
out is listed in docs/FORMATS.md.

## Licence

MIT OR Apache-2.0, at your option: see LICENSE-MIT and LICENSE-APACHE.
