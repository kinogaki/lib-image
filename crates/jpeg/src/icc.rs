//! The ICC profile carried in APP2 "ICC_PROFILE" segments.
//!
//! A profile too long for one segment is split across several, each
//! starting "ICC_PROFILE\0", its sequence number (from 1) and the number
//! of segments. They join as libjpeg-turbo's `jpeg_read_icc_profile`
//! joins them: every segment must agree on the count, carry a sequence
//! number from 1 to the count, none may repeat or be missing, the parts
//! join in sequence order, and a profile of no bytes is none. Which
//! segments count follows libjpeg reading the whole file before the first
//! row of a multi-scan picture: segments between and after its scans
//! count; a one-scan picture counts only those before its scan.
use crate::Error;
use crate::headers::{Reader, marker};
use crate::huffman::next_marker;

const SIGNATURE: &[u8] = b"ICC_PROFILE\0";
/// The signature, the sequence number and the count.
const OVERHEAD: usize = 14;

/// Each marker segment's code and payload, in file order, scans' data
/// skipped; and how many scans there are.
/// A marker segment: its code and payload.
type Segment<'a> = (u8, &'a [u8]);

fn segments(data: &[u8]) -> Result<(Vec<Segment<'_>>, usize), Error> {
    if data.get(..2) != Some(&[0xFF, marker::SOI]) {
        return Err(Error::corrupt("not a JPEG file", 0));
    }
    let mut r = Reader::new(data, 2);
    let mut found = Vec::new();
    let mut scans = 0;
    while r.pos < data.len() {
        let code = r.marker()?;
        if code == marker::EOI {
            break;
        }
        if (marker::RST0..=marker::RST7).contains(&code) {
            continue;
        }
        let (payload, _) = r.segment()?;
        found.push((code, payload));
        if code == marker::SOS {
            scans += 1;
            r.pos = next_marker(data, r.pos, true);
        }
    }
    Ok((found, scans))
}

/// The ICC profile of the JPEG in `data`, or `None` when it has none or
/// its segments are inconsistent.
pub fn icc_profile(data: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let (found, scans) = segments(data)?;
    // A one-scan file is read only through its scan.
    let counted = if scans == 1 {
        let scan = found
            .iter()
            .position(|(code, _)| *code == marker::SOS)
            .expect("one scan");
        &found[..scan]
    } else {
        &found[..]
    };
    let parts: Vec<&[u8]> = counted
        .iter()
        .filter(|(code, payload)| {
            *code == marker::APP2 && payload.len() >= OVERHEAD && payload.starts_with(SIGNATURE)
        })
        .map(|(_, payload)| *payload)
        .collect();
    let Some(first) = parts.first() else {
        return Ok(None);
    };
    let count = usize::from(first[13]);
    let mut ordered: Vec<Option<&[u8]>> = vec![None; count + 1];
    for part in &parts {
        let sequence = usize::from(part[12]);
        if usize::from(part[13]) != count
            || sequence == 0
            || sequence > count
            || ordered[sequence].is_some()
        {
            return Ok(None);
        }
        ordered[sequence] = Some(&part[OVERHEAD..]);
    }
    if count == 0 || ordered[1..].iter().any(Option::is_none) {
        return Ok(None);
    }
    let profile: Vec<u8> = ordered[1..]
        .iter()
        .flat_map(|part| part.expect("present").iter().copied())
        .collect();
    Ok(if profile.is_empty() {
        None
    } else {
        Some(profile)
    })
}
