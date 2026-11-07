//! Huffman decoding tables: an entry's layout, a code's table (its first
//! level, then its second-level tables), built in place for a dynamic block,
//! and the fixed block's.
use crate::Error;
use crate::tables::{
    DIST_BASE, DIST_EXTRA, END_OF_BLOCK, LENGTH_BASE, LENGTH_EXTRA, LITLEN_SYMBOLS,
    fixed_dist_lengths, fixed_litlen_lengths,
};
use std::sync::OnceLock;

/// First-level index bits of the literal/length and distance tables.
pub(super) const LITLEN_BITS: u32 = 11;
pub(super) const DIST_BITS: u32 = 10;
/// The code length code's codes are at most 7 bits: one level.
pub(super) const CODE_LENGTH_BITS: u32 = 7;

/// Entry layout: bits 0..8 the bits it takes (a length's or a distance's
/// extra bits included), 8..12 its code's own length (where those extra
/// bits start), 12..16 what it is, 16..32 its payload. A length, a
/// distance or a code length symbol has no flag: its payload is its base or
/// its symbol.
pub(super) const CODE_SHIFT: u32 = 8;
/// A literal, in bits 16..24.
pub(super) const LITERAL: u32 = 1 << 15;
/// No literal, length or distance: a second-level table ([`SUBTABLE`]),
/// the end of the block ([`END`]) or a code no symbol has ([`INVALID`]).
pub(super) const EXCEPTIONAL: u32 = 1 << 13;
/// With [`EXCEPTIONAL`]: a second-level table, its offset in bits 16..28
/// and its index bits in 28..32.
pub(super) const SUBTABLE: u32 = 1 << 12;
/// A code no symbol has: payload 0.
pub(super) const INVALID: u32 = EXCEPTIONAL;
/// The end of the block: payload 1.
pub(super) const END: u32 = EXCEPTIONAL | 1 << 16;

/// Whether `e` ends the block: its flags and payload, whatever its bits.
#[inline(always)]
pub(super) const fn is_end(e: u32) -> bool {
    e & !0xFFF == END
}

/// An entry for a code of `code` bits followed by `extra` extra bits.
pub(super) const fn entry(code: u32, extra: u32, flags: u32, payload: u32) -> u32 {
    (code + extra) | (code << CODE_SHIFT) | flags | (payload << 16)
}

/// A second-level table's entry index for `e` (a [`SUBTABLE`] entry
/// already consumed) and the bits after it.
#[inline(always)]
pub(super) fn sub_index(e: u32, buffer: u64) -> usize {
    let bits = e >> 28;
    ((e >> 16) & 0xFFF) as usize + (buffer & !u64::MAX.wrapping_shl(bits)) as usize
}

/// A Huffman decoding table: the first level, then the second-level
/// tables. A dynamic block's are built again in place, in the same memory.
#[derive(Default)]
pub(super) struct Table {
    pub(super) entries: Vec<u32>,
    pub(super) mask: u64,
}

impl Table {
    /// The table for `lengths` (0 = unused symbol) with `bits` first-level
    /// bits; `value` gives each symbol's flags, payload and extra-bit
    /// count. Over-subscribed codes fail. A literal/length or distance code
    /// may be incomplete only as a single one-bit code or no code at all
    /// (RFC 1951 §3.2.7, as zlib reads it); its unused entries fail when
    /// decoded. A code length code (`complete`) must be complete.
    pub(super) fn build(
        &mut self,
        lengths: &[u8],
        bits: u32,
        complete: bool,
        value: impl Fn(usize) -> SymbolValue,
    ) -> Result<(), Error> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[usize::from(l)] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for &count in &counts[1..] {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return Err(Error::Invalid("over-subscribed Huffman code"));
            }
        }
        let used: u16 = counts.iter().sum();
        if left > 0 && (complete || !(used == 0 || (used == 1 && counts[1] == 1))) {
            return Err(Error::Invalid("incomplete Huffman code"));
        }
        // The symbols in canonical order: by code length, then by symbol.
        let mut offsets = [0u16; 16];
        for l in 1..15 {
            offsets[l + 1] = offsets[l] + counts[l];
        }
        let mut sorted = [0u16; LITLEN_SYMBOLS];
        for (symbol, &l) in lengths.iter().enumerate() {
            if l > 0 {
                sorted[usize::from(offsets[usize::from(l)])] = symbol as u16;
                offsets[usize::from(l)] += 1;
            }
        }
        // Each code in canonical order, bit-reversed into reading order. The
        // first level grows a code length at a time: doubled, every entry
        // reads one more bit that its shorter code ignores, then each code
        // of that length takes its one entry. Longer codes wait for their
        // second-level tables: those sharing their first-level bits are
        // consecutive, the last the longest, and share one table as large
        // as it needs.
        let size = 1usize << bits;
        let entries = &mut self.entries;
        entries.clear();
        entries.resize(size, INVALID);
        let (mut code, mut at) = (0u32, 0);
        let mut long = [(0u32, 0u32, 0u16); LITLEN_SYMBOLS];
        let mut longs = 0;
        for l in 1..16 {
            if l <= bits {
                entries.copy_within(..1 << (l - 1), 1 << (l - 1));
            }
            for &symbol in &sorted[at..at + usize::from(counts[l as usize])] {
                let read = code.reverse_bits() >> (32 - l);
                if l <= bits {
                    let (flags, payload, extra) = value(usize::from(symbol));
                    entries[read as usize] = entry(l, extra, flags, payload);
                } else {
                    long[longs] = (read, l, symbol);
                    longs += 1;
                }
                code += 1;
            }
            at += usize::from(counts[l as usize]);
            code <<= 1;
        }
        let mask = size as u32 - 1;
        for group in long[..longs].chunk_by(|a, b| a.0 & mask == b.0 & mask) {
            let sub = group[group.len() - 1].1 - bits;
            let offset = entries.len();
            entries[(group[0].0 & mask) as usize] =
                entry(bits, 0, EXCEPTIONAL | SUBTABLE, offset as u32 | (sub << 12));
            entries.resize(offset + (1 << sub), INVALID);
            for &(read, l, symbol) in group {
                let (flags, payload, extra) = value(usize::from(symbol));
                let e = entry(l - bits, extra, flags, payload);
                for slot in ((read >> bits) as usize..1 << sub).step_by(1 << (l - bits)) {
                    entries[offset + slot] = e;
                }
            }
        }
        self.mask = u64::from(mask);
        Ok(())
    }

    /// The literal/length table for `lengths`, in place: literals, lengths
    /// with their extra bits, end of block.
    pub(super) fn build_litlen(&mut self, lengths: &[u8]) -> Result<(), Error> {
        self.build(lengths, LITLEN_BITS, false, litlen_value)
    }

    /// The distance table for `lengths`, in place.
    pub(super) fn build_dist(&mut self, lengths: &[u8]) -> Result<(), Error> {
        self.build(lengths, DIST_BITS, false, dist_value)
    }

    /// The first level, `N` entries.
    pub(super) fn first<const N: usize>(&self) -> &[u32; N] {
        (self.entries[..N]).try_into().expect("a first level")
    }
}

/// A symbol's entry flags, payload and extra-bit count.
type SymbolValue = (u32, u32, u32);

/// A literal/length symbol's flags, payload and extra-bit count.
fn litlen_value(symbol: usize) -> SymbolValue {
    match symbol {
        0..=255 => (LITERAL, symbol as u32, 0),
        END_OF_BLOCK => (END, 0, 0),
        257..=285 => {
            let s = symbol - 257;
            (0, u32::from(LENGTH_BASE[s]), u32::from(LENGTH_EXTRA[s]))
        }
        _ => (INVALID, 0, 0),
    }
}

/// A distance symbol's flags, payload and extra-bit count.
fn dist_value(symbol: usize) -> SymbolValue {
    match symbol {
        0..=29 => (
            0,
            u32::from(DIST_BASE[symbol]),
            u32::from(DIST_EXTRA[symbol]),
        ),
        _ => (INVALID, 0, 0),
    }
}

pub(super) fn fixed_tables() -> &'static (Table, Table) {
    static FIXED: OnceLock<(Table, Table)> = OnceLock::new();
    FIXED.get_or_init(|| {
        let (mut litlen, mut dist) = (Table::default(), Table::default());
        litlen
            .build_litlen(&fixed_litlen_lengths())
            .expect("fixed literal code");
        dist.build_dist(&fixed_dist_lengths())
            .expect("fixed distance code");
        (litlen, dist)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every 15-bit input decodes, through the first level and the
    /// second, to the symbol whose canonical code it starts with (RFC 1951
    /// §3.2.2), taking that code's bits: the fixed codes, a lone one-bit
    /// code, and random complete codes up to 15 bits long.
    #[test]
    fn tables_decode_every_input_canonically() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut random = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut cases = vec![
            (fixed_litlen_lengths().to_vec(), true),
            (fixed_dist_lengths().to_vec(), false),
            (vec![1], false),
            (vec![0, 0, 0, 1], false),
        ];
        for case in 0..24 {
            let symbols = if case % 2 == 0 { 286 } else { 30 };
            // Skewed frequencies make long codes, and second-level tables.
            let freqs: Vec<u32> = (0..symbols)
                .map(|_| match random() {
                    r if r % 4 == 0 => 0,
                    r => 1 << ((r >> 8) % 24),
                })
                .collect();
            cases.push((crate::huffman::lengths(&freqs, 15), symbols == 286));
        }
        let mut second_levels = 0;
        for (lengths, litlen) in &cases {
            let mut table = Table::default();
            let (bits, value): (u32, fn(usize) -> SymbolValue) = match litlen {
                true => (LITLEN_BITS, litlen_value),
                false => (DIST_BITS, dist_value),
            };
            table.build(lengths, bits, false, value).unwrap();
            second_levels += table.entries.len() - (1 << bits);
            // The symbol and code length each input starts with.
            let mut expected = vec![None; 1 << 15];
            let mut counts = [0u32; 16];
            for &l in lengths {
                counts[usize::from(l)] += 1;
            }
            counts[0] = 0;
            let mut next = [0u32; 16];
            for l in 1..16 {
                next[l] = (next[l - 1] + counts[l - 1]) << 1;
            }
            for (symbol, &l) in lengths.iter().enumerate().filter(|(_, l)| **l > 0) {
                let l = u32::from(l);
                let read = next[l as usize].reverse_bits() >> (32 - l);
                next[l as usize] += 1;
                for input in (read as usize..1 << 15).step_by(1 << l) {
                    expected[input] = Some((symbol, l));
                }
            }
            for (input, expected) in expected.into_iter().enumerate() {
                let mut e = table.entries[input & table.mask as usize];
                let mut taken = 0;
                if e & SUBTABLE != 0 {
                    taken = e & 0xFF;
                    e = table.entries[sub_index(e, (input >> taken) as u64)];
                }
                let Some((symbol, l)) = expected else {
                    assert_eq!(e & !0xFFF, INVALID, "{input:#x} decodes nothing");
                    continue;
                };
                let (flags, payload, extra) = value(symbol);
                assert_eq!(e, entry(l - taken, extra, flags, payload), "{input:#x}");
            }
        }
        assert!(second_levels > 0, "some codes reach the second level");
    }
}
