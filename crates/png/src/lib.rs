//! PNG images (ISO/IEC 15948, W3C PNG Third Edition) over
//! [`kinogaki_deflate`].
//!
//! Images are owned and interleaved ([`Image`]): grey, grey and alpha,
//! RGB or RGBA, 8 or 16 bits per sample, straight alpha. [`decode`] reads
//! every standard PNG — greyscale, truecolor, indexed, with or without
//! alpha, 1 to 16 bits per sample, interlaced or not, `tRNS` transparency —
//! keeping the file's channels and sample size; [`encode`] writes any
//! [`Image`] with a per-row adaptive filter. Color management chunks are
//! ignored (samples are taken as sRGB). Chunk CRCs and the zlib checksum
//! are verified.
//!
//! ```
//! use kinogaki_png::{Image, Level, Samples};
//! let image = Image::rgba8(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]);
//! let png = kinogaki_png::encode(&image, Level::Balanced).unwrap();
//! let back = kinogaki_png::decode(&png, 1 << 20).unwrap();
//! assert_eq!(back, image);
//! assert!(matches!(back.samples, Samples::U8(_)));
//! ```
mod decode;
mod encode;
mod filter;

pub use decode::decode;
pub use encode::encode;
pub use kinogaki_deflate::Level;

use std::fmt;

/// The eight bytes every PNG starts with.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

/// An image: `channels` interleaved samples per pixel (1 grey, 2 grey and
/// alpha, 3 RGB, 4 RGBA), rows top to bottom, straight alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// Samples per pixel.
    pub channels: u8,
    /// `width * height * channels` samples.
    pub samples: Samples,
}

/// An image's samples, by size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Samples {
    /// 8 bits per sample.
    U8(Vec<u8>),
    /// 16 bits per sample.
    U16(Vec<u16>),
}

impl Samples {
    /// How many samples there are.
    pub fn len(&self) -> usize {
        match self {
            Samples::U8(v) => v.len(),
            Samples::U16(v) => v.len(),
        }
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Image {
    /// An 8-bit RGBA image.
    pub fn rgba8(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            channels: 4,
            samples: Samples::U8(rgba),
        }
    }

    /// The pixels as 8-bit RGBA (16-bit samples keep their high byte, grey
    /// spreads to red, green and blue, a missing alpha is opaque).
    pub fn to_rgba8(&self) -> Vec<u8> {
        let eight: std::borrow::Cow<[u8]> = match &self.samples {
            Samples::U8(v) => v.into(),
            Samples::U16(v) => v.iter().map(|&s| (s >> 8) as u8).collect(),
        };
        if self.channels == 4 {
            return eight.into_owned();
        }
        let n = usize::from(self.channels.max(1));
        let mut rgba = Vec::with_capacity(eight.len() / n * 4);
        for p in eight.chunks_exact(n) {
            rgba.extend_from_slice(&match p {
                [g] => [*g, *g, *g, 255],
                [g, a] => [*g, *g, *g, *a],
                [r, g, b] => [*r, *g, *b, 255],
                _ => [p[0], p[1], p[2], p[3]],
            });
        }
        rgba
    }
}

/// Why an image could not be encoded or decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a PNG file.
    Signature,
    /// The file breaks the format.
    Invalid(&'static str),
    /// A chunk's CRC does not match its contents.
    Crc,
    /// The image has more pixels than the caller allows.
    TooLarge,
    /// The image data does not decompress.
    Compressed(kinogaki_deflate::Error),
    /// The image given to [`encode`] has no pixels, more than four
    /// channels, or not `width * height * channels` samples.
    Size,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Signature => write!(f, "not a PNG file"),
            Error::Invalid(what) => write!(f, "invalid PNG: {what}"),
            Error::Crc => write!(f, "PNG chunk CRC mismatch"),
            Error::TooLarge => write!(f, "PNG image exceeds the pixel limit"),
            Error::Compressed(e) => write!(f, "PNG image data: {e}"),
            Error::Size => write!(f, "image samples do not match its size and channels"),
        }
    }
}

impl std::error::Error for Error {}

impl From<kinogaki_deflate::Error> for Error {
    fn from(e: kinogaki_deflate::Error) -> Self {
        Error::Compressed(e)
    }
}
