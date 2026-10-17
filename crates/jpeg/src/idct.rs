//! The inverse DCT: dequantized coefficients to 8-bit samples.
//!
//! Full size it is a separable float transform, columns then rows, each
//! 8-point pass split into its even half (a 4-point IDCT: two products
//! and a rotation) and its odd half (four products each), worked on eight
//! columns at once so the compiler keeps them in vector registers. Every
//! product uses the exact cosine, so the result is the mathematical IDCT
//! to within float rounding: f(x) = Σ c(k)/2 F(k) cos((2x+1)kπ/16),
//! c(0) = 1/√2.
//!
//! A scaled IDCT gives N < 8 points a block, each the mean of the 8/N
//! full-size points it covers before rounding (a box-filtered reduction,
//! as libjpeg's reduced IDCTs compute in fixed point): the same separable
//! transform with the cosines averaged first.

/// cos(mπ/16) / 2.
const H1: f32 = 0.490_392_63;
const H2: f32 = 0.461_939_75;
const H3: f32 = 0.415_734_8;
const H5: f32 = 0.277_785_12;
const H6: f32 = 0.191_341_71;
const H7: f32 = 0.097_545_16;
/// c(0)/2 = 1 / (2√2).
const H0: f32 = 0.353_553_38;

/// The 1-D IDCT of the eight rows of `input` (frequencies) into `out`
/// (positions), each row holding eight independent lanes.
#[inline(always)]
fn pass(input: &[f32; 64], out: &mut [f32; 64]) {
    for j in 0..8 {
        let x = |k: usize| input[k * 8 + j];
        // Even half.
        let (p0, p4) = (x(0) * H0, x(4) * H0);
        let (e0, e1) = (p0 + p4, p0 - p4);
        let t2 = x(2) * H2 + x(6) * H6;
        let t3 = x(2) * H6 - x(6) * H2;
        let (a0, a1, a2, a3) = (e0 + t2, e1 + t3, e1 - t3, e0 - t2);
        // Odd half.
        let (x1, x3, x5, x7) = (x(1), x(3), x(5), x(7));
        let o0 = x1 * H1 + x3 * H3 + x5 * H5 + x7 * H7;
        let o1 = x1 * H3 - x3 * H7 - x5 * H1 - x7 * H5;
        let o2 = x1 * H5 - x3 * H1 + x5 * H7 + x7 * H3;
        let o3 = x1 * H7 - x3 * H5 + x5 * H3 - x7 * H1;
        out[j] = a0 + o0;
        out[8 + j] = a1 + o1;
        out[16 + j] = a2 + o2;
        out[24 + j] = a3 + o3;
        out[32 + j] = a3 - o3;
        out[40 + j] = a2 - o2;
        out[48 + j] = a1 - o1;
        out[56 + j] = a0 - o0;
    }
}

#[inline(always)]
fn transpose(m: &[f32; 64]) -> [f32; 64] {
    std::array::from_fn(|i| m[(i % 8) * 8 + i / 8])
}

/// A level-shifted sample rounded to the nearest byte: floor(v + 128.5)
/// clamped to 0..=255.
#[inline(always)]
pub(crate) fn sample(v: f32) -> u8 {
    (v + 128.5).clamp(0.0, 255.0) as u8
}

/// The 8×8 IDCT of `coefficients` (natural order) times `quant` into
/// `out`, rows `stride` bytes apart.
#[inline]
pub(crate) fn idct8(coefficients: &[i16], quant: &[f32; 64], out: &mut [u8], stride: usize) {
    let coefficients: &[i16; 64] = coefficients.try_into().expect("64 coefficients");
    if coefficients[1..].iter().all(|&c| c == 0) {
        // DC alone: a flat block, (c(0)/2)^2 * F = F / 8.
        let value = sample(f32::from(coefficients[0]) * quant[0] * 0.125);
        for row in out.chunks_mut(stride).take(8) {
            row[..8].fill(value);
        }
        return;
    }
    let input: [f32; 64] = std::array::from_fn(|i| f32::from(coefficients[i]) * quant[i]);
    let mut columns = [0.0f32; 64];
    pass(&input, &mut columns);
    // columns[y][u]: transposed, the same pass transforms the rows.
    let mut rows = [0.0f32; 64];
    pass(&transpose(&columns), &mut rows);
    // rows[x][y].
    for (y, line) in out.chunks_mut(stride).take(8).enumerate() {
        for (x, out) in line[..8].iter_mut().enumerate() {
            *out = sample(rows[x * 8 + y]);
        }
    }
}

/// Averaged cosine tables for N = 1, 2 and 4 points: `[level][x][k]` is
/// c(k)/2 times the mean of cos((2n+1)kπ/16) over the 8/N full-size
/// points n that point x covers (N = 1 << level).
pub(crate) struct Scaled {
    tables: [[[f64; 8]; 4]; 3],
}

impl Scaled {
    pub fn new() -> Self {
        let mut tables = [[[0.0f64; 8]; 4]; 3];
        for (level, table) in tables.iter_mut().enumerate() {
            let n = 1usize << level;
            let span = 8 / n;
            for (x, row) in table.iter_mut().take(n).enumerate() {
                for (k, value) in row.iter_mut().enumerate() {
                    let weight = if k == 0 {
                        std::f64::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    let mean: f64 = (0..span)
                        .map(|j| {
                            (((2 * (x * span + j) + 1) * k) as f64 * std::f64::consts::PI / 16.0)
                                .cos()
                        })
                        .sum::<f64>()
                        / span as f64;
                    *value = weight * mean / 2.0;
                }
            }
        }
        Scaled { tables }
    }

    /// The `n`×`n` scaled IDCT (n = 1, 2 or 4) of `coefficients` times
    /// `quant` into `out`, rows `stride` bytes apart.
    pub fn idct(
        &self,
        n: usize,
        coefficients: &[i16],
        quant: &[f32; 64],
        out: &mut [u8],
        stride: usize,
    ) {
        if n == 1 {
            // The DC alone, exactly: (c(0)/2)^2 * F = F / 8.
            out[0] = sample(f32::from(coefficients[0]) * quant[0] * 0.125);
            return;
        }
        // In f64: a reduced block is a handful of points, and the double
        // sums keep exact halves exact where they round.
        let table = &self.tables[n.trailing_zeros() as usize];
        let input: [f64; 64] =
            std::array::from_fn(|i| f64::from(coefficients[i]) * f64::from(quant[i]));
        // Rows first: across[v][x] = Σ_u F[v][u] T[x][u].
        let mut across = [[0.0f64; 4]; 8];
        for (v, row) in across.iter_mut().enumerate() {
            for (x, value) in row.iter_mut().take(n).enumerate() {
                *value = (0..8).map(|u| input[v * 8 + u] * table[x][u]).sum();
            }
        }
        for (y, line) in out.chunks_mut(stride).take(n).enumerate() {
            for (x, out) in line[..n].iter_mut().enumerate() {
                let value: f64 = (0..8).map(|v| table[y][v] * across[v][x]).sum();
                *out = (value + 128.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The IDCT in f64 straight from its definition.
    fn reference(coefficients: &[i16; 64]) -> [f64; 64] {
        std::array::from_fn(|i| {
            let (y, x) = (i / 8, i % 8);
            let mut sum = 0.0;
            for v in 0..8 {
                for u in 0..8 {
                    let cu = if u == 0 {
                        std::f64::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    let cv = if v == 0 {
                        std::f64::consts::FRAC_1_SQRT_2
                    } else {
                        1.0
                    };
                    sum += cu * cv / 4.0
                        * f64::from(coefficients[v * 8 + u])
                        * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()
                        * (((2 * y + 1) * v) as f64 * std::f64::consts::PI / 16.0).cos();
                }
            }
            sum
        })
    }

    #[test]
    fn matches_the_definition() {
        let quant = [1.0f32; 64];
        let mut seed = 7u32;
        let mut exact = 0;
        for trial in 0..2000 {
            let coefficients: [i16; 64] = std::array::from_fn(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let range = if i == 0 {
                    1024
                } else if trial % 2 == 0 {
                    64
                } else {
                    400 / (1 + i as i32)
                };
                ((seed >> 8) as i32 % (2 * range + 1) - range) as i16
            });
            let mut out = [0u8; 64];
            idct8(&coefficients, &quant, &mut out, 8);
            let expected = reference(&coefficients);
            for i in 0..64 {
                let want = (expected[i] + 128.5).clamp(0.0, 255.0).floor() as i32;
                let diff = (i32::from(out[i]) - want).abs();
                assert!(
                    diff <= 1,
                    "trial {trial} sample {i}: {} vs {}",
                    out[i],
                    expected[i] + 128.0
                );
                exact += usize::from(diff == 0);
            }
        }
        assert!(exact >= 2000 * 64 * 9999 / 10000, "{exact}");
    }

    #[test]
    fn scaled_points_are_means_of_full_size_points() {
        let quant = [1.0f32; 64];
        let coefficients: [i16; 64] =
            std::array::from_fn(|i| ((i * 37 % 23) as i16 - 11) * if i < 10 { 9 } else { 1 });
        let full = reference(&coefficients);
        let scaled = Scaled::new();
        for n in [1usize, 2, 4] {
            let mut out = [0u8; 16];
            scaled.idct(n, &coefficients, &quant, &mut out, 4);
            let span = 8 / n;
            for y in 0..n {
                for x in 0..n {
                    let mean: f64 = (0..span * span)
                        .map(|i| full[(y * span + i / span) * 8 + x * span + i % span])
                        .sum::<f64>()
                        / (span * span) as f64;
                    let want = (mean + 128.5).clamp(0.0, 255.0).floor() as i32;
                    assert!((i32::from(out[y * 4 + x]) - want).abs() <= 1, "n {n}");
                }
            }
        }
    }
}
