//! The encoder's transform of one MCU row: color conversion, chroma
//! downsampling, the forward DCT and quantization.
//!
//! Samples convert to level-shifted YCbCr (JFIF) in f32; edges repeat the
//! last column and row out to whole MCUs; chroma averages 2×1 or 2×2
//! luma-resolution samples. The forward DCT is the exact separable float
//! transform, F(u) = c(u)/2 Σ f(x) cos((2x+1)uπ/16), as the decoder's
//! inverse in reverse: even and odd halves over eight lanes at once.
use super::Frame;
use std::ops::Range;

/// cos(mπ/16) / 2, and c(0)/2.
const H1: f32 = 0.490_392_63;
const H2: f32 = 0.461_939_75;
const H3: f32 = 0.415_734_8;
const H5: f32 = 0.277_785_12;
const H6: f32 = 0.191_341_71;
const H7: f32 = 0.097_545_16;
const H0: f32 = 0.353_553_38;

/// The 1-D forward DCT of the eight rows of `input` (positions) into
/// `out` (frequencies), eight independent lanes a row.
#[inline(always)]
fn pass(input: &[f32; 64], out: &mut [f32; 64]) {
    for j in 0..8 {
        let s = |x: usize| input[x * 8 + j];
        let (s07, s16, s25, s34) = (s(0) + s(7), s(1) + s(6), s(2) + s(5), s(3) + s(4));
        let (d0, d1, d2, d3) = (s(0) - s(7), s(1) - s(6), s(2) - s(5), s(3) - s(4));
        let (a, b) = (s07 + s34, s16 + s25);
        let (c, d) = (s07 - s34, s16 - s25);
        out[j] = (a + b) * H0;
        out[32 + j] = (a - b) * H0;
        out[16 + j] = c * H2 + d * H6;
        out[48 + j] = c * H6 - d * H2;
        out[8 + j] = d0 * H1 + d1 * H3 + d2 * H5 + d3 * H7;
        out[24 + j] = d0 * H3 - d1 * H7 - d2 * H1 - d3 * H5;
        out[40 + j] = d0 * H5 - d1 * H1 + d2 * H7 + d3 * H3;
        out[56 + j] = d0 * H7 - d1 * H5 + d2 * H3 - d3 * H1;
    }
}

#[inline(always)]
fn transpose(m: &[f32; 64]) -> [f32; 64] {
    std::array::from_fn(|i| m[(i % 8) * 8 + i / 8])
}

/// The quantized DCT of an 8×8 block of level-shifted samples into
/// `out` (natural order). `reciprocals` holds 1/Q transposed, [u][v].
#[inline]
fn fdct_quantize(block: &[f32; 64], reciprocals: &[f32; 64], out: &mut [i16]) {
    let mut columns = [0.0f32; 64];
    pass(block, &mut columns);
    let mut rows = [0.0f32; 64];
    pass(&transpose(&columns), &mut rows);
    // rows[u][v] is F(v, u).
    for (i, out) in out[..64].iter_mut().enumerate() {
        let (v, u) = (i / 8, i % 8);
        let t = u * 8 + v;
        *out = (rows[t] * reciprocals[t]).round().clamp(-2047.0, 2047.0) as i16;
    }
}

/// Level-shifted component planes of one MCU row.
struct Planes {
    /// Luma-resolution width (whole MCUs) and rows.
    width: usize,
    rows: usize,
    planes: Vec<Vec<f32>>,
}

/// Color-converts MCUs `mcus` of MCU row `row` of `samples` (`channels`
/// a pixel) into level-shifted planes at luma resolution, edges repeated.
fn convert(
    frame: &Frame,
    samples: &[u8],
    channels: usize,
    row: usize,
    mcus: &Range<usize>,
) -> Planes {
    let x0 = mcus.start * frame.mcu_width;
    let width = mcus.len() * frame.mcu_width;
    let rows = frame.mcu_height;
    let count = frame.components.len();
    let mut planes = vec![vec![0.0f32; width * rows]; count];
    let image_width = frame.width;
    // Columns inside the picture; the rest repeat the last.
    let inside = (image_width - x0).min(width);
    for y in 0..rows {
        let source_y = (row * rows + y).min(frame.height - 1);
        let line = &samples[(source_y * image_width + x0) * channels
            ..(source_y * image_width + x0 + inside) * channels];
        let at = y * width;
        if count == 1 {
            let plane = &mut planes[0][at..at + width];
            for (out, pixel) in plane.iter_mut().zip(line.chunks_exact(channels)) {
                *out = f32::from(pixel[0]) - 128.0;
            }
            let last = plane[inside - 1];
            plane[inside..].fill(last);
        } else {
            let [yp, cb, cr] = &mut planes[..] else {
                unreachable!("three planes")
            };
            let (yp, cb, cr) = (
                &mut yp[at..at + width],
                &mut cb[at..at + width],
                &mut cr[at..at + width],
            );
            for (x, pixel) in line.chunks_exact(channels).enumerate() {
                let (r, g, b) = (
                    f32::from(pixel[0]),
                    f32::from(pixel[1]),
                    f32::from(pixel[2]),
                );
                yp[x] = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
                cb[x] = -0.168_736 * r - 0.331_264 * g + 0.5 * b;
                cr[x] = 0.5 * r - 0.418_688 * g - 0.081_312 * b;
            }
            for plane in [yp, cb, cr] {
                let last = plane[inside - 1];
                plane[inside..].fill(last);
            }
        }
    }
    Planes {
        width,
        rows,
        planes,
    }
}

/// The quantized coefficients of MCUs `mcus` of MCU row `row`, in MCU
/// order. Any range gives the same coefficients for an MCU.
pub(crate) fn transform_mcus(
    frame: &Frame,
    samples: &[u8],
    channels: usize,
    row: usize,
    mcus: Range<usize>,
) -> Vec<i16> {
    let planes = convert(frame, samples, channels, row, &mcus);
    let mut out = vec![0i16; mcus.len() * frame.blocks_per_mcu * 64];
    let mut block = [0.0f32; 64];
    // Downsampled chroma planes (shared by Cb and Cr's shape).
    let (h, v) = (frame.components[0].h, frame.components[0].v);
    let chroma: Vec<Vec<f32>> = planes.planes[1..]
        .iter()
        .map(|plane| downsample(plane, planes.width, planes.rows, h, v))
        .collect();
    let chroma_width = planes.width / h;
    let mut at = 0;
    for m in 0..mcus.len() {
        for (c, component) in frame.components.iter().enumerate() {
            let (plane, width) = if c == 0 {
                (&planes.planes[0], planes.width)
            } else {
                (&chroma[c - 1], chroma_width)
            };
            let reciprocals = &frame.reciprocals[usize::from(c > 0)];
            for vi in 0..component.v {
                for hi in 0..component.h {
                    let x0 = (m * component.h + hi) * 8;
                    for y in 0..8 {
                        let start = (vi * 8 + y) * width + x0;
                        block[y * 8..y * 8 + 8].copy_from_slice(&plane[start..start + 8]);
                    }
                    fdct_quantize(&block, reciprocals, &mut out[at..at + 64]);
                    at += 64;
                }
            }
        }
    }
    out
}

/// `plane` (`width` × `rows`) averaged over `h` × `v` cells.
fn downsample(plane: &[f32], width: usize, rows: usize, h: usize, v: usize) -> Vec<f32> {
    if h == 1 && v == 1 {
        return plane.to_vec();
    }
    let (out_width, out_rows) = (width / h, rows / v);
    let scale = 1.0 / (h * v) as f32;
    let mut out = vec![0.0f32; out_width * out_rows];
    for y in 0..out_rows {
        let line = &mut out[y * out_width..(y + 1) * out_width];
        for dy in 0..v {
            let source = &plane[(y * v + dy) * width..(y * v + dy + 1) * width];
            if h == 2 {
                for (o, pair) in line.iter_mut().zip(source.as_chunks::<2>().0) {
                    *o += pair[0] + pair[1];
                }
            } else {
                for (o, &s) in line.iter_mut().zip(source) {
                    *o += s;
                }
            }
        }
        for o in line.iter_mut() {
            *o *= scale;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forward_dct_matches_its_definition() {
        let block: [f32; 64] = std::array::from_fn(|i| ((i * 29 % 97) as f32) - 48.0);
        let ones = [1.0f32; 64];
        let mut out = [0i16; 64];
        // Unquantized (Q = 1), doubled to see half steps.
        let scaled: [f32; 64] = std::array::from_fn(|i| block[i] * 2.0);
        fdct_quantize(&scaled, &ones, &mut out);
        for v in 0..8 {
            for u in 0..8 {
                let mut sum = 0.0f64;
                for y in 0..8 {
                    for x in 0..8 {
                        sum += f64::from(block[y * 8 + x])
                            * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()
                            * (((2 * y + 1) * v) as f64 * std::f64::consts::PI / 16.0).cos();
                    }
                }
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
                let expected = (cu * cv / 4.0 * sum * 2.0).round();
                assert!(
                    (f64::from(out[v * 8 + u]) - expected).abs() <= 1.0,
                    "({v}, {u})"
                );
            }
        }
    }
}
