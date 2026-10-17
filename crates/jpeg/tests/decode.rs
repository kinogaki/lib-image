//! Decoding against libjpeg-turbo, the ICC rules, and malformed input.
//!
//! `tests/fixtures/*.jpg` cover every coding path (made by Pillow, cjpeg
//! and TurboJPEG: baseline and progressive, 4:4:4/4:2:2/4:2:0 and odd
//! sampling factors, grey, RGB, CMYK, YCCK, restart markers, several
//! threads' worth). `tests/fixtures/reference` holds libjpeg-turbo's
//! decodes of them, full size and scaled (`tools/jpeg_oracle.py
//! references`).
use kinogaki_jpeg::{DecodeOptions, Error, Image, Samples};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn bytes(image: &Image) -> &[u8] {
    match &image.samples {
        Samples::U8(v) => v,
        _ => panic!("8-bit samples"),
    }
}

/// Largest and mean absolute sample difference.
fn difference(a: &[u8], b: &[u8]) -> (i32, f64) {
    assert_eq!(a.len(), b.len());
    let max = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (i32::from(x) - i32::from(y)).abs())
        .max()
        .unwrap_or(0);
    let total: i64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| i64::from((i32::from(x) - i32::from(y)).abs()))
        .sum();
    (max, total as f64 / a.len().max(1) as f64)
}

#[test]
fn every_fixture_decodes_as_libjpeg_turbo_does() {
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures().join("reference"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    assert!(names.len() >= 170, "{} references", names.len());
    for reference in names {
        let stem = reference.file_stem().unwrap().to_str().unwrap().to_owned();
        let (name, scale) = match stem.split_once(".s") {
            Some((name, scale)) => (name.to_owned(), scale.parse().unwrap()),
            None => (stem.clone(), 1u8),
        };
        let expected = kinogaki_png::decode(&std::fs::read(&reference).unwrap(), 1 << 24).unwrap();
        let options = DecodeOptions {
            scale,
            ..Default::default()
        };
        let decoded = kinogaki_jpeg::decode(&read(&format!("{name}.jpg")), &options)
            .unwrap_or_else(|e| panic!("{stem}: {e}"));
        assert_eq!(
            (decoded.width, decoded.height, decoded.channels),
            (expected.width, expected.height, expected.channels),
            "{stem}"
        );
        let (max, mean) = difference(bytes(&decoded), bytes(&expected));
        // libjpeg repeats samples for 4:1 and 3:2 subsampling where this
        // decoder interpolates them; everything else is within rounding of
        // libjpeg's fixed-point arithmetic.
        let odd = scale == 1 && (name == "s411" || name == "s32");
        let (limit, mean_limit) = if odd { (48, 4.0) } else { (4, 0.4) };
        assert!(
            max <= limit && mean <= mean_limit,
            "{stem}: max {max} mean {mean:.3}"
        );
    }
}

#[test]
fn threads_and_layouts_give_the_same_pixels() {
    for name in [
        "mt_b420.jpg",
        "mt_b422.jpg",
        "mt_b444.jpg",
        "mt_gray.jpg",
        "mt_p420.jpg",
        "mt_restart.jpg",
    ] {
        let data = read(name);
        let one = kinogaki_jpeg::decode(
            &data,
            &DecodeOptions {
                threads: 1,
                ..Default::default()
            },
        )
        .unwrap();
        for threads in [2, 3, 16] {
            let many = kinogaki_jpeg::decode(
                &data,
                &DecodeOptions {
                    threads,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(many, one, "{name} on {threads} threads");
        }
        let rgba = kinogaki_jpeg::decode_rgba8(&data, &DecodeOptions::default()).unwrap();
        assert_eq!(rgba.channels, 4);
        assert_eq!(rgba.to_rgba8(), one.to_rgba8(), "{name} as RGBA");
    }
    // The pipelined, restart-parallel and progressive routes agree on the
    // same picture (mt_b420, mt_restart and mt_p420 were all made from it).
    let pipelined = kinogaki_jpeg::decode(&read("mt_b420.jpg"), &DecodeOptions::default()).unwrap();
    let restarts =
        kinogaki_jpeg::decode(&read("mt_restart.jpg"), &DecodeOptions::default()).unwrap();
    assert_eq!(pipelined, restarts);
}

#[test]
fn info_reads_the_frame_header() {
    let info = kinogaki_jpeg::info(&read("p420_37x35.jpg")).unwrap();
    assert_eq!(
        (info.width, info.height, info.components, info.progressive),
        (37, 35, 3, true)
    );
    assert_eq!(info.scaled(8), (5, 5));
    assert_eq!(info.scaled(2), (19, 18));
    let cmyk = kinogaki_jpeg::info(&read("cmyk_7x9.jpg")).unwrap();
    assert_eq!((cmyk.components, cmyk.progressive), (4, false));
    assert!(matches!(
        kinogaki_jpeg::info(b"not a jpeg at all"),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn bad_requests_are_refused() {
    let data = read("b420_64x49.jpg");
    assert_eq!(
        kinogaki_jpeg::decode(
            &data,
            &DecodeOptions {
                scale: 3,
                ..Default::default()
            }
        ),
        Err(Error::Invalid("scale must be 1, 2, 4 or 8"))
    );
    assert_eq!(
        kinogaki_jpeg::decode(
            &data,
            &DecodeOptions {
                max_pixels: 64 * 49 - 1,
                ..Default::default()
            }
        ),
        Err(Error::TooLarge)
    );
}

#[test]
fn unsupported_codings_say_so() {
    // The baseline fixture's SOF0 relabelled as arithmetic (SOF9) and as
    // 12-bit precision.
    let data = read("b444_7x9.jpg");
    let sof = data.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    let mut arithmetic = data.clone();
    arithmetic[sof + 1] = 0xC9;
    assert!(matches!(
        kinogaki_jpeg::decode(&arithmetic, &DecodeOptions::default()),
        Err(Error::Unsupported(_))
    ));
    let mut twelve = data.clone();
    twelve[sof + 4] = 12;
    assert!(matches!(
        kinogaki_jpeg::decode(&twelve, &DecodeOptions::default()),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn truncated_files_fail_cleanly() {
    for name in [
        "b420_37x35.jpg",
        "p420_37x35.jpg",
        "restart420.jpg",
        "mt_restart.jpg",
        "prestart444.jpg",
    ] {
        let data = read(name);
        let step = (data.len() / 300).max(1);
        for cut in (0..data.len() - 1).step_by(step) {
            let result = kinogaki_jpeg::decode(&data[..cut], &DecodeOptions::default());
            assert!(
                matches!(result, Err(Error::Corrupt { .. })),
                "{name} cut at {cut}: {result:?}"
            );
        }
    }
}

#[test]
fn mutated_files_never_panic() {
    let mut seed = 0x1234_5678u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for name in [
        "b420_64x49.jpg",
        "p444_64x49.jpg",
        "s21_12.jpg",
        "ycck420.jpg",
        "restart_rows.jpg",
        "pscans.jpg",
    ] {
        let data = read(name);
        for _ in 0..400 {
            let mut mutated = data.clone();
            for _ in 0..1 + next() % 4 {
                let at = next() as usize % mutated.len();
                mutated[at] ^= 1 << (next() % 8);
            }
            for scale in [1, 8] {
                let _ = kinogaki_jpeg::decode(
                    &mutated,
                    &DecodeOptions {
                        scale,
                        ..Default::default()
                    },
                );
            }
        }
    }
}

/// FNV-1a, as the libjpeg-turbo oracle printed each profile.
fn fnv(data: &[u8]) -> u32 {
    data.iter().fold(2_166_136_261u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    })
}

#[test]
fn icc_profiles_join_as_libjpeg_turbo_joins_them() {
    let folder = fixtures().join("icc");
    let expected = std::fs::read_to_string(folder.join("expected.txt")).unwrap();
    let mut count = 0;
    for line in expected.lines() {
        let (name, want) = line.split_once(" icc=").unwrap();
        let profile =
            kinogaki_jpeg::icc_profile(&std::fs::read(folder.join(name)).unwrap()).unwrap();
        let got = match profile {
            Some(bytes) => format!("{}:{:08x}", bytes.len(), fnv(&bytes)),
            None => "-".to_owned(),
        };
        assert_eq!(got, want, "{name}");
        count += 1;
    }
    assert_eq!(count, 18);
}
