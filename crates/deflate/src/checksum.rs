//! CRC-32 (IEEE 802.3, as ZIP, gzip and PNG use it) and Adler-32 (zlib).

/// Eight CRC-32 tables for slicing by eight: `TABLES[0]` is the classic
/// byte table, `TABLES[k][b]` the CRC of byte `b` followed by `k` zero bytes.
const TABLES: [[u32; 256]; 8] = {
    let mut tables = [[0u32; 256]; 8];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        tables[0][byte] = crc;
        byte += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut byte = 0;
        while byte < 256 {
            let previous = tables[k - 1][byte];
            tables[k][byte] = (previous >> 8) ^ tables[0][(previous & 0xFF) as usize];
            byte += 1;
        }
        k += 1;
    }
    tables
};

/// A running CRC-32.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// The CRC of nothing so far.
    pub const fn new() -> Self {
        Self(!0)
    }

    /// Adds `data`.
    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        let (chunks, rest) = data.as_chunks::<8>();
        for chunk in chunks {
            let low = crc ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let high = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            crc = TABLES[7][(low & 0xFF) as usize]
                ^ TABLES[6][((low >> 8) & 0xFF) as usize]
                ^ TABLES[5][((low >> 16) & 0xFF) as usize]
                ^ TABLES[4][(low >> 24) as usize]
                ^ TABLES[3][(high & 0xFF) as usize]
                ^ TABLES[2][((high >> 8) & 0xFF) as usize]
                ^ TABLES[1][((high >> 16) & 0xFF) as usize]
                ^ TABLES[0][(high >> 24) as usize];
        }
        for &byte in rest {
            crc = (crc >> 8) ^ TABLES[0][((crc ^ u32::from(byte)) & 0xFF) as usize];
        }
        self.0 = crc;
    }

    /// The CRC of everything added.
    pub const fn finish(self) -> u32 {
        !self.0
    }
}

/// The CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

/// Adler-32's modulus.
const BASE: u32 = 65_521;
/// Bytes summed between reductions (zlib's NMAX, a multiple of 16): no
/// lane sum can overflow a `u32` within it.
const NMAX: usize = 5552;

/// A running Adler-32.
#[derive(Clone, Copy, Debug)]
pub struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adler32 {
    /// The checksum of nothing so far.
    pub const fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    /// Adds `data`. Sixteen lanes each sum every sixteenth byte (`a`) and
    /// their running sums (`b`), which combine into the block's sums
    /// without a dependency between neighbouring bytes:
    /// `b += n * a0 + 16 * Σ lane_b - Σ i * lane_a_i`. Within a block no
    /// lane can overflow; the sums wrap rather than check, so they
    /// vectorize.
    pub fn update(&mut self, data: &[u8]) {
        let (mut a, mut b) = (self.a, self.b);
        for block in data.chunks(NMAX) {
            let (rows, rest) = block.as_chunks::<16>();
            let mut lane_a = [0u32; 16];
            let mut lane_b = [0u32; 16];
            for row in rows {
                for i in 0..16 {
                    lane_a[i] = lane_a[i].wrapping_add(u32::from(row[i]));
                    lane_b[i] = lane_b[i].wrapping_add(lane_a[i]);
                }
            }
            let n = 16 * rows.len() as u64;
            let sum_a: u64 = lane_a.iter().map(|&x| u64::from(x)).sum();
            let weighted: u64 = (0..16).map(|i| i as u64 * u64::from(lane_a[i])).sum();
            let sum_b: u64 = lane_b.iter().map(|&x| u64::from(x)).sum();
            let b_wide = u64::from(b) + n * u64::from(a) + 16 * sum_b - weighted;
            a = ((u64::from(a) + sum_a) % u64::from(BASE)) as u32;
            b = (b_wide % u64::from(BASE)) as u32;
            for &byte in rest {
                a += u32::from(byte);
                b += a;
            }
            a %= BASE;
            b %= BASE;
        }
        self.a = a;
        self.b = b;
    }

    /// The checksum of everything added.
    pub const fn finish(self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// The Adler-32 of `data`.
pub fn adler32(data: &[u8]) -> u32 {
    let mut adler = Adler32::new();
    adler.update(data);
    adler.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn slicing_matches_bytewise_and_split_updates() {
        let data: Vec<u8> = (0..100_003u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let mut bytewise = !0u32;
        for &byte in &data {
            bytewise = (bytewise >> 8) ^ TABLES[0][((bytewise ^ u32::from(byte)) & 0xFF) as usize];
        }
        assert_eq!(crc32(&data), !bytewise);
        let (mut crc, mut adler) = (Crc32::new(), Adler32::new());
        for piece in data.chunks(997) {
            crc.update(piece);
            adler.update(piece);
        }
        assert_eq!(crc.finish(), crc32(&data));
        assert_eq!(adler.finish(), adler32(&data));
        // Adler-32 by definition, without the deferred modulus.
        let (mut a, mut b) = (1u64, 0u64);
        for &byte in &data {
            a = (a + u64::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        assert_eq!(adler32(&data), ((b << 16) | a) as u32);
        // Bytes of 0xFF for the longest run between reductions.
        let ones = vec![0xFFu8; 3 * NMAX + 11];
        let (mut a, mut b) = (1u64, 0u64);
        for &byte in &ones {
            a = (a + u64::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        assert_eq!(adler32(&ones), ((b << 16) | a) as u32);
    }
}
