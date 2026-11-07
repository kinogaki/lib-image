//! Reading PNGs: their chunks first ([`Png`]), then their pixels.
use crate::encode::MAX_DIMENSION;
use crate::filter;
use crate::{Error, Image, SIGNATURE, Samples};
use kinogaki_deflate::{Crc32, zlib_decompress, zlib_decompress_until};
use std::sync::atomic::{AtomicBool, Ordering};

/// The header's pixel format.
#[derive(Clone, Copy)]
struct Format {
    width: u32,
    height: u32,
    depth: u8,
    color: Color,
    interlaced: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    Grey,
    Rgb,
    Indexed,
    GreyAlpha,
    Rgba,
}

impl Color {
    fn samples(self) -> usize {
        match self {
            Color::Grey | Color::Indexed => 1,
            Color::GreyAlpha => 2,
            Color::Rgb => 3,
            Color::Rgba => 4,
        }
    }
}

impl Format {
    fn bits_per_pixel(self) -> usize {
        self.color.samples() * self.depth as usize
    }

    /// Bytes between a byte and the one its filter predicts from.
    fn filter_bpp(self) -> usize {
        self.bits_per_pixel().div_ceil(8)
    }

    fn row_bytes(self, width: usize) -> usize {
        (width * self.bits_per_pixel()).div_ceil(8)
    }
}

/// `tRNS`: which sample value is transparent, or the palette's alphas.
enum Transparency {
    None,
    Grey(u16),
    Rgb([u16; 3]),
}

/// Adam7's passes: first column and row, column and row steps.
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Decodes the PNG `data`, refusing images of more than `max_pixels`
/// pixels before decompressing them. The image keeps the file's channels
/// (grey, grey and alpha, RGB or RGBA; indexed images become RGB, or RGBA
/// when the palette has transparency; `tRNS` adds an alpha channel) and
/// its sample size: 16-bit files give [`Samples::U16`], all others
/// [`Samples::U8`] with 1, 2 and 4-bit samples spread to 0..=255.
pub fn decode(data: &[u8], max_pixels: u64) -> Result<Image, Error> {
    let png = Png::read(data)?;
    if u64::from(png.width()) * u64::from(png.height()) > max_pixels {
        return Err(Error::TooLarge);
    }
    png.decode(&GO)
}

/// A stop flag never raised.
static GO: AtomicBool = AtomicBool::new(false);

/// A PNG read up to its pixels: its header and the chunks saying how to
/// decode them, CRCs checked and nothing inflated, so a caller sees what
/// decoding would take ([`inflated`](Self::inflated), the image's size)
/// before it [`decode`](Self::decode)s.
pub struct Png<'a> {
    format: Format,
    palette: Vec<[u8; 4]>,
    transparency: Transparency,
    /// The `IDAT` chunks' data, in order.
    idat: Vec<&'a [u8]>,
    /// Each pass: first column and row, steps, columns and rows.
    passes: Vec<[usize; 6]>,
    /// Bytes the image data inflates to: every pass's rows, filter bytes
    /// included.
    inflated: usize,
}

impl<'a> Png<'a> {
    /// The chunks of the PNG `data`, read as far as its pixels. Refused: a
    /// broken chunk or header, and an image whose sizes overflow memory's
    /// addresses.
    pub fn read(data: &'a [u8]) -> Result<Png<'a>, Error> {
        if data.get(..8) != Some(&SIGNATURE[..]) {
            return Err(Error::Signature);
        }
        let mut at = 8;
        let mut format = None;
        let mut palette: Vec<[u8; 4]> = Vec::new();
        let mut transparency = Transparency::None;
        let mut idat = Vec::new();
        let mut ended = false;
        let mut idat_done = false;
        while !ended {
            let (kind, body, next) = chunk(data, at)?;
            at = next;
            match &kind {
                b"IHDR" => {
                    if format.is_some() || at != 8 + 25 {
                        return Err(Error::Invalid("IHDR not first"));
                    }
                    format = Some(header(body)?);
                }
                _ if format.is_none() => return Err(Error::Invalid("IHDR not first")),
                b"PLTE" => {
                    if body.is_empty() || body.len() % 3 != 0 || body.len() > 256 * 3 {
                        return Err(Error::Invalid("PLTE length"));
                    }
                    palette = (body.as_chunks::<3>().0.iter())
                        .map(|c| [c[0], c[1], c[2], 255])
                        .collect();
                }
                b"tRNS" => {
                    let f = format.expect("header read");
                    match f.color {
                        Color::Grey if body.len() == 2 => {
                            transparency =
                                Transparency::Grey(u16::from_be_bytes([body[0], body[1]]));
                        }
                        Color::Rgb if body.len() == 6 => {
                            transparency = Transparency::Rgb([
                                u16::from_be_bytes([body[0], body[1]]),
                                u16::from_be_bytes([body[2], body[3]]),
                                u16::from_be_bytes([body[4], body[5]]),
                            ]);
                        }
                        Color::Indexed if body.len() <= palette.len() => {
                            for (entry, &alpha) in palette.iter_mut().zip(body) {
                                entry[3] = alpha;
                            }
                        }
                        _ => return Err(Error::Invalid("tRNS for this color type")),
                    }
                }
                b"IDAT" => {
                    if idat_done {
                        return Err(Error::Invalid("IDAT chunks not consecutive"));
                    }
                    idat.push(body);
                }
                b"IEND" => ended = true,
                _ => {
                    if kind[0] & 0x20 == 0 {
                        return Err(Error::Invalid("unknown critical chunk"));
                    }
                }
            }
            if !idat.is_empty() && &kind != b"IDAT" {
                idat_done = true;
            }
        }
        let format = format.expect("header read");
        if idat.iter().all(|d| d.is_empty()) {
            return Err(Error::Invalid("no IDAT"));
        }
        if format.color == Color::Indexed && palette.is_empty() {
            return Err(Error::Invalid("indexed image without PLTE"));
        }
        let (w, h) = (format.width as usize, format.height as usize);
        let passes: Vec<[usize; 6]> = match format.interlaced {
            true => (ADAM7.iter())
                .map(|&(x0, y0, dx, dy)| {
                    let across = w.saturating_sub(x0).div_ceil(dx);
                    [x0, y0, dx, dy, across, h.saturating_sub(y0).div_ceil(dy)]
                })
                .filter(|p| p[4] > 0 && p[5] > 0)
                .collect(),
            false => vec![[0, 0, 1, 1, w, h]],
        };
        // At most 2³¹ rows of 2³¹ pixels of 64 bits: checked, as a hostile
        // header names such sizes.
        let inflated = passes.iter().try_fold(0usize, |sum, p| {
            let row = format.row_bytes(p[4]).checked_add(1)?;
            sum.checked_add(row.checked_mul(p[5])?)
        });
        let decoded = w
            .checked_mul(h)
            .and_then(|pixels| pixels.checked_mul(4 * 2));
        let (Some(inflated), Some(_)) = (inflated, decoded) else {
            return Err(Error::TooLarge);
        };
        Ok(Png {
            format,
            palette,
            transparency,
            idat,
            passes,
            inflated,
        })
    }

    /// Pixels across.
    pub fn width(&self) -> u32 {
        self.format.width
    }

    /// Pixels down.
    pub fn height(&self) -> u32 {
        self.format.height
    }

    /// The decoded image's channels: 1 to 4.
    pub fn channels(&self) -> u8 {
        let alpha = match self.format.color {
            Color::GreyAlpha | Color::Rgba => true,
            Color::Grey | Color::Rgb => !matches!(self.transparency, Transparency::None),
            Color::Indexed => self.palette.iter().any(|p| p[3] != 255),
        };
        let colors = match self.format.color {
            Color::Grey | Color::GreyAlpha => 1,
            Color::Rgb | Color::Rgba | Color::Indexed => 3,
        };
        colors + u8::from(alpha)
    }

    /// Whether its samples are 16-bit ([`Samples::U16`]), else 8-bit.
    pub fn wide(&self) -> bool {
        self.format.depth == 16
    }

    /// Bytes of compressed image data.
    pub fn compressed(&self) -> usize {
        self.idat.iter().map(|d| d.len()).sum()
    }

    /// Bytes the image data inflates to: its rows, each with its filter
    /// byte.
    pub fn inflated(&self) -> usize {
        self.inflated
    }

    /// The image, as [`decode`] gives it; [`Error::Stopped`] once `stop` is
    /// raised.
    pub fn decode(&self, stop: &AtomicBool) -> Result<Image, Error> {
        let format = self.format;
        let compressed = self.idat.concat();
        let mut raw = zlib_decompress_until(&compressed, self.inflated, stop)?;
        drop(compressed);
        if raw.len() != self.inflated {
            return Err(Error::Invalid("image data too short"));
        }
        let channels = self.channels();
        let wide = self.wide();
        let (w, h) = (format.width as usize, format.height as usize);
        // Rows that already are the image's layout unfilter in the
        // decompressed buffer itself, which then becomes the image.
        if !format.interlaced
            && format.depth >= 8
            && format.color != Color::Indexed
            && usize::from(channels) == format.color.samples()
        {
            let bytes = in_place(raw, format.row_bytes(w), h, format.filter_bpp(), stop)?;
            let samples = match wide {
                true => Samples::U16(
                    (bytes.as_chunks::<2>().0.iter())
                        .map(|&pair| u16::from_be_bytes(pair))
                        .collect(),
                ),
                false => Samples::U8(bytes),
            };
            return Ok(Image {
                width: format.width,
                height: format.height,
                channels,
                samples,
            });
        }
        let n = usize::from(channels);
        let mut out = vec![0u16; w * h * n];
        let bpp = format.filter_bpp();
        let mut offset = 0;
        for &[x0, y0, dx, dy, pw, ph] in &self.passes {
            let row = format.row_bytes(pw);
            let zeros = vec![0u8; row];
            for py in 0..ph {
                if stop.load(Ordering::Relaxed) {
                    return Err(Error::Stopped);
                }
                // Unfilter in place against the row above, already unfiltered.
                let (done, rest) = raw.split_at_mut(offset);
                let previous = if py == 0 {
                    &zeros[..]
                } else {
                    &done[offset - row..]
                };
                let (kind, current) = (rest[0], &mut rest[1..row + 1]);
                filter::unfilter(kind, current, previous, bpp)
                    .map_err(|()| Error::Invalid("filter type"))?;
                let current = &raw[offset + 1..offset + 1 + row];
                offset += row + 1;
                let y = y0 + py * dy;
                for px in 0..pw {
                    let at = ((y * w) + x0 + px * dx) * n;
                    let pixel = convert(format, current, px, &self.palette, &self.transparency)?;
                    out[at..at + n].copy_from_slice(&pixel[..n]);
                }
            }
        }
        let samples = match wide {
            true => Samples::U16(out),
            false => Samples::U8(out.into_iter().map(|v| v as u8).collect()),
        };
        Ok(Image {
            width: format.width,
            height: format.height,
            channels,
            samples,
        })
    }
}

/// Unfilters rows in the decompressed buffer itself and closes up the
/// filter bytes, so the buffer becomes the image (no second one).
fn in_place(
    mut raw: Vec<u8>,
    row: usize,
    height: usize,
    bpp: usize,
    stop: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let zeros = vec![0u8; row];
    for y in 0..height {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Stopped);
        }
        let source = y * (row + 1) + 1;
        // Row y - 1 is already unfiltered and moved down to (y - 1) * row,
        // below `source`.
        let (done, rest) = raw.split_at_mut(source);
        let previous = if y == 0 {
            &zeros[..]
        } else {
            &done[(y - 1) * row..y * row]
        };
        filter::unfilter(done[source - 1], &mut rest[..row], previous, bpp)
            .map_err(|()| Error::Invalid("filter type"))?;
        raw.copy_within(source..source + row, y * row);
    }
    raw.truncate(row * height);
    Ok(raw)
}

/// The largest ICC profile [`display_tag`] inflates.
const MAX_PROFILE: usize = 1 << 26;

/// A display a PNG says it is for: `sRGB` and no profile, or its `iCCP` profile's
/// name and bytes.
pub type ReadTag = (String, Option<Vec<u8>>);

/// What a PNG says about its display, if it does.
pub fn display_tag(data: &[u8]) -> Result<Option<ReadTag>, Error> {
    if data.get(..8) != Some(&SIGNATURE[..]) {
        return Err(Error::Invalid("not a PNG"));
    }
    let mut at = 8;
    while at < data.len() {
        let (kind, body, next) = chunk(data, at)?;
        match &kind {
            b"sRGB" => return Ok(Some(("sRGB".to_owned(), None))),
            b"iCCP" => {
                let end = (body.iter().position(|b| *b == 0))
                    .ok_or(Error::Invalid("iCCP has no name"))?;
                let name = String::from_utf8_lossy(&body[..end]).into_owned();
                let packed = body.get(end + 2..).ok_or(Error::Invalid("iCCP length"))?;
                return Ok(Some((name, Some(zlib_decompress(packed, MAX_PROFILE)?))));
            }
            b"IDAT" | b"IEND" => return Ok(None),
            _ => at = next,
        }
    }
    Ok(None)
}

/// The chunk at `at`: its type, data and where the next one starts.
fn chunk(data: &[u8], at: usize) -> Result<([u8; 4], &[u8], usize), Error> {
    let truncated = Error::Invalid("file ends inside a chunk");
    let head = data.get(at..at + 8).ok_or(truncated.clone())?;
    let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    if length > MAX_DIMENSION {
        return Err(Error::Invalid("chunk length"));
    }
    let kind: [u8; 4] = head[4..8].try_into().expect("four bytes");
    if !kind.iter().all(u8::is_ascii_alphabetic) {
        return Err(Error::Invalid("chunk type"));
    }
    let end = at + 8 + length as usize;
    let body = data.get(at + 8..end).ok_or(truncated.clone())?;
    let stored = data.get(end..end + 4).ok_or(truncated)?;
    let mut crc = Crc32::new();
    crc.update(&kind);
    crc.update(body);
    if crc.finish() != u32::from_be_bytes(stored.try_into().expect("four bytes")) {
        return Err(Error::Crc);
    }
    Ok((kind, body, end + 4))
}

fn header(body: &[u8]) -> Result<Format, Error> {
    if body.len() != 13 {
        return Err(Error::Invalid("IHDR length"));
    }
    let width = u32::from_be_bytes(body[0..4].try_into().expect("four bytes"));
    let height = u32::from_be_bytes(body[4..8].try_into().expect("four bytes"));
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Error::Invalid("image dimensions"));
    }
    let depth = body[8];
    let color = match body[9] {
        0 => Color::Grey,
        2 => Color::Rgb,
        3 => Color::Indexed,
        4 => Color::GreyAlpha,
        6 => Color::Rgba,
        _ => return Err(Error::Invalid("color type")),
    };
    let depth_ok = match color {
        Color::Grey => matches!(depth, 1 | 2 | 4 | 8 | 16),
        Color::Indexed => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !depth_ok {
        return Err(Error::Invalid("bit depth for this color type"));
    }
    if body[10] != 0 || body[11] != 0 {
        return Err(Error::Invalid("compression or filter method"));
    }
    let interlaced = match body[12] {
        0 => false,
        1 => true,
        _ => return Err(Error::Invalid("interlace method")),
    };
    Ok(Format {
        width,
        height,
        depth,
        color,
        interlaced,
    })
}

/// Sample `i` of a row at `depth` bits.
#[inline]
fn sample(row: &[u8], depth: u8, i: usize) -> u16 {
    match depth {
        8 => u16::from(row[i]),
        16 => u16::from_be_bytes([row[2 * i], row[2 * i + 1]]),
        _ => {
            let bit = i * depth as usize;
            let byte = row[bit / 8];
            let shift = 8 - depth as usize - bit % 8;
            u16::from((byte >> shift) & ((1u8 << depth) - 1))
        }
    }
}

/// A sample at the image's output depth: 16-bit samples stay 16-bit, lower
/// depths spread to 0..=255.
#[inline]
fn widen(value: u16, depth: u8) -> u16 {
    match depth {
        8 | 16 => value,
        _ => (u32::from(value) * 255 / ((1 << depth) - 1)) as u16,
    }
}

/// Pixel `x` of an unfiltered row as up to four output samples (grey or
/// RGB, then alpha when the image has it).
#[inline]
fn convert(
    format: Format,
    row: &[u8],
    x: usize,
    palette: &[[u8; 4]],
    transparency: &Transparency,
) -> Result<[u16; 4], Error> {
    let d = format.depth;
    let n = format.color.samples();
    let s = |k: usize| sample(row, d, x * n + k);
    let opaque = if d == 16 { 0xFFFF } else { 0xFF };
    Ok(match format.color {
        Color::Grey => {
            let v = s(0);
            let alpha = match transparency {
                Transparency::Grey(t) if *t == v => 0,
                _ => opaque,
            };
            [widen(v, d), alpha, 0, 0]
        }
        Color::Rgb => {
            let (r, g, b) = (s(0), s(1), s(2));
            let alpha = match transparency {
                Transparency::Rgb(t) if *t == [r, g, b] => 0,
                _ => opaque,
            };
            [r, g, b, alpha]
        }
        Color::Indexed => palette
            .get(s(0) as usize)
            .ok_or(Error::Invalid("palette index out of range"))?
            .map(u16::from),
        Color::GreyAlpha => [s(0), s(1), 0, 0],
        Color::Rgba => [s(0), s(1), s(2), s(3)],
    })
}
