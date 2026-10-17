//! Why a JPEG could not be decoded or encoded.
use std::fmt;

/// Why a JPEG could not be decoded or encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file breaks the format; `at` is the byte offset where it was
    /// found.
    Corrupt {
        /// What is wrong.
        what: &'static str,
        /// Byte offset into the file.
        at: usize,
    },
    /// A valid JPEG this decoder does not read (lossless, arithmetic
    /// coding, 12-bit samples...).
    Unsupported(&'static str),
    /// The request itself is wrong (scale, quality, image shape).
    Invalid(&'static str),
    /// The image has more pixels than the caller allows.
    TooLarge,
}

impl Error {
    pub(crate) fn corrupt(what: &'static str, at: usize) -> Self {
        Error::Corrupt { what, at }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Corrupt { what, at } => write!(f, "invalid JPEG at byte {at}: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported JPEG: {what}"),
            Error::Invalid(what) => write!(f, "invalid JPEG request: {what}"),
            Error::TooLarge => write!(f, "JPEG image exceeds the pixel limit"),
        }
    }
}

impl std::error::Error for Error {}
