//! Where quantized coefficients live between entropy decoding and the
//! IDCT: 64 `i16` a block in natural order.
//!
//! Two layouts. A baseline frame coded in one interleaved scan keeps its
//! blocks in MCU order (each MCU's blocks together, components in frame
//! order, each component's `v × h` blocks row by row), so any run of MCUs,
//! a restart interval or an MCU row, is one contiguous slice that a thread
//! can own. Everything else keeps one plane of blocks per component, `bx`
//! blocks across and `by` down. In both an MCU row is contiguous per
//! component, which is what a render band reads.
use crate::headers::Frame;

/// Coefficients of a whole frame.
pub(crate) enum Store {
    /// All MCUs, in raster order.
    Mcu(Vec<i16>),
    /// One plane of blocks per frame component and, for a progressive
    /// frame, each block's nonzero AC coefficients by zigzag index (bit k
    /// for coefficient k), which refinement scans walk instead of every
    /// position.
    Planar {
        planes: Vec<Vec<i16>>,
        masks: Vec<Vec<u64>>,
    },
}

/// Block offsets within an MCU of the MCU layout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct McuShape {
    /// Blocks in one MCU.
    pub blocks: usize,
    /// Each frame component's first block in the MCU.
    pub offsets: [usize; 4],
    /// Each component's horizontal sampling factor.
    pub h: [usize; 4],
}

impl McuShape {
    pub fn of(frame: &Frame) -> Self {
        let mut shape = McuShape {
            blocks: 0,
            offsets: [0; 4],
            h: [1; 4],
        };
        for (i, c) in frame.components.iter().enumerate() {
            shape.offsets[i] = shape.blocks;
            shape.h[i] = c.h;
            shape.blocks += c.h * c.v;
        }
        shape
    }

    /// Coefficients in one MCU.
    pub fn len(&self) -> usize {
        self.blocks * 64
    }
}

impl Store {
    pub fn mcu(frame: &Frame) -> Store {
        Store::Mcu(vec![0; frame.mcux * frame.mcuy * McuShape::of(frame).len()])
    }

    pub fn planar(frame: &Frame) -> Store {
        Store::Planar {
            planes: frame
                .components
                .iter()
                .map(|c| vec![0; c.bx * c.by * 64])
                .collect(),
            masks: if frame.progressive {
                frame
                    .components
                    .iter()
                    .map(|c| vec![0; c.bx * c.by])
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    /// Read access to MCU row `band`.
    pub fn band<'a>(&'a self, frame: &Frame, band: usize) -> BandView<'a> {
        match self {
            Store::Mcu(data) => {
                let row = frame.mcux * McuShape::of(frame).len();
                BandView::Mcu(&data[band * row..(band + 1) * row], McuShape::of(frame))
            }
            Store::Planar { planes, .. } => BandView::Planar(
                planes
                    .iter()
                    .zip(&frame.components)
                    .map(|(plane, c)| {
                        let row = c.bx * c.v * 64;
                        &plane[band * row..(band + 1) * row]
                    })
                    .collect(),
            ),
        }
    }
}

/// One MCU row's coefficients, read by a render band.
pub(crate) enum BandView<'a> {
    Mcu(&'a [i16], McuShape),
    /// Per frame component: its `v` block rows of the band.
    Planar(Vec<&'a [i16]>),
}

impl BandView<'_> {
    /// Block (`row`, `column`) of component `c` within the band, `row` in
    /// 0..v and `column` in 0..bx (the component's blocks across).
    #[inline]
    pub fn block(&self, c: usize, row: usize, column: usize, bx: usize) -> &[i16] {
        let at = match self {
            BandView::Mcu(_, shape) => {
                let h = shape.h[c];
                ((column / h) * shape.blocks + shape.offsets[c] + row * h + column % h) * 64
            }
            BandView::Planar(_) => (row * bx + column) * 64,
        };
        let data = match self {
            BandView::Mcu(data, _) => data,
            BandView::Planar(planes) => planes[c],
        };
        &data[at..at + 64]
    }
}

/// Write access to a run of MCUs (MCU layout) or block rows (planar), for
/// a scan decoder.
pub(crate) enum Blocks<'a> {
    /// MCUs from `first` on.
    Mcu {
        data: &'a mut [i16],
        shape: McuShape,
        first: usize,
    },
    /// Each frame component's plane (or part of it, from block row
    /// `first_row` of a component with `v` block rows an MCU row), with
    /// its blocks across.
    Planar {
        planes: Vec<&'a mut [i16]>,
        /// Nonzero masks (progressive frames; empty otherwise).
        masks: Vec<&'a mut [u64]>,
        bx: [usize; 4],
        first_rows: [usize; 4],
    },
}

impl Blocks<'_> {
    /// The block of component `c` at (`row`, `column`) of its plane, which
    /// is block (`vi`, `hi`) of MCU `m` of an interleaved scan.
    #[inline(always)]
    pub fn mcu_block(
        &mut self,
        m: usize,
        c: usize,
        (row, column): (usize, usize),
        (vi, hi): (usize, usize),
    ) -> &mut [i16] {
        match self {
            Blocks::Mcu { data, shape, first } => {
                let at =
                    ((m - *first) * shape.blocks + shape.offsets[c] + vi * shape.h[c] + hi) * 64;
                &mut data[at..at + 64]
            }
            Blocks::Planar {
                planes,
                bx,
                first_rows,
                ..
            } => {
                let at = ((row - first_rows[c]) * bx[c] + column) * 64;
                &mut planes[c][at..at + 64]
            }
        }
    }

    /// Block (`row`, `column`) of component `c` (planar layout), with its
    /// nonzero mask when the frame keeps them.
    #[inline(always)]
    pub fn component_block(
        &mut self,
        c: usize,
        row: usize,
        column: usize,
    ) -> (&mut [i16], Option<&mut u64>) {
        match self {
            Blocks::Mcu { .. } => unreachable!("one-component scans use the planar layout"),
            Blocks::Planar {
                planes,
                masks,
                bx,
                first_rows,
                ..
            } => {
                let index = (row - first_rows[c]) * bx[c] + column;
                (
                    &mut planes[c][index * 64..index * 64 + 64],
                    masks.get_mut(c).map(|m| &mut m[index]),
                )
            }
        }
    }
}
