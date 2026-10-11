mod model;
mod parser;

use std::collections::HashMap;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use model::{Feature, FeatureKind, MoleculeType, Record, Strand, Topology};

/// Summarize GenBank and FASTA files.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// GenBank (.gb, .gbk) or FASTA (.fa, .fasta) file
    file: PathBuf,

    /// Rows to show in the feature table [default: 20, or all with --tsv]
    #[arg(short = 'n', long, value_name = "N", conflicts_with = "all")]
    rows: Option<usize>,

    /// Show every row of the feature table
    #[arg(short, long)]
    all: bool,

    /// Only list features of these types, e.g. `-t CDS,tRNA` [default: CDS and RNA features]
    #[arg(short = 't', long = "type", value_name = "TYPE", value_delimiter = ',')]
    types: Vec<String>,

    /// Only show features overlapping START..END (1-based, inclusive, like GenBank)
    #[arg(short, long, value_name = "START..END", value_parser = parse_region)]
    region: Option<Range<usize>>,

    /// Bases of sequence to preview (0 hides it)
    #[arg(short, long, value_name = "N", default_value_t = 60)]
    preview: usize,

    /// Print only the feature table, as tab-separated values with a header row
    #[arg(long)]
    tsv: bool,
}

impl Cli {
    fn row_limit(&self) -> usize {
        match (self.all, self.rows) {
            (true, _) => usize::MAX,
            (false, Some(n)) => n,
            (false, None) if self.tsv => usize::MAX,
            (false, None) => 20,
        }
    }

    /// The region clipped to one record, as a 0-based half-open range.
    /// `None` means the whole record.
    fn window(&self, record: &Record) -> Option<Range<usize>> {
        self.region
            .as_ref()
            .map(|r| r.start.min(record.len())..r.end.min(record.len()))
    }

    /// Whether a feature belongs in the table.
    fn wants(&self, feature: &Feature) -> bool {
        if self.types.is_empty() {
            is_product_feature(&feature.kind)
        } else {
            let key = feature.kind.as_key();
            self.types.iter().any(|t| t.eq_ignore_ascii_case(key))
        }
    }
}

/// Parse `START..END` or `START-END` (1-based, inclusive; commas allowed)
/// into a 0-based half-open range.
fn parse_region(s: &str) -> Result<Range<usize>, String> {
    let (start, end) = s
        .split_once("..")
        .or_else(|| s.split_once('-'))
        .ok_or_else(|| format!("expected START..END, got `{s}`"))?;

    let number = |n: &str| {
        n.trim()
            .replace(',', "")
            .parse::<usize>()
            .map_err(|_| format!("`{}` is not a position", n.trim()))
    };
    let (start, end) = (number(start)?, number(end)?);

    if start == 0 {
        return Err("positions start at 1".to_string());
    }
    if start > end {
        return Err(format!("start ({start}) is after end ({end})"));
    }
    Ok(start - 1..end)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let records = parser::parse_file(&cli.file)?;

    let mut out = BufWriter::new(io::stdout().lock());
    let result = if cli.tsv {
        write_tsv(&mut out, &records, &cli)
    } else {
        view(&mut out, &records, &cli)
    };

    match result.and_then(|()| out.flush()) {
        // The reader went away (e.g. `| head`): stop quietly.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

const TSV_COLUMNS: [&str; 9] = [
    "record",
    "type",
    "start",
    "end",
    "strand",
    "partial",
    "locus_tag",
    "gene",
    "product",
];

/// Write the feature table for every record as TSV. Coordinates are 1-based
/// inclusive, as in GenBank; `partial` is `<`, `>`, `<>` or empty.
fn write_tsv(out: &mut impl Write, records: &[Record], cli: &Cli) -> io::Result<()> {
    writeln!(out, "{}", TSV_COLUMNS.join("\t"))?;

    let mut remaining = cli.row_limit();
    for record in records {
        let window = cli.window(record);
        let rows = record
            .features
            .iter()
            .filter(|f| window.as_ref().is_none_or(|w| f.location.overlaps(w)))
            .filter(|f| cli.wants(f));

        for feature in rows {
            if remaining == 0 {
                return Ok(());
            }
            remaining -= 1;
            writeln!(out, "{}", tsv_row(record, feature).join("\t"))?;
        }
    }
    Ok(())
}

fn tsv_row(record: &Record, feature: &Feature) -> [String; 9] {
    let loc = &feature.location;
    let partial = match (loc.partial_start, loc.partial_end) {
        (true, true) => "<>",
        (true, false) => "<",
        (false, true) => ">",
        (false, false) => "",
    };
    let strand = match loc.strand {
        Strand::Forward => "+",
        Strand::Reverse => "-",
        Strand::Unknown => ".",
    };
    let text = |key: &str| tsv_field(feature.qualifier(key).unwrap_or(""));

    [
        tsv_field(&record.name),
        feature.kind.as_key().to_string(),
        (loc.start() + 1).to_string(),
        loc.end().to_string(),
        strand.to_string(),
        partial.to_string(),
        text("locus_tag"),
        text("gene"),
        text("product"),
    ]
}

/// Tabs and line breaks would break the columns, so turn them into spaces.
fn tsv_field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

/// Print a text summary of each record.
fn view(out: &mut impl Write, records: &[Record], cli: &Cli) -> io::Result<()> {
    for (i, record) in records.iter().enumerate() {
        if i > 0 {
            writeln!(out)?;
        }
        print_record(out, record, cli)?;
    }
    Ok(())
}

fn print_record(out: &mut impl Write, record: &Record, cli: &Cli) -> io::Result<()> {
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
    writeln!(out, "{header}")?;

    if let Some(definition) = &record.definition {
        writeln!(out, "  {definition}")?;
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
        writeln!(out, "  {}", details.join("   "))?;
    }

    if let Some(organism) = &record.organism {
        writeln!(out, "  Organism: {}", organism.name)?;
        if !organism.lineage.is_empty() {
            writeln!(out, "  Lineage:  {}", organism.lineage.join(" > "))?;
        }
    }

    let window = cli.window(record);
    if let (Some(region), Some(window)) = (&cli.region, &window) {
        if window.is_empty() {
            writeln!(
                out,
                "  Region {}..{} is beyond the end of this record",
                thousands(region.start + 1),
                thousands(region.end)
            )?;
            return Ok(());
        }
        writeln!(
            out,
            "  Region:   {}..{} ({} bp)",
            thousands(window.start + 1),
            thousands(window.end),
            thousands(window.len())
        )?;
    }

    let shown = window.clone().unwrap_or(0..record.len());
    if cli.preview > 0 && !shown.is_empty() {
        let end = shown.end.min(shown.start + cli.preview);
        let more = if end < shown.end { "..." } else { "" };
        writeln!(
            out,
            "  Sequence: {}{more}",
            String::from_utf8_lossy(&record.sequence[shown.start..end])
        )?;
    }

    if record.features.is_empty() {
        return Ok(());
    }

    let features: Vec<&Feature> = record
        .features
        .iter()
        .filter(|f| window.as_ref().is_none_or(|w| f.location.overlaps(w)))
        .collect();

    writeln!(out)?;
    print_feature_counts(out, &features, window.is_some())?;
    writeln!(out)?;
    print_feature_table(out, record, &features, cli)
}

fn print_feature_counts(
    out: &mut impl Write,
    features: &[&Feature],
    in_region: bool,
) -> io::Result<()> {
    if features.is_empty() {
        writeln!(out, "  No features in this region")?;
        return Ok(());
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for feature in features {
        *counts.entry(feature.kind.as_key()).or_default() += 1;
    }

    // Most common first; ties alphabetically so the output is stable.
    let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let scope = if in_region { " in region" } else { "" };
    writeln!(out, "  Features ({}{scope}):", thousands(features.len()))?;
    let width = counts.iter().map(|(kind, _)| kind.len()).max().unwrap_or(0);
    for (kind, count) in counts {
        writeln!(out, "    {kind:<width$}  {:>7}", thousands(count))?;
    }
    Ok(())
}

fn print_feature_table(
    out: &mut impl Write,
    record: &Record,
    features: &[&Feature],
    cli: &Cli,
) -> io::Result<()> {
    let rows: Vec<&Feature> = features.iter().copied().filter(|f| cli.wants(f)).collect();
    if rows.is_empty() {
        if !cli.types.is_empty() && !features.is_empty() {
            writeln!(out, "  No features of type {}", cli.types.join(", "))?;
        }
        return Ok(());
    }
    let limit = cli.row_limit();
    if limit == 0 {
        return Ok(());
    }

    // Wide enough for "<1234567" on either side of "..".
    let coord_width = record.len().to_string().len() + 1;
    let kind_width = rows
        .iter()
        .map(|f| f.kind.as_key().len())
        .max()
        .unwrap_or(0)
        .max("type".len());

    writeln!(
        out,
        "  {:>w$}  {:<w$}  ±  {:<k$}  {:<10}  product",
        "start",
        "end",
        "type",
        "label",
        w = coord_width,
        k = kind_width,
    )?;
    for feature in rows.iter().take(limit) {
        let loc = &feature.location;
        // Display in GenBank's 1-based inclusive coordinates.
        let start = format!(
            "{}{}",
            if loc.partial_start { "<" } else { "" },
            loc.start() + 1
        );
        let end = format!("{}{}", if loc.partial_end { ">" } else { "" }, loc.end());
        let strand = match loc.strand {
            Strand::Forward => '+',
            Strand::Reverse => '-',
            Strand::Unknown => '?',
        };
        writeln!(
            out,
            "  {start:>w$}  {end:<w$}  {strand}  {:<k$}  {:<10}  {}",
            feature.kind.as_key(),
            feature.label().unwrap_or("-"),
            feature.qualifier("product").unwrap_or(""),
            w = coord_width,
            k = kind_width,
        )?;
    }
    if rows.len() > limit {
        writeln!(
            out,
            "  ... and {} more (use --all or --rows N to see them)",
            thousands(rows.len() - limit)
        )?;
    }
    Ok(())
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
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_regions() {
        assert_eq!(parse_region("649..2067"), Ok(648..2067));
        assert_eq!(parse_region("649-2067"), Ok(648..2067));
        assert_eq!(parse_region("10,000..20,000"), Ok(9_999..20_000));
        assert_eq!(parse_region("5..5"), Ok(4..5));
    }

    #[test]
    fn rejects_bad_regions() {
        assert!(parse_region("100").is_err());
        assert!(parse_region("0..10").is_err());
        assert!(parse_region("20..10").is_err());
        assert!(parse_region("a..10").is_err());
    }

    #[test]
    fn row_limit_defaults() {
        let parse = |args: &[&str]| Cli::try_parse_from([&["gv", "x.gb"], args].concat()).unwrap();
        assert_eq!(parse(&[]).row_limit(), 20);
        assert_eq!(parse(&["--tsv"]).row_limit(), usize::MAX);
        assert_eq!(parse(&["--tsv", "-n", "5"]).row_limit(), 5);
        assert_eq!(parse(&["--all"]).row_limit(), usize::MAX);
    }

    #[test]
    fn tsv_rows_from_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/CP000283_5k.gb");
        let records = parser::parse_file(&path).unwrap();
        let record = &records[0];

        let first = tsv_row(record, &record.features[2]);
        assert_eq!(
            first,
            [
                "CP000283",
                "CDS",
                "649",
                "2067",
                "+",
                "",
                "RPD_0001",
                "",
                "chromosomal replication initiator protein DnaA",
            ]
        );

        let last = tsv_row(record, &record.features[8]);
        assert_eq!(&last[2..6], ["4985", "5000", "+", ">"]);
    }

    #[test]
    fn tsv_fields_have_no_tabs_or_newlines() {
        assert_eq!(tsv_field("a\tb\nc"), "a b c");
    }

    #[test]
    fn rows_and_all_conflict() {
        assert!(Cli::try_parse_from(["gv", "x.gb", "--rows", "5", "--all"]).is_err());
    }

    #[test]
    fn types_split_on_commas() {
        let cli = Cli::try_parse_from(["gv", "x.gb", "-t", "CDS,tRNA", "-t", "rRNA"]).unwrap();
        assert_eq!(cli.types, ["CDS", "tRNA", "rRNA"]);
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(4_892_717), "4,892,717");
    }
}
