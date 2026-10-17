//! Scanline filters (PNG §7.3, §9): each row is stored as its difference
//! from a predictor over the pixel to the left (`a`), above (`b`) and above
//! left (`c`), `bpp` bytes apart.

/// None, Sub, Up, Average, Paeth.
pub const TYPES: u8 = 5;

/// The Paeth predictor, without branches: the nearest of `a`, `b` and `c`
/// to `a + b - c`, ties going to `a`, then `b`.
#[inline]
pub fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let pa = (ib - ic).abs();
    let pb = (ia - ic).abs();
    let pc = (ia + ib - 2 * ic).abs();
    let b_or_c = if pb <= pc { b } else { c };
    if pa <= pb && pa <= pc { a } else { b_or_c }
}

/// Paeth in its threshold form, equal to [`paeth`] for every input (see
/// the tests) with a shorter dependency chain from one pixel to the next:
/// with `lo`/`hi` the smaller/larger of a and b and t = 3c - a - b, the
/// prediction is hi when t <= lo, lo when hi <= t, else c.
#[inline]
fn paeth_threshold(a: i16, b: i16, c: i16) -> i16 {
    let t = 3 * c - a - b;
    let (lo, hi) = (a.min(b), a.max(b));
    if t <= lo {
        hi
    } else if hi <= t {
        lo
    } else {
        c
    }
}

/// Reverses filter `kind` on `row` in place; `previous` is the decoded row
/// above (zeros for the first row).
pub fn unfilter(kind: u8, row: &mut [u8], previous: &[u8], bpp: usize) -> Result<(), ()> {
    match kind {
        0 => {}
        2 => {
            for (x, &b) in row.iter_mut().zip(previous) {
                *x = x.wrapping_add(b);
            }
        }
        1 | 3 | 4 => match bpp {
            1 => unfilter_pixels::<1>(kind, row, previous),
            2 => unfilter_pixels::<2>(kind, row, previous),
            3 => unfilter_pixels::<3>(kind, row, previous),
            4 => unfilter_pixels::<4>(kind, row, previous),
            6 => unfilter_pixels::<6>(kind, row, previous),
            _ => unfilter_pixels::<8>(kind, row, previous),
        },
        _ => return Err(()),
    }
    Ok(())
}

/// Sub, Average and Paeth a pixel (`N` bytes) at a time: the bytes of one
/// pixel are independent, so each step works on all of them together.
fn unfilter_pixels<const N: usize>(kind: u8, row: &mut [u8], previous: &[u8]) {
    let (pixels, _) = row.as_chunks_mut::<N>();
    let (above, _) = previous.as_chunks::<N>();
    let mut a = [0u8; N];
    match kind {
        1 => {
            for x in pixels {
                for k in 0..N {
                    x[k] = x[k].wrapping_add(a[k]);
                }
                a = *x;
            }
        }
        3 => {
            for (x, b) in pixels.iter_mut().zip(above) {
                for k in 0..N {
                    x[k] = x[k].wrapping_add(((u16::from(a[k]) + u16::from(b[k])) / 2) as u8);
                }
                a = *x;
            }
        }
        _ => {
            let mut a = [0i16; N];
            let mut c = [0i16; N];
            for (x, b8) in pixels.iter_mut().zip(above) {
                let b = b8.map(i16::from);
                for k in 0..N {
                    let predicted = paeth_threshold(a[k], b[k], c[k]);
                    a[k] = (i16::from(x[k]) + predicted) & 0xFF;
                    x[k] = a[k] as u8;
                }
                c = b;
            }
        }
    }
}

/// Writes `row` filtered with `kind` into `out` (the same length).
pub fn filter(kind: u8, row: &[u8], previous: &[u8], bpp: usize, out: &mut [u8]) {
    match kind {
        0 => out.copy_from_slice(row),
        1 => {
            out[..bpp].copy_from_slice(&row[..bpp]);
            for i in bpp..row.len() {
                out[i] = row[i].wrapping_sub(row[i - bpp]);
            }
        }
        2 => {
            for i in 0..row.len() {
                out[i] = row[i].wrapping_sub(previous[i]);
            }
        }
        3 => {
            for i in 0..row.len() {
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                out[i] = row[i].wrapping_sub(((u16::from(a) + u16::from(previous[i])) / 2) as u8);
            }
        }
        _ => {
            for i in 0..bpp {
                out[i] = row[i].wrapping_sub(previous[i]);
            }
            for i in bpp..row.len() {
                out[i] = row[i].wrapping_sub(paeth(row[i - bpp], previous[i], previous[i - bpp]));
            }
        }
    }
}

/// libpng's heuristic cost of a filtered row: the sum of its bytes taken as
/// signed magnitudes.
pub fn cost(filtered: &[u8]) -> u64 {
    filtered
        .iter()
        .map(|&b| u64::from((b as i8).unsigned_abs()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filter_inverts() {
        let previous: Vec<u8> = (0..48u32).map(|i| (i * 37 % 251) as u8).collect();
        let row: Vec<u8> = (0..48u32).map(|i| (i * 91 % 253) as u8).collect();
        for bpp in [1, 2, 3, 4, 6, 8] {
            for kind in 0..TYPES {
                let mut out = vec![0; row.len()];
                filter(kind, &row, &previous, bpp, &mut out);
                unfilter(kind, &mut out, &previous, bpp).unwrap();
                assert_eq!(out, row, "kind {kind} bpp {bpp}");
            }
        }
        assert!(unfilter(5, &mut [0], &[0], 1).is_err());
    }

    #[test]
    fn the_threshold_form_is_paeth() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                for c in 0..=255u8 {
                    let t = paeth_threshold(a.into(), b.into(), c.into());
                    assert_eq!(t as u8, paeth(a, b, c), "{a} {b} {c}");
                }
            }
        }
    }

    #[test]
    fn paeth_prefers_a_then_b() {
        assert_eq!(paeth(10, 10, 10), 10);
        assert_eq!(paeth(1, 200, 1), 200);
        assert_eq!(paeth(200, 1, 1), 200);
        assert_eq!(paeth(5, 9, 7), 7);
    }
}
