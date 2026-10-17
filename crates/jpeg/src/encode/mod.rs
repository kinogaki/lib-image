//! Baseline encoding.
//!
//! MCU rows are independent: a worker converts, downsamples, transforms
//! and Huffman-codes a row into its own bit stream, starting from the DC
//! predictors of the row before (it transforms that row's last MCU again
//! rather than wait for it), and the calling thread joins the streams in
//! order with byte stuffing as they arrive, workers a few rows ahead.
//! Optimized tables need every row's statistics first, so that route
//! transforms every row, counts, then codes. Thread count never changes
//! the bytes.
mod entropy;
mod transform;

use crate::Error;
use crate::headers::ZIGZAG;
use crate::render::thread_count;
use entropy::{BitWriter, Codes, Joiner, Spec};
use kinogaki_image::{Image, Samples};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Chroma resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Subsampling {
    /// Full-resolution chroma.
    S444,
    /// Half across.
    S422,
    /// Half across and down.
    #[default]
    S420,
}

/// How an encode runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeOptions {
    /// 1 through 100: the Annex K tables scaled as IJG scales them.
    pub quality: u8,
    /// Chroma resolution (ignored for grey).
    pub subsampling: Subsampling,
    /// A restart marker after every MCU row, so decoders can split the
    /// entropy decoding across threads.
    pub restart: bool,
    /// Huffman tables fitted to the picture (a few percent smaller; every
    /// row's coefficients are held until the end).
    pub optimize: bool,
    /// Threads, counting the caller's; 0 uses every processor.
    pub threads: usize,
    /// An ICC profile to embed (APP2 "ICC_PROFILE" segments).
    pub icc_profile: Option<Vec<u8>>,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            quality: 90,
            subsampling: Subsampling::S420,
            restart: false,
            optimize: false,
            threads: 0,
            icc_profile: None,
        }
    }
}

const BASE_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const BASE_CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// One component of the encoded frame.
pub(crate) struct Component {
    pub h: usize,
    pub v: usize,
}

/// The frame being encoded.
pub(crate) struct Frame {
    pub width: usize,
    pub height: usize,
    pub components: Vec<Component>,
    /// MCU size in luma samples, MCUs across and down.
    pub mcu_width: usize,
    pub mcu_height: usize,
    pub mcux: usize,
    pub mcuy: usize,
    pub blocks_per_mcu: usize,
    /// Quantization tables (natural order), luma then chroma.
    pub quant: [[u16; 64]; 2],
    /// 1/Q of each table, transposed ([u][v]) as the DCT leaves it.
    pub reciprocals: [[f32; 64]; 2],
}

impl Frame {
    fn new(
        width: usize,
        height: usize,
        grey: bool,
        subsampling: Subsampling,
        quality: u8,
    ) -> Frame {
        let (h, v) = match (grey, subsampling) {
            (true, _) | (false, Subsampling::S444) => (1, 1),
            (false, Subsampling::S422) => (2, 1),
            (false, Subsampling::S420) => (2, 2),
        };
        let mut components = vec![Component { h, v }];
        if !grey {
            components.push(Component { h: 1, v: 1 });
            components.push(Component { h: 1, v: 1 });
        }
        let quality = u32::from(quality);
        let scale = if quality < 50 {
            5000 / quality
        } else {
            200 - quality * 2
        };
        let table = |base: &[u16; 64]| -> [u16; 64] {
            std::array::from_fn(|i| ((u32::from(base[i]) * scale + 50) / 100).clamp(1, 255) as u16)
        };
        let quant = [table(&BASE_LUMA), table(&BASE_CHROMA)];
        let reciprocal = |q: &[u16; 64]| -> [f32; 64] {
            std::array::from_fn(|t| 1.0 / f32::from(q[(t % 8) * 8 + t / 8]))
        };
        let reciprocals = [reciprocal(&quant[0]), reciprocal(&quant[1])];
        let (mcu_width, mcu_height) = (8 * h, 8 * v);
        Frame {
            width,
            height,
            blocks_per_mcu: components.iter().map(|c| c.h * c.v).sum(),
            components,
            mcu_width,
            mcu_height,
            mcux: width.div_ceil(mcu_width),
            mcuy: height.div_ceil(mcu_height),
            quant,
            reciprocals,
        }
    }

    /// Each component's DC after the last MCU of `row`'s coefficients
    /// (or of those of that MCU alone).
    fn last_dc(&self, row: &[i16]) -> [i32; 3] {
        let mut dc = [0; 3];
        let last = &row[row.len() - self.blocks_per_mcu * 64..];
        let mut block = 0;
        for (c, component) in self.components.iter().enumerate() {
            block += component.h * component.v;
            dc[c] = i32::from(last[(block - 1) * 64]);
        }
        dc
    }
}

/// Encodes `image` (8-bit grey, grey and alpha, RGB or RGBA; alpha is
/// dropped) as a baseline JPEG.
pub fn encode(image: &Image, options: &EncodeOptions) -> Result<Vec<u8>, Error> {
    let (width, height) = (image.width as usize, image.height as usize);
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 {
        return Err(Error::Invalid("dimensions must be 1 through 65535"));
    }
    if !(1..=100).contains(&options.quality) {
        return Err(Error::Invalid("quality must be 1 through 100"));
    }
    let channels = usize::from(image.channels);
    let Samples::U8(samples) = &image.samples else {
        return Err(Error::Invalid("JPEG encodes 8-bit samples"));
    };
    if !(1..=4).contains(&channels) || samples.len() != width * height * channels {
        return Err(Error::Invalid("samples do not match the size and channels"));
    }
    let frame = Frame::new(
        width,
        height,
        channels <= 2,
        options.subsampling,
        options.quality,
    );
    let threads = thread_count(options.threads, width * height);
    let tables: Vec<(Spec, Spec)> = if options.optimize {
        Vec::new()
    } else {
        let mut tables = vec![(
            Spec::standard(&entropy::DC_LUMA_COUNTS, &entropy::DC_VALUES),
            Spec::standard(&entropy::AC_LUMA_COUNTS, &entropy::AC_LUMA_VALUES),
        )];
        if frame.components.len() > 1 {
            tables.push((
                Spec::standard(&entropy::DC_CHROMA_COUNTS, &entropy::DC_VALUES),
                Spec::standard(&entropy::AC_CHROMA_COUNTS, &entropy::AC_CHROMA_VALUES),
            ));
        }
        tables
    };
    let transform =
        |row: usize| transform::transform_mcus(&frame, samples, channels, row, 0..frame.mcux);
    let mut out = Vec::with_capacity(width * height / 4 + 1024);
    if options.optimize {
        let rows = map(threads, frame.mcuy, transform);
        let tables = optimal_tables(&frame, &rows, options.restart, threads);
        write_header(&mut out, &frame, &tables, options)?;
        let codes: Vec<(Codes, Codes)> = tables
            .iter()
            .map(|(dc, ac)| (Codes::of(dc), Codes::of(ac)))
            .collect();
        let streams = map(threads, frame.mcuy, |r| {
            let predictors = if options.restart || r == 0 {
                [0; 3]
            } else {
                frame.last_dc(&rows[r - 1])
            };
            encode_row(&frame, &rows[r], predictors, &codes, options.restart)
        });
        join(&mut out, streams, options.restart);
    } else {
        write_header(&mut out, &frame, &tables, options)?;
        let codes: Vec<(Codes, Codes)> = tables
            .iter()
            .map(|(dc, ac)| (Codes::of(dc), Codes::of(ac)))
            .collect();
        let row_stream = |r: usize| {
            let coefficients = transform(r);
            // The predictors this row starts from: the DCs of the row
            // before's last MCU, transformed again here so rows never wait
            // for each other.
            let predictors = if options.restart || r == 0 {
                [0; 3]
            } else {
                frame.last_dc(&transform::transform_mcus(
                    &frame,
                    samples,
                    channels,
                    r - 1,
                    frame.mcux - 1..frame.mcux,
                ))
            };
            encode_row(&frame, &coefficients, predictors, &codes, options.restart)
        };
        let mut joiner = Joiner::resume(&mut out, 0, 0);
        ordered(threads, frame.mcuy, threads * 4, row_stream, |r, stream| {
            joiner.append(&stream.bytes, stream.tail, stream.tail_count);
            if options.restart && r + 1 < frame.mcuy {
                joiner.marker(0xD0 + (r % 8) as u8);
            }
        });
        joiner.pad();
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

/// One MCU row's bit stream (padded to a byte when it ends at a restart
/// marker).
struct Stream {
    bytes: Vec<u8>,
    tail: u8,
    tail_count: u32,
}

fn encode_row(
    frame: &Frame,
    row: &[i16],
    mut predictors: [i32; 3],
    codes: &[(Codes, Codes)],
    restart: bool,
) -> Stream {
    let mut out = BitWriter::with_capacity(row.len() / 4);
    for mcu in row.chunks_exact(frame.blocks_per_mcu * 64) {
        let mut blocks = mcu.as_chunks::<64>().0.iter();
        for (c, component) in frame.components.iter().enumerate() {
            let (dc, ac) = &codes[usize::from(c > 0)];
            for block in blocks.by_ref().take(component.h * component.v) {
                entropy::encode_block(&mut out, block, &mut predictors[c], dc, ac);
            }
        }
    }
    if restart {
        out.pad();
    }
    let (bytes, tail, tail_count) = out.finish();
    Stream {
        bytes,
        tail,
        tail_count,
    }
}

/// Tables fitted to every row's statistics.
fn optimal_tables(
    frame: &Frame,
    rows: &[Vec<i16>],
    restart: bool,
    threads: usize,
) -> Vec<(Spec, Spec)> {
    let tables = if frame.components.len() > 1 { 2 } else { 1 };
    let counts = map(threads, rows.len(), |r| {
        let mut predictors = if restart || r == 0 {
            [0; 3]
        } else {
            frame.last_dc(&rows[r - 1])
        };
        let mut freq = vec![([0u32; 256], [0u32; 256]); tables];
        for mcu in rows[r].chunks_exact(frame.blocks_per_mcu * 64) {
            let mut blocks = mcu.as_chunks::<64>().0.iter();
            for (c, component) in frame.components.iter().enumerate() {
                let (dc, ac) = &mut freq[usize::from(c > 0)];
                for block in blocks.by_ref().take(component.h * component.v) {
                    entropy::count_block(block, &mut predictors[c], dc, ac);
                }
            }
        }
        freq
    });
    let mut total = vec![([0u32; 256], [0u32; 256]); tables];
    for freq in counts {
        for (t, (dc, ac)) in freq.iter().enumerate() {
            for i in 0..256 {
                total[t].0[i] += dc[i];
                total[t].1[i] += ac[i];
            }
        }
    }
    total
        .iter()
        .map(|(dc, ac)| (Spec::optimal(dc), Spec::optimal(ac)))
        .collect()
}

/// Appends every row's stream (with restart markers between rows when
/// they restart) and pads the data.
fn join(out: &mut Vec<u8>, streams: Vec<Stream>, restart: bool) {
    let rows = streams.len();
    let mut joiner = Joiner::resume(out, 0, 0);
    for (row, stream) in streams.into_iter().enumerate() {
        joiner.append(&stream.bytes, stream.tail, stream.tail_count);
        if restart && row + 1 < rows {
            joiner.marker(0xD0 + (row % 8) as u8);
        }
    }
    joiner.pad();
}

/// The marker segments from SOI through SOS.
fn write_header(
    out: &mut Vec<u8>,
    frame: &Frame,
    tables: &[(Spec, Spec)],
    options: &EncodeOptions,
) -> Result<(), Error> {
    out.extend_from_slice(&[0xFF, 0xD8]);
    segment(out, 0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    if let Some(profile) = &options.icc_profile {
        const PART: usize = 65_519;
        let parts = profile.len().div_ceil(PART);
        if profile.is_empty() || parts > 255 {
            return Err(Error::Invalid("ICC profile must be 1 byte to 16 MB"));
        }
        for (i, part) in profile.chunks(PART).enumerate() {
            let mut payload = b"ICC_PROFILE\0".to_vec();
            payload.extend_from_slice(&[i as u8 + 1, parts as u8]);
            payload.extend_from_slice(part);
            segment(out, 0xE2, &payload);
        }
    }
    let count = frame.components.len();
    let mut dqt = Vec::new();
    for (t, table) in frame
        .quant
        .iter()
        .enumerate()
        .take(if count > 1 { 2 } else { 1 })
    {
        dqt.push(t as u8);
        dqt.extend(ZIGZAG.iter().map(|&natural| table[natural] as u8));
    }
    segment(out, 0xDB, &dqt);
    let mut sof = vec![
        8,
        (frame.height >> 8) as u8,
        frame.height as u8,
        (frame.width >> 8) as u8,
        frame.width as u8,
        count as u8,
    ];
    for (c, component) in frame.components.iter().enumerate() {
        sof.extend_from_slice(&[
            c as u8 + 1,
            (component.h << 4 | component.v) as u8,
            u8::from(c > 0),
        ]);
    }
    segment(out, 0xC0, &sof);
    let mut dht = Vec::new();
    for (t, (dc, ac)) in tables.iter().enumerate() {
        for (class, spec) in [(0u8, dc), (1u8, ac)] {
            dht.push(class << 4 | t as u8);
            dht.extend_from_slice(&spec.counts);
            dht.extend_from_slice(&spec.values);
        }
    }
    segment(out, 0xC4, &dht);
    if options.restart {
        segment(out, 0xDD, &(frame.mcux as u16).to_be_bytes());
    }
    let mut sos = vec![count as u8];
    for c in 0..count {
        sos.extend_from_slice(&[c as u8 + 1, if c > 0 { 0x11 } else { 0x00 }]);
    }
    sos.extend_from_slice(&[0, 63, 0]);
    segment(out, 0xDA, &sos);
    Ok(())
}

fn segment(out: &mut Vec<u8>, code: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xFF, code]);
    out.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(payload);
}

/// `make(i)` for `i` in `0..count` on up to `threads` threads, each
/// result handed to `take` on the calling thread in order as soon as it
/// and those before it are made; at most `window` are made ahead.
fn ordered<T: Send>(
    threads: usize,
    count: usize,
    window: usize,
    make: impl Fn(usize) -> T + Sync,
    mut take: impl FnMut(usize, T),
) {
    if threads <= 1 {
        for i in 0..count {
            take(i, make(i));
        }
        return;
    }
    struct Queue<T> {
        next: usize,
        taken: usize,
        made: Vec<Option<T>>,
    }
    let queue = Mutex::new(Queue {
        next: 0,
        taken: 0,
        made: (0..count).map(|_| None).collect(),
    });
    let changed = std::sync::Condvar::new();
    let work = || {
        loop {
            let i = {
                let mut q = queue.lock().expect("queue");
                while q.next < count && q.next >= q.taken + window {
                    q = changed.wait(q).expect("queue");
                }
                if q.next >= count {
                    return;
                }
                q.next += 1;
                q.next - 1
            };
            let value = make(i);
            queue.lock().expect("queue").made[i] = Some(value);
            changed.notify_all();
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(work);
        }
        for i in 0..count {
            let value = {
                let mut q = queue.lock().expect("queue");
                loop {
                    if let Some(value) = q.made[i].take() {
                        q.taken = i + 1;
                        break value;
                    }
                    q = changed.wait(q).expect("queue");
                }
            };
            changed.notify_all();
            take(i, value);
        }
    });
}

/// `f(i)` for `i` in `0..count`, on up to `threads` threads, in order.
fn map<T: Send>(threads: usize, count: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    if threads <= 1 || count <= 1 {
        return (0..count).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<T>>> = (0..count).map(|_| Mutex::new(None)).collect();
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= count {
                return;
            }
            let value = f(i);
            *results[i].lock().expect("result") = Some(value);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads.min(count) {
            scope.spawn(work);
        }
        work();
    });
    results
        .into_iter()
        .map(|r| r.into_inner().expect("result").expect("every index"))
        .collect()
}
