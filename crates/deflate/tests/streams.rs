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
    assert_eq!(count, 11);
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
