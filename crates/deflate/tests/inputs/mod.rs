//! The shared test inputs; tools/zlib_oracle.py makes the same bytes.

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = (self.0 * 1_103_515_245 + 12_345) % (1 << 31);
        self.0
    }
}

/// The named inputs.
pub fn inputs() -> Vec<(&'static str, Vec<u8>)> {
    let words: [&[u8]; 8] = [
        b"mesh",
        b"polygon",
        b"edge",
        b"point",
        b"Kinogaki",
        b" ",
        b"\n",
        b"vertex",
    ];
    let mut x = Lcg(1);
    let mut text = Vec::new();
    while text.len() < 50_000 {
        text.extend_from_slice(words[(x.next() >> 16) as usize % words.len()]);
    }
    let mut x = Lcg(7);
    let noise = (0..70_000).map(|_| (x.next() >> 16) as u8).collect();
    let mut x = Lcg(3);
    let mut runs = Vec::new();
    while runs.len() < 100_000 {
        let v = x.next();
        runs.extend(std::iter::repeat_n(
            (v >> 16) as u8,
            1 + (v >> 8) as usize % 600,
        ));
    }
    vec![("text", text), ("noise", noise), ("runs", runs)]
}
