//! Upsampling and color conversion of output rows.
//!
//! A component sample at an output pixel is the bilinear interpolation
//! of its plane at the pixel's centre, (x + 0.5) · num/den − 0.5, clamped
//! to the samples covering the picture: for 2:1 subsampling that is
//! libjpeg's "fancy" triangle filter, weights 3/4 and 1/4. Scaled decodes
//! repeat samples where libjpeg does (1/8 scale, ratios other than 2:1).
//! Color conversion is the JFIF YCbCr matrix (or Adobe's RGB, CMYK and
//! YCCK) in f32, rounded to the nearest 8-bit value.

/// How plane samples map onto output pixels along one axis.
pub(crate) enum Axis {
    /// One plane sample per pixel.
    Identity,
    /// Pixel `i` takes sample `i * num / den`.
    Replicate { num: usize, den: usize },
    /// Pixel `i` blends samples `first[i]` and `first[i] + 1` with
    /// weight `weight[i]` on the second (the second index clamped to the
    /// plane).
    Linear {
        first: Vec<u32>,
        second: Vec<u32>,
        weight: Vec<f32>,
    },
}

impl Axis {
    /// The mapping of `count` output pixels onto a plane of `limit`
    /// samples, `valid` of them covering the picture, at `num/den` plane
    /// samples a pixel.
    pub fn new(
        count: usize,
        num: usize,
        den: usize,
        valid: usize,
        limit: usize,
        replicate: bool,
    ) -> Axis {
        if num == den {
            return Axis::Identity;
        }
        if replicate {
            return Axis::Replicate { num, den };
        }
        let last = valid.max(1) - 1;
        let mut first = Vec::with_capacity(count);
        let mut second = Vec::with_capacity(count);
        let mut weight = Vec::with_capacity(count);
        for i in 0..count {
            let s = ((i as f64 + 0.5) * num as f64 / den as f64 - 0.5).clamp(0.0, last as f64);
            let a = s.floor() as usize;
            first.push(a as u32);
            second.push((a + 1).min(limit - 1) as u32);
            weight.push((s - a as f64) as f32);
        }
        Axis::Linear {
            first,
            second,
            weight,
        }
    }

    /// The plane samples output pixels up to `count` read: one past the
    /// highest index.
    pub fn reach(&self, count: usize, limit: usize) -> usize {
        match self {
            Axis::Identity => count.min(limit),
            Axis::Replicate { num, den } => ((count.max(1) - 1) * num / den + 1).min(limit),
            Axis::Linear { second, .. } => {
                second.iter().map(|&s| s as usize + 1).max().unwrap_or(0)
            }
        }
    }
}

/// How samples turn into RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    Gray,
    YCbCr,
    Rgb,
    /// YCCK (Adobe transform 2): YCbCr inverted, times K.
    Ycck,
    /// Adobe's inverted CMYK (transform 0): R = C·K/255.
    AdobeCmyk,
    /// Plain CMYK: R = (255 − C)(255 − K)/255.
    Cmyk,
}

impl Model {
    /// The model of a frame with `ids` component IDs and the Adobe
    /// segment's transform, if any.
    pub fn of(ids: &[u8], adobe: Option<u8>) -> Model {
        match ids.len() {
            1 => Model::Gray,
            3 if adobe == Some(0) || ids == b"RGB" => Model::Rgb,
            3 => Model::YCbCr,
            _ => match adobe {
                Some(2) => Model::Ycck,
                Some(0) => Model::AdobeCmyk,
                _ => Model::Cmyk,
            },
        }
    }
}

#[inline(always)]
fn byte(v: f32) -> u8 {
    (v + 0.5).clamp(0.0, 255.0) as u8
}

/// Converts one row: `rows` holds each component's samples, `out` takes
/// `step` bytes a pixel (1 grey, 3 RGB, 4 RGBA with opaque alpha).
pub(crate) fn convert(model: Model, rows: &[Vec<f32>], width: usize, out: &mut [u8], step: usize) {
    let out = &mut out[..width * step];
    match model {
        Model::Gray => {
            let y = &rows[0][..width];
            if step == 1 {
                for (o, &v) in out.iter_mut().zip(y) {
                    *o = byte(v);
                }
            } else {
                for (o, &v) in out.chunks_exact_mut(step).zip(y) {
                    let v = byte(v);
                    o[..3].fill(v);
                }
            }
        }
        Model::YCbCr => {
            let (y, cb, cr) = (&rows[0][..width], &rows[1][..width], &rows[2][..width]);
            for (((o, &y), &cb), &cr) in out.chunks_exact_mut(step).zip(y).zip(cb).zip(cr) {
                let (cb, cr) = (cb - 128.0, cr - 128.0);
                o[0] = byte(y + 1.402 * cr);
                o[1] = byte(y - 0.344_136 * cb - 0.714_136 * cr);
                o[2] = byte(y + 1.772 * cb);
            }
        }
        Model::Rgb => {
            let (r, g, b) = (&rows[0][..width], &rows[1][..width], &rows[2][..width]);
            for (((o, &r), &g), &b) in out.chunks_exact_mut(step).zip(r).zip(g).zip(b) {
                o[0] = byte(r);
                o[1] = byte(g);
                o[2] = byte(b);
            }
        }
        Model::Ycck | Model::AdobeCmyk | Model::Cmyk => {
            let (a, b, c, k) = (
                &rows[0][..width],
                &rows[1][..width],
                &rows[2][..width],
                &rows[3][..width],
            );
            for (i, o) in out.chunks_exact_mut(step).enumerate() {
                let (a, b, c, k) = (a[i], b[i], c[i], k[i]);
                let rgb = match model {
                    Model::Ycck => {
                        let (cb, cr) = (b - 128.0, c - 128.0);
                        let red = (a + 1.402 * cr).clamp(0.0, 255.0);
                        let green = (a - 0.344_136 * cb - 0.714_136 * cr).clamp(0.0, 255.0);
                        let blue = (a + 1.772 * cb).clamp(0.0, 255.0);
                        [
                            (255.0 - red) * k / 255.0,
                            (255.0 - green) * k / 255.0,
                            (255.0 - blue) * k / 255.0,
                        ]
                    }
                    Model::AdobeCmyk => [a * k / 255.0, b * k / 255.0, c * k / 255.0],
                    _ => {
                        let white = (255.0 - k) / 255.0;
                        [
                            (255.0 - a) * white,
                            (255.0 - b) * white,
                            (255.0 - c) * white,
                        ]
                    }
                };
                o[0] = byte(rgb[0]);
                o[1] = byte(rgb[1]);
                o[2] = byte(rgb[2]);
            }
        }
    }
    if step == 4 {
        for o in out.as_chunks_mut::<4>().0 {
            o[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_to_one_is_the_triangle_filter() {
        let Axis::Linear {
            first,
            second,
            weight,
        } = Axis::new(6, 1, 2, 3, 4, false)
        else {
            panic!("linear");
        };
        // Pixel 0 sits on sample 0; then 3/4 and 1/4 either side of each
        // boundary; the last pixel repeats the last valid sample.
        assert_eq!(first, vec![0, 0, 0, 1, 1, 2]);
        assert_eq!(second, vec![1, 1, 1, 2, 2, 3]);
        assert_eq!(weight, vec![0.0, 0.25, 0.75, 0.25, 0.75, 0.0]);
    }

    #[test]
    fn models_follow_ids_and_the_adobe_transform() {
        assert_eq!(Model::of(b"\x01\x02\x03", None), Model::YCbCr);
        assert_eq!(Model::of(b"RGB", None), Model::Rgb);
        assert_eq!(Model::of(b"\x01\x02\x03", Some(0)), Model::Rgb);
        assert_eq!(Model::of(b"\x01\x02\x03\x04", Some(2)), Model::Ycck);
        assert_eq!(Model::of(b"\x01\x02\x03\x04", None), Model::Cmyk);
    }
}
