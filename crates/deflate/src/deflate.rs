//! Compression into raw DEFLATE streams (RFC 1951).
//!
//! LZ77 over hash chains of four-byte prefixes (zlib's scheme: a head per
//! hash, a previous link per window position, a chain-length budget and,
//! above [`Level::Fast`], one-step lazy matching). Symbols gather into
//! blocks; each block is written as whichever of dynamic Huffman, fixed
//! Huffman or stored costs fewest bits.
use crate::huffman;
use crate::tables::{
    CODE_LENGTH_ORDER, DIST_EXTRA, END_OF_BLOCK, LENGTH_BASE, LENGTH_EXTRA, LENGTH_SYMBOL,
    MAX_BITS, MAX_CODE_LENGTH_BITS, MAX_MATCH, MIN_MATCH, WINDOW, dist_symbol, fixed_dist_lengths,
    fixed_litlen_lengths,
};

/// How hard the encoder looks for matches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// Short chains, no lazy matching: the fastest, for scratch output.
    Fast,
    /// zlib's default trade-off.
    #[default]
    Balanced,
    /// Long chains: the smallest output, several times slower.
    Small,
}

/// Search parameters per level.
struct Params {
    /// Chain links followed per search.
    chain: usize,
    /// A match this long ends the search.
    nice: usize,
    /// Lazy matching: look one byte ahead unless the match is this long.
    lazy: Option<usize>,
}

impl Level {
    fn params(self) -> Params {
        match self {
            Level::Fast => Params {
                chain: 4,
                nice: 32,
                lazy: None,
            },
            Level::Balanced => Params {
                chain: 128,
                nice: 128,
                lazy: Some(16),
            },
            Level::Small => Params {
                chain: 2048,
                nice: MAX_MATCH,
                lazy: Some(MAX_MATCH),
            },
        }
    }
}

const HASH_BITS: u32 = 15;
const WINDOW_MASK: usize = WINDOW - 1;
/// Symbols gathered before a block is written.
const BLOCK_SYMBOLS: usize = 1 << 15;
/// A stored block's longest body.
const STORED_MAX: usize = 65_535;

/// Writes bits least-significant first.
pub(crate) struct BitWriter {
    pub(crate) out: Vec<u8>,
    buffer: u64,
    count: u32,
}

impl BitWriter {
    pub(crate) fn new(out: Vec<u8>) -> Self {
        Self {
            out,
            buffer: 0,
            count: 0,
        }
    }

    /// Writes the low `n` (at most 32) bits of `value`.
    #[inline]
    fn put(&mut self, value: u32, n: u32) {
        self.buffer |= u64::from(value) << self.count;
        self.count += n;
        if self.count >= 32 {
            self.out
                .extend_from_slice(&(self.buffer as u32).to_le_bytes());
            self.buffer >>= 32;
            self.count -= 32;
        }
    }

    /// Pads to a byte boundary with zero bits and writes out the buffer.
    fn align(&mut self) {
        while self.count > 0 {
            self.out.push(self.buffer as u8);
            self.buffer >>= 8;
            self.count = self.count.saturating_sub(8);
        }
        self.buffer = 0;
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

/// One LZ77 symbol: a literal byte, or a match.
#[derive(Clone, Copy)]
struct Symbol(u32);

impl Symbol {
    const MATCH: u32 = 1 << 31;

    fn literal(byte: u8) -> Self {
        Self(u32::from(byte))
    }

    fn reference(length: usize, distance: usize) -> Self {
        Self(Self::MATCH | ((length as u32) << 16) | distance as u32)
    }
}

/// Symbols of the block being gathered, with their frequencies.
struct Block {
    symbols: Vec<Symbol>,
    litlen: [u32; 286],
    dist: [u32; 30],
    /// The input the block covers starts here.
    start: usize,
}

impl Block {
    fn new(start: usize) -> Self {
        Self {
            symbols: Vec::with_capacity(BLOCK_SYMBOLS),
            litlen: [0; 286],
            dist: [0; 30],
            start,
        }
    }

    #[inline]
    fn literal(&mut self, byte: u8) {
        self.litlen[byte as usize] += 1;
        self.symbols.push(Symbol::literal(byte));
    }

    #[inline]
    fn reference(&mut self, length: usize, distance: usize) {
        self.litlen[257 + LENGTH_SYMBOL[length - 3] as usize] += 1;
        self.dist[dist_symbol(distance)] += 1;
        self.symbols.push(Symbol::reference(length, distance));
    }
}

/// Compresses `data` into a raw DEFLATE stream appended to `out`.
pub fn deflate_into(data: &[u8], level: Level, out: Vec<u8>) -> Vec<u8> {
    let mut writer = BitWriter::new(out);
    let params = level.params();
    let mut head = vec![0u32; 1 << HASH_BITS];
    let mut prev = vec![0u32; WINDOW];
    let mut block = Block::new(0);
    let mut pos = 0;
    // A match found at `pos - 1` waiting for the lazy check at `pos`.
    let mut pending: Option<(usize, usize)> = None;
    let hash_end = data.len().saturating_sub(3);
    while pos < data.len() {
        let (length, distance) = if pos < hash_end {
            let found = longest_match(data, pos, &head, &prev, &params, pending);
            insert(data, pos, &mut head, &mut prev);
            found
        } else {
            (0, 0)
        };
        if let Some((p_len, p_dist)) = pending.take() {
            if length > p_len {
                // The later match is better: the earlier byte goes as a literal.
                block.literal(data[pos - 1]);
            } else {
                block.reference(p_len, p_dist);
                // The match started at pos - 1; pos is already inserted.
                let end = pos - 1 + p_len;
                for p in pos + 1..end.min(hash_end) {
                    insert(data, p, &mut head, &mut prev);
                }
                pos = end;
                if block.symbols.len() >= BLOCK_SYMBOLS {
                    write_block(&mut writer, &block, &data[block.start..pos], false);
                    block = Block::new(pos);
                }
                continue;
            }
        }
        if length >= MIN_MATCH {
            if params.lazy.is_some_and(|lazy| length < lazy) && pos + 1 < data.len() {
                pending = Some((length, distance));
                pos += 1;
                continue;
            }
            block.reference(length, distance);
            for p in pos + 1..(pos + length).min(hash_end) {
                insert(data, p, &mut head, &mut prev);
            }
            pos += length;
        } else {
            block.literal(data[pos]);
            pos += 1;
        }
        if block.symbols.len() >= BLOCK_SYMBOLS {
            write_block(&mut writer, &block, &data[block.start..pos], false);
            block = Block::new(pos);
        }
    }
    if let Some((p_len, p_dist)) = pending {
        block.reference(p_len, p_dist);
    }
    write_block(&mut writer, &block, &data[block.start..], true);
    writer.finish()
}

#[inline]
fn hash(data: &[u8], pos: usize) -> usize {
    let word = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
    (word.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Links `pos` into its hash chain (positions are stored plus one).
#[inline]
fn insert(data: &[u8], pos: usize, head: &mut [u32], prev: &mut [u32]) {
    let h = hash(data, pos);
    prev[pos & WINDOW_MASK] = head[h];
    head[h] = pos as u32 + 1;
}

/// The longest earlier match for `pos` within the window, as (length,
/// distance); length 0 when there is none of at least [`MIN_MATCH`] (or
/// none longer than the `pending` lazy match).
fn longest_match(
    data: &[u8],
    pos: usize,
    head: &[u32],
    prev: &[u32],
    params: &Params,
    pending: Option<(usize, usize)>,
) -> (usize, usize) {
    let max = (data.len() - pos).min(MAX_MATCH);
    let mut best = pending.map_or(MIN_MATCH - 1, |(l, _)| l);
    if best >= max {
        return (0, 0);
    }
    let mut chain = params.chain;
    // A good pending match halves the search, as zlib does.
    if best >= params.lazy.unwrap_or(usize::MAX) / 2 {
        chain = chain.div_ceil(4);
    }
    let mut best_distance = 0;
    let mut link = head[hash(data, pos)];
    let window_start = pos.saturating_sub(WINDOW);
    let here = &data[pos..pos + max];
    while link != 0 && chain > 0 {
        let candidate = link as usize - 1;
        if candidate < window_start || candidate >= pos {
            break;
        }
        chain -= 1;
        let there = &data[candidate..];
        if there[best] == here[best] && there[0] == here[0] {
            let length = common_prefix(there, here);
            if length > best {
                best = length;
                best_distance = pos - candidate;
                if length >= params.nice || length == max {
                    break;
                }
            }
        }
        let next = prev[candidate & WINDOW_MASK];
        // Links only point backwards; a forward one is a stale slot.
        if next >= link {
            break;
        }
        link = next;
    }
    if best_distance == 0 {
        (0, 0)
    } else {
        (best, best_distance)
    }
}

/// How many leading bytes `a` and `b` share (`b` bounds the count).
#[inline]
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let mut n = 0;
    while n + 8 <= b.len() {
        let x = u64::from_le_bytes(a[n..n + 8].try_into().expect("eight bytes"));
        let y = u64::from_le_bytes(b[n..n + 8].try_into().expect("eight bytes"));
        let diff = x ^ y;
        if diff != 0 {
            return n + (diff.trailing_zeros() / 8) as usize;
        }
        n += 8;
    }
    while n < b.len() && a[n] == b[n] {
        n += 1;
    }
    n
}

/// A Huffman code: lengths and bit-reversed codes.
struct Code {
    lengths: Vec<u8>,
    codes: Vec<u16>,
}

impl Code {
    fn new(lengths: Vec<u8>) -> Self {
        let codes = huffman::codes(&lengths);
        Self { lengths, codes }
    }

    #[inline]
    fn put(&self, writer: &mut BitWriter, symbol: usize) {
        writer.put(
            u32::from(self.codes[symbol]),
            u32::from(self.lengths[symbol]),
        );
    }
}

/// The code length symbols (with their extra bits) for `lengths`.
fn run_lengths(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let value = lengths[i];
        let mut run = 1;
        while i + run < lengths.len() && lengths[i + run] == value {
            run += 1;
        }
        i += run;
        if value == 0 {
            while run >= 11 {
                let n = run.min(138);
                out.push((18, (n - 11) as u8));
                run -= n;
            }
            if run >= 3 {
                out.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            out.push((value, 0));
            run -= 1;
            while run >= 3 {
                let n = run.min(6);
                out.push((16, (n - 3) as u8));
                run -= n;
            }
        }
        out.extend(std::iter::repeat_n((value, 0), run));
    }
    out
}

const CODE_LENGTH_EXTRA: [u32; 19] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Bits the block's symbols take with these code lengths.
fn data_bits(block: &Block, litlen: &[u8], dist: &[u8]) -> u64 {
    let mut bits = u64::from(litlen[END_OF_BLOCK]);
    for (s, &f) in block.litlen.iter().enumerate() {
        let extra = if s > 256 { LENGTH_EXTRA[s - 257] } else { 0 };
        bits += u64::from(f) * u64::from(litlen[s] + extra);
    }
    for (s, &f) in block.dist.iter().enumerate() {
        bits += u64::from(f) * u64::from(dist[s] + DIST_EXTRA[s]);
    }
    bits
}

/// Writes `block` (covering `input`) in its cheapest form.
fn write_block(writer: &mut BitWriter, block: &Block, input: &[u8], last: bool) {
    let mut litlen_freqs = block.litlen;
    litlen_freqs[END_OF_BLOCK] = 1;
    let mut litlen = huffman::lengths(&litlen_freqs, MAX_BITS);
    let mut dist = huffman::lengths(&block.dist, MAX_BITS);
    if dist.iter().all(|&l| l == 0) {
        // No matches: one unused one-bit distance code keeps every decoder happy.
        dist[0] = 1;
    }
    let hlit = 257
        + litlen[257..]
            .iter()
            .rposition(|&l| l > 0)
            .map_or(0, |p| p + 1);
    let hdist = 1 + dist[1..].iter().rposition(|&l| l > 0).map_or(0, |p| p + 1);
    litlen.truncate(hlit);
    dist.truncate(hdist);
    let mut all = litlen.clone();
    all.extend_from_slice(&dist);
    let runs = run_lengths(&all);
    let mut cl_freqs = [0u32; 19];
    for &(s, _) in &runs {
        cl_freqs[s as usize] += 1;
    }
    if cl_freqs.iter().filter(|&&f| f > 0).count() == 1 {
        // The code length code must be complete: give it a second symbol.
        let unused = usize::from(cl_freqs[0] > 0);
        cl_freqs[unused] = 1;
    }
    let cl_lengths = huffman::lengths(&cl_freqs, MAX_CODE_LENGTH_BITS);
    let hclen = 4 + CODE_LENGTH_ORDER[4..]
        .iter()
        .rposition(|&s| cl_lengths[s] > 0)
        .map_or(0, |p| p + 1);
    let header_bits: u64 = 14
        + 3 * hclen as u64
        + runs
            .iter()
            .map(|&(s, _)| {
                u64::from(cl_lengths[s as usize]) + u64::from(CODE_LENGTH_EXTRA[s as usize])
            })
            .sum::<u64>();
    let dynamic = 3 + header_bits + data_bits(block, &litlen_padded(&litlen), &dist_padded(&dist));
    let fixed_litlen = fixed_litlen_lengths();
    let fixed_dist = fixed_dist_lengths();
    let fixed = 3 + data_bits(block, &fixed_litlen, &fixed_dist);
    let stored_blocks = input.len().div_ceil(STORED_MAX).max(1) as u64;
    // Header, padding to a byte (at most 7 bits) and LEN/NLEN per block.
    let stored = stored_blocks * (3 + 7 + 32) + 8 * input.len() as u64;
    if stored <= dynamic.min(fixed) {
        write_stored(writer, input, last);
    } else if fixed <= dynamic {
        writer.put(u32::from(last) | (1 << 1), 3);
        write_symbols(
            writer,
            block,
            &Code::new(fixed_litlen.to_vec()),
            &Code::new(fixed_dist.to_vec()),
        );
    } else {
        writer.put(u32::from(last) | (2 << 1), 3);
        writer.put((hlit - 257) as u32, 5);
        writer.put((hdist - 1) as u32, 5);
        writer.put((hclen - 4) as u32, 4);
        for &s in &CODE_LENGTH_ORDER[..hclen] {
            writer.put(u32::from(cl_lengths[s]), 3);
        }
        let cl = Code::new(cl_lengths);
        for &(s, extra) in &runs {
            cl.put(writer, s as usize);
            let n = CODE_LENGTH_EXTRA[s as usize];
            if n > 0 {
                writer.put(u32::from(extra), n);
            }
        }
        write_symbols(
            writer,
            block,
            &Code::new(litlen_padded(&litlen)),
            &Code::new(dist_padded(&dist)),
        );
    }
}

fn litlen_padded(lengths: &[u8]) -> Vec<u8> {
    let mut padded = lengths.to_vec();
    padded.resize(286, 0);
    padded
}

fn dist_padded(lengths: &[u8]) -> Vec<u8> {
    let mut padded = lengths.to_vec();
    padded.resize(30, 0);
    padded
}

fn write_symbols(writer: &mut BitWriter, block: &Block, litlen: &Code, dist: &Code) {
    for &Symbol(s) in &block.symbols {
        if s & Symbol::MATCH == 0 {
            litlen.put(writer, s as usize);
            continue;
        }
        let length = ((s >> 16) & 0x1FF) as usize;
        let distance = (s & 0xFFFF) as usize;
        let ls = LENGTH_SYMBOL[length - 3] as usize;
        litlen.put(writer, 257 + ls);
        let extra = u32::from(LENGTH_EXTRA[ls]);
        if extra > 0 {
            writer.put((length - LENGTH_BASE[ls] as usize) as u32, extra);
        }
        let ds = dist_symbol(distance);
        dist.put(writer, ds);
        let extra = u32::from(DIST_EXTRA[ds]);
        if extra > 0 {
            writer.put(
                (distance - crate::tables::DIST_BASE[ds] as usize) as u32,
                extra,
            );
        }
    }
    litlen.put(writer, END_OF_BLOCK);
}

fn write_stored(writer: &mut BitWriter, input: &[u8], last: bool) {
    let mut chunks = input.chunks(STORED_MAX).peekable();
    if chunks.peek().is_none() {
        writer.put(u32::from(last), 3);
        writer.align();
        writer.out.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
        return;
    }
    while let Some(chunk) = chunks.next() {
        let final_chunk = last && chunks.peek().is_none();
        writer.put(u32::from(final_chunk), 3);
        writer.align();
        let len = chunk.len() as u16;
        writer.out.extend_from_slice(&len.to_le_bytes());
        writer.out.extend_from_slice(&(!len).to_le_bytes());
        writer.out.extend_from_slice(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_length_symbols() {
        let runs = run_lengths(&[0, 0, 0, 5, 5, 5, 5, 5, 0, 0, 7]);
        let symbols: Vec<u8> = runs.iter().map(|r| r.0).collect();
        assert_eq!(symbols, vec![17, 5, 16, 0, 0, 7]);
        assert_eq!(runs[2].1, 1, "four repeats");
        let zeros = run_lengths(&[0; 150]);
        assert_eq!(zeros, vec![(18, 127), (18, 1)]);
    }

    #[test]
    fn common_prefix_counts_across_words() {
        let a = b"abcdefghijklmnopq";
        assert_eq!(common_prefix(a, b"abcdefghijklmnopq"), 17);
        assert_eq!(common_prefix(a, b"abcdefghijkXmnop"), 11);
        assert_eq!(common_prefix(a, b"abX"), 2);
    }
}
