//! DEFLATE's format constants (RFC 1951 §3.2.5–3.2.7).

/// The base match length of each length symbol 257..=285.
pub const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits after each length symbol.
pub const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// The base distance of each distance symbol 0..=29.
pub const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits after each distance symbol.
pub const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order code length code lengths are stored in.
pub const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Literal/length symbols (0..=285 are valid; 286 and 287 only fill the fixed code).
pub const LITLEN_SYMBOLS: usize = 288;
/// Distance symbols (0..=29 are valid; 30 and 31 only fill the fixed code).
pub const DIST_SYMBOLS: usize = 32;
/// The end-of-block symbol.
pub const END_OF_BLOCK: usize = 256;
/// The longest code a literal/length or distance code may have.
pub const MAX_BITS: u8 = 15;
/// The longest code a code length code may have.
pub const MAX_CODE_LENGTH_BITS: u8 = 7;
/// The history a distance may reach back into.
pub const WINDOW: usize = 32_768;
/// The shortest and longest matches.
pub const MIN_MATCH: usize = 3;
/// See [`MIN_MATCH`].
pub const MAX_MATCH: usize = 258;

/// The fixed code's literal/length code lengths.
pub fn fixed_litlen_lengths() -> [u8; LITLEN_SYMBOLS] {
    let mut lengths = [8u8; LITLEN_SYMBOLS];
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths
}

/// The fixed code's distance code lengths.
pub fn fixed_dist_lengths() -> [u8; DIST_SYMBOLS] {
    [5; DIST_SYMBOLS]
}

/// The length symbol index (0..29, add 257) of each match length minus 3.
pub const LENGTH_SYMBOL: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut symbol = 0;
    while symbol < 29 {
        let start = LENGTH_BASE[symbol] as usize - 3;
        let end = if symbol == 28 {
            256
        } else {
            LENGTH_BASE[symbol + 1] as usize - 3
        };
        let mut length = start;
        while length < end {
            table[length] = symbol as u8;
            length += 1;
        }
        symbol += 1;
    }
    table
};

/// The distance symbol of `distance` (1..=32768).
#[inline]
pub fn dist_symbol(distance: usize) -> usize {
    if distance <= 256 {
        DIST_SYMBOL_SMALL[distance - 1] as usize
    } else {
        DIST_SYMBOL_LARGE[(distance - 1) >> 7] as usize
    }
}

/// Distance symbols of distances 1..=256, by `distance - 1`.
const DIST_SYMBOL_SMALL: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut d = 0;
    while d < 256 {
        table[d] = slow_dist_symbol(d + 1);
        d += 1;
    }
    table
};

/// Distance symbols of distances 257..=32768, by `(distance - 1) >> 7`
/// (every symbol from 16 on spans a multiple of 128 distances).
const DIST_SYMBOL_LARGE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 2;
    while i < 256 {
        table[i] = slow_dist_symbol((i << 7) + 1);
        i += 1;
    }
    table
};

const fn slow_dist_symbol(distance: usize) -> u8 {
    let mut symbol = 29;
    while DIST_BASE[symbol] as usize > distance {
        symbol -= 1;
    }
    symbol as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_tables_invert_the_bases() {
        for length in MIN_MATCH..=MAX_MATCH {
            let s = LENGTH_SYMBOL[length - 3] as usize;
            let base = LENGTH_BASE[s] as usize;
            assert!(
                base <= length && length - base < 1 << LENGTH_EXTRA[s],
                "{length}"
            );
            if length == 258 {
                assert_eq!(s, 28);
            }
        }
        for distance in 1..=WINDOW {
            let s = dist_symbol(distance);
            let base = DIST_BASE[s] as usize;
            assert!(
                base <= distance && distance - base < 1 << DIST_EXTRA[s],
                "{distance}"
            );
        }
    }
}
