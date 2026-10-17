//! Marker segments: the byte reader, the frame header, quantization and
//! Huffman tables, restart intervals and scan headers (ITU T.81 Annex B).
use crate::Error;
use crate::huffman::Table;

/// Natural (row-major) position of each zigzag index.
pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Marker codes (the byte after 0xFF).
pub(crate) mod marker {
    pub const SOF0: u8 = 0xC0;
    pub const SOF1: u8 = 0xC1;
    pub const SOF2: u8 = 0xC2;
    pub const DHT: u8 = 0xC4;
    pub const RST0: u8 = 0xD0;
    pub const RST7: u8 = 0xD7;
    pub const SOI: u8 = 0xD8;
    pub const EOI: u8 = 0xD9;
    pub const SOS: u8 = 0xDA;
    pub const DQT: u8 = 0xDB;
    pub const DRI: u8 = 0xDD;
    pub const APP2: u8 = 0xE2;
    pub const APP14: u8 = 0xEE;

    /// Start-of-frame codes other than baseline, extended and progressive
    /// Huffman: lossless, hierarchical and arithmetic coding.
    pub fn unsupported_frame(code: u8) -> bool {
        (0xC3..=0xCF).contains(&code) && code != DHT && code != 0xC8 && code != 0xCC
    }
}

/// Bytes of a file read front to back, big-endian, with the file offset
/// of each failure.
pub(crate) struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or(Error::corrupt("file ends early", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from(self.u8()?) << 8 | u16::from(self.u8()?))
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let bytes = self
            .data
            .get(self.pos..self.pos.saturating_add(n))
            .ok_or(Error::corrupt(
                "segment runs past the end of the file",
                self.pos,
            ))?;
        self.pos += n;
        Ok(bytes)
    }

    /// The next marker's code; fill bytes (0xFF runs) before it are
    /// skipped.
    pub fn marker(&mut self) -> Result<u8, Error> {
        let at = self.pos;
        if self.u8()? != 0xFF {
            return Err(Error::corrupt("marker expected", at));
        }
        let mut code = self.u8()?;
        while code == 0xFF {
            code = self.u8()?;
        }
        if code == 0 {
            return Err(Error::corrupt("stuffed byte outside a scan", at));
        }
        Ok(code)
    }

    /// A marker segment's payload (after its two length bytes), and its
    /// offset in the file.
    pub fn segment(&mut self) -> Result<(&'a [u8], usize), Error> {
        let at = self.pos;
        let length = usize::from(self.u16()?);
        if length < 2 {
            return Err(Error::corrupt("marker segment length below two", at));
        }
        let start = self.pos;
        Ok((self.take(length - 2)?, start))
    }
}

/// One color component of the frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Component {
    pub id: u8,
    /// Sampling factors, 1 through 4.
    pub h: usize,
    pub v: usize,
    /// Quantization table selector, 0 through 3.
    pub quant: usize,
    /// Blocks across and down, padded to whole MCUs.
    pub bx: usize,
    pub by: usize,
    /// Blocks covering the picture (what a one-component scan codes).
    pub actual_bx: usize,
    pub actual_by: usize,
}

/// The frame header: size, coding and components.
#[derive(Clone, Debug, Default)]
pub(crate) struct Frame {
    pub width: usize,
    pub height: usize,
    pub progressive: bool,
    pub components: Vec<Component>,
    pub max_h: usize,
    pub max_v: usize,
    /// MCUs across and down.
    pub mcux: usize,
    pub mcuy: usize,
}

impl Frame {
    /// Parses an SOF0/1/2 payload starting at file offset `at`.
    pub fn parse(payload: &[u8], at: usize, code: u8) -> Result<Frame, Error> {
        let mut r = Reader::new(payload, 0);
        let fail = |what, r: &Reader| Error::corrupt(what, at + r.pos);
        let precision = r.u8().map_err(|_| fail("short frame header", &r))?;
        if precision != 8 {
            return Err(Error::Unsupported("sample precision other than 8 bits"));
        }
        let height = usize::from(r.u16().map_err(|_| fail("short frame header", &r))?);
        let width = usize::from(r.u16().map_err(|_| fail("short frame header", &r))?);
        let count = usize::from(r.u8().map_err(|_| fail("short frame header", &r))?);
        if !matches!(count, 1 | 3 | 4) {
            return Err(Error::Unsupported("component count other than 1, 3 or 4"));
        }
        if width == 0 || height == 0 {
            return Err(Error::Unsupported("zero dimensions (DNL-defined height)"));
        }
        let mut frame = Frame {
            width,
            height,
            progressive: code == marker::SOF2,
            ..Frame::default()
        };
        let mut blocks = 0;
        for _ in 0..count {
            let id = r.u8().map_err(|_| fail("short frame header", &r))?;
            let sampling = r.u8().map_err(|_| fail("short frame header", &r))?;
            let quant = usize::from(r.u8().map_err(|_| fail("short frame header", &r))?);
            let (h, v) = (usize::from(sampling >> 4), usize::from(sampling & 15));
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) || quant > 3 {
                return Err(fail("invalid sampling factors or table selector", &r));
            }
            if frame.components.iter().any(|c| c.id == id) {
                return Err(fail("duplicate component ID", &r));
            }
            frame.max_h = frame.max_h.max(h);
            frame.max_v = frame.max_v.max(v);
            blocks += h * v;
            frame.components.push(Component {
                id,
                h,
                v,
                quant,
                ..Component::default()
            });
        }
        if blocks > 10 && count > 1 {
            return Err(fail("an MCU of more than ten blocks", &r));
        }
        frame.mcux = width.div_ceil(frame.max_h * 8);
        frame.mcuy = height.div_ceil(frame.max_v * 8);
        let (max_h, max_v) = (frame.max_h, frame.max_v);
        for c in &mut frame.components {
            c.bx = frame.mcux * c.h;
            c.by = frame.mcuy * c.v;
            c.actual_bx = (width * c.h).div_ceil(max_h * 8);
            c.actual_by = (height * c.v).div_ceil(max_v * 8);
        }
        Ok(frame)
    }
}

/// Quantization tables in natural order, and which are defined.
#[derive(Clone, Debug)]
pub(crate) struct Quantization {
    pub tables: [[u16; 64]; 4],
    pub present: [bool; 4],
}

impl Default for Quantization {
    fn default() -> Self {
        Self {
            tables: [[0; 64]; 4],
            present: [false; 4],
        }
    }
}

impl Quantization {
    /// Reads a DQT payload at file offset `at`.
    pub fn parse(&mut self, payload: &[u8], at: usize) -> Result<(), Error> {
        let mut r = Reader::new(payload, 0);
        while r.pos < payload.len() {
            let start = r.pos;
            let selector = r.u8()?;
            let (precision, index) = (selector >> 4, usize::from(selector & 15));
            if index > 3 || precision > 1 {
                return Err(Error::corrupt(
                    "invalid quantization table selector",
                    at + start,
                ));
            }
            for &natural in &ZIGZAG {
                let value = if precision == 1 {
                    r.u16()
                } else {
                    r.u8().map(u16::from)
                }
                .map_err(|_| Error::corrupt("short quantization table", at + r.pos))?;
                if value == 0 {
                    return Err(Error::corrupt("zero quantization value", at + r.pos));
                }
                self.tables[index][natural] = value;
            }
            self.present[index] = true;
        }
        Ok(())
    }
}

/// Reads a DHT payload at file offset `at` into `tables` (DC 0..3, then AC
/// 4..7).
pub(crate) fn parse_huffman(
    payload: &[u8],
    at: usize,
    tables: &mut [Option<Box<Table>>; 8],
) -> Result<(), Error> {
    let mut r = Reader::new(payload, 0);
    while r.pos < payload.len() {
        let start = r.pos;
        let selector = r.u8()?;
        if selector >> 4 > 1 || selector & 15 > 3 {
            return Err(Error::corrupt("invalid Huffman table selector", at + start));
        }
        let counts: [u8; 16] = r
            .take(16)
            .map_err(|_| Error::corrupt("short Huffman table", at + start))?
            .try_into()
            .expect("sixteen counts");
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        let symbols = r
            .take(total)
            .map_err(|_| Error::corrupt("short Huffman table", at + start))?;
        let table = Table::build(&counts, symbols, selector >> 4 == 1)
            .ok_or(Error::corrupt("invalid Huffman code lengths", at + start))?;
        tables[usize::from(selector & 15) + if selector >> 4 == 1 { 4 } else { 0 }] =
            Some(Box::new(table));
    }
    Ok(())
}

/// One scan's parameters, from its SOS header.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scan {
    /// Frame component indices, in scan order.
    pub components: Vec<usize>,
    /// DC and AC table per scan component.
    pub dc: Vec<usize>,
    pub ac: Vec<usize>,
    /// Spectral selection and successive approximation.
    pub ss: usize,
    pub se: usize,
    pub ah: u8,
    pub al: u8,
}

impl Scan {
    /// Parses an SOS payload at file offset `at`, checking it against the
    /// frame and, for progressive frames, against each coefficient's coding
    /// history in `progress` (the bit each was last coded to, -1 before
    /// any).
    pub fn parse(
        payload: &[u8],
        at: usize,
        frame: &Frame,
        progress: &mut [[i8; 64]],
    ) -> Result<Scan, Error> {
        let mut r = Reader::new(payload, 0);
        let short = |r: &Reader| Error::corrupt("short scan header", at + r.pos);
        let count = usize::from(r.u8().map_err(|_| short(&r))?);
        if count == 0 || count > frame.components.len() {
            return Err(Error::corrupt("invalid scan component count", at));
        }
        let mut scan = Scan::default();
        for _ in 0..count {
            let id = r.u8().map_err(|_| short(&r))?;
            let tables = r.u8().map_err(|_| short(&r))?;
            if tables >> 4 > 3 || tables & 15 > 3 {
                return Err(Error::corrupt("invalid scan table selector", at + r.pos));
            }
            let index = frame
                .components
                .iter()
                .position(|c| c.id == id)
                .ok_or(Error::corrupt(
                    "scan names an unknown component",
                    at + r.pos,
                ))?;
            if scan.components.contains(&index) {
                return Err(Error::corrupt("scan names a component twice", at + r.pos));
            }
            scan.components.push(index);
            scan.dc.push(usize::from(tables >> 4));
            scan.ac.push(usize::from(tables & 15));
        }
        if count > 1
            && scan
                .components
                .iter()
                .map(|&i| frame.components[i].h * frame.components[i].v)
                .sum::<usize>()
                > 10
        {
            return Err(Error::corrupt("an MCU of more than ten blocks", at));
        }
        scan.ss = usize::from(r.u8().map_err(|_| short(&r))?);
        scan.se = usize::from(r.u8().map_err(|_| short(&r))?);
        let approximation = r.u8().map_err(|_| short(&r))?;
        (scan.ah, scan.al) = (approximation >> 4, approximation & 15);
        let (ss, se, ah, al) = (scan.ss, scan.se, scan.ah, scan.al);
        let valid = if frame.progressive {
            ss <= se
                && se <= 63
                && (ss != 0 || se == 0)
                && (ss == 0 || count == 1)
                && al <= 13
                && (ah == 0 || ah == al + 1)
        } else {
            ss == 0 && se == 63 && ah == 0 && al == 0
        };
        if !valid {
            return Err(Error::corrupt(
                "invalid spectral selection or approximation",
                at,
            ));
        }
        for &c in &scan.components {
            for last in &mut progress[c][ss..=se] {
                if (ah == 0 && *last != -1) || (ah != 0 && *last != ah as i8) {
                    return Err(Error::corrupt("progressive scans out of order", at));
                }
                *last = al as i8;
            }
        }
        Ok(scan)
    }
}
