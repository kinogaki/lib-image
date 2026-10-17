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
| `kinogaki-jpeg` | crates/jpeg | JPEG (planned: port of luce-jpeg). |
| `kinogaki-exr` | crates/exr | OpenEXR with Cryptomatte (planned: port of luce-exr). |

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

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Independent oracles (local only, never dependencies): `tools/zlib_oracle.py`
(Python's zlib) and `tools/png_oracle.py` (Pillow) write the test fixtures
and check this library's output; see docs/ARCHITECTURE.md.
