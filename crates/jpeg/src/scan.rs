//! Entropy decoding of scans into coefficients: sequential (baseline)
//! blocks and the four kinds of progressive scan (T.81 Annex F and G).
use crate::Error;
use crate::coefficients::Blocks;
use crate::headers::{Frame, Scan, ZIGZAG};
use crate::huffman::{Bits, END_OF_BLOCK, Table};
use std::ops::Range;

/// What a scan decoder needs of the frame and its tables.
pub(crate) struct ScanContext<'t> {
    /// Per scan component: frame index, sampling factors, blocks across
    /// and down covered (one-component scans), and its tables.
    pub components: Vec<ScanComponent<'t>>,
    /// MCUs across and down.
    pub mcux: usize,
    pub mcuy: usize,
    /// MCUs per restart interval, 0 for none.
    pub restart: usize,
    pub ss: usize,
    pub se: usize,
    pub ah: u8,
    pub al: u8,
}

pub(crate) struct ScanComponent<'t> {
    pub index: usize,
    pub h: usize,
    pub v: usize,
    pub dc: Option<&'t Table>,
    pub ac: Option<&'t Table>,
}

impl<'t> ScanContext<'t> {
    pub fn new(
        frame: &Frame,
        scan: &Scan,
        tables: &'t [Option<Box<Table>>; 8],
        restart: usize,
        at: usize,
    ) -> Result<Self, Error> {
        let mut components = Vec::new();
        for (i, &index) in scan.components.iter().enumerate() {
            let c = &frame.components[index];
            // Progressive refinement and AC-only scans need no DC table, DC
            // scans no AC table.
            let needs_dc = scan.ss == 0 && scan.ah == 0;
            let needs_ac = scan.se > 0;
            let dc = tables[scan.dc[i]].as_deref();
            let ac = tables[4 + scan.ac[i]].as_deref();
            if (needs_dc && dc.is_none()) || (needs_ac && ac.is_none()) {
                return Err(Error::corrupt("scan uses an undefined Huffman table", at));
            }
            components.push(ScanComponent {
                index,
                h: c.h,
                v: c.v,
                dc,
                ac,
            });
        }
        let (mcux, mcuy) = if components.len() == 1 {
            let c = &frame.components[scan.components[0]];
            (c.actual_bx, c.actual_by)
        } else {
            (frame.mcux, frame.mcuy)
        };
        Ok(ScanContext {
            components,
            mcux,
            mcuy,
            restart,
            ss: scan.ss,
            se: scan.se,
            ah: scan.ah,
            al: scan.al,
        })
    }

    pub fn interleaved(&self) -> bool {
        self.components.len() > 1
    }

    pub fn mcus(&self) -> usize {
        self.mcux * self.mcuy
    }
}

/// A decoder's place between calls: DC predictors, the end-of-band run of
/// a progressive AC scan, and the next restart marker expected.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cursor {
    pub predictors: [i32; 4],
    pub eobrun: u32,
    pub expected: u8,
}

impl Cursor {
    /// The cursor at the start of a scan or restart interval `interval`.
    pub fn at_interval(interval: usize) -> Self {
        Cursor {
            predictors: [0; 4],
            eobrun: 0,
            expected: 0xD0 + (interval % 8) as u8,
        }
    }
}

/// Which coding a scan uses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Sequential,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

impl Kind {
    pub fn of(progressive: bool, context: &ScanContext) -> Kind {
        match (progressive, context.ss == 0, context.ah == 0) {
            (false, _, _) => Kind::Sequential,
            (true, true, true) => Kind::DcFirst,
            (true, true, false) => Kind::DcRefine,
            (true, false, true) => Kind::AcFirst,
            (true, false, false) => Kind::AcRefine,
        }
    }
}

/// A run of a scan's MCUs to decode: `mcus`, and whether the reader stands
/// at the start of the first one's restart interval (no marker before it).
pub(crate) struct Span {
    pub mcus: Range<usize>,
    pub started: bool,
}

impl Span {
    /// Whether a restart marker comes before MCU `m`.
    #[inline(always)]
    fn restarts_before(&self, context: &ScanContext, m: usize) -> bool {
        context.restart > 0
            && m > 0
            && m.is_multiple_of(context.restart)
            && !(self.started && m == self.mcus.start)
    }
}

/// Decodes `span` of the scan from `bits` into `blocks`, with restart
/// markers between intervals. Errors carry the reader's file offset.
pub(crate) fn decode_mcus(
    kind: Kind,
    context: &ScanContext,
    bits: &mut Bits,
    cursor: &mut Cursor,
    blocks: &mut Blocks,
    span: Span,
) -> Result<(), Error> {
    if let (Kind::Sequential, true, Blocks::Mcu { data, shape, first }) =
        (kind, context.interleaved(), &mut *blocks)
    {
        let mcus = &mut data
            [(span.mcus.start - *first) * shape.len()..(span.mcus.end - *first) * shape.len()];
        return sequential_mcus(context, bits, cursor, mcus, span);
    }
    let fail = |what, bits: &Bits| Error::corrupt(what, bits.pos);
    for m in span.mcus.clone() {
        if span.restarts_before(context, m) {
            bits.restart(cursor.expected)
                .map_err(|what| fail(what, bits))?;
            *cursor = Cursor::at_interval(m / context.restart);
        }
        let (mx, my) = (m % context.mcux, m / context.mcux);
        if context.interleaved() {
            for (i, sc) in context.components.iter().enumerate() {
                for vi in 0..sc.v {
                    for hi in 0..sc.h {
                        let block = blocks.mcu_block(
                            m,
                            sc.index,
                            (my * sc.v + vi, mx * sc.h + hi),
                            (vi, hi),
                        );
                        decode_block(kind, context, sc, bits, &mut cursor.at(i), (block, &mut 0))
                            .map_err(|what| fail(what, bits))?;
                    }
                }
            }
        } else {
            let sc = &context.components[0];
            let (block, mask) = blocks.component_block(sc.index, my, mx);
            let mut unused = 0;
            let target = (block, mask.unwrap_or(&mut unused));
            decode_block(kind, context, sc, bits, &mut cursor.at(0), target)
                .map_err(|what| fail(what, bits))?;
        }
        if bits.overrun() {
            return Err(fail("entropy-coded data ends inside an MCU", bits));
        }
    }
    Ok(())
}

/// The hot path of [`decode_mcus`]: baseline MCUs of an interleaved scan
/// in the MCU layout (`data` holds exactly `span`'s), where each MCU's
/// blocks follow in scan order (an interleaved scan names its components
/// in frame order).
fn sequential_mcus(
    context: &ScanContext,
    bits: &mut Bits,
    cursor: &mut Cursor,
    data: &mut [i16],
    span: Span,
) -> Result<(), Error> {
    let fail = |what, bits: &Bits| Error::corrupt(what, bits.pos);
    let mut tables = [(None, None, 0usize); 4];
    for (slot, sc) in tables.iter_mut().zip(&context.components) {
        *slot = (sc.dc, sc.ac, sc.h * sc.v);
    }
    let tables = &tables[..context.components.len()];
    let mcu_len = data.len() / span.mcus.len().max(1);
    // The reader works on a local copy, kept in registers.
    let mut local = bits.clone();
    let result = (|| {
        let bits = &mut local;
        for (m, mcu) in span.mcus.clone().zip(data.chunks_exact_mut(mcu_len)) {
            if span.restarts_before(context, m) {
                bits.restart(cursor.expected)
                    .map_err(|what| fail(what, bits))?;
                *cursor = Cursor::at_interval(m / context.restart);
            }
            let mut blocks = mcu.as_chunks_mut::<64>().0.iter_mut();
            for (i, &(dc, ac, count)) in tables.iter().enumerate() {
                let (dc, ac) = (dc.expect("checked"), ac.expect("checked"));
                for block in blocks.by_ref().take(count) {
                    sequential(bits, dc, ac, &mut cursor.predictors[i], block)
                        .map_err(|what| fail(what, bits))?;
                }
            }
            if bits.overrun() {
                return Err(fail("entropy-coded data ends inside an MCU", bits));
            }
        }
        Ok(())
    })();
    *bits = local;
    result
}

/// One scan component's share of a [`Cursor`].
pub(crate) struct ComponentCursor<'c> {
    predictor: &'c mut i32,
    eobrun: &'c mut u32,
}

impl Cursor {
    fn at(&mut self, i: usize) -> ComponentCursor<'_> {
        ComponentCursor {
            predictor: &mut self.predictors[i],
            eobrun: &mut self.eobrun,
        }
    }
}

/// Decodes one block of a scan of `kind` into `block`, keeping its nonzero
/// `mask` (progressive AC scans).
#[inline(always)]
fn decode_block(
    kind: Kind,
    context: &ScanContext,
    sc: &ScanComponent,
    bits: &mut Bits,
    cursor: &mut ComponentCursor,
    (block, mask): (&mut [i16], &mut u64),
) -> Result<(), &'static str> {
    let block: &mut [i16; 64] = block.try_into().expect("64 coefficients");
    match kind {
        Kind::Sequential => sequential(
            bits,
            sc.dc.expect("checked"),
            sc.ac.expect("checked"),
            cursor.predictor,
            block,
        ),
        Kind::DcFirst => {
            let diff = dc_difference(bits, sc.dc.expect("checked"))?;
            *cursor.predictor = cursor.predictor.wrapping_add(diff);
            block[0] = (*cursor.predictor << context.al) as i16;
            Ok(())
        }
        Kind::DcRefine => {
            if bits.bit() {
                block[0] |= 1 << context.al;
            }
            Ok(())
        }
        Kind::AcFirst => ac_first(
            bits,
            sc.ac.expect("checked"),
            context,
            cursor.eobrun,
            block,
            mask,
        ),
        Kind::AcRefine => ac_refine(
            bits,
            sc.ac.expect("checked"),
            context,
            cursor.eobrun,
            block,
            mask,
        ),
    }
}

/// A DC difference: its size category, then that many magnitude bits.
#[inline(always)]
fn dc_difference(bits: &mut Bits, dc: &Table) -> Result<i32, &'static str> {
    bits.ensure();
    let fast = dc.fast(bits);
    if fast != 0 {
        bits.consume((fast & 0xFF) as u32);
        return Ok(fast >> 16);
    }
    let size = dc.symbol(bits).ok_or("invalid Huffman code")?;
    if size > 11 {
        return Err("DC difference too large");
    }
    Ok(bits.receive_extend(u32::from(size)))
}

/// One baseline block: its DC difference and run-length coded AC
/// coefficients, stored in natural order. `block` is zero on entry.
#[inline(always)]
fn sequential(
    bits: &mut Bits,
    dc: &Table,
    ac: &Table,
    predictor: &mut i32,
    block: &mut [i16; 64],
) -> Result<(), &'static str> {
    *predictor = predictor.wrapping_add(dc_difference(bits, dc)?);
    block[0] = *predictor as i16;
    let mut k = 1usize;
    while k < 64 {
        bits.ensure();
        let fast = ac.fast(bits);
        if fast != 0 {
            bits.consume((fast & 0xFF) as u32);
            let run = ((fast >> 8) & 0xFF) as usize;
            if run == END_OF_BLOCK as usize {
                break;
            }
            k += run;
            if k > 63 {
                // A zero run (the only zero-valued entry) may overshoot
                // the block's end, as libjpeg allows.
                if fast >> 16 == 0 {
                    break;
                }
                return Err("AC coefficients run past the block");
            }
            block[ZIGZAG[k] & 63] = (fast >> 16) as i16;
            k += 1;
            continue;
        }
        let symbol = ac.symbol(bits).ok_or("invalid Huffman code")?;
        let (run, size) = (usize::from(symbol >> 4), u32::from(symbol & 15));
        if size == 0 {
            if run != 15 {
                break;
            }
            k += 16;
            continue;
        }
        if size > 10 {
            return Err("AC coefficient too large");
        }
        k += run;
        if k > 63 {
            return Err("AC coefficients run past the block");
        }
        block[ZIGZAG[k] & 63] = bits.receive_extend(size) as i16;
        k += 1;
    }
    Ok(())
}

/// The first scan of a spectral band: coefficients `ss..=se` scaled by
/// `2^al`, with end-of-band runs across blocks.
fn ac_first(
    bits: &mut Bits,
    ac: &Table,
    context: &ScanContext,
    eobrun: &mut u32,
    block: &mut [i16; 64],
    mask: &mut u64,
) -> Result<(), &'static str> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = context.ss;
    while k <= context.se {
        bits.ensure();
        let fast = ac.fast(bits);
        if fast != 0 {
            bits.consume((fast & 0xFF) as u32);
            let run = ((fast >> 8) & 0xFF) as usize;
            if run == END_OF_BLOCK as usize {
                break;
            }
            k += run;
            if k > context.se {
                if fast >> 16 == 0 {
                    break;
                }
                return Err("AC coefficients run past the spectral band");
            }
            let value = fast >> 16;
            block[ZIGZAG[k] & 63] = (value << context.al) as i16;
            *mask |= u64::from(value != 0) << k;
            k += 1;
            continue;
        }
        let symbol = ac.symbol(bits).ok_or("invalid Huffman code")?;
        let (run, size) = (u32::from(symbol >> 4), u32::from(symbol & 15));
        if size == 0 {
            if run < 15 {
                *eobrun = (1 << run) - 1 + bits.take(run);
                break;
            }
            k += 16;
            continue;
        }
        k += run as usize;
        if k > context.se {
            return Err("AC coefficients run past the spectral band");
        }
        block[ZIGZAG[k]] = (bits.receive_extend(size) << context.al) as i16;
        *mask |= 1 << k;
        k += 1;
    }
    Ok(())
}

/// A refinement scan of a spectral band: one more bit of every
/// coefficient already nonzero, and newly nonzero ones of magnitude
/// `2^al` placed among the zeros (T.81 G.1.2.3). The block's nonzero mask
/// finds the coefficients to refine and the zeros to skip without visiting
/// every position.
fn ac_refine(
    bits: &mut Bits,
    ac: &Table,
    context: &ScanContext,
    eobrun: &mut u32,
    block: &mut [i16; 64],
    mask: &mut u64,
) -> Result<(), &'static str> {
    let p1 = 1i16 << context.al;
    let m1 = -1i16 << context.al;
    let se = context.se;
    // Positions k..=se as bits.
    let band = |k: usize| -> u64 {
        if k > se {
            0
        } else {
            (u64::MAX >> (63 - se)) & (u64::MAX << k)
        }
    };
    let mut k = context.ss;
    if *eobrun == 0 {
        while k <= se {
            bits.ensure();
            let symbol = ac.symbol(bits).ok_or("invalid Huffman code")?;
            let (run, size) = (u32::from(symbol >> 4), symbol & 15);
            let mut value = 0i16;
            if size != 0 {
                if size != 1 {
                    return Err("refinement coefficient larger than one bit");
                }
                value = if bits.bit() { p1 } else { m1 };
            } else if run != 15 {
                *eobrun = (1 << run) + bits.take(run);
                break;
            }
            // The zero the new value goes to: past `run` zeros from k
            // (se + 1 when the band has fewer).
            let mut zeros = !*mask & band(k);
            for _ in 0..run {
                zeros &= zeros.wrapping_sub(1);
            }
            let target = if zeros == 0 {
                se + 1
            } else {
                zeros.trailing_zeros() as usize
            };
            // Refine the nonzero coefficients before it, in order.
            refine(bits, block, *mask & band(k) & !band(target), p1, m1);
            if value != 0 {
                if target > se {
                    return Err("refinement coefficient past the spectral band");
                }
                block[ZIGZAG[target] & 63] = value;
                *mask |= 1 << target;
            }
            k = target + 1;
        }
    }
    if *eobrun > 0 {
        refine(bits, block, *mask & band(k), p1, m1);
        *eobrun -= 1;
    }
    Ok(())
}

/// One correction bit for each coefficient in `positions` (zigzag bits),
/// lowest first: a set bit adds `2^al` away from zero unless that bit is
/// already set.
#[inline(always)]
fn refine(bits: &mut Bits, block: &mut [i16; 64], mut positions: u64, p1: i16, m1: i16) {
    while positions != 0 {
        let k = positions.trailing_zeros() as usize;
        positions &= positions - 1;
        let coefficient = &mut block[ZIGZAG[k] & 63];
        if bits.bit() && *coefficient & p1 == 0 {
            *coefficient += if *coefficient >= 0 { p1 } else { m1 };
        }
    }
}
