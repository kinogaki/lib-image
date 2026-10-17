//! IEEE 754 binary16 conversions, exact both ways: every half is a float,
//! and a float rounds to the nearest half, ties to even, overflowing to
//! infinity; NaNs stay NaNs (quiet, top payload bits kept).

/// The float a half's bits stand for.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from(bits >> 10) & 0x1F;
    let mantissa = u32::from(bits & 0x03FF);
    let magnitude = match exponent {
        0 => {
            // Zero or subnormal: mantissa * 2^-24, exact in f32.
            let value = mantissa as f32 * f32::from_bits(0x3380_0000);
            return if sign == 0 { value } else { -value };
        }
        0x1F => 0x7F80_0000 | (mantissa << 13),
        _ => ((exponent + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits(sign | magnitude)
}

/// The half nearest `value` (ties to even).
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x007F_FFFF;
    if exponent == 0xFF {
        // Infinity, or NaN kept quiet with its top payload bits.
        let nan = if mantissa != 0 {
            0x0200 | (mantissa >> 13) as u16
        } else {
            0
        };
        return sign | 0x7C00 | nan;
    }
    let unbiased = exponent - 127;
    if unbiased > 15 {
        return sign | 0x7C00;
    }
    if unbiased >= -14 {
        // Normal: drop 13 mantissa bits, rounding to nearest even; a carry
        // out of the mantissa bumps the exponent (to infinity at the top).
        let half = (((unbiased + 15) as u32) << 10) | (mantissa >> 13);
        let rest = mantissa & 0x1FFF;
        let round = u32::from(rest > 0x1000 || (rest == 0x1000 && half & 1 == 1));
        return sign | (half + round) as u16;
    }
    if unbiased < -25 {
        return sign;
    }
    // Subnormal: the full significand shifted down, rounded to nearest even.
    let significand = mantissa | 0x0080_0000;
    let shift = (-unbiased - 14 + 13) as u32;
    let half = significand >> shift;
    let rest = significand & ((1 << shift) - 1);
    let middle = 1 << (shift - 1);
    let round = u32::from(rest > middle || (rest == middle && half & 1 == 1));
    sign | (half + round) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_half_round_trips() {
        for bits in 0..=u16::MAX {
            let value = f16_to_f32(bits);
            if value.is_nan() {
                assert!(f16_to_f32(f32_to_f16(value)).is_nan(), "{bits:#06x}");
            } else {
                assert_eq!(f32_to_f16(value), bits, "{bits:#06x}");
            }
        }
    }

    #[test]
    fn floats_round_to_nearest_even() {
        assert_eq!(f32_to_f16(1.0), 0x3C00);
        assert_eq!(f32_to_f16(1.000_488_3), 0x3C00); // halfway: even stays
        assert_eq!(f32_to_f16(1.001_464_8), 0x3C02); // halfway: odd rounds up
        assert_eq!(f32_to_f16(65_504.0), 0x7BFF);
        assert_eq!(f32_to_f16(65_520.0), 0x7C00); // halfway to the next: infinity
        assert_eq!(f32_to_f16(-0.0), 0x8000);
        assert_eq!(f32_to_f16(5.960_464_5e-8), 0x0001); // the smallest subnormal
        assert_eq!(f32_to_f16(2.980_232_2e-8), 0x0000); // half of it: ties to even
        assert_eq!(f32_to_f16(3.0e-8), 0x0001);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(f16_to_f32(0x8001), -5.960_464_5e-8);
        // Every float halfway between neighbouring halves rounds to the even one.
        for bits in 0x0000u16..0x7BFF {
            let (low, high) = (f16_to_f32(bits) as f64, f16_to_f32(bits + 1) as f64);
            let middle = ((low + high) / 2.0) as f32;
            let even = if bits & 1 == 0 { bits } else { bits + 1 };
            assert_eq!(f32_to_f16(middle), even, "{bits:#06x}");
        }
    }
}
