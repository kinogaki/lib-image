//! The bit reader a stream is decoded through.
use super::table::{CODE_SHIFT, SUBTABLE, Table, sub_index};
use crate::Error;

/// Reads bits from a byte slice, least-significant bit first. Past the end
/// it reads zero bytes, counting them, so a truncated stream fails cleanly.
#[derive(Clone, Copy)]
pub(super) struct Bits<'a> {
    pub(super) data: &'a [u8],
    /// The next byte to load.
    pub(super) pos: usize,
    pub(super) buffer: u64,
    pub(super) count: u32,
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            count: 0,
        }
    }

    /// Loads bytes until at least 56 bits are buffered. The word load also
    /// sets bits above `count`; they are the next bytes' own bits, so a
    /// later load ORs the same values in. The tail path takes and returns
    /// the state by value, so a decoding loop keeps it in registers; the
    /// arithmetic wraps (it can't overflow) to keep checks out of that loop.
    #[inline(always)]
    pub(super) fn refill(&mut self) {
        if let Some(word) = self.data.get(self.pos..self.pos.wrapping_add(8)) {
            let word = u64::from_le_bytes(word.try_into().expect("eight bytes"));
            self.buffer |= word.wrapping_shl(self.count);
            let taken = 63u32.wrapping_sub(self.count) / 8;
            self.pos += taken as usize;
            self.count += taken * 8;
        } else {
            (self.pos, self.buffer, self.count) =
                refill_tail(self.data, self.pos, self.buffer, self.count);
        }
    }

    /// Fails once more than the stream's bits have been consumed.
    #[inline]
    pub(super) fn check(&self) -> Result<(), Error> {
        if self.consumed_bits() > self.data.len() * 8 {
            Err(Error::Truncated)
        } else {
            Ok(())
        }
    }

    pub(super) fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.count as usize
    }

    /// Drops `n` (under 64) bits; the buffer must hold them.
    #[inline(always)]
    pub(super) fn consume(&mut self, n: u32) {
        self.buffer = self.buffer.wrapping_shr(n);
        self.count = self.count.wrapping_sub(n);
    }

    /// Takes entry `e`'s bits and returns its extra bits' value (0 for an
    /// entry without any). The shift reads the low six bits of `e`, which
    /// are its bit count.
    #[inline(always)]
    pub(super) fn take_entry(&mut self, e: u32) -> usize {
        let saved = self.buffer;
        self.buffer = saved.wrapping_shr(e);
        self.count = self.count.wrapping_sub(e & 0xFF);
        let below = saved & !u64::MAX.wrapping_shl(e & 0xFF);
        below.wrapping_shr((e >> CODE_SHIFT) & 0xF) as usize
    }

    /// `n` (at most 32) bits; the buffer must hold them.
    #[inline(always)]
    pub(super) fn take(&mut self, n: u32) -> u32 {
        let value = (self.buffer & ((1u64 << n) - 1)) as u32;
        self.consume(n);
        value
    }

    /// Up to 32 bits, refilling first.
    pub(super) fn bits(&mut self, n: u32) -> u32 {
        if self.count < n {
            self.refill();
        }
        self.take(n)
    }

    /// Drops the bits up to the next byte boundary and returns the byte
    /// position reached; the buffer is emptied.
    pub(super) fn align(&mut self) -> usize {
        self.consume(self.count % 8);
        let at = self.pos - self.count as usize / 8;
        self.pos = at;
        self.buffer = 0;
        self.count = 0;
        at
    }

    /// The entry of `table` the next bits name, through its second level
    /// when there is one; not taken. The buffer must hold 15 bits.
    #[inline(always)]
    pub(super) fn decode(&mut self, table: &Table) -> u32 {
        let e = table.entries[(self.buffer & table.mask) as usize];
        if e & SUBTABLE == 0 {
            return e;
        }
        self.consume(e & 0xFF);
        table.entries[sub_index(e, self.buffer)]
    }
}

/// [`Bits::refill`] past the last whole word: byte by byte, zeros past the
/// end (counted by `pos`, so a truncated stream fails its check).
#[cold]
#[inline(never)]
fn refill_tail(data: &[u8], mut pos: usize, mut buffer: u64, mut count: u32) -> (usize, u64, u32) {
    while count <= 56 {
        let byte = data.get(pos).copied().unwrap_or(0);
        buffer |= u64::from(byte) << count;
        pos += 1;
        count += 8;
    }
    (pos, buffer, count)
}
