//! Round trips, zlib-made streams, and malformed input.
use kinogaki_deflate::{
    Error, Level, deflate, inflate, inflate_into, zlib_compress, zlib_decompress,
};
use std::path::Path;

mod inputs;

const LEVELS: [Level; 3] = [Level::Fast, Level::Balanced, Level::Small];

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn zlib_made_streams_decompress() {
    let mut count = 0;
    for (name, data) in inputs::inputs() {
        for entry in std::fs::read_dir(fixtures()).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_str().unwrap().to_string();
            if !file.starts_with(&format!("{name}-")) {
                continue;
            }
            let packed = std::fs::read(&path).unwrap();
            let out = if file.ends_with(".zlib") {
                zlib_decompress(&packed, data.len()).unwrap()
            } else {
                inflate(&packed, data.len()).unwrap()
            };
            assert!(out == data, "{file}");
            count += 1;
        }
    }
    assert_eq!(count, 13);
}

fn edge_inputs() -> Vec<Vec<u8>> {
    let mut all: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        b"ab".to_vec(),
        b"abc".to_vec(),
        b"abcd".to_vec(),
        b"aaaa".to_vec(),
        vec![7; 258],
        vec![7; 259],
        vec![7; 100_000],
        (0..=255).collect(),
    ];
    // A match exactly at the window's reach: 32768 bytes back.
    let mut far: Vec<u8> = (0..32_768u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    far.extend_from_within(0..300);
    all.push(far);
    all.extend(inputs::inputs().into_iter().map(|(_, d)| d));
    all
}

#[test]
fn round_trips_at_every_level() {
    let oracle = std::env::var_os("KINOGAKI_DEFLATE_ORACLE_DIR");
    for (i, data) in edge_inputs().iter().enumerate() {
        for level in LEVELS {
            let raw = deflate(data, level);
            assert_eq!(&inflate(&raw, data.len()).unwrap(), data, "{i} {level:?}");
            let z = zlib_compress(data, level);
            assert_eq!(
                &zlib_decompress(&z, data.len()).unwrap(),
                data,
                "{i} {level:?}"
            );
            // Incompressible input grows only by the stored framing.
            assert!(
                raw.len() <= data.len() + 5 * (data.len() / 32_768 + 1),
                "{i}"
            );
            if let Some(dir) = &oracle {
                let stem = Path::new(dir).join(format!("{i}-{level:?}"));
                std::fs::write(stem.with_extension("zlib"), &z).unwrap();
                std::fs::write(stem.with_extension("raw"), data).unwrap();
            }
        }
    }
}

#[test]
fn compresses_as_well_as_zlib() {
    // zlib's own sizes for the fixtures at levels 1, 6 and 9 (the files).
    for (name, data) in inputs::inputs() {
        if name == "noise" {
            continue;
        }
        for (level, zlib_level) in [(Level::Fast, 1), (Level::Balanced, 6), (Level::Small, 9)] {
            let theirs = std::fs::metadata(fixtures().join(format!("{name}-{zlib_level}.zlib")))
                .map(|m| m.len() as f64);
            let ours = zlib_compress(&data, level).len() as f64;
            if let Ok(theirs) = theirs {
                assert!(
                    ours <= theirs * 1.10,
                    "{name} {level:?}: {ours} vs zlib {theirs}"
                );
            }
        }
    }
}

#[test]
fn limits_and_trailing_data() {
    let data = vec![1u8; 10_000];
    let raw = deflate(&data, Level::Balanced);
    assert_eq!(inflate(&raw, 9_999), Err(Error::TooLarge));
    let mut stream = raw.clone();
    stream.extend_from_slice(b"after");
    let mut out = Vec::new();
    assert_eq!(inflate_into(&stream, &mut out, usize::MAX), Ok(raw.len()));
    let mut z = zlib_compress(&data, Level::Fast);
    let n = z.len();
    z[n - 1] ^= 1;
    assert_eq!(zlib_decompress(&z, usize::MAX), Err(Error::Checksum));
    assert_eq!(zlib_decompress(&[0x78], usize::MAX), Err(Error::Truncated));
    assert!(
        zlib_decompress(&[0x78, 0x9D, 3, 0], usize::MAX).is_err(),
        "bad FCHECK"
    );
}

#[test]
fn every_truncation_and_corruption_fails_cleanly() {
    let (_, text) = &inputs::inputs()[0];
    let z = zlib_compress(&text[..4000], Level::Balanced);
    for cut in 0..z.len() {
        assert!(zlib_decompress(&z[..cut], usize::MAX).is_err(), "cut {cut}");
    }
    // Flipped bits: any result but a panic (a flip may still decode).
    let mut x = 12_345u64;
    for _ in 0..20_000 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let mut bad = z.clone();
        let at = (x >> 33) as usize % bad.len();
        bad[at] ^= 1 << ((x >> 29) & 7);
        let _ = zlib_decompress(&bad, 1 << 20);
        let _ = inflate(&bad[2..], 1 << 20);
    }
}

/// A raw DEFLATE stream written a block at a time: stored blocks and fixed
/// Huffman blocks of literals and matches chosen exactly, so a test names
/// every match a decoder meets.
#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    bits: u64,
    count: u32,
}

/// What a fixed block holds.
#[derive(Clone, Copy)]
enum Symbol {
    Literal(u8),
    Match { length: usize, distance: usize },
}

/// RFC 1951 §3.2.5: each length and distance symbol's base and extra bits.
const LENGTH_BASE: [usize; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [usize; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

impl Writer {
    /// `n` bits of `value`, least-significant first.
    fn put(&mut self, value: u32, n: u32) {
        self.bits |= u64::from(value) << self.count;
        self.count += n;
        while self.count >= 8 {
            self.out.push(self.bits as u8);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    /// A Huffman code of `n` bits, most-significant first.
    fn code(&mut self, code: u32, n: u32) {
        self.put(code.reverse_bits() >> (32 - n), n);
    }

    /// Fixed code symbol `symbol` (RFC 1951 §3.2.6).
    fn fixed_symbol(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0x30 + symbol, 8),
            144..=255 => self.code(0x190 + symbol - 144, 9),
            256..=279 => self.code(symbol - 256, 7),
            _ => self.code(0xC0 + symbol - 280, 8),
        }
    }

    fn stored(&mut self, data: &[u8], last: bool) {
        self.put(u32::from(last), 3);
        if self.count > 0 {
            self.put(0, 8 - self.count);
        }
        let len = data.len() as u16;
        self.out.extend_from_slice(&len.to_le_bytes());
        self.out.extend_from_slice(&(!len).to_le_bytes());
        self.out.extend_from_slice(data);
    }

    fn fixed(&mut self, symbols: &[Symbol], last: bool) {
        self.put(u32::from(last) | 2, 3);
        for &symbol in symbols {
            match symbol {
                Symbol::Literal(byte) => self.fixed_symbol(u32::from(byte)),
                Symbol::Match { length, distance } => {
                    let l = LENGTH_BASE.iter().rposition(|&b| b <= length).unwrap();
                    self.fixed_symbol(257 + l as u32);
                    self.put((length - LENGTH_BASE[l]) as u32, LENGTH_EXTRA[l]);
                    let d = DIST_BASE.iter().rposition(|&b| b <= distance).unwrap();
                    self.code(d as u32, 5);
                    self.put((distance - DIST_BASE[d]) as u32, DIST_EXTRA[d]);
                }
            }
        }
        self.fixed_symbol(256);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.put(0, 8 - self.count);
        }
        self.out
    }
}

/// `history` then `symbols` expanded a byte at a time: what the stream
/// decodes to.
fn expand(history: &[u8], symbols: &[Symbol]) -> Vec<u8> {
    let mut out = history.to_vec();
    for &symbol in symbols {
        match symbol {
            Symbol::Literal(byte) => out.push(byte),
            Symbol::Match { length, distance } => {
                for _ in 0..length {
                    out.push(out[out.len() - distance]);
                }
            }
        }
    }
    out
}

/// Every distance a short period can have, others up to the window's
/// reach, with every length up to 40 and around each chunk size: as the
/// stream's last symbol (ending exactly at a limit of its length, and one
/// byte past one) and followed by literals.
#[test]
fn matches_of_every_short_distance_and_length_decode_exactly() {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let history: Vec<u8> = (0..32_768)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let distances = (1..=40).chain([63, 64, 65, 255, 256, 257, 1_000, 32_767, 32_768]);
    for distance in distances {
        let lengths = (3..=40).chain([63, 64, 65, 127, 128, 129, 255, 256, 257, 258]);
        for length in lengths {
            let matched = Symbol::Match { length, distance };
            for tail in [&[][..], &[Symbol::Literal(7), Symbol::Literal(200)]] {
                let symbols: Vec<Symbol> =
                    [matched].into_iter().chain(tail.iter().copied()).collect();
                let mut w = Writer::default();
                w.stored(&history[..distance], false);
                w.fixed(&symbols, true);
                let stream = w.finish();
                let expected = expand(&history[..distance], &symbols);
                let at = format!("distance {distance}, length {length}, {} after", tail.len());
                assert_eq!(
                    inflate(&stream, expected.len()).as_ref(),
                    Ok(&expected),
                    "{at}"
                );
                assert_eq!(inflate(&stream, usize::MAX).as_ref(), Ok(&expected), "{at}");
                assert_eq!(
                    inflate(&stream, expected.len() - 1),
                    Err(Error::TooLarge),
                    "{at}"
                );
            }
        }
    }
}

/// Stored blocks between Huffman blocks, an empty one among them, and
/// matches reaching back across block boundaries into stored bytes.
#[test]
fn stored_and_huffman_blocks_interleave() {
    let mut w = Writer::default();
    let first = b"Kinogaki stored bytes";
    w.stored(first, false);
    let reach = Symbol::Match {
        length: 9,
        distance: first.len(),
    };
    let symbols = [Symbol::Literal(b'!'), reach, Symbol::Literal(b'.')];
    w.fixed(&symbols, false);
    w.stored(&[], false);
    w.stored(b"tail", false);
    let back = Symbol::Match {
        length: 30,
        distance: 31,
    };
    w.fixed(&[back], true);
    let stream = w.finish();
    let mut expected = expand(first, &symbols);
    expected.extend_from_slice(b"tail");
    let expected = expand(&expected, &[back]);
    assert_eq!(inflate(&stream, usize::MAX).as_ref(), Ok(&expected));
    // What the blocks hold, through the encoder's dynamic blocks.
    let made = deflate(&expected, Level::Balanced);
    assert_eq!(inflate(&made, expected.len()), Ok(expected));
}

/// Seeded data of literals and copies of earlier spans at every reach, at
/// every level: exact limits, an unknown size (the output grows), and
/// appended after a caller's bytes, which are no history.
#[test]
fn random_streams_round_trip_at_every_level() {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut random = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    for size in [1, 7, 300, 4_096, 70_000, 400_000] {
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            if data.len() < 8 || random(3) == 0 {
                let run = 1 + random(40) as usize;
                data.extend((0..run).map(|_| random(256) as u8));
            } else {
                let distance = 1 + random(data.len().min(40_000) as u64) as usize;
                for _ in 0..3 + random(300) {
                    data.push(data[data.len() - distance]);
                }
            }
        }
        data.truncate(size);
        for level in LEVELS {
            let raw = deflate(&data, level);
            assert_eq!(
                inflate(&raw, data.len()).as_ref(),
                Ok(&data),
                "{size} {level:?}"
            );
            assert_eq!(
                inflate(&raw, usize::MAX).as_ref(),
                Ok(&data),
                "{size} {level:?}"
            );
            let mut out = b"caller".to_vec();
            assert_eq!(inflate_into(&raw, &mut out, 6 + data.len()), Ok(raw.len()));
            assert!(out[6..] == data[..], "{size} {level:?} appended");
            let z = zlib_compress(&data, level);
            assert_eq!(zlib_decompress(&z, data.len()).as_ref(), Ok(&data));
        }
    }
}

/// A final dynamic block's header whose codes are given by the lengths of
/// literal/length symbols `litlen` (up to symbol `count - 1`) and of one
/// distance code: every code-length symbol a run of zeros (18) or a length
/// of 1 or 2, coded `18: 0, 1: 10, 2: 11`.
fn dynamic_header(litlen: &[(usize, u32)], count: usize) -> Writer {
    let mut w = Writer::default();
    w.put(1, 1);
    w.put(2, 2);
    w.put(count as u32 - 257, 5);
    w.put(0, 5);
    // Code-length code lengths in RFC 1951's order, up to symbol 1.
    let order = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1];
    w.put(order.len() as u32 - 4, 4);
    for symbol in order {
        let length = match symbol {
            18 => 1,
            1 | 2 => 2,
            _ => 0,
        };
        w.put(length, 3);
    }
    let mut lengths = vec![0u32; count];
    for &(symbol, length) in litlen {
        lengths[symbol] = length;
    }
    lengths.push(1); // the distance code
    let mut at = 0;
    while at < lengths.len() {
        let zeros = (lengths[at..].iter().take_while(|&&l| l == 0).count()).min(138);
        if zeros >= 11 {
            w.code(0, 1);
            w.put(zeros as u32 - 11, 7);
            at += zeros;
            continue;
        }
        assert!(lengths[at] > 0, "short runs of zeros aren't coded here");
        w.code(if lengths[at] == 1 { 0b10 } else { 0b11 }, 2);
        at += 1;
    }
    w
}

#[test]
fn a_stream_cut_short_ends_at_its_end_not_the_limit() {
    // Literal 0 coded `0`, end of block `1`: past the input, the zero bits
    // the reader supplies would decode as literals up to the limit.
    let mut whole = dynamic_header(&[(0, 1), (256, 1)], 257);
    for _ in 0..10 {
        whole.code(0, 1);
    }
    whole.code(1, 1);
    assert_eq!(inflate(&whole.finish(), 1 << 20), Ok(vec![0; 10]));
    let cut = dynamic_header(&[(0, 1), (256, 1)], 257).finish();
    let started = std::time::Instant::now();
    assert_eq!(inflate(&cut, 1 << 30), Err(Error::Truncated));
    assert!(started.elapsed().as_secs() < 2);
    let mut out = Vec::new();
    assert_eq!(inflate_into(&cut, &mut out, 1 << 30), Err(Error::Truncated));
    assert!(
        out.len() < 1 << 10,
        "{} bytes decoded past the end",
        out.len()
    );
}

#[test]
fn a_stream_never_gives_more_than_deflate_can_code() {
    // Literal 0 (`10`), then matches of 258 at distance 1 in two bits each
    // (length 285 `0`, distance `0`), then the end (`11`): DEFLATE's most
    // output a byte, which decodes whole.
    let mut w = dynamic_header(&[(0, 2), (256, 2), (285, 1)], 286);
    w.code(0b10, 2);
    let matches = 4000;
    for _ in 0..matches {
        w.code(0, 2);
    }
    w.code(0b11, 2);
    let stream = w.finish();
    let out = inflate(&stream, usize::MAX).unwrap();
    assert_eq!(out.len(), 1 + 258 * matches);
    assert!(out.len() <= 1032 * stream.len());
    // Cut short, the same stream is refused.
    let cut = &stream[..stream.len() / 2];
    assert_eq!(inflate(cut, usize::MAX), Err(Error::Truncated));
}

#[test]
fn a_raised_stop_ends_a_decode() {
    let data = vec![7u8; 4 << 20];
    let packed = zlib_compress(&data, Level::Fast);
    let go = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        kinogaki_deflate::zlib_decompress_until(&packed, data.len(), &go).as_ref(),
        Ok(&data)
    );
    let stop = std::sync::atomic::AtomicBool::new(true);
    assert_eq!(
        kinogaki_deflate::zlib_decompress_until(&packed, data.len(), &stop),
        Err(Error::Stopped)
    );
}
