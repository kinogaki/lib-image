//! Decompression of raw DEFLATE streams (RFC 1951).
//!
//! Bits are read least-significant first from a 64-bit buffer refilled a
//! word at a time. Each Huffman code decodes through a table indexed by the
//! next [`FAST_BITS`] bits; longer codes continue in a second-level table
//! (zlib's layout). Matches copy within the output, which is the history.
use crate::Error;
use crate::tables::{
    CODE_LENGTH_ORDER, DIST_BASE, DIST_EXTRA, END_OF_BLOCK, LENGTH_BASE, LENGTH_EXTRA,
    fixed_dist_lengths, fixed_litlen_lengths,
};
use std::sync::OnceLock;

/// Bits the first-level table resolves.
const FAST_BITS: u32 = 10;
const FAST_MASK: u64 = (1 << FAST_BITS) - 1;
/// Entry flags: the entry points at a second-level table, or no code ends here.
const SUBTABLE: u32 = 1 << 31;
const INVALID: u32 = 1 << 30;

/// Reads bits from a byte slice, least-significant bit first. Past the end
/// it reads zero bytes, counting them, so a truncated stream fails cleanly.
struct Bits<'a> {
    data: &'a [u8],
    /// The next byte to load.
    pos: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            count: 0,
        }
    }

    /// Loads bytes until at least 56 bits are buffered.
    #[inline]
    fn refill(&mut self) {
        if let Some(word) = self.data.get(self.pos..self.pos + 8) {
            let word = u64::from_le_bytes(word.try_into().expect("eight bytes"));
            self.buffer |= word << self.count;
            let taken = (63 - self.count) / 8;
            self.pos += taken as usize;
            self.count += taken * 8;
        } else {
            while self.count <= 56 {
                let byte = self.data.get(self.pos).copied().unwrap_or(0);
                self.buffer |= u64::from(byte) << self.count;
                self.pos += 1;
                self.count += 8;
            }
        }
    }

    /// Fails once more than the stream's bits have been consumed.
    #[inline]
    fn check(&self) -> Result<(), Error> {
        if self.consumed_bits() > self.data.len() * 8 {
            Err(Error::Truncated)
        } else {
            Ok(())
        }
    }

    fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.count as usize
    }

    #[inline]
    fn peek(&self) -> u64 {
        self.buffer
    }

    #[inline]
    fn consume(&mut self, n: u32) {
        self.buffer >>= n;
        self.count -= n;
    }

    /// `n` (at most 32) bits; the buffer must hold them.
    #[inline]
    fn take(&mut self, n: u32) -> u32 {
        let value = (self.buffer & ((1u64 << n) - 1)) as u32;
        self.consume(n);
        value
    }

    /// Up to 32 bits, refilling first.
    fn bits(&mut self, n: u32) -> u32 {
        if self.count < n {
            self.refill();
        }
        self.take(n)
    }

    /// Drops the bits up to the next byte boundary and returns the byte
    /// position reached; the buffer is emptied.
    fn align(&mut self) -> usize {
        self.consume(self.count % 8);
        let at = self.pos - self.count as usize / 8;
        self.pos = at;
        self.buffer = 0;
        self.count = 0;
        at
    }
}

/// A Huffman decoding table.
struct Table {
    entries: Vec<u32>,
}

/// An entry: symbol (or subtable offset) in the low 16 bits, then the
/// code's length (or the subtable's index bits).
const fn entry(value: u32, bits: u32) -> u32 {
    value | (bits << 16)
}

impl Table {
    /// The table for `lengths` (0 = unused symbol). Over-subscribed codes
    /// fail. A literal/length or distance code may be incomplete only as a
    /// single one-bit code or no code at all (RFC 1951 §3.2.7, as zlib
    /// reads it); its unused entries fail when decoded. A code length code
    /// (`complete`) must be complete.
    fn new(lengths: &[u8], complete: bool) -> Result<Self, Error> {
        let mut counts = [0u32; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left: i64 = 1;
        for &count in &counts[1..] {
            left = left * 2 - i64::from(count);
            if left < 0 {
                return Err(Error::Invalid("over-subscribed Huffman code"));
            }
        }
        let used: u32 = counts.iter().sum();
        if left > 0 && (complete || !(used == 0 || (used == 1 && counts[1] == 1))) {
            return Err(Error::Invalid("incomplete Huffman code"));
        }
        let mut next = [0u32; 16];
        let mut code = 0;
        for bits in 1..16 {
            code = (code + counts[bits - 1]) << 1;
            next[bits] = code;
        }
        // Canonical codes, bit-reversed into reading order.
        let codes: Vec<(u32, u32, u32)> = lengths
            .iter()
            .enumerate()
            .filter(|&(_, &l)| l > 0)
            .map(|(symbol, &l)| {
                let l = u32::from(l);
                let code = next[l as usize];
                next[l as usize] += 1;
                (code.reverse_bits() >> (32 - l), l, symbol as u32)
            })
            .collect();
        // Second-level sizes: the longest code under each first-level prefix.
        let mut sub_bits = vec![0u32; 1 << FAST_BITS];
        for &(code, l, _) in &codes {
            if l > FAST_BITS {
                let prefix = (code & FAST_MASK as u32) as usize;
                sub_bits[prefix] = sub_bits[prefix].max(l - FAST_BITS);
            }
        }
        let mut entries = vec![INVALID; 1 << FAST_BITS];
        for (prefix, &bits) in sub_bits.iter().enumerate() {
            if bits > 0 {
                entries[prefix] = SUBTABLE | entry(entries.len() as u32, bits);
                entries.resize(entries.len() + (1 << bits), INVALID);
            }
        }
        for &(code, l, symbol) in &codes {
            if l <= FAST_BITS {
                let mut slot = code as usize;
                while slot < 1 << FAST_BITS {
                    entries[slot] = entry(symbol, l);
                    slot += 1 << l;
                }
            } else {
                let prefix = (code & FAST_MASK as u32) as usize;
                let head = entries[prefix];
                let (offset, bits) = ((head & 0xFFFF) as usize, (head >> 16) & 0xFF);
                let mut slot = (code >> FAST_BITS) as usize;
                while slot < 1 << bits {
                    entries[offset + slot] = entry(symbol, l - FAST_BITS);
                    slot += 1 << (l - FAST_BITS);
                }
            }
        }
        Ok(Self { entries })
    }

    /// The next symbol. The buffer must hold at least 15 bits.
    #[inline]
    fn decode(&self, bits: &mut Bits) -> Result<usize, Error> {
        let mut e = self.entries[(bits.peek() & FAST_MASK) as usize];
        if e & SUBTABLE != 0 {
            bits.consume(FAST_BITS);
            let sub = (e >> 16) & 0xFF;
            let index = (bits.peek() & ((1 << sub) - 1)) as usize;
            e = self.entries[(e & 0xFFFF) as usize + index];
        }
        if e & INVALID != 0 {
            return Err(Error::Invalid("unused Huffman code"));
        }
        bits.consume((e >> 16) & 0xFF);
        Ok((e & 0xFFFF) as usize)
    }
}

fn fixed_tables() -> &'static (Table, Table) {
    static FIXED: OnceLock<(Table, Table)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let litlen = Table::new(&fixed_litlen_lengths(), false).expect("fixed literal code");
        let dist = Table::new(&fixed_dist_lengths(), false).expect("fixed distance code");
        (litlen, dist)
    })
}

/// Decompresses the raw DEFLATE stream at the start of `data`, appending to
/// `out` (whose earlier bytes are not history). Fails if `out` would grow
/// past `limit` bytes. Returns the bytes of `data` the stream used.
pub fn inflate_into(data: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    let start = out.len();
    let mut bits = Bits::new(data);
    loop {
        bits.refill();
        let last = bits.take(1) == 1;
        match bits.take(2) {
            0 => {
                let at = bits.align();
                let header = data.get(at..at + 4).ok_or(Error::Truncated)?;
                let len = u16::from_le_bytes([header[0], header[1]]);
                let nlen = u16::from_le_bytes([header[2], header[3]]);
                if len != !nlen {
                    return Err(Error::Invalid("stored block length check"));
                }
                let body = data
                    .get(at + 4..at + 4 + len as usize)
                    .ok_or(Error::Truncated)?;
                if out.len() + body.len() > limit {
                    return Err(Error::TooLarge);
                }
                out.extend_from_slice(body);
                bits.pos = at + 4 + len as usize;
            }
            1 => {
                let (litlen, dist) = fixed_tables();
                block(&mut bits, litlen, dist, out, start, limit)?;
            }
            2 => {
                let (litlen, dist) = dynamic_tables(&mut bits)?;
                block(&mut bits, &litlen, &dist, out, start, limit)?;
            }
            _ => return Err(Error::Invalid("reserved block type")),
        }
        bits.check()?;
        if last {
            break;
        }
    }
    Ok(bits.consumed_bits().div_ceil(8))
}

/// Reads a dynamic block's code definitions.
fn dynamic_tables(bits: &mut Bits) -> Result<(Table, Table), Error> {
    bits.refill();
    let hlit = bits.take(5) as usize + 257;
    let hdist = bits.take(5) as usize + 1;
    let hclen = bits.take(4) as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(Error::Invalid("too many length or distance codes"));
    }
    let mut code_lengths = [0u8; 19];
    for &symbol in &CODE_LENGTH_ORDER[..hclen] {
        code_lengths[symbol] = bits.bits(3) as u8;
    }
    let code_table = Table::new(&code_lengths, true)?;
    let mut lengths = [0u8; 286 + 30];
    let mut i = 0;
    while i < hlit + hdist {
        bits.refill();
        let symbol = code_table.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                let previous = i
                    .checked_sub(1)
                    .map(|p| lengths[p])
                    .ok_or(Error::Invalid("repeat with no previous length"))?;
                (previous, 3 + bits.take(2) as usize)
            }
            17 => (0, 3 + bits.take(3) as usize),
            _ => (0, 11 + bits.take(7) as usize),
        };
        if i + repeat > hlit + hdist {
            return Err(Error::Invalid("code lengths overrun"));
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    bits.check()?;
    if lengths[END_OF_BLOCK] == 0 {
        return Err(Error::Invalid("no end-of-block code"));
    }
    Ok((
        Table::new(&lengths[..hlit], false)?,
        Table::new(&lengths[hlit..hlit + hdist], false)?,
    ))
}

/// Decodes one Huffman block. Distances may reach back to `start`.
fn block(
    bits: &mut Bits,
    litlen: &Table,
    dist: &Table,
    out: &mut Vec<u8>,
    start: usize,
    limit: usize,
) -> Result<(), Error> {
    loop {
        // 56 buffered bits cover a length code with its extra bits (20)
        // and a distance code (15); its extra bits (13) may need a refill.
        bits.refill();
        let symbol = litlen.decode(bits)?;
        if symbol < 256 {
            if out.len() >= limit {
                return Err(Error::TooLarge);
            }
            out.push(symbol as u8);
            continue;
        }
        if symbol == END_OF_BLOCK {
            return Ok(());
        }
        let s = symbol - 257;
        if s >= 29 {
            return Err(Error::Invalid("invalid length symbol"));
        }
        let length = LENGTH_BASE[s] as usize + bits.take(u32::from(LENGTH_EXTRA[s])) as usize;
        let d = dist.decode(bits)?;
        if d >= 30 {
            return Err(Error::Invalid("invalid distance symbol"));
        }
        let distance = DIST_BASE[d] as usize + bits.bits(u32::from(DIST_EXTRA[d])) as usize;
        if distance > out.len() - start {
            return Err(Error::Invalid("distance before the start of the stream"));
        }
        if out.len() + length > limit {
            return Err(Error::TooLarge);
        }
        copy_match(out, distance, length);
        if bits.pos > bits.data.len() + 8 {
            bits.check()?;
        }
    }
}

/// Appends `length` bytes copied from `distance` back (they may overlap).
#[inline]
fn copy_match(out: &mut Vec<u8>, distance: usize, length: usize) {
    let from = out.len() - distance;
    if distance >= length {
        out.extend_from_within(from..from + length);
    } else if distance == 1 {
        let byte = out[from];
        out.resize(out.len() + length, byte);
    } else {
        // Each copy doubles the repeating span.
        let mut left = length;
        while left > 0 {
            let n = left.min(out.len() - from);
            out.extend_from_within(from..from + n);
            left -= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inflate(data: &[u8]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        inflate_into(data, &mut out, usize::MAX).map(|_| out)
    }

    #[test]
    fn stored_and_fixed_blocks() {
        // A final stored block holding "abc".
        assert_eq!(
            inflate(&[0x01, 0x03, 0x00, 0xFC, 0xFF, b'a', b'b', b'c']).unwrap(),
            b"abc"
        );
        // zlib's raw deflate of "aaaaaaaaaa" at level 9.
        assert_eq!(
            inflate(&[0x4B, 0x4C, 0x84, 0x01, 0x00]).unwrap(),
            b"aaaaaaaaaa"
        );
        // Empty fixed block.
        assert_eq!(inflate(&[0x03, 0x00]).unwrap(), b"");
    }

    #[test]
    fn malformed_streams_fail() {
        assert_eq!(inflate(&[]), Err(Error::Truncated));
        assert!(inflate(&[0x07]).is_err(), "reserved block type");
        assert!(inflate(&[0x01, 0x03, 0x00, 0xFC, 0xFE, b'a', b'b', b'c']).is_err());
        assert!(inflate(&[0x01, 0x03, 0x00, 0xFC, 0xFF, b'a']).is_err());
        // A fixed block whose first symbol is a match (length 3, code
        // 0000001; distance 1, code 00000): nothing to copy from.
        let mut packed = 0u32;
        let mut n = 0;
        for (value, width) in [(1, 1), (1, 2), (0b100_0000, 7), (0, 5), (0, 7)] {
            packed |= value << n;
            n += width;
        }
        assert_eq!(
            inflate(&packed.to_le_bytes()[..3]),
            Err(Error::Invalid("distance before the start of the stream"))
        );
        let mut out = Vec::new();
        assert_eq!(
            inflate_into(&[0x4B, 0x4C, 0x84, 0x01, 0x00], &mut out, 9),
            Err(Error::TooLarge)
        );
    }

    #[test]
    fn overlapping_copies() {
        let mut out = b"ab".to_vec();
        copy_match(&mut out, 2, 7);
        assert_eq!(out, b"ababababa");
        copy_match(&mut out, 1, 3);
        assert_eq!(out, b"ababababaaaa");
        copy_match(&mut out, 5, 5);
        assert_eq!(out, b"ababababaaaabaaaa");
    }
}
