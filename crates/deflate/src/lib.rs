//! DEFLATE (RFC 1951) and zlib (RFC 1950) compression and decompression,
//! with the CRC-32 and Adler-32 checksums the formats around them use.
//! One implementation shared by PNG, ZIP (usdz) and anything else that
//! needs it; no unsafe code and no dependencies.
//!
//! ```
//! let data = b"Kinogaki Kinogaki Kinogaki".repeat(10);
//! let packed = kinogaki_deflate::zlib_compress(&data, kinogaki_deflate::Level::Balanced);
//! assert!(packed.len() < data.len());
//! assert_eq!(kinogaki_deflate::zlib_decompress(&packed, data.len()).unwrap(), data);
//! ```
mod checksum;
mod deflate;
mod huffman;
mod inflate;
mod tables;

pub use checksum::{Adler32, Crc32, adler32, crc32};
pub use deflate::Level;

use std::fmt;

/// Why a stream could not be decompressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The stream ends early.
    Truncated,
    /// The stream breaks the format.
    Invalid(&'static str),
    /// The output would pass the caller's limit.
    TooLarge,
    /// The zlib checksum does not match the data.
    Checksum,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => write!(f, "compressed data ends early"),
            Error::Invalid(what) => write!(f, "invalid compressed data: {what}"),
            Error::TooLarge => write!(f, "decompressed data exceeds its limit"),
            Error::Checksum => write!(f, "compressed data checksum mismatch"),
        }
    }
}

impl std::error::Error for Error {}

/// Compresses `data` as a raw DEFLATE stream.
pub fn deflate(data: &[u8], level: Level) -> Vec<u8> {
    deflate::deflate_into(data, level, Vec::with_capacity(data.len() / 2 + 64))
}

/// Decompresses the raw DEFLATE stream at the start of `data`, producing at
/// most `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    inflate::inflate_into(data, &mut out, limit)?;
    Ok(out)
}

/// Decompresses the raw DEFLATE stream at the start of `data`, appending at
/// most `limit` bytes in all to `out`; returns how many bytes of `data` the
/// stream used.
pub fn inflate_into(data: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    inflate::inflate_into(data, out, limit)
}

/// Compresses `data` as a zlib stream (a two-byte header, DEFLATE, Adler-32).
pub fn zlib_compress(data: &[u8], level: Level) -> Vec<u8> {
    // CM 8 (deflate), CINFO 7 (32 KiB window); FLEVEL as zlib sets it, FCHECK
    // making the header a multiple of 31.
    let cmf = 0x78u8;
    let flevel = match level {
        Level::Fast => 0,
        Level::Balanced => 2,
        Level::Small => 3,
    };
    let flg = flevel << 6;
    let flg = flg + (31 - ((u16::from(cmf) << 8 | u16::from(flg)) % 31) as u8) % 31;
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.extend_from_slice(&[cmf, flg]);
    let mut out = deflate::deflate_into(data, level, out);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Decompresses a zlib stream, producing at most `limit` bytes; the
/// checksum is verified. Preset dictionaries are not supported.
pub fn zlib_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let header = data.get(..2).ok_or(Error::Truncated)?;
    let (cmf, flg) = (header[0], header[1]);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(Error::Invalid("zlib header"));
    }
    if flg & 0x20 != 0 {
        return Err(Error::Invalid("zlib preset dictionary"));
    }
    let mut out = Vec::new();
    let used = inflate::inflate_into(&data[2..], &mut out, limit)?;
    let trailer = data.get(2 + used..2 + used + 4).ok_or(Error::Truncated)?;
    if u32::from_be_bytes(trailer.try_into().expect("four bytes")) != adler32(&out) {
        return Err(Error::Checksum);
    }
    Ok(out)
}
