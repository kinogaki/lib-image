//! Huffman coding for the encoder: the Annex K tables, optimal tables
//! fitted to a picture's statistics (T.81 K.2), each MCU row's bit stream
//! and the joining of streams with byte stuffing.
use crate::headers::ZIGZAG;

pub(crate) const DC_LUMA_COUNTS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
pub(crate) const DC_CHROMA_COUNTS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
pub(crate) const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
pub(crate) const AC_LUMA_COUNTS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
pub(crate) const AC_LUMA_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];
pub(crate) const AC_CHROMA_COUNTS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
pub(crate) const AC_CHROMA_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// A table as a DHT segment states it: code counts by length, symbols.
#[derive(Clone, Debug)]
pub(crate) struct Spec {
    pub counts: [u8; 16],
    pub values: Vec<u8>,
}

impl Spec {
    pub fn standard(counts: &[u8; 16], values: &[u8]) -> Spec {
        Spec {
            counts: *counts,
            values: values.to_vec(),
        }
    }

    /// The optimal table for symbol frequencies `freq` (T.81 K.2): code
    /// lengths by Huffman's procedure, limited to 16 bits, one code point
    /// reserved so no code is all ones.
    pub fn optimal(freq: &[u32; 256]) -> Spec {
        let mut freq: Vec<u64> = freq.iter().map(|&f| u64::from(f)).collect();
        freq.push(1); // the reserved symbol 256
        let mut size = [0usize; 257];
        let mut others = [usize::MAX; 257];
        loop {
            // The two least frequent live symbols (ties: the larger index
            // first, as K.2 picks them).
            let mut v1 = usize::MAX;
            let mut v2 = usize::MAX;
            for i in 0..257 {
                if freq[i] == 0 {
                    continue;
                }
                if v1 == usize::MAX || freq[i] <= freq[v1] {
                    v2 = v1;
                    v1 = i;
                } else if v2 == usize::MAX || freq[i] <= freq[v2] {
                    v2 = i;
                }
            }
            if v2 == usize::MAX {
                break;
            }
            freq[v1] += freq[v2];
            freq[v2] = 0;
            let mut i = v1;
            size[i] += 1;
            while others[i] != usize::MAX {
                i = others[i];
                size[i] += 1;
            }
            others[i] = v2;
            let mut i = v2;
            size[i] += 1;
            while others[i] != usize::MAX {
                i = others[i];
                size[i] += 1;
            }
        }
        let mut bits = [0usize; 64];
        for &s in &size {
            if s > 0 {
                bits[s.min(63)] += 1;
            }
        }
        // Limit lengths to 16 (K.3's adjustment).
        let mut i = 63;
        while i > 16 {
            while bits[i] > 0 {
                let mut j = i - 2;
                while bits[j] == 0 {
                    j -= 1;
                }
                bits[i] -= 2;
                bits[i - 1] += 1;
                bits[j + 1] += 2;
                bits[j] -= 1;
            }
            i -= 1;
        }
        // Drop the reserved symbol's code, one of the longest.
        let mut i = 16;
        while bits[i] == 0 {
            i -= 1;
        }
        bits[i] -= 1;
        let mut counts = [0u8; 16];
        for (n, count) in counts.iter_mut().enumerate() {
            *count = bits[n + 1] as u8;
        }
        // Symbols by increasing code length, each length in symbol order.
        let mut values = Vec::new();
        for length in 1..64 {
            for (symbol, &s) in size.iter().enumerate().take(256) {
                if s == length {
                    values.push(symbol as u8);
                }
            }
        }
        Spec { counts, values }
    }
}

/// Codes for encoding: each symbol's code and length (0: none).
#[derive(Clone)]
pub(crate) struct Codes {
    code: [u16; 256],
    length: [u8; 256],
}

impl Codes {
    pub fn of(spec: &Spec) -> Codes {
        let mut codes = Codes {
            code: [0; 256],
            length: [0; 256],
        };
        let mut code = 0u16;
        let mut k = 0;
        for (n, &count) in spec.counts.iter().enumerate() {
            for _ in 0..count {
                let symbol = usize::from(spec.values[k]);
                codes.code[symbol] = code;
                codes.length[symbol] = n as u8 + 1;
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        codes
    }
}

/// Bits of an MCU row, most significant first, not yet byte-stuffed.
#[derive(Default)]
pub(crate) struct BitWriter {
    pub bytes: Vec<u8>,
    acc: u64,
    count: u32,
}

impl BitWriter {
    pub fn with_capacity(bytes: usize) -> Self {
        BitWriter {
            bytes: Vec::with_capacity(bytes),
            acc: 0,
            count: 0,
        }
    }

    /// Appends the low `n` bits (at most 32) of `value`.
    #[inline(always)]
    pub fn put(&mut self, value: u32, n: u32) {
        self.acc = self.acc << n | u64::from(value) & ((1u64 << n) - 1);
        self.count += n;
        if self.count >= 32 {
            self.count -= 32;
            self.bytes
                .extend_from_slice(&((self.acc >> self.count) as u32).to_be_bytes());
        }
    }

    /// The whole bytes and the bits left over (value, count < 8).
    pub fn finish(mut self) -> (Vec<u8>, u8, u32) {
        while self.count >= 8 {
            self.count -= 8;
            self.bytes.push((self.acc >> self.count) as u8);
        }
        let tail = (self.acc & ((1 << self.count) - 1)) as u8;
        (self.bytes, tail, self.count)
    }

    /// Pads with one bits to a whole byte (before a marker or the end).
    pub fn pad(&mut self) {
        let fill = (8 - self.count % 8) % 8;
        self.put((1 << fill) - 1, fill);
    }
}

/// The size category of a coefficient: the bits of its magnitude.
#[inline(always)]
fn category(value: i32) -> u32 {
    32 - value.unsigned_abs().leading_zeros()
}

/// Codes one block: its DC difference from `predictor` and its AC
/// coefficients (natural order) as runs and sizes.
#[inline]
pub(crate) fn encode_block(
    out: &mut BitWriter,
    block: &[i16; 64],
    predictor: &mut i32,
    dc: &Codes,
    ac: &Codes,
) {
    let value = i32::from(block[0]);
    let diff = value - *predictor;
    *predictor = value;
    let size = category(diff);
    out.put(
        u32::from(dc.code[size as usize]),
        u32::from(dc.length[size as usize]),
    );
    if size > 0 {
        out.put((diff - i32::from(diff < 0)) as u32, size);
    }
    let mut run = 0u32;
    for &natural in &ZIGZAG[1..] {
        let value = i32::from(block[natural]);
        if value == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            out.put(u32::from(ac.code[0xF0]), u32::from(ac.length[0xF0]));
            run -= 16;
        }
        let size = category(value);
        let symbol = (run << 4 | size) as usize;
        out.put(u32::from(ac.code[symbol]), u32::from(ac.length[symbol]));
        out.put((value - i32::from(value < 0)) as u32, size);
        run = 0;
    }
    if run > 0 {
        out.put(u32::from(ac.code[0]), u32::from(ac.length[0]));
    }
}

/// Counts the symbols `encode_block` would code, for optimal tables.
#[inline]
pub(crate) fn count_block(
    block: &[i16; 64],
    predictor: &mut i32,
    dc: &mut [u32; 256],
    ac: &mut [u32; 256],
) {
    let value = i32::from(block[0]);
    dc[category(value - *predictor) as usize] += 1;
    *predictor = value;
    let mut run = 0u32;
    for &natural in &ZIGZAG[1..] {
        let value = i32::from(block[natural]);
        if value == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            ac[0xF0] += 1;
            run -= 16;
        }
        ac[(run << 4 | category(value)) as usize] += 1;
        run = 0;
    }
    if run > 0 {
        ac[0] += 1;
    }
}

/// Appends bits to a file's entropy-coded data, stuffing a zero after
/// every 0xFF byte.
pub(crate) struct Joiner<'o> {
    out: &'o mut Vec<u8>,
    /// Pending bits at the bottom of `carry`, `count` of them (< 8).
    carry: u8,
    count: u32,
}

impl<'o> Joiner<'o> {
    /// A joiner over `out`, whose last `count` bits (`carry`'s low bits)
    /// are still pending.
    pub fn resume(out: &'o mut Vec<u8>, carry: u8, count: u32) -> Self {
        Joiner { out, carry, count }
    }

    /// Appends `bytes` then the `tail_count` low bits of `tail`.
    pub fn append(&mut self, bytes: &[u8], tail: u8, tail_count: u32) {
        if self.count == 0 {
            stuff(self.out, bytes);
        } else {
            // Each byte shifts right by `count`, the bits pushed out
            // carrying into the next.
            let shift = self.count;
            let mut carry = u64::from(self.carry);
            let (words, rest) = bytes.as_chunks::<8>();
            for &chunk in words {
                let word = u64::from_be_bytes(chunk);
                let shifted = carry << (64 - shift) | word >> shift;
                carry = word & ((1 << shift) - 1);
                stuff(self.out, &shifted.to_be_bytes());
            }
            for &b in rest {
                let shifted = (carry << (8 - shift)) as u8 | b >> shift;
                carry = u64::from(b) & ((1 << shift) - 1);
                stuff(self.out, &[shifted]);
            }
            self.carry = carry as u8;
        }
        self.put_small(tail, tail_count);
    }

    /// Appends up to eight bits.
    fn put_small(&mut self, bits: u8, n: u32) {
        let total = self.count + n;
        let value = u16::from(self.carry) << n | u16::from(bits) & ((1 << n) - 1);
        if total >= 8 {
            let byte = (value >> (total - 8)) as u8;
            stuff(self.out, &[byte]);
            self.count = total - 8;
            self.carry = (value & ((1 << self.count) - 1)) as u8;
        } else {
            self.count = total;
            self.carry = value as u8;
        }
    }

    /// Pads the last byte with one bits.
    pub fn pad(&mut self) {
        if self.count > 0 {
            let fill = 8 - self.count;
            self.put_small((1 << fill) - 1, fill);
        }
    }

    /// Appends a marker; the data must be byte-aligned.
    pub fn marker(&mut self, code: u8) {
        debug_assert_eq!(self.count, 0);
        self.out.extend_from_slice(&[0xFF, code]);
    }
}

/// `bytes` onto `out` with a zero after each 0xFF.
fn stuff(out: &mut Vec<u8>, bytes: &[u8]) {
    let mut start = 0;
    for chunk in bytes.chunks(8) {
        if chunk.contains(&0xFF) {
            break;
        }
        start += chunk.len();
    }
    out.extend_from_slice(&bytes[..start]);
    let mut rest = &bytes[start..];
    while let Some(i) = rest.iter().position(|&b| b == 0xFF) {
        out.extend_from_slice(&rest[..=i]);
        out.push(0);
        rest = &rest[i + 1..];
    }
    out.extend_from_slice(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimal_tables_fit_sixteen_bits_and_reserve_all_ones() {
        let mut freq = [0u32; 256];
        // A Fibonacci spread forces lengths past sixteen before limiting.
        let (mut a, mut b) = (1u32, 1u32);
        for f in freq.iter_mut().take(40) {
            *f = a;
            (a, b) = (b, a.saturating_add(b));
        }
        let spec = Spec::optimal(&freq);
        assert_eq!(spec.values.len(), 40);
        let mut kraft = 0u64;
        for (n, &count) in spec.counts.iter().enumerate() {
            kraft += u64::from(count) << (15 - n);
        }
        assert!(kraft < 1 << 16, "one code point stays free");
    }

    #[test]
    fn joining_shifts_and_stuffs() {
        let mut out = Vec::new();
        let mut joiner = Joiner::resume(&mut out, 0, 0);
        joiner.append(&[], 0b1, 1);
        joiner.append(&[0xFF; 9], 0b0101, 4);
        joiner.pad();
        // A one bit, nine 0xFF bytes and 0101, padded: nine 0xFF bytes
        // (each stuffed), then 1 0101 111.
        let mut expected = Vec::new();
        for _ in 0..9 {
            expected.extend_from_slice(&[0xFF, 0x00]);
        }
        expected.push(0xAF);
        assert_eq!(out, expected);
    }
}
