//! Core data model for gbtui.
//!
//! Coordinates are 0-based, half-open (`start..end`), converted from GenBank's
//! 1-based inclusive coordinates at parse time.

use std::ops::Range;

/// One sequence record (a GenBank LOCUS entry or a FASTA record).
#[derive(Debug, Clone)]
pub struct Record {
    /// LOCUS name, or the FASTA record name.
    pub name: String,
    /// ACCESSION (first accession only).
    pub accession: Option<String>,
    /// VERSION, e.g. "CP000283.1".
    pub version: Option<String>,
    /// DEFINITION line, or the FASTA description.
    pub definition: Option<String>,
    pub molecule: MoleculeType,
    pub topology: Topology,
    /// GenBank division code, e.g. "BCT".
    pub division: Option<String>,
    /// LOCUS date, kept as written (e.g. "28-JAN-2014").
    pub date: Option<String>,
    pub organism: Option<Organism>,
    pub features: Vec<Feature>,
    /// Raw sequence bytes (lowercase as in the source file).
    pub sequence: Vec<u8>,
}

impl Record {
    pub fn len(&self) -> usize {
        self.sequence.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Organism {
    /// SOURCE / ORGANISM name.
    pub name: String,
    /// Taxonomic lineage, highest rank first.
    pub lineage: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MoleculeType {
    Dna,
    Rna,
    Protein,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Topology {
    #[default]
    Linear,
    Circular,
}

/// An annotated feature from the FEATURES table.
#[derive(Debug, Clone)]
pub struct Feature {
    pub kind: FeatureKind,
    pub location: Location,
    /// Qualifiers in file order. Keys may repeat (e.g. `db_xref`).
    pub qualifiers: Vec<(String, Option<String>)>,
}

impl Feature {
    /// First value for a qualifier key.
    pub fn qualifier(&self, key: &str) -> Option<&str> {
        self.qualifiers
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Short display label: gene, then locus_tag, then product.
    pub fn label(&self) -> Option<&str> {
        ["gene", "locus_tag", "product"]
            .iter()
            .find_map(|k| self.qualifier(k))
    }

    /// The feature's DNA, 5'→3' on its own strand, in upper case.
    ///
    /// Segments are joined in biological order, and reverse-complemented
    /// for a reverse-strand feature, so a CDS comes out as the coding
    /// sequence (ATG first). A mixed-strand join (`Strand::Unknown`) is
    /// returned as written, forward strand throughout. Segments past the
    /// end of `genome` are clipped.
    pub fn sequence(&self, genome: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for segment in &self.location.segments {
            let start = segment.start.min(genome.len());
            let end = segment.end.min(genome.len());
            let bases = genome[start..end].iter().map(u8::to_ascii_uppercase);
            match self.location.strand {
                Strand::Reverse => out.extend(bases.rev().map(complement)),
                _ => out.extend(bases),
            }
        }
        out
    }
}

/// Complement of an (upper-case) IUPAC nucleotide code; anything else is
/// returned unchanged.
pub fn complement(base: u8) -> u8 {
    match base {
        b'A' => b'T',
        b'T' | b'U' => b'A',
        b'G' => b'C',
        b'C' => b'G',
        b'R' => b'Y',
        b'Y' => b'R',
        b'K' => b'M',
        b'M' => b'K',
        b'B' => b'V',
        b'V' => b'B',
        b'D' => b'H',
        b'H' => b'D',
        other => other,
    }
}

/// Feature keys the viewer treats specially; everything else is `Other`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureKind {
    Source,
    Gene,
    Cds,
    Mrna,
    Trna,
    Rrna,
    RepeatRegion,
    MiscFeature,
    Other(String),
}

impl FeatureKind {
    pub fn from_key(key: &str) -> Self {
        match key {
            "source" => Self::Source,
            "gene" => Self::Gene,
            "CDS" => Self::Cds,
            "mRNA" => Self::Mrna,
            "tRNA" => Self::Trna,
            "rRNA" => Self::Rrna,
            "repeat_region" => Self::RepeatRegion,
            "misc_feature" => Self::MiscFeature,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_key(&self) -> &str {
        match self {
            Self::Source => "source",
            Self::Gene => "gene",
            Self::Cds => "CDS",
            Self::Mrna => "mRNA",
            Self::Trna => "tRNA",
            Self::Rrna => "rRNA",
            Self::RepeatRegion => "repeat_region",
            Self::MiscFeature => "misc_feature",
            Self::Other(key) => key,
        }
    }
}

/// A feature location, flattened to ordered segments on a single strand.
///
/// Covers simple ranges (`649..2067`), `complement(...)` and `join(...)`.
/// Partial ends (`<1..`, `..>5000`) are recorded as flags. As in GenBank,
/// they refer to the low and high coordinate, not the 5'/3' end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Segments in biological order, each 0-based half-open.
    pub segments: Vec<Range<usize>>,
    pub strand: Strand,
    /// Feature extends below its lowest coordinate (`<`).
    pub partial_start: bool,
    /// Feature extends beyond its highest coordinate (`>`).
    pub partial_end: bool,
}

impl Location {
    /// Leftmost start coordinate across all segments.
    pub fn start(&self) -> usize {
        self.segments.iter().map(|s| s.start).min().unwrap_or(0)
    }

    /// Rightmost end coordinate across all segments.
    pub fn end(&self) -> usize {
        self.segments.iter().map(|s| s.end).max().unwrap_or(0)
    }

    /// Whether any segment overlaps a 0-based half-open window.
    pub fn overlaps(&self, window: &Range<usize>) -> bool {
        self.segments
            .iter()
            .any(|s| s.start < window.end && window.start < s.end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strand {
    Forward,
    Reverse,
    #[default]
    Unknown,
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn feature(segments: Vec<Range<usize>>, strand: Strand) -> Feature {
        Feature {
            kind: FeatureKind::Cds,
            location: Location {
                segments,
                strand,
                partial_start: false,
                partial_end: false,
            },
            qualifiers: Vec::new(),
        }
    }

    const GENOME: &[u8] = b"aaatgccgtaggcttaa";

    #[test]
    fn forward_feature_is_upper_cased_slice() {
        let f = feature(vec![2..8], Strand::Forward);
        assert_eq!(f.sequence(GENOME), b"ATGCCG");
    }

    #[test]
    fn reverse_feature_is_reverse_complemented() {
        // complement(3..8) over "tgccgt" (0-based 2..8) -> "ACGGCA"
        let f = feature(vec![2..8], Strand::Reverse);
        assert_eq!(f.sequence(GENOME), b"CGGCAT");
    }

    #[test]
    fn join_concatenates_in_biological_order() {
        // complement(join(3..5,10..13)) is flattened as [9..13, 2..5]
        let f = feature(vec![9..13, 2..5], Strand::Reverse);
        // revcomp("aggc") + revcomp("atg") = "GCCT" + "CAT"
        assert_eq!(f.sequence(GENOME), b"GCCTCAT");
    }

    #[test]
    fn segments_past_the_end_are_clipped() {
        let f = feature(vec![14..40], Strand::Forward);
        assert_eq!(f.sequence(GENOME), b"TAA");
    }

    #[test]
    fn complement_handles_iupac_codes() {
        let codes = b"ACGTURYKMBVDHSWN";
        let expected = b"TGCAAYRMKVBHDSWN";
        let got: Vec<u8> = codes.iter().map(|&b| complement(b)).collect();
        assert_eq!(got, expected);
    }
}
