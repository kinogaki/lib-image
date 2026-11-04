//! PNG images (ISO/IEC 15948, W3C PNG Third Edition) over
//! [`kinogaki_deflate`].
//!
//! Images are owned and interleaved ([`Image`], shared by every lib-image
//! format): grey, grey and alpha,
//! RGB or RGBA, 8 or 16 bits per sample, straight alpha. [`decode`] reads
//! every standard PNG — greyscale, truecolor, indexed, with or without
//! alpha, 1 to 16 bits per sample, interlaced or not, `tRNS` transparency —
//! keeping the file's channels and sample size; [`encode`] writes any
//! [`Image`] with a per-row adaptive filter, [`encode_tagged`] saying which display
//! its code values are for, [`display_tag`] reading that back; decoding ignores
//! color management chunks (samples are taken as sRGB). Chunk CRCs and the zlib checksum
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

pub use decode::{ReadTag, decode, display_tag};
pub use encode::{DisplayTag, encode, encode_tagged};
pub use kinogaki_deflate::Level;
pub use kinogaki_image::{Image, Samples};

use std::fmt;

/// The eight bytes every PNG starts with.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

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
    /// The image given to [`encode`] has samples PNG cannot hold (32-bit
    /// integers or floats).
    SampleType,
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
            Error::SampleType => write!(f, "PNG holds 8 or 16-bit integer samples only"),
        }
    }
}

impl std::error::Error for Error {}

impl From<kinogaki_deflate::Error> for Error {
    fn from(e: kinogaki_deflate::Error) -> Self {
        Error::Compressed(e)
    }
}
