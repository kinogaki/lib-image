//! JPEG (ITU T.81): baseline and progressive decoding, baseline encoding.
//!
//! Decoding reads 8-bit Huffman-coded JPEGs: sequential and progressive
//! scans, restart markers, any sampling factors, greyscale, YCbCr, RGB,
//! CMYK and YCCK (converted to RGB). [`decode`] gives the picture's own
//! channels (1 grey or 3 RGB), [`decode_rgba8`] 8-bit RGBA; both can
//! scale to 1/2, 1/4 or 1/8 straight from the DCT. Decoding runs on every
//! processor: restart intervals entropy-decode in parallel, and the IDCT
//! and color conversion run in bands behind the entropy decoding.
//! [`icc_profile`] joins an embedded ICC profile.
//!
//! [`encode`] writes baseline JPEGs from grey or color images at a
//! quality, with 4:4:4, 4:2:2 or 4:2:0 chroma, optional restart markers
//! and optimized Huffman tables, on every processor too.
//!
//! ```
//! use kinogaki_jpeg::{DecodeOptions, EncodeOptions};
//! use kinogaki_image::Image;
//! let pixels: Vec<u8> = (0..64 * 48 * 4).map(|i| (i % 251) as u8).collect();
//! let image = Image::rgba8(64, 48, pixels);
//! let jpeg = kinogaki_jpeg::encode(&image, &EncodeOptions::default()).unwrap();
//! let info = kinogaki_jpeg::info(&jpeg).unwrap();
//! assert_eq!((info.width, info.height, info.components), (64, 48, 3));
//! let back = kinogaki_jpeg::decode(&jpeg, &DecodeOptions::default()).unwrap();
//! assert_eq!((back.width, back.height, back.channels), (64, 48, 3));
//! ```
mod coefficients;
mod color;
mod decode;
mod encode;
mod error;
mod headers;
mod huffman;
mod icc;
mod idct;
mod render;
mod scan;

pub use encode::{EncodeOptions, Subsampling, encode};
pub use error::Error;
pub use icc::icc_profile;
pub use kinogaki_image::{Image, Samples};

/// A JPEG's size and coding, from its frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// 1 greyscale, 3 YCbCr or RGB, 4 CMYK or YCCK.
    pub components: u8,
    /// Progressive (SOF2) rather than sequential.
    pub progressive: bool,
}

impl Info {
    /// The size of a decode at 1/`scale`, rounded up as libjpeg rounds.
    pub fn scaled(&self, scale: u8) -> (u32, u32) {
        let scale = u32::from(scale.max(1));
        (self.width.div_ceil(scale), self.height.div_ceil(scale))
    }
}

/// How a decode runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeOptions {
    /// 1, 2, 4 or 8: decode at 1/`scale` of the full size, straight from
    /// the DCT (each pixel the mean of the full-size pixels it covers).
    pub scale: u8,
    /// Threads, counting the caller's; 0 uses every processor.
    pub threads: usize,
    /// The most pixels (full size) a picture may have.
    pub max_pixels: u64,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            scale: 1,
            threads: 0,
            max_pixels: 1 << 30,
        }
    }
}

/// The frame header of the JPEG in `data`.
pub fn info(data: &[u8]) -> Result<Info, Error> {
    let frame = decode::frame_of(data)?;
    Ok(Info {
        width: frame.width as u32,
        height: frame.height as u32,
        components: frame.components.len() as u8,
        progressive: frame.progressive,
    })
}

/// Decodes `data` into the picture's own channels: 1 (grey) or 3 (RGB;
/// CMYK and YCCK are converted), 8 bits a sample.
pub fn decode(data: &[u8], options: &DecodeOptions) -> Result<Image, Error> {
    image(decode::decode(data, options, false)?)
}

/// Decodes `data` into 8-bit RGBA, alpha opaque.
pub fn decode_rgba8(data: &[u8], options: &DecodeOptions) -> Result<Image, Error> {
    image(decode::decode(data, options, true)?)
}

fn image(decoded: decode::Decoded) -> Result<Image, Error> {
    Ok(Image {
        width: decoded.width as u32,
        height: decoded.height as u32,
        channels: decoded.step as u8,
        samples: Samples::U8(decoded.pixels),
    })
}
