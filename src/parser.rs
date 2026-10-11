//! File parsing: GenBank via gb-io, FASTA via noodles.

use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::Path;

use anyhow::{Context, Result, bail};
use gb_io::seq::{After, Before, Location as GbLocation, Seq, Topology as GbTopology};
use noodles::fasta;

use crate::model::{
    Feature, FeatureKind, Location, MoleculeType, Organism, Record, Strand, Topology,
};

/// Parse every record in a GenBank file.
pub fn parse_genbank(path: &Path) -> Result<Vec<Record>> {
    let seqs = gb_io::reader::parse_file(path)
        .with_context(|| format!("failed to parse GenBank file {}", path.display()))?;
    Ok(seqs.into_iter().map(convert_seq).collect())
}

fn convert_seq(seq: Seq) -> Record {
    let accession = seq
        .accession
        .as_deref()
        .and_then(|a| a.split_whitespace().next())
        .map(str::to_string);
    let name = seq
        .name
        .clone()
        .or_else(|| accession.clone())
        .unwrap_or_else(|| "unnamed".to_string());

    Record {
        name,
        accession,
        version: seq.version,
        definition: seq.definition,
        molecule: convert_molecule(seq.molecule_type.as_deref()),
        topology: match seq.topology {
            GbTopology::Linear => Topology::Linear,
            GbTopology::Circular => Topology::Circular,
        },
        division: Some(seq.division).filter(|d| !d.is_empty()),
        date: seq.date.map(|d| d.to_string()),
        organism: seq
            .source
            .and_then(|s| s.organism)
            .map(|o| convert_organism(&o)),
        features: seq.features.into_iter().map(convert_feature).collect(),
        sequence: seq.seq,
    }
}

fn convert_molecule(molecule: Option<&str>) -> MoleculeType {
    match molecule {
        Some(m) if m.contains("DNA") => MoleculeType::Dna,
        Some(m) if m.contains("RNA") => MoleculeType::Rna,
        Some(m) if m.eq_ignore_ascii_case("aa") => MoleculeType::Protein,
        _ => MoleculeType::Unknown,
    }
}

/// gb-io keeps the ORGANISM block as one string: the name on the first line,
/// then the lineage wrapped over further lines, `;`-separated, ending in `.`.
fn convert_organism(raw: &str) -> Organism {
    let mut lines = raw.lines();
    let name = lines.next().unwrap_or_default().trim().to_string();
    let lineage = lines
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches('.')
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Organism { name, lineage }
}

fn convert_feature(feature: gb_io::seq::Feature) -> Feature {
    Feature {
        kind: FeatureKind::from_key(&feature.kind),
        location: convert_location(&feature.location),
        qualifiers: feature
            .qualifiers
            .into_iter()
            .map(|(k, v)| (k.into_owned(), v))
            .collect(),
    }
}

/// A flattened piece of a gb-io location, before strands are merged.
struct Segment {
    range: Range<usize>,
    strand: Strand,
    partial_start: bool,
    partial_end: bool,
}

fn convert_location(location: &GbLocation) -> Location {
    let mut segments = Vec::new();
    flatten(location, false, &mut segments);

    let strand = match segments.first() {
        Some(first) if segments.iter().all(|s| s.strand == first.strand) => first.strand,
        _ => Strand::Unknown,
    };

    Location {
        partial_start: segments.iter().any(|s| s.partial_start),
        partial_end: segments.iter().any(|s| s.partial_end),
        segments: segments.into_iter().map(|s| s.range).collect(),
        strand,
    }
}

/// Appends segments in biological (5'→3') order. `complement(...)` reverses
/// the order of its children as well as their strand.
fn flatten(location: &GbLocation, reverse: bool, out: &mut Vec<Segment>) {
    let strand = if reverse { Strand::Reverse } else { Strand::Forward };
    match location {
        GbLocation::Range((start, Before(before)), (end, After(after))) => out.push(Segment {
            range: to_usize(*start)..to_usize(*end),
            strand,
            partial_start: *before,
            partial_end: *after,
        }),
        // A site between two adjacent bases: a zero-width segment. On a
        // circular sequence `last^1` puts the site at the end.
        GbLocation::Between(a, b) => {
            let pos = to_usize(if b > a { *b } else { a + 1 });
            out.push(Segment {
                range: pos..pos,
                strand,
                partial_start: false,
                partial_end: false,
            });
        }
        GbLocation::Complement(inner) => {
            let mut inner_segments = Vec::new();
            flatten(inner, !reverse, &mut inner_segments);
            inner_segments.reverse();
            out.extend(inner_segments);
        }
        GbLocation::Join(parts) | GbLocation::Order(parts) | GbLocation::Bond(parts) => {
            for part in parts {
                flatten(part, reverse, out);
            }
        }
        // one-of(...) lists alternatives; show the first.
        GbLocation::OneOf(options) => {
            if let Some(first) = options.first() {
                flatten(first, reverse, out);
            }
        }
        // Positions in another record, and gaps, have nothing to draw here.
        GbLocation::External(..) | GbLocation::Gap(_) => {}
    }
}

fn to_usize(pos: i64) -> usize {
    usize::try_from(pos).unwrap_or(0)
}

/// Parse every record in a FASTA file (no features).
pub fn parse_fasta(path: &Path) -> Result<Vec<Record>> {
    let mut reader = fasta::io::reader::Builder
        .build_from_path(path)
        .with_context(|| format!("failed to open FASTA file {}", path.display()))?;

    reader
        .records()
        .map(|result| {
            let record = result
                .with_context(|| format!("failed to parse FASTA file {}", path.display()))?;
            Ok(convert_fasta_record(&record))
        })
        .collect()
}

fn convert_fasta_record(record: &fasta::Record) -> Record {
    Record {
        name: String::from_utf8_lossy(record.name()).into_owned(),
        accession: None,
        version: None,
        definition: record.description().map(|d| d.to_string()),
        molecule: MoleculeType::Unknown,
        topology: Topology::Linear,
        division: None,
        date: None,
        organism: None,
        features: Vec::new(),
        sequence: record.sequence().as_ref().to_vec(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Format {
    GenBank,
    Fasta,
}

/// Choose a parser from the file extension, falling back to the first
/// non-blank byte (`LOCUS` vs `>`) for unrecognized extensions.
pub fn parse_file(path: &Path) -> Result<Vec<Record>> {
    match detect_format(path)? {
        Format::GenBank => parse_genbank(path),
        Format::Fasta => parse_fasta(path),
    }
}

fn detect_format(path: &Path) -> Result<Format> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    match ext.as_deref() {
        Some("gb" | "gbk" | "gbff" | "genbank") => Ok(Format::GenBank),
        Some("fa" | "fasta" | "fas" | "fna" | "ffn" | "faa" | "frn") => Ok(Format::Fasta),
        _ => sniff_format(path),
    }
}

fn sniff_format(path: &Path) -> Result<Format> {
    let mut head = [0u8; 512];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .with_context(|| format!("failed to read {}", path.display()))?;

    let start = head[..n].trim_ascii_start();
    if start.starts_with(b"LOCUS") {
        Ok(Format::GenBank)
    } else if start.starts_with(b">") {
        Ok(Format::Fasta)
    } else {
        bail!(
            "unrecognized file format for {} (expected GenBank or FASTA)",
            path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{FeatureKind, MoleculeType, Strand, Topology};

    #[test]
    fn parses_cp000283_5k_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/CP000283_5k.gb");
        let records = parse_genbank(&path).unwrap();
        assert_eq!(records.len(), 1);

        let record = &records[0];
        assert_eq!(record.name, "CP000283");
        assert_eq!(record.accession.as_deref(), Some("CP000283"));
        assert_eq!(record.version.as_deref(), Some("CP000283.1"));
        assert_eq!(record.molecule, MoleculeType::Dna);
        assert_eq!(record.topology, Topology::Linear);
        assert_eq!(record.division.as_deref(), Some("BCT"));
        assert_eq!(record.date.as_deref(), Some("28-JAN-2014"));
        assert_eq!(record.len(), 5000);
        assert!(record.sequence.starts_with(b"aatcaattcc"));

        let organism = record.organism.as_ref().unwrap();
        assert_eq!(organism.name, "Rhodopseudomonas palustris BisB5");
        assert_eq!(organism.lineage.first().map(String::as_str), Some("Bacteria"));
        assert_eq!(organism.lineage.last().map(String::as_str), Some("Rhodopseudomonas"));
        assert_eq!(organism.lineage.len(), 7);

        assert_eq!(record.features.len(), 9);

        // CDS 649..2067 -> 0-based half-open 648..2067.
        let dnaa = &record.features[2];
        assert_eq!(dnaa.kind, FeatureKind::Cds);
        assert_eq!(dnaa.location.segments, vec![648..2067]);
        assert_eq!(dnaa.location.strand, Strand::Forward);
        assert_eq!(dnaa.label(), Some("RPD_0001"));
        let xrefs = dnaa.qualifiers.iter().filter(|(k, _)| k == "db_xref");
        assert_eq!(xrefs.count(), 4);
        let translation = dnaa.qualifier("translation").unwrap();
        assert!(translation.starts_with("MSNMEHDRW") && translation.ends_with("RQLQE"));
        assert!(!translation.contains(char::is_whitespace));

        // CDS 4985..>5000
        let gyrb = &record.features[8];
        assert_eq!(gyrb.location.segments, vec![4984..5000]);
        assert!(!gyrb.location.partial_start);
        assert!(gyrb.location.partial_end);
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)
    }

    #[test]
    fn parses_fasta_fixture() {
        let records = parse_fasta(&fixture("two_records.fasta")).unwrap();
        assert_eq!(records.len(), 2);

        let first = &records[0];
        assert_eq!(first.name, "CP000283_1_120");
        assert_eq!(
            first.definition.as_deref(),
            Some("Rhodopseudomonas palustris BisB5, bases 1-120")
        );
        // Line breaks are removed; case is kept.
        assert_eq!(first.len(), 120);
        assert!(first.sequence.starts_with(b"aatcaattcc"));
        assert!(first.sequence.ends_with(b"ggcggcggtt"));
        assert!(first.features.is_empty());

        let second = &records[1];
        assert_eq!(second.name, "no_description");
        assert_eq!(second.definition, None);
        assert_eq!(second.sequence, b"ACGTNNNNACGT");
    }

    #[test]
    fn parse_file_dispatches_on_extension() {
        let gb = parse_file(&fixture("CP000283_5k.gb")).unwrap();
        assert_eq!(gb[0].features.len(), 9);

        let fa = parse_file(&fixture("two_records.fasta")).unwrap();
        assert_eq!(fa.len(), 2);
    }

    #[test]
    fn detects_format_from_contents_for_unknown_extensions() {
        let dir = std::env::temp_dir().join(format!("genome-viewer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let gb = dir.join("genome.txt");
        std::fs::copy(fixture("CP000283_5k.gb"), &gb).unwrap();
        assert_eq!(detect_format(&gb).unwrap(), Format::GenBank);

        let fa = dir.join("reads.seq");
        std::fs::copy(fixture("two_records.fasta"), &fa).unwrap();
        assert_eq!(detect_format(&fa).unwrap(), Format::Fasta);

        let other = dir.join("notes.txt");
        std::fs::write(&other, "hello").unwrap();
        assert!(parse_file(&other).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        assert_eq!(detect_format(Path::new("x.GBK")).unwrap(), Format::GenBank);
        assert_eq!(detect_format(Path::new("x.FASTA")).unwrap(), Format::Fasta);
    }

    fn location(gb: &str) -> crate::model::Location {
        convert_location(&GbLocation::from_gb_format(gb).unwrap())
    }

    #[test]
    fn complement_is_reverse_strand() {
        let loc = location("complement(<10..20)");
        assert_eq!(loc.segments, vec![9..20]);
        assert_eq!(loc.strand, Strand::Reverse);
        assert!(loc.partial_start);
        assert!(!loc.partial_end);
    }

    #[test]
    fn complemented_join_is_in_biological_order() {
        let loc = location("complement(join(10..20,30..40))");
        assert_eq!(loc.segments, vec![29..40, 9..20]);
        assert_eq!(loc.strand, Strand::Reverse);
    }

    #[test]
    fn mixed_strand_join_is_unknown() {
        let loc = location("join(10..20,complement(30..40))");
        assert_eq!(loc.segments, vec![9..20, 29..40]);
        assert_eq!(loc.strand, Strand::Unknown);
    }

    /// Full R. palustris BisB5 genome. Not committed; see /data in .gitignore.
    /// Fetch with:
    /// curl -o data/CP000283.gb "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi?db=nuccore&id=CP000283.1&rettype=gbwithparts&retmode=text"
    #[test]
    #[ignore = "needs data/CP000283.gb; run with cargo test -- --ignored"]
    fn parses_full_cp000283() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/CP000283.gb");
        if !path.exists() {
            eprintln!("skipping: {} not found", path.display());
            return;
        }

        let records = parse_genbank(&path).unwrap();
        assert_eq!(records.len(), 1);

        let record = &records[0];
        assert_eq!(record.name, "CP000283");
        assert_eq!(record.len(), 4_892_717);
        assert_eq!(record.topology, Topology::Circular);
        assert_eq!(record.features.len(), 9_485);

        let cds = record
            .features
            .iter()
            .filter(|f| f.kind == FeatureKind::Cds)
            .count();
        assert_eq!(cds, 4_397);

        assert!(
            record
                .features
                .iter()
                .any(|f| f.location.strand == Strand::Reverse)
        );
    }
}
