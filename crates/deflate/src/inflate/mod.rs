//! Decompression of raw DEFLATE streams (RFC 1951).
//!
//! Bits are read least-significant first from a 64-bit buffer refilled a
//! word at a time, so one refill covers a whole length/distance pair. Each
//! Huffman code decodes through a table indexed by the next bits (11 for
//! literals/lengths, 10 for distances); longer codes continue in a
//! second-level table. An entry says how many bits it takes, a length's or
//! a distance's extra bits included, so one shift consumes the symbol and
//! its extra bits together; it carries a literal, or a length or distance
//! base. A dynamic block's tables are built in place, the first level
//! doubled a code length at a time. Output goes into a buffer kept a match
//! plus a chunk longer than what is written, so matches copy whole chunks.
mod bits;
mod table;

use crate::Error;
use crate::tables::{CODE_LENGTH_ORDER, END_OF_BLOCK, MAX_MATCH};
use bits::Bits;
use table::{
    CODE_LENGTH_BITS, DIST_BITS, EXCEPTIONAL, LITERAL, LITLEN_BITS, SUBTABLE, Table, fixed_tables,
    is_end, sub_index,
};

/// Bytes written past a match's end by whole-chunk copies, plus one
/// match: the output buffer always has this much room beyond `pos`.
const SLACK: usize = MAX_MATCH + 16;

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
    let mut dynamic = Dynamic::default();
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
                dynamic.read(&mut bits)?;
                block(&mut bits, &dynamic.litlen, &dynamic.dist, out)?;
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

/// A dynamic block's tables, built again in place for each block.
#[derive(Default)]
struct Dynamic {
    code_lengths: Table,
    litlen: Table,
    dist: Table,
}

impl Dynamic {
    /// Reads a dynamic block's code definitions into the tables.
    fn read(&mut self, bits: &mut Bits) -> Result<(), Error> {
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
        // Complete, so every entry is a symbol.
        let table = &mut self.code_lengths;
        table.build(&code_lengths, CODE_LENGTH_BITS, true, |s| (0, s as u32, 0))?;
        let mut lengths = [0u8; 286 + 30];
        let mut i = 0;
        while i < hlit + hdist {
            bits.refill();
            let e = bits.decode(table);
            bits.take_entry(e);
            let symbol = (e >> 16) as usize;
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
        self.litlen.build_litlen(&lengths[..hlit])?;
        self.dist.build_dist(&lengths[hlit..hlit + hdist])
    }
}

/// Decodes one Huffman block, on a copy of the bit reader kept in
/// registers.
fn block(bits: &mut Bits, litlen: &Table, dist: &Table, out: &mut Output) -> Result<(), Error> {
    let mut local = Bits { ..*bits };
    let result = block_with(&mut local, litlen, dist, out);
    *bits = local;
    result
}

#[inline(always)]
fn block_with(
    bits: &mut Bits,
    litlen: &Table,
    dist: &Table,
    out: &mut Output,
) -> Result<(), Error> {
    let first: &[u32; 1 << LITLEN_BITS] = litlen.first();
    let dist_first: &[u32; 1 << DIST_BITS] = dist.first();
    let lookup = |buffer: u64| first[(buffer & ((1 << LITLEN_BITS) - 1)) as usize];
    loop {
        out.reserve();
        // Below `end` there is room in the buffer for any symbol; below
        // `fast` any symbol also stays within the limit, so the loop there
        // checks neither (a match is at most 258 bytes).
        let end = out.buf.len() - SLACK;
        let fast = end.min(out.limit.saturating_sub(MAX_MATCH));
        let (buf, start, limit) = (&mut out.buf[..], out.start, out.limit);
        let mut pos = out.pos;
        // The next symbol's entry is looked up ahead, the buffer holding at
        // least 56 bits for it: refills only add bits above those already
        // buffered, so it stays valid.
        bits.refill();
        let mut e = lookup(bits.buffer);
        let done = loop {
            if pos >= fast {
                break Ok(false);
            }
            if e & LITERAL != 0 {
                // Up to three literals a refill: 56 bits cover two
                // first-level codes (22) and the next symbol's 48.
                bits.take_entry(e);
                buf[pos] = (e >> 16) as u8;
                e = lookup(bits.buffer);
                if e & LITERAL != 0 {
                    bits.take_entry(e);
                    buf[pos + 1] = (e >> 16) as u8;
                    e = lookup(bits.buffer);
                    if e & LITERAL != 0 {
                        bits.take_entry(e);
                        buf[pos + 2] = (e >> 16) as u8;
                        pos += 3;
                        e = lookup(bits.buffer);
                        bits.refill();
                        continue;
                    }
                    pos += 1;
                }
                pos += 1;
                bits.refill();
                continue;
            }
            if e & EXCEPTIONAL == 0 {
                // A length: 56 bits cover it (15 + 5) and a distance (15 +
                // 13).
                let length = (e >> 16) as usize + bits.take_entry(e);
                let mut d = dist_first[(bits.buffer & ((1 << DIST_BITS) - 1)) as usize];
                if d & SUBTABLE != 0 {
                    bits.take_entry(d);
                    d = dist.entries[sub_index(d, bits.buffer)];
                }
                if d & EXCEPTIONAL != 0 {
                    break Err(Error::Invalid("invalid distance code"));
                }
                let distance = (d >> 16) as usize + bits.take_entry(d);
                if distance > pos - start {
                    break Err(Error::Invalid("distance before the start of the stream"));
                }
                if bits.pos > bits.data.len() + 8
                    && let Err(e) = bits.check()
                {
                    break Err(e);
                }
                // The next entry first, its lookup running beside the copy:
                // from the bits left when they hold a first-level index.
                if bits.count >= LITLEN_BITS {
                    e = lookup(bits.buffer);
                    bits.refill();
                } else {
                    bits.refill();
                    e = lookup(bits.buffer);
                }
                copy_match(buf, pos, distance, length);
                pos += length;
                continue;
            }
            if e & SUBTABLE != 0 {
                // The rest of a long code: decoded as a first-level entry.
                bits.take_entry(e);
                e = litlen.entries[sub_index(e, bits.buffer)];
                continue;
            }
            if is_end(e) {
                bits.take_entry(e);
                break Ok(true);
            }
            break Err(Error::Invalid("invalid literal/length code"));
        };
        let done = match done {
            // Near the limit (or the buffer's end): one symbol at a time,
            // each checked against the limit.
            Ok(false) if pos < end => careful(bits, litlen, dist, buf, start, limit, &mut pos),
            other => other,
        };
        out.pos = pos;
        match done {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Decodes symbols one at a time with every write checked against
/// `limit`, until the buffer's end region (`Ok(false)`) or the block's end.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn careful(
    bits: &mut Bits,
    litlen: &Table,
    dist: &Table,
    buf: &mut [u8],
    start: usize,
    limit: usize,
    pos: &mut usize,
) -> Result<bool, Error> {
    let end = buf.len() - SLACK;
    while *pos < end {
        bits.refill();
        let e = bits.decode(litlen);
        if e & LITERAL != 0 {
            if *pos == limit {
                return Err(Error::TooLarge);
            }
            bits.take_entry(e);
            buf[*pos] = (e >> 16) as u8;
            *pos += 1;
        } else if e & EXCEPTIONAL == 0 {
            let length = (e >> 16) as usize + bits.take_entry(e);
            let d = bits.decode(dist);
            if d & EXCEPTIONAL != 0 {
                return Err(Error::Invalid("invalid distance code"));
            }
            let distance = (d >> 16) as usize + bits.take_entry(d);
            if distance > *pos - start {
                return Err(Error::Invalid("distance before the start of the stream"));
            }
            if *pos + length > limit {
                return Err(Error::TooLarge);
            }
            copy_match(buf, *pos, distance, length);
            *pos += length;
        } else if is_end(e) {
            bits.take_entry(e);
            return Ok(true);
        } else {
            return Err(Error::Invalid("invalid literal/length code"));
        }
        bits.check()?;
    }
    Ok(false)
}

/// Writes `length` bytes at `pos` copied from `distance` back (they may
/// overlap). `buf` holds [`SLACK`] bytes past `pos`; the copies write whole
/// chunks, up to fifteen bytes past the match.
#[inline(always)]
fn copy_match(buf: &mut [u8], pos: usize, distance: usize, length: usize) {
    if length > 32 && distance >= length {
        // Long and apart from what it reads: one copy.
        buf.copy_within(pos - distance..pos - distance + length, pos);
    } else if distance >= 16 {
        copy_chunks::<16>(buf, pos, distance, length);
    } else if distance >= 8 {
        copy_chunks::<8>(buf, pos, distance, length);
    } else {
        // A period shorter than a word: sixteen bytes of its repeats, made
        // by doubling its bytes, written whole, each write starting where
        // the period starts again (a step a multiple of it).
        let read = u64::from_le_bytes(*chunk_mut(buf, pos - distance));
        let mut pattern = u128::from(read & !u64::MAX.wrapping_shl(8 * distance as u32));
        let mut filled = distance;
        while filled < 16 {
            pattern |= pattern << (8 * filled);
            filled *= 2;
        }
        let pattern = pattern.to_le_bytes();
        let step = 16 - 16 % distance;
        let mut at = pos;
        let end = pos + length;
        while at < end {
            *chunk_mut(buf, at) = pattern;
            at += step;
        }
    }
}

/// Copies `length` bytes to `pos` from `distance` (at least `N`) back, `N`
/// bytes at a time: each read comes from bytes already final.
#[inline(always)]
fn copy_chunks<const N: usize>(buf: &mut [u8], pos: usize, distance: usize, length: usize) {
    let mut at = pos;
    let end = pos + length;
    loop {
        let read: [u8; N] = *chunk_mut(buf, at - distance);
        *chunk_mut(buf, at) = read;
        at += N;
        if at >= end {
            break;
        }
    }
}

/// The `N` bytes of `buf` from `at`.
#[inline(always)]
fn chunk_mut<const N: usize>(buf: &mut [u8], at: usize) -> &mut [u8; N] {
    buf[at..].first_chunk_mut().expect("room for a chunk")
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
}
