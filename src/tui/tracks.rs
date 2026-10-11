//! Terminal-independent helpers for drawing tracks: lane packing, ruler
//! ticks, overview density and GenBank-style location strings.

use crate::model::{Location, Strand};

/// Assign each feature (given as `(start, end)` in start order) to the first
/// lane where it doesn't overlap the previous feature. Returns the lane of
/// each feature and the number of lanes used.
pub fn pack_lanes(spans: impl IntoIterator<Item = (usize, usize)>) -> (Vec<usize>, usize) {
    let mut lane_ends: Vec<usize> = Vec::new();
    let lanes = spans
        .into_iter()
        .map(
            |(start, end)| match lane_ends.iter().position(|&e| e <= start) {
                Some(lane) => {
                    lane_ends[lane] = end;
                    lane
                }
                None => {
                    lane_ends.push(end);
                    lane_ends.len() - 1
                }
            },
        )
        .collect();
    (lanes, lane_ends.len())
}

/// The smallest "nice" tick spacing (1, 2 or 5 × 10^k bp) that leaves at
/// least `min_cols` columns between ticks.
pub fn tick_spacing(bp_per_col: f64, min_cols: usize) -> usize {
    let min_bp = (bp_per_col * min_cols as f64).max(1.0);
    let mut magnitude = 1usize;
    loop {
        for step in [1, 2, 5] {
            let spacing = step * magnitude;
            if spacing as f64 >= min_bp {
                return spacing;
            }
        }
        magnitude *= 10;
    }
}

/// Count feature starts falling in each of `bins` equal slices of `len` bp.
pub fn density(starts: impl IntoIterator<Item = usize>, len: usize, bins: usize) -> Vec<usize> {
    let mut counts = vec![0; bins];
    if bins == 0 || len == 0 {
        return counts;
    }
    for start in starts {
        let bin = (start as u128 * bins as u128 / len as u128) as usize;
        counts[bin.min(bins - 1)] += 1;
    }
    counts
}

/// Block character for a count relative to the maximum.
pub fn spark(count: usize, max: usize) -> char {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if count == 0 || max == 0 {
        ' '
    } else {
        BARS[((count - 1) * BARS.len() / max).min(BARS.len() - 1)]
    }
}

/// Rebuild a GenBank-style location string, e.g. `complement(<13025..15799)`.
pub fn location_string(location: &Location) -> String {
    // Segments are stored in biological order; GenBank lists them low to high.
    let mut segments = location.segments.clone();
    if location.strand == Strand::Reverse {
        segments.reverse();
    }

    let last = segments.len().saturating_sub(1);
    let parts: Vec<String> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let lt = if i == 0 && location.partial_start {
                "<"
            } else {
                ""
            };
            let gt = if i == last && location.partial_end {
                ">"
            } else {
                ""
            };
            if s.end == s.start + 1 && lt.is_empty() && gt.is_empty() {
                format!("{}", s.end)
            } else {
                format!("{lt}{}..{gt}{}", s.start + 1, s.end)
            }
        })
        .collect();

    let joined = if parts.len() > 1 {
        format!("join({})", parts.join(","))
    } else {
        parts.join("")
    };
    if location.strand == Strand::Reverse {
        format!("complement({joined})")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_overlapping_features_into_lanes() {
        // RPD_0011 / RPD_0012 share base 13025; the third fits back in lane 0.
        let (lanes, count) = pack_lanes([(12200, 13025), (13024, 15799), (15806, 18746)]);
        assert_eq!(lanes, [0, 1, 0]);
        assert_eq!(count, 2);
    }

    #[test]
    fn adjacent_features_share_a_lane() {
        let (lanes, count) = pack_lanes([(0, 10), (10, 20)]);
        assert_eq!(lanes, [0, 0]);
        assert_eq!(count, 1);
    }

    #[test]
    fn tick_spacing_is_nice() {
        assert_eq!(tick_spacing(1.0, 12), 20);
        assert_eq!(tick_spacing(45.0, 12), 1000);
        assert_eq!(tick_spacing(25_000.0, 12), 500_000);
        assert_eq!(tick_spacing(0.5, 12), 10);
    }

    #[test]
    fn density_bins() {
        assert_eq!(density([0, 1, 50, 99], 100, 4), [2, 0, 1, 1]);
    }

    #[test]
    fn spark_scales() {
        assert_eq!(spark(0, 10), ' ');
        assert_eq!(spark(1, 10), '▁');
        assert_eq!(spark(10, 10), '█');
    }

    #[test]
    // One-segment locations really are a Vec holding a single Range.
    #[allow(clippy::single_range_in_vec_init)]
    fn location_strings() {
        let loc = |segments, strand, partial_start, partial_end| Location {
            segments,
            strand,
            partial_start,
            partial_end,
        };
        assert_eq!(
            location_string(&loc(vec![648..2067], Strand::Forward, false, false)),
            "649..2067"
        );
        assert_eq!(
            location_string(&loc(vec![4984..5000], Strand::Forward, false, true)),
            "4985..>5000"
        );
        assert_eq!(
            location_string(&loc(vec![29..40, 9..20], Strand::Reverse, true, false)),
            "complement(join(<10..20,30..40))"
        );
    }
}
