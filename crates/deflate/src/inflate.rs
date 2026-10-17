//! Decompression of raw DEFLATE streams (RFC 1951).
//!
//! Bits are read least-significant first from a 64-bit buffer refilled a
//! word at a time, so one refill covers a whole length/distance pair. Each
//! Huffman code decodes through a table indexed by the next bits (11 for
//! literals/lengths, 10 for distances); longer codes continue in a
//! second-level table. Table entries carry what the decoder needs directly:
//! one or two literals (two short literal codes resolve in one lookup), or a
//! length or distance base with its extra-bit count. Output goes into a
//! buffer kept a match plus a word longer than what is written, so matches
//! copy eight bytes at a time.
use crate::Error;
use crate::tables::{
    CODE_LENGTH_ORDER, DIST_BASE, DIST_EXTRA, END_OF_BLOCK, LENGTH_BASE, LENGTH_EXTRA, MAX_MATCH,
    fixed_dist_lengths, fixed_litlen_lengths,
};
use std::sync::OnceLock;

/// First-level index bits of the literal/length and distance tables.
const LITLEN_BITS: u32 = 11;
const DIST_BITS: u32 = 10;
/// The code length code's codes are at most 7 bits: one level.
const CODE_LENGTH_BITS: u32 = 7;

/// Entry layout: bits 0..5 the bits to consume; 5..8 the kind; 8.. payload.
const CONSUMED: u32 = 0x1F;
const KIND_SHIFT: u32 = 5;
/// No code ends here (an unused code of an incomplete or invalid set).
const INVALID: u32 = 0;
/// One literal in bits 8..16.
const LITERAL: u32 = 1;
/// Two literals in bits 8..16 and 16..24.
const LITERALS: u32 = 2;
/// A length or distance: base in bits 8..24, extra-bit count in 24..28.
const BASE: u32 = 3;
const END: u32 = 4;
/// A second-level table: offset in bits 8..24, index bits in 24..28.
const SUBTABLE: u32 = 5;
/// A plain symbol (code length codes) in bits 8..16.
const SYMBOL: u32 = 6;

const fn entry(consumed: u32, kind: u32, payload: u32) -> u32 {
    consumed | (kind << KIND_SHIFT) | (payload << 8)
}

#[inline]
const fn kind(e: u32) -> u32 {
    (e >> KIND_SHIFT) & 7
}

/// Bytes written past a match's end by the eight-byte copies, plus one
/// match: the output buffer always has this much room beyond `pos`.
const SLACK: usize = MAX_MATCH + 16;

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

    /// Loads bytes until at least 56 bits are buffered. The word load also
    /// sets bits above `count`; they are the next bytes' own bits, so a
    /// later load ORs the same values in.
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(word) = self.data.get(self.pos..self.pos + 8) {
            let word = u64::from_le_bytes(word.try_into().expect("eight bytes"));
            self.buffer |= word << self.count;
            let taken = (63 - self.count) / 8;
            self.pos += taken as usize;
            self.count += taken * 8;
        } else {
            self.refill_tail();
        }
    }

    #[cold]
    fn refill_tail(&mut self) {
        while self.count <= 56 {
            let byte = self.data.get(self.pos).copied().unwrap_or(0);
            self.buffer |= u64::from(byte) << self.count;
            self.pos += 1;
            self.count += 8;
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

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.buffer >>= n;
        self.count -= n;
    }

    /// `n` (at most 32) bits; the buffer must hold them.
    #[inline(always)]
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

    /// Decodes one entry of `table`; the buffer must hold 15 bits.
    #[inline(always)]
    fn decode(&mut self, table: &Table) -> u32 {
        let mut e = table.entries[(self.buffer & table.mask) as usize];
        if kind(e) == SUBTABLE {
            self.consume(table.bits);
            let sub = (e >> 24) & 0xF;
            let index = (self.buffer & ((1 << sub) - 1)) as usize;
            e = table.entries[((e >> 8) & 0xFFFF) as usize + index];
        }
        self.consume(e & CONSUMED);
        e
    }
}

/// A Huffman decoding table.
struct Table {
    entries: Vec<u32>,
    bits: u32,
    mask: u64,
}

impl Table {
    /// The table for `lengths` (0 = unused symbol) with `bits` first-level
    /// bits; `value` gives each symbol's entry kind and payload. Over-
    /// subscribed codes fail. A literal/length or distance code may be
    /// incomplete only as a single one-bit code or no code at all (RFC 1951
    /// §3.2.7, as zlib reads it); its unused entries fail when decoded. A
    /// code length code (`complete`) must be complete.
    fn new(
        lengths: &[u8],
        bits: u32,
        complete: bool,
        value: impl Fn(usize) -> (u32, u32),
    ) -> Result<Self, Error> {
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
        for l in 1..16 {
            code = (code + counts[l - 1]) << 1;
            next[l] = code;
        }
        // Canonical codes, bit-reversed into reading order.
        let codes: Vec<(u32, u32, usize)> = lengths
            .iter()
            .enumerate()
            .filter(|&(_, &l)| l > 0)
            .map(|(symbol, &l)| {
                let l = u32::from(l);
                let code = next[l as usize];
                next[l as usize] += 1;
                (code.reverse_bits() >> (32 - l), l, symbol)
            })
            .collect();
        let size = 1usize << bits;
        let mask = size as u32 - 1;
        // Second-level sizes: the longest code under each first-level prefix.
        let mut sub_bits = vec![0u32; size];
        for &(code, l, _) in &codes {
            if l > bits {
                let prefix = (code & mask) as usize;
                sub_bits[prefix] = sub_bits[prefix].max(l - bits);
            }
        }
        let mut entries = vec![INVALID; size];
        for (prefix, &sb) in sub_bits.iter().enumerate() {
            if sb > 0 {
                let offset = entries.len() as u32;
                entries[prefix] = entry(0, SUBTABLE, offset | (sb << 16));
                entries.resize(entries.len() + (1 << sb), INVALID);
            }
        }
        for &(code, l, symbol) in &codes {
            let (kind, payload) = value(symbol);
            if l <= bits {
                let mut slot = code as usize;
                while slot < size {
                    entries[slot] = entry(l, kind, payload);
                    slot += 1 << l;
                }
            } else {
                let head = entries[(code & mask) as usize];
                let (offset, sb) = (((head >> 8) & 0xFFFF) as usize, (head >> 24) & 0xF);
                let mut slot = (code >> bits) as usize;
                while slot < 1 << sb {
                    entries[offset + slot] = entry(l - bits, kind, payload);
                    slot += 1 << (l - bits);
                }
            }
        }
        Ok(Self {
            entries,
            bits,
            mask: u64::from(mask),
        })
    }

    /// The literal/length table: literals, lengths with their extra bits,
    /// end of block; then pairs of short literal codes merged into one entry.
    fn litlen(lengths: &[u8]) -> Result<Self, Error> {
        let mut table = Self::new(lengths, LITLEN_BITS, false, |symbol| match symbol {
            0..=255 => (LITERAL, symbol as u32),
            END_OF_BLOCK => (END, 0),
            257..=285 => {
                let s = symbol - 257;
                (
                    BASE,
                    u32::from(LENGTH_BASE[s]) | (u32::from(LENGTH_EXTRA[s]) << 16),
                )
            }
            _ => (INVALID, 0),
        })?;
        let size = 1usize << LITLEN_BITS;
        for i in 0..size {
            let first = table.entries[i];
            if kind(first) != LITERAL {
                continue;
            }
            let l1 = first & CONSUMED;
            let second = table.entries[i >> l1];
            let l2 = second & CONSUMED;
            // The second code is whole within the remaining index bits.
            if kind(second) == LITERAL && l1 + l2 <= LITLEN_BITS {
                let pair = ((first >> 8) & 0xFF) | (((second >> 8) & 0xFF) << 8);
                table.entries[i] = entry(l1 + l2, LITERALS, pair);
            }
        }
        Ok(table)
    }

    fn dist(lengths: &[u8]) -> Result<Self, Error> {
        Self::new(lengths, DIST_BITS, false, |symbol| match symbol {
            0..=29 => (
                BASE,
                u32::from(DIST_BASE[symbol]) | (u32::from(DIST_EXTRA[symbol]) << 16),
            ),
            _ => (INVALID, 0),
        })
    }
}

fn fixed_tables() -> &'static (Table, Table) {
    static FIXED: OnceLock<(Table, Table)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let litlen = Table::litlen(&fixed_litlen_lengths()).expect("fixed literal code");
        let dist = Table::dist(&fixed_dist_lengths()).expect("fixed distance code");
        (litlen, dist)
    })
}

/// From this size on, output buffers are fresh zeroed allocations (the
/// system's zero pages, measured faster than writing zeros); smaller ones
/// reuse the allocator's memory and write zeros (measured faster than
/// mapping fresh pages for each stream, as EXR's many small blocks do).
const FRESH_PAGES: usize = 4 << 20;

fn zeroed(len: usize) -> Vec<u8> {
    vec![0u8; len]
}

/// The output: `buf[..pos]` is written; `buf` stays at least [`SLACK`]
/// longer than `pos` and is cut to `pos` at the end.
struct Output<'a> {
    buf: &'a mut Vec<u8>,
    pos: usize,
    /// History starts here (earlier bytes belong to the caller).
    start: usize,
    limit: usize,
}

impl Output<'_> {
    /// Makes room for at least [`SLACK`] bytes after `pos`.
    #[cold]
    fn grow(&mut self) {
        let len = self.buf.len();
        let wanted = (len * 2)
            .max(self.pos + SLACK + (1 << 16))
            .min(self.limit.saturating_add(SLACK));
        let wanted = wanted.max(self.pos + SLACK + 64);
        if wanted >= FRESH_PAGES {
            // Zeroed pages straight from the system instead of written
            // zeros; only the written bytes are copied over.
            let mut bigger = zeroed(wanted);
            bigger[..self.pos].copy_from_slice(&self.buf[..self.pos]);
            *self.buf = bigger;
        } else {
            self.buf.resize(wanted, 0);
        }
    }

    #[inline(always)]
    fn reserve(&mut self) {
        if self.pos + SLACK >= self.buf.len() {
            self.grow();
        }
    }
}

/// Decompresses the raw DEFLATE stream at the start of `data`, appending to
/// `out` (whose earlier bytes are not history). Fails if `out` would grow
/// past `limit` bytes. Returns the bytes of `data` the stream used.
pub fn inflate_into(data: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    let start = out.len();
    // A first guess at the size: the limit itself when it is near enough
    // to be the caller's exact size (PNG, EXR and ZIP know it), else four
    // times the input, bounded by the limit.
    let room = limit.saturating_sub(start);
    let guess = start
        + if room <= data.len().saturating_mul(64) {
            room
        } else {
            data.len().saturating_mul(4)
        };
    if out.is_empty() && guess + SLACK >= FRESH_PAGES {
        *out = zeroed(guess + SLACK);
    } else {
        out.resize(guess + SLACK, 0);
    }
    let mut output = Output {
        buf: out,
        pos: start,
        start,
        limit,
    };
    let result = blocks(data, &mut output);
    let pos = output.pos;
    out.truncate(pos);
    result
}

fn blocks(data: &[u8], out: &mut Output) -> Result<usize, Error> {
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
                if out.pos + body.len() > out.limit {
                    return Err(Error::TooLarge);
                }
                while out.pos + body.len() + SLACK > out.buf.len() {
                    out.grow();
                }
                out.buf[out.pos..out.pos + body.len()].copy_from_slice(body);
                out.pos += body.len();
                bits.pos = at + 4 + len as usize;
            }
            1 => {
                let (litlen, dist) = fixed_tables();
                block(&mut bits, litlen, dist, out)?;
            }
            2 => {
                let (litlen, dist) = dynamic_tables(&mut bits)?;
                block(&mut bits, &litlen, &dist, out)?;
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
    let code_table = Table::new(&code_lengths, CODE_LENGTH_BITS, true, |s| {
        (SYMBOL, s as u32)
    })?;
    let mut lengths = [0u8; 286 + 30];
    let mut i = 0;
    while i < hlit + hdist {
        bits.refill();
        let e = bits.decode(&code_table);
        if kind(e) != SYMBOL {
            return Err(Error::Invalid("unused code length code"));
        }
        let symbol = (e >> 8) as usize;
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
        Table::litlen(&lengths[..hlit])?,
        Table::dist(&lengths[hlit..hlit + hdist])?,
    ))
}

/// Decodes one Huffman block.
fn block(bits: &mut Bits, litlen: &Table, dist: &Table, out: &mut Output) -> Result<(), Error> {
    loop {
        out.reserve();
        // Positions below `end` have room for any symbol in the buffer; the
        // loop works on locals.
        let end = out.buf.len() - SLACK;
        let (buf, start, limit) = (&mut out.buf[..], out.start, out.limit);
        let mut pos = out.pos;
        let done = loop {
            if pos >= end {
                break Ok(false);
            }
            // 56 buffered bits cover the longest length code with its extra
            // bits (15 + 5) and distance code with its extra bits (15 + 13).
            bits.refill();
            let e = bits.decode(litlen);
            match kind(e) {
                LITERAL => {
                    if pos >= limit {
                        break Err(Error::TooLarge);
                    }
                    buf[pos] = (e >> 8) as u8;
                    pos += 1;
                    // Literals usually follow literals: up to three more
                    // from the same refill (15 + 3 * 11 bits of the 56),
                    // when their codes resolve in the first-level table.
                    for _ in 0..3 {
                        let next = litlen.entries[(bits.buffer & litlen.mask) as usize];
                        let n = match kind(next) {
                            LITERAL => 1,
                            LITERALS => 2,
                            _ => break,
                        };
                        if pos + n > limit {
                            break;
                        }
                        bits.consume(next & CONSUMED);
                        buf[pos] = (next >> 8) as u8;
                        buf[pos + 1] = (next >> 16) as u8;
                        pos += n;
                    }
                }
                LITERALS => {
                    if pos + 2 > limit {
                        break Err(Error::TooLarge);
                    }
                    buf[pos] = (e >> 8) as u8;
                    buf[pos + 1] = (e >> 16) as u8;
                    pos += 2;
                }
                BASE => {
                    let length = ((e >> 8) & 0xFFFF) as usize + bits.take((e >> 24) & 0xF) as usize;
                    let d = bits.decode(dist);
                    if kind(d) != BASE {
                        break Err(Error::Invalid("invalid distance code"));
                    }
                    let distance =
                        ((d >> 8) & 0xFFFF) as usize + bits.take((d >> 24) & 0xF) as usize;
                    if distance > pos - start {
                        break Err(Error::Invalid("distance before the start of the stream"));
                    }
                    if pos + length > limit {
                        break Err(Error::TooLarge);
                    }
                    copy_match(buf, pos, distance, length);
                    pos += length;
                }
                END => break Ok(true),
                _ => break Err(Error::Invalid("invalid literal/length code")),
            }
            if bits.pos > bits.data.len() + 8
                && let Err(e) = bits.check()
            {
                break Err(e);
            }
        };
        out.pos = pos;
        match done {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Writes `length` bytes at `pos` copied from `distance` back (they may
/// overlap). `buf` holds [`SLACK`] bytes past `pos`; copies of eight bytes
/// may write up to seven bytes past the match.
#[inline(always)]
fn copy_match(buf: &mut [u8], pos: usize, distance: usize, length: usize) {
    let from = pos - distance;
    if distance >= 8 {
        // Each eight-byte read comes from bytes already final: the source
        // runs at least eight bytes behind the destination.
        let mut i = 0;
        while i < length {
            buf.copy_within(from + i..from + i + 8, pos + i);
            i += 8;
        }
    } else if distance == 1 {
        let byte = buf[from];
        buf[pos..pos + length].fill(byte);
    } else {
        // A period shorter than a word: write its first repeats byte by
        // byte up to a multiple of the period of at least eight bytes, then
        // copy words from that far back (the same pattern, already final).
        let span = distance * 8_usize.div_ceil(distance);
        let head = span.min(length);
        for i in 0..head {
            buf[pos + i] = buf[from + i];
        }
        let mut i = head;
        while i < length {
            buf.copy_within(pos + i - span..pos + i - span + 8, pos + i);
            i += 8;
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
        for distance in 1..=20 {
            for length in [3, 7, 8, 9, 31, 258] {
                let mut buf: Vec<u8> = (0..distance as u8).collect();
                let pos = buf.len();
                buf.resize(pos + length + SLACK, 0xEE);
                copy_match(&mut buf, pos, distance, length);
                let expected: Vec<u8> = (0..pos + length).map(|i| (i % distance) as u8).collect();
                assert_eq!(&buf[..pos + length], &expected[..], "{distance} {length}");
            }
        }
    }

    #[test]
    fn literal_pairs_cover_short_codes() {
        let (litlen, _) = fixed_tables();
        // Fixed literals 0..144 take 8 bits: 8 + 8 > 11, no pairs; lengths
        // and end of block take 7.
        assert!(
            litlen.entries[..1 << LITLEN_BITS]
                .iter()
                .all(|&e| kind(e) != LITERALS)
        );
        // A code where 'a' and 'b' take 2 bits each pairs them.
        let mut lengths = [0u8; 257];
        lengths[b'a' as usize] = 1;
        lengths[b'b' as usize] = 2;
        lengths[256] = 2;
        let table = Table::litlen(&lengths).unwrap();
        assert!(table.entries.iter().any(|&e| kind(e) == LITERALS));
    }
}
