//! The sRGB transfer curve (IEC 61966-2-1) from linear floats to 8-bit
//! codes, rounded to nearest: code `n` covers the linear values between
//! the decodings of `n - 0.5` and `n + 0.5`, so a binary search over those
//! 255 boundaries gives the exact rounding without a power per sample.
use std::sync::OnceLock;

/// The linear value where code `n` begins, for `n` in 1..=255.
fn thresholds() -> &'static [f32; 255] {
    static TABLE: OnceLock<[f32; 255]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let encoded = (i as f64 + 0.5) / 255.0;
            let linear = if encoded <= 0.040_45 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            linear as f32
        })
    })
}

/// `linear` (clamped to 0..=1) as an sRGB-coded byte; NaN is 0.
pub fn encode8(linear: f32) -> u8 {
    thresholds().partition_point(|&t| t <= linear) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_curve() {
        for i in 0..=1000 {
            let v = i as f32 / 1000.0;
            let x = f64::from(v);
            let encoded = if x <= 0.003_130_8 {
                x * 12.92
            } else {
                1.055 * x.powf(1.0 / 2.4) - 0.055
            };
            let expected = (encoded * 255.0).round() as i32;
            assert!((i32::from(encode8(v)) - expected).abs() <= 1, "{v}");
        }
        assert_eq!(encode8(-1.0), 0);
        assert_eq!(encode8(f32::NAN), 0);
        assert_eq!(encode8(7.0), 255);
    }
}
