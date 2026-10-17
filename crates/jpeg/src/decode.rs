//! The decoder: the walk over marker segments, each scan's entropy
//! decoding, and the render.
//!
//! Three routes, all giving the same pixels:
//! - A baseline frame in one scan with restart intervals, on several
//!   threads: the intervals entropy-decode in parallel (each owns its run
//!   of MCUs in the MCU layout), then the bands render in parallel.
//! - Any other baseline frame in one scan: the calling thread
//!   entropy-decodes MCU rows while the other threads render the rows it
//!   has finished, so the rendering hides behind the entropy decoding.
//! - Progressive frames and baseline frames of several scans: every scan
//!   decodes into whole planes of coefficients, then the bands render in
//!   parallel.
use crate::coefficients::{BandView, Blocks, McuShape, Store};
use crate::color::Model;
use crate::headers::{Frame, Quantization, Reader, Scan, marker, parse_huffman};
use crate::huffman::{Bits, Table, next_marker};
use crate::render::{self, Plan, Render};
use crate::scan::{Cursor, Kind, ScanContext, Span, decode_mcus};
use crate::{DecodeOptions, Error};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A decoded picture: 8-bit samples, `step` a pixel.
pub(crate) struct Decoded {
    pub width: usize,
    pub height: usize,
    pub step: usize,
    pub pixels: Vec<u8>,
}

/// Everything read from the marker segments so far.
struct State {
    frame: Option<Frame>,
    quant: Quantization,
    tables: [Option<Box<Table>>; 8],
    restart: usize,
    adobe: Option<u8>,
    /// Each component's coefficient coding so far (progressive order
    /// checks; a baseline component's single scan).
    progress: Vec<[i8; 64]>,
    store: Option<Store>,
    /// A one-scan baseline picture renders during its scan.
    rendered: Option<Vec<u8>>,
}

/// Decodes `data` at `options.scale` into `step`-byte pixels (`step` 0 is
/// the picture's own: 1 grey, 3 color).
pub(crate) fn decode(data: &[u8], options: &DecodeOptions, rgba: bool) -> Result<Decoded, Error> {
    let scale = usize::from(options.scale);
    if !matches!(scale, 1 | 2 | 4 | 8) {
        return Err(Error::Invalid("scale must be 1, 2, 4 or 8"));
    }
    if data.get(..2) != Some(&[0xFF, marker::SOI]) {
        return Err(Error::corrupt("not a JPEG file", 0));
    }
    let mut s = State {
        frame: None,
        quant: Quantization::default(),
        tables: Default::default(),
        restart: 0,
        adobe: None,
        progress: Vec::new(),
        store: None,
        rendered: None,
    };
    let mut r = Reader::new(data, 2);
    let step_of = |frame: &Frame| {
        if rgba {
            4
        } else if frame.components.len() == 1 {
            1
        } else {
            3
        }
    };
    loop {
        if r.pos >= data.len() {
            return Err(Error::corrupt("end of image marker missing", r.pos));
        }
        let at = r.pos;
        let code = r.marker()?;
        match code {
            marker::EOI => break,
            marker::RST0..=marker::RST7 => continue,
            marker::SOF0 | marker::SOF1 | marker::SOF2 => {
                let (payload, start) = r.segment()?;
                if s.frame.is_some() {
                    return Err(Error::corrupt("second frame header", at));
                }
                let frame = Frame::parse(payload, start, code)?;
                if frame.width as u64 * frame.height as u64 > options.max_pixels {
                    return Err(Error::TooLarge);
                }
                s.progress = vec![[-1; 64]; frame.components.len()];
                s.frame = Some(frame);
            }
            code if marker::unsupported_frame(code) => {
                return Err(Error::Unsupported(
                    "lossless, hierarchical or arithmetic coding",
                ));
            }
            marker::DQT => {
                let (payload, start) = r.segment()?;
                s.quant.parse(payload, start)?;
            }
            marker::DHT => {
                let (payload, start) = r.segment()?;
                parse_huffman(payload, start, &mut s.tables)?;
            }
            marker::DRI => {
                let (payload, start) = r.segment()?;
                if payload.len() != 2 {
                    return Err(Error::corrupt(
                        "restart interval segment is not two bytes",
                        start,
                    ));
                }
                s.restart = usize::from(u16::from_be_bytes([payload[0], payload[1]]));
            }
            marker::APP14 => {
                let (payload, _) = r.segment()?;
                if payload.len() >= 12 && payload.starts_with(b"Adobe") {
                    s.adobe = Some(payload[11]);
                }
            }
            marker::SOS => {
                let (payload, start) = r.segment()?;
                let frame = s
                    .frame
                    .as_ref()
                    .ok_or(Error::corrupt("scan before the frame header", at))?;
                let scan = Scan::parse(payload, start, frame, &mut s.progress)?;
                let context = ScanContext::new(frame, &scan, &s.tables, s.restart, start)?;
                let entropy = r.pos;
                let whole = scan.components.len() == frame.components.len();
                r.pos = if !frame.progressive && whole && s.store.is_none() && s.rendered.is_none()
                {
                    check_quant(frame, &s.quant, at)?;
                    let plan = Plan::new(
                        frame,
                        &s.quant,
                        Model::of(&ids(frame), s.adobe),
                        scale,
                        step_of(frame),
                    );
                    let threads = render::thread_count(options.threads, frame.width * frame.height);
                    let (pixels, end) = one_scan(frame, &context, data, entropy, &plan, threads)?;
                    s.rendered = Some(pixels);
                    end
                } else {
                    if s.rendered.is_some() {
                        return Err(Error::corrupt("scan after the frame's only scan", at));
                    }
                    let store = s.store.get_or_insert_with(|| Store::planar(frame));
                    whole_scan(frame, &context, data, entropy, store)?
                };
            }
            _ => {
                r.segment()?;
            }
        }
    }
    let frame = s
        .frame
        .ok_or(Error::corrupt("no frame header", data.len()))?;
    let step = step_of(&frame);
    let (width, height) = (frame.width.div_ceil(scale), frame.height.div_ceil(scale));
    if let Some(pixels) = s.rendered {
        return Ok(Decoded {
            width,
            height,
            step,
            pixels,
        });
    }
    let store = s.store.ok_or(Error::corrupt("no scan", data.len()))?;
    check_quant(&frame, &s.quant, data.len())?;
    if s.progress.iter().any(|p| p[0] == -1) {
        return Err(Error::corrupt("a component has no DC scan", data.len()));
    }
    let plan = Plan::new(
        &frame,
        &s.quant,
        Model::of(&ids(&frame), s.adobe),
        scale,
        step,
    );
    let threads = render::thread_count(options.threads, frame.width * frame.height);
    let bands = (0..frame.mcuy)
        .map(|band| store.band(&frame, band))
        .collect();
    let pixels = render::render_all(&plan, bands, threads);
    Ok(Decoded {
        width,
        height,
        step,
        pixels,
    })
}

fn ids(frame: &Frame) -> Vec<u8> {
    frame.components.iter().map(|c| c.id).collect()
}

fn check_quant(frame: &Frame, quant: &Quantization, at: usize) -> Result<(), Error> {
    if frame.components.iter().any(|c| !quant.present[c.quant]) {
        return Err(Error::corrupt(
            "a component's quantization table is missing",
            at,
        ));
    }
    Ok(())
}

/// Decodes a scan into whole planes of coefficients; where the scan's
/// data ends.
fn whole_scan(
    frame: &Frame,
    context: &ScanContext,
    data: &[u8],
    entropy: usize,
    store: &mut Store,
) -> Result<usize, Error> {
    let Store::Planar { planes, masks } = store else {
        unreachable!("several scans use the planar layout");
    };
    let mut blocks = Blocks::Planar {
        planes: planes.iter_mut().map(|p| p.as_mut_slice()).collect(),
        masks: masks.iter_mut().map(|m| m.as_mut_slice()).collect(),
        bx: std::array::from_fn(|i| frame.components.get(i).map_or(0, |c| c.bx)),
        first_rows: [0; 4],
    };
    let mut bits = Bits::new(data, entropy);
    let mut cursor = Cursor::at_interval(0);
    let kind = Kind::of(frame.progressive, context);
    decode_mcus(
        kind,
        context,
        &mut bits,
        &mut cursor,
        &mut blocks,
        Span {
            mcus: 0..context.mcus(),
            started: true,
        },
    )?;
    Ok(next_marker(data, bits.pos, true))
}

/// Decodes and renders a baseline frame coded in one scan; the pixels and
/// where the scan's data ends.
fn one_scan(
    frame: &Frame,
    context: &ScanContext,
    data: &[u8],
    entropy: usize,
    plan: &Plan,
    threads: usize,
) -> Result<(Vec<u8>, usize), Error> {
    let interleaved = context.interleaved();
    let shape = McuShape::of(frame);
    let mut coefficients = match interleaved {
        true => Store::mcu(frame),
        false => Store::planar(frame),
    };
    if interleaved && context.restart > 0 && threads > 1 {
        let Store::Mcu(all) = &mut coefficients else {
            unreachable!()
        };
        if let Some(end) = parallel_intervals(context, data, entropy, all, shape, threads)? {
            let bands = (0..frame.mcuy)
                .map(|band| coefficients.band(frame, band))
                .collect();
            return Ok((render::render_all(plan, bands, threads), end));
        }
    }
    let mut out = vec![0u8; plan.height * plan.row_bytes()];
    let end = {
        let slots = render::band_slots(plan, &mut out);
        let render = Render::new(plan);
        let chunks: Vec<&mut [i16]> = match &mut coefficients {
            Store::Mcu(all) => all.chunks_mut(frame.mcux * shape.len()).collect(),
            Store::Planar { planes, .. } => {
                let c = &frame.components[0];
                planes[0].chunks_mut(c.bx * c.v * 64).collect()
            }
        };
        render::run_workers(&render, &slots, threads, |render| {
            let result = pipelined(frame, context, data, entropy, chunks, shape, render);
            if result.is_err() {
                render.stop();
            }
            result
        })?
    };
    Ok((out, end))
}

/// The calling thread's part of a pipelined render: entropy-decodes each
/// MCU row into its chunk and hands it to the render.
fn pipelined<'c>(
    frame: &Frame,
    context: &ScanContext,
    data: &[u8],
    entropy: usize,
    chunks: Vec<&'c mut [i16]>,
    shape: McuShape,
    render: &Render<'_, 'c>,
) -> Result<usize, Error> {
    let mut bits = Bits::new(data, entropy);
    let mut cursor = Cursor::at_interval(0);
    for (band, chunk) in chunks.into_iter().enumerate() {
        if context.interleaved() {
            let (first, end) = (band * frame.mcux, (band + 1) * frame.mcux);
            let mut blocks = Blocks::Mcu {
                data: chunk,
                shape,
                first,
            };
            decode_mcus(
                Kind::Sequential,
                context,
                &mut bits,
                &mut cursor,
                &mut blocks,
                Span {
                    mcus: first..end,
                    started: band == 0,
                },
            )?;
            let Blocks::Mcu { data, .. } = blocks else {
                unreachable!()
            };
            render.publish(band, BandView::Mcu(data, shape));
        } else {
            // One component: the band is `v` block rows of the scan's.
            let c = &frame.components[0];
            let rows = (band * c.v).min(c.actual_by)..((band + 1) * c.v).min(c.actual_by);
            let mut blocks = Blocks::Planar {
                planes: vec![chunk],
                masks: Vec::new(),
                bx: [c.bx, 0, 0, 0],
                first_rows: [band * c.v, 0, 0, 0],
            };
            let (first, end) = (rows.start * context.mcux, rows.end * context.mcux);
            decode_mcus(
                Kind::Sequential,
                context,
                &mut bits,
                &mut cursor,
                &mut blocks,
                Span {
                    mcus: first..end,
                    started: band == 0,
                },
            )?;
            let Blocks::Planar { mut planes, .. } = blocks else {
                unreachable!()
            };
            let chunk: &'c [i16] = planes.pop().expect("one plane");
            render.publish(band, BandView::Planar(vec![chunk]));
        }
    }
    Ok(next_marker(data, bits.pos, true))
}

/// Entropy-decodes the restart intervals of a one-scan interleaved frame
/// on `threads` threads into `all` (MCU layout); where the scan ends, or
/// `None` when its restart markers are not all there (the sequential
/// route then finds what is wrong).
fn parallel_intervals(
    context: &ScanContext,
    data: &[u8],
    entropy: usize,
    all: &mut [i16],
    shape: McuShape,
    threads: usize,
) -> Result<Option<usize>, Error> {
    let total = context.mcus();
    let interval = context.restart;
    let count = total.div_ceil(interval);
    // Where each interval's data starts: after the previous one's marker.
    let mut starts = Vec::with_capacity(count);
    starts.push(entropy);
    let mut pos = entropy;
    while starts.len() < count {
        let at = next_marker(data, pos, false);
        match data.get(at + 1) {
            Some(code) if (marker::RST0..=marker::RST7).contains(code) => {
                pos = at + 2;
                starts.push(pos);
            }
            _ => return Ok(None),
        }
    }
    let chunks: Vec<Mutex<Option<&mut [i16]>>> = all
        .chunks_mut(interval * shape.len())
        .map(|c| Mutex::new(Some(c)))
        .collect();
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<(usize, Error)>> = Mutex::new(None);
    let end = AtomicUsize::new(0);
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= count {
                return;
            }
            let chunk = chunks[i]
                .lock()
                .expect("interval")
                .take()
                .expect("each interval once");
            let first = i * interval;
            let last = (first + interval).min(total);
            let mut bits = Bits::new(data, starts[i]);
            let mut cursor = Cursor::at_interval(i);
            let mut blocks = Blocks::Mcu {
                data: chunk,
                shape,
                first,
            };
            let mut result = decode_mcus(
                Kind::Sequential,
                context,
                &mut bits,
                &mut cursor,
                &mut blocks,
                Span {
                    mcus: first..last,
                    started: true,
                },
            );
            if result.is_ok() {
                if i + 1 < count {
                    result = bits
                        .restart(cursor.expected)
                        .map_err(|what| Error::corrupt(what, bits.pos));
                } else {
                    end.store(next_marker(data, bits.pos, true), Ordering::Relaxed);
                }
            }
            if let Err(error) = result {
                let mut failed = failure.lock().expect("failure");
                if failed.as_ref().is_none_or(|(j, _)| i < *j) {
                    *failed = Some((i, error));
                }
            }
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(work);
        }
        work();
    });
    if let Some((_, error)) = failure.into_inner().expect("failure") {
        return Err(error);
    }
    Ok(Some(end.into_inner()))
}

/// The frame header of `data`, read without decoding anything else.
pub(crate) fn frame_of(data: &[u8]) -> Result<Frame, Error> {
    if data.get(..2) != Some(&[0xFF, marker::SOI]) {
        return Err(Error::corrupt("not a JPEG file", 0));
    }
    let mut r = Reader::new(data, 2);
    loop {
        let at = r.pos;
        let code = r.marker()?;
        match code {
            marker::SOF0 | marker::SOF1 | marker::SOF2 => {
                let (payload, start) = r.segment()?;
                return Frame::parse(payload, start, code);
            }
            marker::EOI | marker::SOS => {
                return Err(Error::corrupt("no frame header before the scan", at));
            }
            marker::RST0..=marker::RST7 => {}
            code if marker::unsupported_frame(code) => {
                return Err(Error::Unsupported(
                    "lossless, hierarchical or arithmetic coding",
                ));
            }
            _ => {
                r.segment()?;
            }
        }
    }
}
