//! Length-limited Huffman code lengths and canonical codes for the encoder.

/// Code lengths for symbol frequencies `freqs`, no longer than `limit`.
/// Unused symbols get 0; a lone used symbol gets length 1.
pub fn lengths(freqs: &[u32], limit: u8) -> Vec<u8> {
    let mut lengths = vec![0u8; freqs.len()];
    let mut used: Vec<(u32, u16)> = freqs
        .iter()
        .enumerate()
        .filter(|&(_, &f)| f > 0)
        .map(|(s, &f)| (f, s as u16))
        .collect();
    match used.len() {
        0 => return lengths,
        1 => {
            lengths[used[0].1 as usize] = 1;
            return lengths;
        }
        _ => {}
    }
    // Ascending frequency (ties by symbol, so lengths are deterministic).
    used.sort_unstable();
    let depths = tree_depths(&used);
    // Limit the depths: count codes per length, fold the too-long ones into
    // `limit`, then rebalance until the Kraft sum is exactly one.
    let limit = limit as usize;
    let mut counts = vec![0u32; limit.max(*depths.iter().max().expect("symbols")) + 1];
    for &d in &depths {
        counts[d] += 1;
    }
    for d in limit + 1..counts.len() {
        counts[limit] += counts[d];
        counts[d] = 0;
    }
    let mut total: u64 = (1..=limit)
        .map(|d| u64::from(counts[d]) << (limit - d))
        .sum();
    while total > 1 << limit {
        counts[limit] -= 1;
        for d in (1..limit).rev() {
            if counts[d] != 0 {
                counts[d] -= 1;
                counts[d + 1] += 2;
                break;
            }
        }
        total -= 1;
    }
    // The most frequent symbols take the shortest codes.
    let mut symbols = used.iter().rev().map(|&(_, s)| s as usize);
    for (d, &count) in counts.iter().enumerate().take(limit + 1).skip(1) {
        for _ in 0..count {
            lengths[symbols.next().expect("a symbol per code")] = d as u8;
        }
    }
    lengths
}

/// The depth of each leaf of a Huffman tree over `sorted` (ascending
/// frequency), in the same order: the two-queue construction.
fn tree_depths(sorted: &[(u32, u16)]) -> Vec<usize> {
    let n = sorted.len();
    // Nodes 0..n are leaves; n.. are internal nodes in creation order, whose
    // weights are non-decreasing, so the two queues stay sorted.
    let mut weight: Vec<u64> = sorted.iter().map(|&(f, _)| u64::from(f)).collect();
    let mut parent = vec![0usize; 2 * n - 1];
    let (mut leaf, mut internal) = (0, n);
    for _ in 0..n - 1 {
        let mut pick = || {
            if leaf < n && (internal >= weight.len() || weight[leaf] <= weight[internal]) {
                leaf += 1;
                leaf - 1
            } else {
                internal += 1;
                internal - 1
            }
        };
        let (a, b) = (pick(), pick());
        let node = weight.len();
        weight.push(weight[a] + weight[b]);
        parent[a] = node;
        parent[b] = node;
    }
    // Depths top-down: the root (last node) has depth 0.
    let mut depth = vec![0usize; 2 * n - 1];
    for node in (0..2 * n - 2).rev() {
        depth[node] = depth[parent[node]] + 1;
    }
    depth.truncate(n);
    depth
}

/// Canonical codes for `lengths`, bit-reversed for least-significant-first
/// writing.
pub fn codes(lengths: &[u8]) -> Vec<u16> {
    let mut counts = [0u16; 16];
    for &l in lengths {
        counts[l as usize] += 1;
    }
    counts[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + counts[bits - 1]) << 1;
        next[bits] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let code = next[l as usize];
            next[l as usize] += 1;
            code.reverse_bits() >> (16 - l)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kraft(lengths: &[u8]) -> f64 {
        lengths
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 0.5f64.powi(i32::from(l)))
            .sum()
    }

    #[test]
    fn optimal_for_small_alphabets() {
        assert_eq!(lengths(&[5, 0, 0], 15), vec![1, 0, 0]);
        assert_eq!(lengths(&[1, 1], 15), vec![1, 1]);
        assert_eq!(lengths(&[1, 1, 2, 4], 15), vec![3, 3, 2, 1]);
    }

    #[test]
    fn limits_hold_and_codes_stay_complete() {
        // Fibonacci frequencies make the deepest possible tree.
        let mut freqs = vec![1u32, 1];
        while freqs.len() < 30 {
            let n = freqs.len();
            freqs.push(freqs[n - 1] + freqs[n - 2]);
        }
        for limit in [7u8, 9, 15] {
            let l = lengths(&freqs, limit);
            assert!(
                l.iter().all(|&x| (1..=limit).contains(&x)),
                "{limit}: {l:?}"
            );
            assert!((kraft(&l) - 1.0).abs() < 1e-12, "{limit}");
            // More frequent never longer.
            for i in 1..freqs.len() {
                assert!(l[i] <= l[i - 1]);
            }
        }
    }

    #[test]
    fn canonical_codes() {
        // RFC 1951 §3.2.2's example: lengths (3, 3, 3, 3, 3, 2, 4, 4).
        let c = codes(&[3, 3, 3, 3, 3, 2, 4, 4]);
        let msb = |code: u16, l: u32| code.reverse_bits() >> (16 - l);
        assert_eq!(
            [msb(c[0], 3), msb(c[5], 2), msb(c[6], 4), msb(c[7], 4)],
            [0b010, 0b00, 0b1110, 0b1111]
        );
    }
}
