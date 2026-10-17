//! The owned image every lib-image format decodes to and encodes from.
//!
//! An [`Image`] is `channels` interleaved samples per pixel, rows top to
//! bottom, straight (not premultiplied) alpha. Integer samples are
//! display-referred (sRGB coded for color); float samples are
//! scene-linear, as OpenEXR stores them. Half floats are kept as their
//! IEEE 754 binary16 bits ([`f16_to_f32`], [`f32_to_f16`]) since Rust's
//! `f16` is not stable.
//!
//! ```
//! use kinogaki_image::{Image, Samples};
//! let image = Image::new(1, 1, 3, Samples::F32(vec![1.0, 0.215_860_5, 0.0])).unwrap();
//! assert_eq!(image.to_rgba8(), vec![255, 128, 0, 255]);
//! ```
mod half;
mod srgb;

pub use half::{f16_to_f32, f32_to_f16};

use std::fmt;

/// An image: `channels` interleaved samples per pixel (1 grey, 2 grey and
/// alpha, 3 RGB, 4 RGBA), rows top to bottom, straight alpha.
#[derive(Clone, Debug, PartialEq)]
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

/// An image's samples, by type.
#[derive(Clone, Debug, PartialEq)]
pub enum Samples {
    /// 8-bit unsigned integers.
    U8(Vec<u8>),
    /// 16-bit unsigned integers.
    U16(Vec<u16>),
    /// 32-bit unsigned integers (OpenEXR UINT channels: IDs, counts).
    U32(Vec<u32>),
    /// IEEE 754 binary16 floats, as their bits.
    F16(Vec<u16>),
    /// IEEE 754 binary32 floats.
    F32(Vec<f32>),
}

impl Samples {
    /// How many samples there are.
    pub fn len(&self) -> usize {
        match self {
            Samples::U8(v) => v.len(),
            Samples::U16(v) | Samples::F16(v) => v.len(),
            Samples::U32(v) => v.len(),
            Samples::F32(v) => v.len(),
        }
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An image whose samples do not fill `width * height * channels`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeError;

impl fmt::Display for SizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "image samples do not match its size and channels")
    }
}

impl std::error::Error for SizeError {}

impl Image {
    /// An image, checked: `samples` must hold `width * height * channels`
    /// values and `channels` must be 1 through 4.
    pub fn new(width: u32, height: u32, channels: u8, samples: Samples) -> Result<Self, SizeError> {
        let image = Self {
            width,
            height,
            channels,
            samples,
        };
        if !(1..=4).contains(&channels) || image.sample_count() != Some(image.samples.len()) {
            return Err(SizeError);
        }
        Ok(image)
    }

    /// An 8-bit RGBA image.
    pub fn rgba8(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            channels: 4,
            samples: Samples::U8(rgba),
        }
    }

    /// `width * height * channels`, or `None` when that overflows.
    pub fn sample_count(&self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(usize::from(self.channels))
    }

    /// The pixels as 8-bit RGBA for display. Integer samples keep their
    /// high byte; float samples are clamped to 0..=1 and their color takes
    /// the sRGB transfer curve (alpha stays linear), rounded to nearest.
    /// Grey spreads to red, green and blue; a missing alpha is opaque.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let n = usize::from(self.channels.max(1));
        let has_alpha = n == 2 || n == 4;
        let eight: std::borrow::Cow<[u8]> = match &self.samples {
            Samples::U8(v) => v.into(),
            Samples::U16(v) => v.iter().map(|&s| (s >> 8) as u8).collect(),
            Samples::U32(v) => v.iter().map(|&s| (s >> 24) as u8).collect(),
            Samples::F16(v) => float_bytes(v.iter().map(|&s| f16_to_f32(s)), n, has_alpha),
            Samples::F32(v) => float_bytes(v.iter().copied(), n, has_alpha),
        };
        if n == 4 {
            return eight.into_owned();
        }
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

/// Float samples as display bytes: color through the sRGB curve, the
/// last of each pixel's `n` samples linear when it is alpha.
fn float_bytes(
    samples: impl Iterator<Item = f32>,
    n: usize,
    has_alpha: bool,
) -> std::borrow::Cow<'static, [u8]> {
    samples
        .enumerate()
        .map(|(i, v)| {
            if has_alpha && i % n == n - 1 {
                (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            } else {
                srgb::encode8(v)
            }
        })
        .collect::<Vec<u8>>()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_checks_the_sample_count() {
        assert!(Image::new(2, 2, 3, Samples::U8(vec![0; 12])).is_ok());
        assert_eq!(
            Image::new(2, 2, 3, Samples::U8(vec![0; 11])),
            Err(SizeError)
        );
        assert_eq!(Image::new(1, 1, 5, Samples::U8(vec![0; 5])), Err(SizeError));
        assert_eq!(Image::new(1, 1, 0, Samples::U8(vec![])), Err(SizeError));
    }

    #[test]
    fn to_rgba8_spreads_grey_and_encodes_floats() {
        let grey = Image::new(1, 1, 2, Samples::U16(vec![0x1234, 0xFF00])).unwrap();
        assert_eq!(grey.to_rgba8(), vec![0x12, 0x12, 0x12, 0xFF]);
        let linear = Image::new(1, 1, 4, Samples::F32(vec![0.0, 0.5, 2.0, 0.5])).unwrap();
        // 0.5 linear is 0.7354 in sRGB: 187.5 rounds to 188.
        assert_eq!(linear.to_rgba8(), vec![0, 188, 255, 128]);
        let half = Image::new(1, 1, 1, Samples::F16(vec![f32_to_f16(1.0)])).unwrap();
        assert_eq!(half.to_rgba8(), vec![255, 255, 255, 255]);
    }
}
