//! Writing 8-bit RGBA PNGs.
use crate::filter::{self, TYPES};
use crate::{Error, Image, SIGNATURE, Samples};
use kinogaki_deflate::{Crc32, Level, zlib_compress};

/// The largest width or height PNG allows.
pub(crate) const MAX_DIMENSION: u32 = (1 << 31) - 1;

/// A PNG of `image`. Each row takes the filter whose output has the
/// smallest sum of magnitudes (libpng's heuristic); `level` sets the
/// compression effort.
pub fn encode(image: &Image, level: Level) -> Result<Vec<u8>, Error> {
    encode_tagged(image, level, None)
}

/// Which display a PNG's code values are for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DisplayTag<'a> {
    /// sRGB: an `sRGB` chunk.
    Srgb,
    /// An ICC profile: an `iCCP` chunk.
    Icc {
        /// The profile's name (1 to 79 Latin-1 characters).
        name: &'a str,
        /// The profile.
        profile: &'a [u8],
    },
}

/// A PNG of `image` as [`encode`] writes it, saying which display it is for.
pub fn encode_tagged(
    image: &Image,
    level: Level,
    tag: Option<DisplayTag<'_>>,
) -> Result<Vec<u8>, Error> {
    let (width, height) = (image.width, image.height);
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Error::Invalid("image dimensions"));
    }
    let channels = usize::from(image.channels);
    if !(1..=4).contains(&channels)
        || u64::from(width) * u64::from(height) * channels as u64 != image.samples.len() as u64
    {
        return Err(Error::Size);
    }
    // Samples as PNG stores them: bytes, 16-bit ones big-endian.
    let (bytes, depth): (std::borrow::Cow<[u8]>, u8) = match &image.samples {
        Samples::U8(v) => (v.into(), 8),
        Samples::U16(v) => (v.iter().flat_map(|s| s.to_be_bytes()).collect(), 16),
        Samples::U32(_) | Samples::F16(_) | Samples::F32(_) => return Err(Error::SampleType),
    };
    let bpp = channels * usize::from(depth / 8);
    let row = width as usize * bpp;
    let mut filtered = Vec::with_capacity((row + 1) * height as usize);
    let zeros = vec![0u8; row];
    let mut candidates = vec![vec![0u8; row]; TYPES as usize];
    for y in 0..height as usize {
        let current = &bytes[y * row..(y + 1) * row];
        let previous = if y == 0 {
            &zeros[..]
        } else {
            &bytes[(y - 1) * row..y * row]
        };
        let mut best = (u64::MAX, 0u8);
        for kind in 0..TYPES {
            let out = &mut candidates[kind as usize];
            filter::filter(kind, current, previous, bpp, out);
            let cost = filter::cost(out);
            if cost < best.0 {
                best = (cost, kind);
            }
        }
        filtered.push(best.1);
        filtered.extend_from_slice(&candidates[best.1 as usize]);
    }
    let data = zlib_compress(&filtered, level);
    let mut png = Vec::with_capacity(data.len() + 64);
    png.extend_from_slice(&SIGNATURE);
    let mut header = [0u8; 13];
    header[..4].copy_from_slice(&width.to_be_bytes());
    header[4..8].copy_from_slice(&height.to_be_bytes());
    // Grey, grey and alpha, RGB, RGBA; deflate, adaptive filtering, no
    // interlace.
    let color = [0, 4, 2, 6][channels - 1];
    header[8..].copy_from_slice(&[depth, color, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    match tag {
        None => {}
        // Perceptual intent.
        Some(DisplayTag::Srgb) => chunk(&mut png, b"sRGB", &[0]),
        Some(DisplayTag::Icc { name, profile }) => {
            let mut body = name.as_bytes().to_vec();
            // The name's end; deflate.
            body.extend([0, 0]);
            body.extend(zlib_compress(profile, level));
            chunk(&mut png, b"iCCP", &body);
        }
    }
    // A chunk holds less than 2 GiB.
    for piece in data.chunks(1 << 30) {
        chunk(&mut png, b"IDAT", piece);
    }
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, display_tag};

    #[test]
    fn a_tag_reads_back_and_leaves_the_pixels() {
        let image = Image::rgba8(1, 1, vec![1, 2, 3, 4]);
        let plain = encode(&image, Level::Fast).unwrap();
        assert_eq!(display_tag(&plain).unwrap(), None);
        let srgb = encode_tagged(&image, Level::Fast, Some(DisplayTag::Srgb)).unwrap();
        assert_eq!(display_tag(&srgb).unwrap(), Some(("sRGB".into(), None)));
        let tag = DisplayTag::Icc {
            name: "Display P3",
            profile: &[9; 200],
        };
        let icc = encode_tagged(&image, Level::Fast, Some(tag)).unwrap();
        let expected = Some(("Display P3".into(), Some(vec![9; 200])));
        assert_eq!(display_tag(&icc).unwrap(), expected);
        assert_eq!(decode(&icc, 16).unwrap().to_rgba8(), [1, 2, 3, 4]);
    }
}
