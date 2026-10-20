# gbtui

A terminal viewer for GenBank and FASTA files. Open a genome, scroll along it,
find a gene by locus tag or product, and copy its DNA or protein sequence out
for use elsewhere. No GUI, no Java, no browser; it runs anywhere you have a
terminal, including over SSH.

```
gbtui -i genome.gb
```

## Features

- Interactive viewer (`-i`) with forward and reverse strand tracks, a ruler,
  a base-level sequence line when zoomed in, and an optional GC skew track
- Sortable feature list, search by locus tag, gene, product or protein ID,
  and go-to by position, range or locus tag
- Details panel for the selected feature: location, qualifiers, translation
  and the feature's DNA (5'→3' on its own strand; reverse-strand features are
  reverse-complemented, `join(...)` features are spliced)
- `y` copies the selected feature's DNA to the system clipboard
- Non-interactive mode prints a summary and feature table; `--tsv` gives
  machine-readable output for pipelines

## Install

gbtui is written in Rust. If you don't have a Rust toolchain, install one
first with [rustup](https://rustup.rs) (one command, no admin rights needed):

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then install gbtui straight from this repository:

```
cargo install --git https://github.com/csaltikov/gbtui
```

That builds the binary and puts it in `~/.cargo/bin`, which rustup adds to
your `PATH`. Run `gbtui --version` to check.

To work on the code instead, clone and build:

```
git clone git@github.com:csaltikov/gbtui.git
cd gbtui
cargo build --release
./target/release/gbtui -i tests/data/CP000283_5k.gb
```

### Clipboard support

The `y` key hands the sequence to your platform's clipboard command, so
nothing extra is needed on macOS (`pbcopy`). On Linux install one of
`wl-clipboard` (Wayland), `xclip` or `xsel` (X11):

```
sudo apt install wl-clipboard
```

Over SSH there is no clipboard to copy to; the sequence is drawn on its own
lines in the details panel so you can select it in the terminal instead.

## Usage

```
gbtui genome.gb                 # summary and the first 20 features
gbtui genome.gb -a -t CDS,tRNA  # every CDS and tRNA
gbtui genome.gb -r 10000..25000 # features in a region (1-based, inclusive)
gbtui genome.gb --tsv > feats.tsv
gbtui genome.gb -i              # interactive viewer
gbtui genome.gb -i -r 10000..25000 -t CDS
```

### Keys in the interactive viewer

| Key | Action |
|---|---|
| `← →` `h l` | pan |
| `+ -` / `0` | zoom in / out / whole record |
| `Tab` `Shift-Tab` | select next / previous feature |
| `Enter` | zoom to the selected feature |
| `f` | open the sortable feature list (`s` sort, `S` reverse) |
| `/` then `n` `N` | search, next / previous match |
| `g` | go to a position, `START..END`, or locus tag |
| `Backspace` `b` | back to the previous view |
| `y` | copy the selected feature's DNA to the clipboard |
| `c` | GC skew track on / off (remembered between runs) |
| `[ ]` | previous / next record in a multi-record file |
| `↑ ↓` `k j` | scroll the details panel |
| `?` | help |
| `q` | quit |

Settings (currently just the GC skew toggle) are kept in
`~/.config/gbtui/settings.conf`.

## File formats

GenBank flat files (`.gb`, `.gbk`), including multi-record files and
`complement`, `join` and partial (`<`, `>`) locations, and FASTA (`.fa`,
`.fasta`). FASTA records have sequence but no features.
