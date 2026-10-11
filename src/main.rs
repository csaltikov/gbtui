mod model;
mod parser;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use model::{Feature, FeatureKind, MoleculeType, Record, Strand, Topology};

/// Rows shown in the feature table before it is cut off.
const MAX_FEATURE_ROWS: usize = 20;
/// Bases shown in the sequence preview.
const PREVIEW_BASES: usize = 60;

fn main() -> Result<()> {
    let path: PathBuf = std::env::args_os()
        .nth(1)
        .context("usage: genome-viewer <file.gb|file.fasta>")?
        .into();

    let records = parser::parse_file(&path)?;
    view(&records)
}

/// Print a text summary of each record.
fn view(records: &[Record]) -> Result<()> {
    for (i, record) in records.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_record(record);
    }
    Ok(())
}

fn print_record(record: &Record) {
    let mut header = format!("{}  {} bp", record.name, thousands(record.len()));
    if let Some(molecule) = molecule_name(record.molecule) {
        header.push_str("  ");
        header.push_str(molecule);
    }
    if !record.features.is_empty() || record.topology == Topology::Circular {
        header.push_str(match record.topology {
            Topology::Linear => "  linear",
            Topology::Circular => "  circular",
        });
    }
    println!("{header}");

    if let Some(definition) = &record.definition {
        println!("  {definition}");
    }

    let mut details = Vec::new();
    // VERSION already includes the accession, so show ACCESSION only as a fallback.
    if let Some(version) = &record.version {
        details.push(format!("Version: {version}"));
    } else if let Some(accession) = &record.accession {
        details.push(format!("Accession: {accession}"));
    }
    if let Some(division) = &record.division {
        details.push(format!("Division: {division}"));
    }
    if let Some(date) = &record.date {
        details.push(format!("Date: {date}"));
    }
    if !details.is_empty() {
        println!("  {}", details.join("   "));
    }

    if let Some(organism) = &record.organism {
        println!("  Organism: {}", organism.name);
        if !organism.lineage.is_empty() {
            println!("  Lineage:  {}", organism.lineage.join(" > "));
        }
    }

    if !record.sequence.is_empty() {
        let preview = &record.sequence[..record.len().min(PREVIEW_BASES)];
        let more = if record.len() > PREVIEW_BASES { "..." } else { "" };
        println!("  Sequence: {}{more}", String::from_utf8_lossy(preview));
    }

    if record.features.is_empty() {
        return;
    }

    println!();
    print_feature_counts(record);
    println!();
    print_feature_table(record);
}

fn print_feature_counts(record: &Record) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for feature in &record.features {
        *counts.entry(feature.kind.as_key()).or_default() += 1;
    }

    // Most common first; ties alphabetically so the output is stable.
    let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    println!("  Features ({}):", thousands(record.features.len()));
    let width = counts.iter().map(|(kind, _)| kind.len()).max().unwrap_or(0);
    for (kind, count) in counts {
        println!("    {kind:<width$}  {:>7}", thousands(count));
    }
}

fn print_feature_table(record: &Record) {
    let rows: Vec<&Feature> = record
        .features
        .iter()
        .filter(|f| is_product_feature(&f.kind))
        .collect();
    if rows.is_empty() {
        return;
    }

    // Wide enough for "<1234567" on either side of "..".
    let coord_width = record.len().to_string().len() + 1;
    let kind_width = rows
        .iter()
        .map(|f| f.kind.as_key().len())
        .max()
        .unwrap_or(0)
        .max("type".len());

    println!(
        "  {:>w$}  {:<w$}  ±  {:<k$}  {:<10}  product",
        "start",
        "end",
        "type",
        "label",
        w = coord_width,
        k = kind_width,
    );
    for feature in rows.iter().take(MAX_FEATURE_ROWS) {
        let loc = &feature.location;
        // Display in GenBank's 1-based inclusive coordinates.
        let start = format!("{}{}", if loc.partial_start { "<" } else { "" }, loc.start() + 1);
        let end = format!("{}{}", if loc.partial_end { ">" } else { "" }, loc.end());
        let strand = match loc.strand {
            Strand::Forward => '+',
            Strand::Reverse => '-',
            Strand::Unknown => '?',
        };
        println!(
            "  {start:>w$}  {end:<w$}  {strand}  {:<k$}  {:<10}  {}",
            feature.kind.as_key(),
            feature.label().unwrap_or("-"),
            feature.qualifier("product").unwrap_or(""),
            w = coord_width,
            k = kind_width,
        );
    }
    if rows.len() > MAX_FEATURE_ROWS {
        println!(
            "  ... and {} more",
            thousands(rows.len() - MAX_FEATURE_ROWS)
        );
    }
}

/// Features that carry a gene product: CDSs and RNAs. `gene` features are
/// skipped because they duplicate their CDS/RNA and have no product.
fn is_product_feature(kind: &FeatureKind) -> bool {
    match kind {
        FeatureKind::Cds | FeatureKind::Mrna | FeatureKind::Trna | FeatureKind::Rrna => true,
        FeatureKind::Other(key) => key.ends_with("RNA"),
        _ => false,
    }
}

fn molecule_name(molecule: MoleculeType) -> Option<&'static str> {
    match molecule {
        MoleculeType::Dna => Some("DNA"),
        MoleculeType::Rna => Some("RNA"),
        MoleculeType::Protein => Some("protein"),
        MoleculeType::Unknown => None,
    }
}

/// 4892717 -> "4,892,717"
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(4_892_717), "4,892,717");
    }
}
