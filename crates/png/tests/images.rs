//! Decoding independently written PNGs, round trips, and malformed files.
use kinogaki_png::{Error, Image, Level, Samples, decode, encode};
use std::path::Path;

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn every_format_decodes_as_the_specification_says() {
    let mut count = 0;
    for entry in std::fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "png") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let image = decode(&std::fs::read(&path).unwrap(), 1 << 20).unwrap();
        let expected = std::fs::read(path.with_extension("rgba")).unwrap();
        assert!(image.to_rgba8() == expected, "{name}");
        let pixels = image.width as usize * image.height as usize;
        assert_eq!(image.samples.len(), pixels * usize::from(image.channels));
        // The file's own layout: tRNS adds alpha, 16-bit stays 16-bit.
        let layout = match name.as_str() {
            "c0-d16-i0" | "c4-d16-i0" => Some((2, true)),
            "c0-d4-i1" => Some((2, false)),
            "c2-d8-i1" | "c3-d4-i0" | "c6-d8-i0" => Some((4, false)),
            "c6-d16-i1" => Some((4, true)),
            "pillow-RGB" => Some((3, false)),
            "pillow-L" => Some((1, false)),
            _ => None,
        };
        if let Some((channels, wide)) = layout {
            assert_eq!(image.channels, channels, "{name}");
            assert_eq!(matches!(image.samples, Samples::U16(_)), wide, "{name}");
        }
        // Re-encoding keeps every sample.
        let again = decode(&encode(&image, Level::Fast).unwrap(), 1 << 20).unwrap();
        assert_eq!(again, image, "{name}");
        count += 1;
    }
    // 15 color type and depth pairs, each plain and interlaced; 5 from Pillow.
    assert_eq!(count, 35);
}

/// A picture with flat areas, gradients and noise, like a UI capture.
fn picture(width: u32, height: u32) -> Vec<u8> {
    let mut x = 99u64;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for py in 0..height {
        for px in 0..width {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let noise = (x >> 59) as u8;
            let pixel = if px < width / 3 {
                [40, 40, 44, 255]
            } else if py < height / 2 {
                [(px % 256) as u8, (py % 256) as u8, 128, 255]
            } else {
                [noise, noise.wrapping_mul(3), 200, (px % 256) as u8]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

#[test]
fn round_trips() {
    let oracle = std::env::var_os("KINOGAKI_PNG_ORACLE_DIR");
    for (w, h) in [(1, 1), (2, 3), (61, 17), (300, 200)] {
        let rgba = picture(w, h);
        for level in [Level::Fast, Level::Balanced, Level::Small] {
            let original = Image::rgba8(w, h, rgba.clone());
            let png = encode(&original, level).unwrap();
            assert_eq!(decode(&png, 1 << 20).unwrap(), original);
            if let Some(dir) = &oracle {
                let stem = Path::new(dir).join(format!("{w}x{h}-{level:?}"));
                std::fs::write(stem.with_extension("png"), &png).unwrap();
                std::fs::write(stem.with_extension("rgba"), &rgba).unwrap();
            }
        }
    }
}

#[test]
fn every_layout_round_trips() {
    let oracle = std::env::var_os("KINOGAKI_PNG_ORACLE_DIR");
    for channels in 1..=4u8 {
        let n = 23 * 9 * usize::from(channels);
        for samples in [
            Samples::U8((0..n).map(|i| (i * 37 % 256) as u8).collect()),
            Samples::U16((0..n).map(|i| (i * 40_503 % 65_536) as u16).collect()),
        ] {
            let image = Image {
                width: 23,
                height: 9,
                channels,
                samples,
            };
            let png = encode(&image, Level::Balanced).unwrap();
            assert_eq!(decode(&png, 1 << 20).unwrap(), image);
            if let Some(dir) = &oracle
                && matches!(image.samples, Samples::U8(_))
            {
                let stem = Path::new(dir).join(format!("layout-{channels}"));
                std::fs::write(stem.with_extension("png"), &png).unwrap();
                std::fs::write(stem.with_extension("rgba"), image.to_rgba8()).unwrap();
            }
        }
    }
}

#[test]
fn bad_input_fails_cleanly() {
    let short = Image::rgba8(2, 2, vec![0; 15]);
    assert_eq!(encode(&short, Level::Fast), Err(Error::Size));
    let five = Image {
        width: 1,
        height: 1,
        channels: 5,
        samples: Samples::U8(vec![0; 5]),
    };
    assert_eq!(encode(&five, Level::Fast), Err(Error::Size));
    assert!(encode(&Image::rgba8(0, 2, vec![]), Level::Fast).is_err());
    let png = encode(&Image::rgba8(61, 17, picture(61, 17)), Level::Balanced).unwrap();
    assert_eq!(decode(&png, 61 * 17 - 1), Err(Error::TooLarge));
    assert_eq!(decode(b"GIF89a", 1 << 20), Err(Error::Signature));
    let mut bad = png.clone();
    bad[20] ^= 1;
    assert_eq!(decode(&bad, 1 << 20), Err(Error::Crc));
    for cut in 0..png.len() {
        assert!(decode(&png[..cut], 1 << 20).is_err(), "cut {cut}");
    }
    let mut x = 5u64;
    for _ in 0..5000 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let mut bad = png.clone();
        let at = (x >> 33) as usize % bad.len();
        bad[at] = (x >> 20) as u8;
        let _ = decode(&bad, 1 << 20);
    }
}
