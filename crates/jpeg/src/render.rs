//! Rendering coefficients into pixels: the plan (each component's IDCT
//! size, plane and sampling ratio against output pixels) and the band
//! schedule that runs it on every processor.
//!
//! Work goes in MCU-row bands. An IDCT task turns a band's coefficients
//! into that band's rows of each component plane; a color task upsamples
//! and converts a band's output rows, reading plane rows of the bands
//! either side too, so it waits for their IDCT. Workers take the lowest
//! ready color task first, else the next IDCT task. Coefficient bands can
//! arrive while workers run: a baseline picture in one scan is rendered
//! behind its entropy decoding (see `decode`).
use crate::coefficients::BandView;
use crate::color::{self, Axis, Model};
use crate::headers::{Frame, Quantization};
use crate::idct::{Scaled, idct8};
use std::sync::{Condvar, Mutex, OnceLock};

/// One component's part of the plan.
pub(crate) struct ComponentPlan {
    pub v: usize,
    pub bx: usize,
    /// IDCT output block size: 8 full size, fewer when DCT-scaled.
    pub size: usize,
    /// Plane samples across (`bx * size`) and rows a band holds.
    pub stride: usize,
    pub band_rows: usize,
    pub quant: [f32; 64],
    pub x: Axis,
    pub y: Axis,
    /// Plane samples across that output pixels read.
    pub reach: usize,
}

/// How a frame renders at a scale into 8-bit pixels.
pub(crate) struct Plan {
    pub width: usize,
    pub height: usize,
    /// Bytes a pixel: 1, 3 or 4.
    pub step: usize,
    pub model: Model,
    pub components: Vec<ComponentPlan>,
    /// Output rows an MCU row gives, and MCU rows.
    pub band_rows: usize,
    pub bands: usize,
    scaled: Scaled,
}

impl Plan {
    /// The plan for `frame` at 1/`scale` with `step` bytes a pixel.
    pub fn new(
        frame: &Frame,
        quant: &Quantization,
        model: Model,
        scale: usize,
        step: usize,
    ) -> Plan {
        let n = 8 / scale;
        let width = frame.width.div_ceil(scale);
        let height = frame.height.div_ceil(scale);
        let components = frame
            .components
            .iter()
            .map(|c| {
                // Scaled, a subsampled component takes a larger IDCT where
                // that replaces upsampling by a power of two both ways, as
                // libjpeg does.
                let mut size = n;
                while size < 8
                    && (frame.max_h * n).is_multiple_of(c.h * size * 2)
                    && (frame.max_v * n).is_multiple_of(c.v * size * 2)
                {
                    size *= 2;
                }
                let stride = c.bx * size;
                let rows = c.by * size;
                let (num_x, den_x) = (c.h * size, n * frame.max_h);
                let (num_y, den_y) = (c.v * size, n * frame.max_v);
                let valid_w = (width * num_x).div_ceil(den_x);
                let valid_h = (height * num_y).div_ceil(den_y);
                let halves = (num_x == den_x || num_x * 2 == den_x)
                    && (num_y == den_y || num_y * 2 == den_y);
                let replicate = scale > 1
                    && (num_x != den_x || num_y != den_y)
                    && (scale == 8 || !halves || valid_w <= 2);
                let x = Axis::new(width, num_x, den_x, valid_w, stride, replicate);
                let y = Axis::new(height, num_y, den_y, valid_h, rows, replicate);
                let reach = x.reach(width, stride);
                let table = &quant.tables[c.quant];
                ComponentPlan {
                    v: c.v,
                    bx: c.bx,
                    size,
                    stride,
                    band_rows: c.v * size,
                    quant: std::array::from_fn(|i| f32::from(table[i])),
                    x,
                    y,
                    reach,
                }
            })
            .collect();
        Plan {
            width,
            height,
            step,
            model,
            components,
            band_rows: frame.max_v * n,
            bands: frame.mcuy,
            scaled: Scaled::new(),
        }
    }

    /// Output bytes a row.
    pub fn row_bytes(&self) -> usize {
        self.width * self.step
    }

    /// The IDCT of band `band`'s coefficients into each component's band
    /// of plane rows.
    fn idct(&self, coefficients: &BandView) -> Vec<Vec<u8>> {
        self.components
            .iter()
            .enumerate()
            .map(|(c, plan)| {
                let mut plane = vec![0u8; plan.stride * plan.band_rows];
                // Columns past what output pixels read are left zero.
                let columns = plan.reach.div_ceil(plan.size).min(plan.bx);
                for row in 0..plan.v {
                    let rows = &mut plane
                        [row * plan.size * plan.stride..(row + 1) * plan.size * plan.stride];
                    for column in 0..columns {
                        let block = coefficients.block(c, row, column, plan.bx);
                        let out = &mut rows[column * plan.size..];
                        if plan.size == 8 {
                            idct8(block, &plan.quant, out, plan.stride);
                        } else {
                            self.scaled
                                .idct(plan.size, block, &plan.quant, out, plan.stride);
                        }
                    }
                }
                plane
            })
            .collect()
    }

    /// Upsamples and converts band `band`'s output rows into `out`.
    fn color(
        &self,
        band: usize,
        planes: &[OnceLock<Vec<Vec<u8>>>],
        out: &mut [u8],
        scratch: &mut Scratch,
    ) {
        let first = band * self.band_rows;
        let rows = out.len() / self.row_bytes();
        scratch.prepare(self);
        for (local, line) in out
            .chunks_exact_mut(self.row_bytes())
            .enumerate()
            .take(rows)
        {
            let y = first + local;
            for (c, plan) in self.components.iter().enumerate() {
                let plane_row = |r: usize| -> &[u8] {
                    let band = planes[r / plan.band_rows].get().expect("IDCT finished");
                    let at = (r % plan.band_rows) * plan.stride;
                    &band[c][at..at + plan.stride]
                };
                let reach = plan.reach;
                let Scratch { rows, vertical } = &mut *scratch;
                // The plane row (or vertical blend) this output row reads.
                let source: &[f32] = match &plan.y {
                    Axis::Identity => {
                        widen(&plane_row(y)[..reach], &mut vertical[..reach]);
                        &vertical[..reach]
                    }
                    Axis::Replicate { num, den } => {
                        widen(&plane_row(y * num / den)[..reach], &mut vertical[..reach]);
                        &vertical[..reach]
                    }
                    Axis::Linear {
                        first,
                        second,
                        weight,
                    } => {
                        let (a, b, w) = (
                            plane_row(first[y] as usize),
                            plane_row(second[y] as usize),
                            weight[y],
                        );
                        if w == 0.0 {
                            widen(&a[..reach], &mut vertical[..reach]);
                        } else {
                            for ((v, &a), &b) in vertical[..reach]
                                .iter_mut()
                                .zip(&a[..reach])
                                .zip(&b[..reach])
                            {
                                *v = f32::from(a) * (1.0 - w) + f32::from(b) * w;
                            }
                        }
                        &vertical[..reach]
                    }
                };
                let row = &mut rows[c][..self.width];
                match &plan.x {
                    Axis::Identity => row.copy_from_slice(&source[..self.width]),
                    Axis::Replicate { num, den } => {
                        for (x, v) in row.iter_mut().enumerate() {
                            *v = source[x * num / den];
                        }
                    }
                    Axis::Linear {
                        first,
                        second,
                        weight,
                    } => {
                        for (((v, &a), &b), &w) in row.iter_mut().zip(first).zip(second).zip(weight)
                        {
                            *v = source[a as usize] * (1.0 - w) + source[b as usize] * w;
                        }
                    }
                }
            }
            color::convert(self.model, &scratch.rows, self.width, line, self.step);
        }
    }
}

#[inline]
fn widen(bytes: &[u8], out: &mut [f32]) {
    for (o, &b) in out.iter_mut().zip(bytes) {
        *o = f32::from(b);
    }
}

/// A worker's rows: each component's upsampled output row and a
/// vertically blended plane row.
#[derive(Default)]
struct Scratch {
    rows: Vec<Vec<f32>>,
    vertical: Vec<f32>,
}

impl Scratch {
    fn prepare(&mut self, plan: &Plan) {
        if self.rows.len() != plan.components.len() {
            self.rows = vec![vec![0.0; plan.width]; plan.components.len()];
            let widest = plan.components.iter().map(|c| c.stride).max().unwrap_or(0);
            self.vertical = vec![0.0; widest.max(plan.width)];
        }
    }
}

/// Coefficient bands as they become available, and the render's progress.
pub(crate) struct Render<'p, 'c> {
    plan: &'p Plan,
    coefficients: Vec<OnceLock<BandView<'c>>>,
    planes: Vec<OnceLock<Vec<Vec<u8>>>>,
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    /// Bands whose coefficients are in (a prefix).
    available: usize,
    /// The next band to IDCT and to color.
    next_idct: usize,
    next_color: usize,
    idct_done: Vec<bool>,
    colored: usize,
    stopped: bool,
}

/// One unit of work.
enum Task {
    Idct(usize),
    Color(usize),
}

impl<'p, 'c> Render<'p, 'c> {
    pub fn new(plan: &'p Plan) -> Self {
        Render {
            plan,
            coefficients: (0..plan.bands).map(|_| OnceLock::new()).collect(),
            planes: (0..plan.bands).map(|_| OnceLock::new()).collect(),
            state: Mutex::new(State {
                available: 0,
                next_idct: 0,
                next_color: 0,
                idct_done: vec![false; plan.bands],
                colored: 0,
                stopped: false,
            }),
            wake: Condvar::new(),
        }
    }

    /// Hands band `band`'s coefficients to the workers; bands arrive in
    /// order.
    pub fn publish(&self, band: usize, coefficients: BandView<'c>) {
        let _ = self.coefficients[band].set(coefficients);
        let mut state = self.state.lock().expect("render state");
        state.available = band + 1;
        drop(state);
        // One new task: one sleeper is enough (busy workers look again
        // before they sleep).
        self.wake.notify_one();
    }

    /// Stops the workers: the decode failed.
    pub fn stop(&self) {
        self.state.lock().expect("render state").stopped = true;
        self.wake.notify_all();
    }

    /// Runs tasks until every band is colored (or the render stops),
    /// writing each band's rows into its slot of `out`.
    pub fn work(&self, out: &[Mutex<&mut [u8]>]) {
        let mut scratch = Scratch::default();
        while let Some(task) = self.next_task() {
            match task {
                Task::Idct(band) => {
                    let coefficients = self.coefficients[band].get().expect("published");
                    let _ = self.planes[band].set(self.plan.idct(coefficients));
                    self.state.lock().expect("render state").idct_done[band] = true;
                    // It may make a color task ready; this worker looks
                    // again itself, so one more is enough.
                    self.wake.notify_one();
                }
                Task::Color(band) => {
                    let mut rows = out[band].lock().expect("band rows");
                    self.plan.color(band, &self.planes, &mut rows, &mut scratch);
                    let mut state = self.state.lock().expect("render state");
                    state.colored += 1;
                    if state.colored == self.plan.bands {
                        drop(state);
                        self.wake.notify_all();
                    }
                }
            }
        }
    }

    fn next_task(&self) -> Option<Task> {
        let bands = self.plan.bands;
        let mut state = self.state.lock().expect("render state");
        loop {
            if state.stopped || state.colored == bands {
                return None;
            }
            let band = state.next_color;
            if band < bands
                && (band.saturating_sub(1)..(band + 2).min(bands)).all(|b| state.idct_done[b])
            {
                state.next_color += 1;
                return Some(Task::Color(band));
            }
            if state.next_idct < state.available {
                state.next_idct += 1;
                return Some(Task::Idct(state.next_idct - 1));
            }
            if state.next_color == bands {
                // Every task is claimed; nothing left for this worker.
                return None;
            }
            state = self.wake.wait(state).expect("render state");
        }
    }
}

/// Splits `out` into each band's rows for [`Render::work`].
pub(crate) fn band_slots<'o>(plan: &Plan, out: &'o mut [u8]) -> Vec<Mutex<&'o mut [u8]>> {
    out.chunks_mut(plan.band_rows * plan.row_bytes())
        .map(Mutex::new)
        .collect()
}

/// How many threads to use, counting the caller's: `requested` (0 for
/// every processor), one for small pictures.
pub(crate) fn thread_count(requested: usize, pixels: usize) -> usize {
    if pixels < 65_536 {
        return 1;
    }
    let wanted = if requested == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        requested
    };
    wanted.clamp(1, 64)
}

/// Renders a frame whose coefficients are all decoded.
pub(crate) fn render_all(plan: &Plan, bands: Vec<BandView>, threads: usize) -> Vec<u8> {
    let mut out = vec![0u8; plan.height * plan.row_bytes()];
    {
        let slots = band_slots(plan, &mut out);
        let render = Render::new(plan);
        for (band, view) in bands.into_iter().enumerate() {
            render.publish(band, view);
        }
        run_workers(&render, &slots, threads, |_| {});
    }
    out
}

/// Runs `render`'s tasks on `threads` threads (the caller's one of them),
/// the caller first running `lead` (which publishes bands for a pipelined
/// render) before it joins in.
pub(crate) fn run_workers<'c, R>(
    render: &Render<'_, 'c>,
    slots: &[Mutex<&mut [u8]>],
    threads: usize,
    lead: impl FnOnce(&Render<'_, 'c>) -> R,
) -> R {
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(|| render.work(slots));
        }
        let result = lead(render);
        render.work(slots);
        result
    })
}
