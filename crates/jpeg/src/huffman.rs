//! Huffman decoding tables and the entropy-coded bit reader.
//!
//! A code of up to [`LOOKAHEAD`] bits decodes with one lookup on the next
//! eleven bits. When a code and the magnitude bits after it fit in those
//! eleven bits together (most coefficients of a photograph), a second
//! table gives the run, the coefficient's value and the bits used by both
//! from the same index. Longer codes take the canonical `maxcode` walk.

/// Bits resolved by one table lookup.
pub(crate) const LOOKAHEAD: u32 = 11;
/// The run field of an AC table's end-of-block fast entry.
pub(crate) const END_OF_BLOCK: i32 = 0x80;
const ENTRIES: usize = 1 << LOOKAHEAD;

/// A decoding table built from a DHT segment.
pub(crate) struct Table {
    /// Next eleven bits → `(length << 8) | symbol`, or 0 for a longer code.
    lookup: [u16; ENTRIES],
    /// Next eleven bits → `(value << 16) | (run << 8) | bits used`, or 0
    /// when the code and its magnitude bits do not fit. AC tables also
    /// hold their zero runs this way (ZRL: run 15, value 0) and end of
    /// block as [`END_OF_BLOCK`] in the run.
    fast: [i32; ENTRIES],
    /// The largest code of each length (index 1..=16), -1 for none.
    maxcode: [i32; 18],
    /// Index into `symbols` of a length's first code, less that code.
    offset: [i32; 17],
    symbols: [u8; 256],
}

/// `bits` (`size` of them) as the signed value JPEG codes them as
/// (T.81 F.2.2.1 EXTEND).
#[inline(always)]
pub(crate) fn extend(bits: u32, size: u32) -> i32 {
    if size == 0 {
        return 0;
    }
    let bits = bits as i32;
    if bits < 1 << (size - 1) {
        bits - (1 << size) + 1
    } else {
        bits
    }
}

impl Table {
    /// The table for code `counts` (codes of each length 1..=16) and
    /// `symbols`; `None` when the counts oversubscribe the code space. An
    /// AC table's symbols are run/size pairs; a DC table's are sizes.
    pub fn build(counts: &[u8; 16], symbols: &[u8], ac: bool) -> Option<Table> {
        let mut table = Table {
            lookup: [0; ENTRIES],
            fast: [0; ENTRIES],
            maxcode: [-1; 18],
            offset: [0; 17],
            symbols: [0; 256],
        };
        if symbols.len() > 256 {
            return None;
        }
        let mut left: i32 = 1;
        for &count in counts {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return None;
            }
        }
        table.symbols[..symbols.len()].copy_from_slice(symbols);
        let mut code: i32 = 0;
        let mut k = 0usize;
        for length in 1..=16usize {
            let count = usize::from(counts[length - 1]);
            table.offset[length] = k as i32 - code;
            for _ in 0..count {
                let symbol = symbols[k];
                if length <= LOOKAHEAD as usize {
                    let spare = LOOKAHEAD as usize - length;
                    let first = (code as usize) << spare;
                    for index in first..first + (1 << spare) {
                        table.lookup[index] = (length as u16) << 8 | u16::from(symbol);
                        let (run, size) = if ac {
                            (symbol >> 4, symbol & 15)
                        } else {
                            (0, symbol)
                        };
                        let used = length + usize::from(size);
                        if ac && symbol == 0x00 {
                            table.fast[index] = END_OF_BLOCK << 8 | length as i32;
                        } else if ac && symbol == 0xF0 {
                            table.fast[index] = 15 << 8 | length as i32;
                        } else if size != 0 && size <= 15 && used <= LOOKAHEAD as usize {
                            let magnitude =
                                (index >> (LOOKAHEAD as usize - used)) & ((1 << size) - 1);
                            let value = extend(magnitude as u32, u32::from(size));
                            table.fast[index] = value << 16 | i32::from(run) << 8 | used as i32;
                        }
                    }
                }
                k += 1;
                code += 1;
            }
            if count > 0 {
                table.maxcode[length] = code - 1;
            }
            code <<= 1;
        }
        table.maxcode[17] = i32::MAX;
        Some(table)
    }

    /// The fast entry for the next eleven bits of `bits`.
    #[inline(always)]
    pub fn fast(&self, bits: &Bits) -> i32 {
        self.fast[bits.peek(LOOKAHEAD) as usize]
    }

    /// Decodes one symbol; `None` for a code the table does not have. The
    /// reader must hold at least sixteen bits.
    #[inline(always)]
    pub fn symbol(&self, bits: &mut Bits) -> Option<u8> {
        let entry = self.lookup[bits.peek(LOOKAHEAD) as usize];
        if entry != 0 {
            bits.consume(u32::from(entry >> 8));
            return Some(entry as u8);
        }
        self.slow_symbol(bits)
    }

    #[cold]
    fn slow_symbol(&self, bits: &mut Bits) -> Option<u8> {
        for length in LOOKAHEAD as usize + 1..=16 {
            let code = bits.peek(length as u32) as i32;
            if code <= self.maxcode[length] {
                bits.consume(length as u32);
                return self
                    .symbols
                    .get((self.offset[length] + code) as usize)
                    .copied();
            }
        }
        None
    }
}

/// Entropy-coded data read most significant bit first, stuffed zero bytes
/// removed. At a marker or the end of the data the reader supplies zero
/// bits and counts them as `padded`: reading into them is an overrun.
#[derive(Clone)]
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    /// Next unread byte.
    pub pos: usize,
    /// Unread bits at the top; `count` of them are valid, the last `padded`
    /// of those are fill.
    acc: u64,
    count: u32,
    padded: u32,
}

impl<'a> Bits<'a> {
    /// A reader over `data` (the scan's bytes through to its end) from
    /// `pos`.
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            acc: 0,
            count: 0,
            padded: 0,
        }
    }

    /// Tops up to at least 57 bits.
    #[inline(always)]
    pub fn ensure(&mut self) {
        if self.count < 32 {
            self.refill();
        }
    }

    /// Eight bytes at once while none of them is 0xFF (a stuffed byte or
    /// a marker); else byte by byte, out of line. The state stays in
    /// registers: the slow path takes and returns it by value.
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let word = u64::from_be_bytes(chunk.try_into().expect("eight bytes"));
            if (!word).wrapping_sub(0x0101_0101_0101_0101) & word & 0x8080_8080_8080_8080 == 0 {
                let bytes = (64 - self.count) >> 3;
                let taken = bytes * 8;
                self.acc |= (word >> (64 - taken)) << (64 - taken - self.count);
                self.count += taken;
                self.pos += bytes as usize;
                return;
            }
        }
        (self.pos, self.acc, self.count, self.padded) =
            refill_bytes(self.data, self.pos, self.acc, self.count, self.padded);
    }

    /// The next `n` bits (1..=32) without consuming them.
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        (self.acc >> (64 - n)) as u32
    }

    #[inline(always)]
    pub fn consume(&mut self, n: u32) {
        // n < 64 and n <= count by construction (wrapping ops keep the
        // overflow checks out of the hot loop).
        self.acc = self.acc.wrapping_shl(n);
        self.count = self.count.wrapping_sub(n);
    }

    /// `n` bits (0..=16) as an unsigned value.
    #[inline(always)]
    pub fn take(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.ensure();
        let value = self.peek(n);
        self.consume(n);
        value
    }

    /// One bit.
    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        self.take(1) == 1
    }

    /// `size` magnitude bits as a signed coefficient.
    #[inline(always)]
    pub fn receive_extend(&mut self, size: u32) -> i32 {
        extend(self.take(size), size)
    }

    /// Whether the reads so far went past the data into fill bits.
    #[inline(always)]
    pub fn overrun(&self) -> bool {
        self.count < self.padded
    }

    /// Skips the restart marker `expected` (RST0..RST7) where the data
    /// stands, discarding the bits buffered before it. `Err` names what is
    /// wrong. Inlined so a scan loop's reader never has its address taken.
    #[inline(always)]
    pub fn restart(&mut self, expected: u8) -> Result<(), &'static str> {
        if self.count >= self.padded + 8 {
            return Err("restart interval ends before its data");
        }
        if self.data.get(self.pos) != Some(&0xFF) {
            return Err("restart marker expected");
        }
        let mut at = self.pos + 1;
        while self.data.get(at) == Some(&0xFF) {
            at += 1;
        }
        match self.data.get(at) {
            Some(&code) if code == expected => {}
            Some(_) => return Err("restart marker out of sequence"),
            None => return Err("restart marker missing"),
        }
        self.pos = at + 1;
        self.acc = 0;
        self.count = 0;
        self.padded = 0;
        Ok(())
    }
}

/// [`Bits::refill`]'s byte-by-byte path: stuffed bytes removed, zero
/// bytes supplied (and counted as `padded`) at a marker or the end.
#[cold]
#[inline(never)]
fn refill_bytes(
    data: &[u8],
    mut pos: usize,
    mut acc: u64,
    mut count: u32,
    mut padded: u32,
) -> (usize, u64, u32, u32) {
    while count <= 56 {
        let mut byte = 0u64;
        match data.get(pos) {
            Some(&0xFF) if data.get(pos + 1) == Some(&0) => {
                byte = 0xFF;
                pos += 2;
            }
            Some(&value) if value != 0xFF => {
                byte = u64::from(value);
                pos += 1;
            }
            _ => padded += 8,
        }
        acc |= byte << (56 - count);
        count += 8;
    }
    (pos, acc, count, padded)
}

/// Where the marker after entropy-coded data starting at `pos` is: the
/// first 0xFF followed by neither a stuffed zero, a fill 0xFF nor a
/// restart marker (restart markers are skipped when `skip_restarts`).
pub(crate) fn next_marker(data: &[u8], mut pos: usize, skip_restarts: bool) -> usize {
    while pos + 1 < data.len() {
        if data[pos] == 0xFF {
            let next = data[pos + 1];
            let restart = (0xD0..=0xD7).contains(&next);
            if next != 0 && next != 0xFF && !(restart && skip_restarts) {
                return pos;
            }
            if next == 0xFF {
                pos += 1;
                continue;
            }
            pos += 2;
            continue;
        }
        pos += 1;
    }
    data.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_maps_magnitudes_to_signed_values() {
        assert_eq!(extend(0, 1), -1);
        assert_eq!(extend(1, 1), 1);
        assert_eq!(extend(0b00, 2), -3);
        assert_eq!(extend(0b11, 2), 3);
        assert_eq!(extend(0b0111, 4), -8);
    }

    #[test]
    fn oversubscribed_tables_are_refused() {
        let mut counts = [0u8; 16];
        counts[0] = 3;
        assert!(Table::build(&counts, &[1, 2, 3], false).is_none());
    }

    #[test]
    fn stuffed_bytes_and_markers() {
        // 0xFF00 is one 0xFF data byte; 0xFFD9 ends the data.
        let data = [0xAB, 0xFF, 0x00, 0xCD, 0xFF, 0xD9];
        let mut bits = Bits::new(&data, 0);
        assert_eq!(bits.take(8), 0xAB);
        assert_eq!(bits.take(8), 0xFF);
        assert_eq!(bits.take(8), 0xCD);
        assert!(!bits.overrun());
        assert_eq!(bits.take(8), 0);
        assert!(bits.overrun());
        assert_eq!(next_marker(&data, 0, true), 4);
    }
}
