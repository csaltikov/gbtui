//! Core data model for genome-viewer.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strand {
    Forward,
    Reverse,
    #[default]
    Unknown,
}
