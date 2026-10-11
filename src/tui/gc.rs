//! GC skew, (G − C) / (G + C), over arbitrary windows of a sequence.

use std::ops::Range;

/// Bases per block of the running totals. Smaller blocks mean less scanning
/// at window edges; larger ones mean less memory (two `u32`s per block).
const BLOCK: usize = 64;

/// Running G and C totals per block, so any window is counted in
/// O(BLOCK) time however large it is.
pub struct GcCounts {
    /// `g[i]` and `c[i]` count bases in blocks `0..i`.
    g: Vec<u32>,
    c: Vec<u32>,
    /// Lowest and highest points of the cumulative skew, if the sequence has
    /// any G or C.
    pub cumulative_min: Option<usize>,
    pub cumulative_max: Option<usize>,
}

fn is_g(b: u8) -> bool {
    matches!(b, b'G' | b'g')
}

fn is_c(b: u8) -> bool {
    matches!(b, b'C' | b'c')
}

impl GcCounts {
    pub fn new(seq: &[u8]) -> Self {
        let blocks = seq.len().div_ceil(BLOCK);
        let mut g = Vec::with_capacity(blocks + 1);
        let mut c = Vec::with_capacity(blocks + 1);
        let (mut total_g, mut total_c) = (0u32, 0u32);
        g.push(0);
        c.push(0);

        // Cumulative skew: +1 per G, -1 per C along the sequence.
        let (mut walk, mut min, mut max) = (0i64, 0i64, 0i64);
        let (mut min_at, mut max_at) = (None, None);

        for (i, chunk) in seq.chunks(BLOCK).enumerate() {
            for (j, &b) in chunk.iter().enumerate() {
                if is_g(b) {
                    total_g += 1;
                    walk += 1;
                    if walk > max || max_at.is_none() {
                        max = walk;
                        max_at = Some(i * BLOCK + j);
                    }
                } else if is_c(b) {
                    total_c += 1;
                    walk -= 1;
                    if walk < min || min_at.is_none() {
                        min = walk;
                        min_at = Some(i * BLOCK + j);
                    }
                }
            }
            g.push(total_g);
            c.push(total_c);
        }

        GcCounts {
            g,
            c,
            cumulative_min: min_at,
            cumulative_max: max_at,
        }
    }

    /// G and C counts in `range` of `seq` (the sequence this was built from).
    pub fn count(&self, seq: &[u8], range: Range<usize>) -> (u32, u32) {
        let range = range.start.min(seq.len())..range.end.min(seq.len());
        let first_full = range.start.div_ceil(BLOCK);
        let last_full = range.end / BLOCK;
        if first_full >= last_full {
            return scan(&seq[range]);
        }
        let (head_g, head_c) = scan(&seq[range.start..first_full * BLOCK]);
        let (tail_g, tail_c) = scan(&seq[last_full * BLOCK..range.end]);
        (
            self.g[last_full] - self.g[first_full] + head_g + tail_g,
            self.c[last_full] - self.c[first_full] + head_c + tail_c,
        )
    }

    /// GC skew of `range`, or `None` if it has no G or C.
    pub fn skew(&self, seq: &[u8], range: Range<usize>) -> Option<f64> {
        let (g, c) = self.count(seq, range);
        (g + c > 0).then(|| (g as f64 - c as f64) / (g + c) as f64)
    }
}

fn scan(bases: &[u8]) -> (u32, u32) {
    bases.iter().fold((0, 0), |(g, c), &b| {
        (g + is_g(b) as u32, c + is_c(b) as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_a_direct_scan() {
        // Deterministic pseudo-random sequence over several blocks.
        let seq: Vec<u8> = (0..1000u32)
            .map(|i| b"ACGTgcNa"[(i.wrapping_mul(2654435761) >> 7) as usize % 8])
            .collect();
        let counts = GcCounts::new(&seq);
        for range in [0..0, 0..1000, 3..61, 63..65, 64..128, 10..990, 500..2000] {
            let clipped = range.start.min(1000)..range.end.min(1000);
            assert_eq!(
                counts.count(&seq, range.clone()),
                scan(&seq[clipped]),
                "{range:?}"
            );
        }
    }

    #[test]
    fn skew_sign_and_empty_windows() {
        let seq = b"GGGCATATAT";
        let counts = GcCounts::new(seq);
        assert_eq!(counts.skew(seq, 0..4), Some(0.5));
        assert_eq!(counts.skew(seq, 4..10), None);
    }

    #[test]
    fn cumulative_extremes() {
        // Walk: C C C G G G G G -> -1 -2 -3 -2 -1 0 1 2
        let seq = b"CCCGGGGG";
        let counts = GcCounts::new(seq);
        assert_eq!(counts.cumulative_min, Some(2));
        assert_eq!(counts.cumulative_max, Some(7));
    }
}
