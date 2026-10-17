//! Encoding: every option decodes close to its source, thread counts never
//! change the bytes, bad requests are refused. With
//! `KINOGAKI_JPEG_ORACLE_DIR` set, each encoding is also written there with
//! its source as PNG for `tools/jpeg_oracle.py check` (libjpeg-turbo).
use kinogaki_jpeg::{DecodeOptions, EncodeOptions, Error, Image, Samples, Subsampling};

/// A smooth picture with an edge and a little noise: JPEG's statistics.
fn picture(width: u32, height: u32, channels: u8) -> Image {
    let mut seed = 99u32;
    let mut samples = Vec::with_capacity((width * height * u32::from(channels)) as usize);
    for y in 0..height {
        for x in 0..width {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = seed >> 28;
            let edge = if x * 3 > width * 2 { 40 } else { 0 };
            let rgb = [
                x * 200 / width + edge + noise,
                y * 220 / height + noise,
                (x + y) * 200 / (width + height) + 30 + noise,
            ];
            for c in 0..usize::from(channels) {
                samples.push(match (channels, c) {
                    (1 | 2, 0) => ((rgb[0] * 3 + rgb[1] * 6 + rgb[2]) / 10) as u8,
                    (2, _) | (4, 3) => 255,
                    _ => rgb[c] as u8,
                });
            }
        }
    }
    Image::new(width, height, channels, Samples::U8(samples)).unwrap()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let error: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    if error == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / error).log10()
    }
}

/// Source and decode as RGB (or grey) bytes, for PSNR.
fn color_bytes(image: &Image) -> Vec<u8> {
    let rgba = image.to_rgba8();
    if image.channels <= 2 {
        rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
    } else {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect()
    }
}

fn oracle_write(name: &str, source: &Image, jpeg: &[u8]) {
    if let Ok(directory) = std::env::var("KINOGAKI_JPEG_ORACLE_DIR") {
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join(format!("{name}.jpg")), jpeg).unwrap();
        let png = kinogaki_png::encode(source, kinogaki_png::Level::Fast).unwrap();
        std::fs::write(directory.join(format!("{name}.png")), png).unwrap();
    }
}

#[test]
fn every_option_decodes_close_to_its_source() {
    let cases: [(u8, Subsampling, f64); 6] = [
        (90, Subsampling::S444, 36.5),
        (90, Subsampling::S422, 35.5),
        (90, Subsampling::S420, 35.5),
        (50, Subsampling::S420, 32.5),
        (100, Subsampling::S444, 50.0),
        (10, Subsampling::S420, 27.5),
    ];
    for (width, height) in [(1, 1), (7, 9), (37, 35), (300, 200)] {
        for channels in [1, 2, 3, 4] {
            let source = picture(width, height, channels);
            for (quality, subsampling, floor) in cases {
                for (restart, optimize) in
                    [(false, false), (true, false), (false, true), (true, true)]
                {
                    let options = EncodeOptions {
                        quality,
                        subsampling,
                        restart,
                        optimize,
                        ..Default::default()
                    };
                    let jpeg = kinogaki_jpeg::encode(&source, &options).unwrap();
                    let decoded = kinogaki_jpeg::decode(&jpeg, &DecodeOptions::default()).unwrap();
                    assert_eq!((decoded.width, decoded.height), (width, height));
                    assert_eq!(decoded.channels, if channels <= 2 { 1 } else { 3 });
                    let value = psnr(&color_bytes(&source), &color_bytes(&decoded));
                    let name = format!(
                        "{width}x{height}-c{channels}-q{quality}-{subsampling:?}-r{}-o{}",
                        u8::from(restart),
                        u8::from(optimize)
                    );
                    // Tiny pictures are mostly edge: no floor below 37 × 35.
                    assert!(width < 37 || value >= floor, "{name}: {value:.2} dB");
                    oracle_write(&name, &source, &jpeg);
                }
            }
        }
    }
}

#[test]
fn threads_never_change_the_bytes() {
    let source = picture(1031, 517, 3);
    for subsampling in [Subsampling::S444, Subsampling::S420] {
        for (restart, optimize) in [(false, false), (true, false), (false, true)] {
            let options = |threads| EncodeOptions {
                subsampling,
                restart,
                optimize,
                threads,
                ..Default::default()
            };
            let one = kinogaki_jpeg::encode(&source, &options(1)).unwrap();
            for threads in [2, 5, 16] {
                assert_eq!(
                    kinogaki_jpeg::encode(&source, &options(threads)).unwrap(),
                    one,
                    "{threads} threads"
                );
            }
        }
    }
}

#[test]
fn optimized_tables_are_smaller_and_restarts_decode_in_parallel() {
    let source = picture(640, 480, 3);
    let plain = kinogaki_jpeg::encode(&source, &EncodeOptions::default()).unwrap();
    let optimized = kinogaki_jpeg::encode(
        &source,
        &EncodeOptions {
            optimize: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        optimized.len() < plain.len(),
        "{} vs {}",
        optimized.len(),
        plain.len()
    );
    let restarted = kinogaki_jpeg::encode(
        &source,
        &EncodeOptions {
            restart: true,
            ..Default::default()
        },
    )
    .unwrap();
    let a = kinogaki_jpeg::decode(&plain, &DecodeOptions::default()).unwrap();
    let b = kinogaki_jpeg::decode(&restarted, &DecodeOptions::default()).unwrap();
    assert_eq!(a, b, "restart markers change only the entropy coding");
}

#[test]
fn an_embedded_icc_profile_reads_back() {
    let profile: Vec<u8> = (0..70_000).map(|i| (i % 251) as u8).collect();
    let source = picture(4, 4, 3);
    let jpeg = kinogaki_jpeg::encode(
        &source,
        &EncodeOptions {
            icc_profile: Some(profile.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(kinogaki_jpeg::icc_profile(&jpeg).unwrap(), Some(profile));
    let plain = kinogaki_jpeg::encode(&source, &EncodeOptions::default()).unwrap();
    assert_eq!(kinogaki_jpeg::icc_profile(&plain).unwrap(), None);
}

#[test]
fn bad_requests_are_refused() {
    let source = picture(4, 4, 3);
    for quality in [0, 101] {
        let options = EncodeOptions {
            quality,
            ..Default::default()
        };
        assert!(matches!(
            kinogaki_jpeg::encode(&source, &options),
            Err(Error::Invalid(_))
        ));
    }
    let float = Image::new(1, 1, 1, Samples::F32(vec![0.5])).unwrap();
    assert!(matches!(
        kinogaki_jpeg::encode(&float, &EncodeOptions::default()),
        Err(Error::Invalid(_))
    ));
    let empty = Image {
        width: 0,
        height: 4,
        channels: 3,
        samples: Samples::U8(vec![]),
    };
    assert!(matches!(
        kinogaki_jpeg::encode(&empty, &EncodeOptions::default()),
        Err(Error::Invalid(_))
    ));
    let short = Image {
        width: 4,
        height: 4,
        channels: 3,
        samples: Samples::U8(vec![0; 47]),
    };
    assert!(matches!(
        kinogaki_jpeg::encode(&short, &EncodeOptions::default()),
        Err(Error::Invalid(_))
    ));
}
