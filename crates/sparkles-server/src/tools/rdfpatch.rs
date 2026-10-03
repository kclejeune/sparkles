//! `sparkles rdfpatch`, Jena's `rdfpatch`: parse RDF Patch files, write their rows back
//! as text, and print the counts of data, prefix and transaction rows to standard error.

use anyhow::{Context, Result, bail};
use sparkles::patch::{PatchReader, PatchRow, PatchWriter};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct RdfpatchArgs {
    /// The patch files, `-` for standard input. `.trp` is the binary form, anything
    /// else the text form, unless --format says otherwise.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// The form of the input: text or binary (RDF Thrift)
    #[arg(long, value_parser = ["text", "binary"])]
    format: Option<String>,
    /// Write the rows in the binary form instead of text
    #[arg(long)]
    binary_out: bool,
}

/// The form of a patch file: `--format`, else its extension (`.trp` binary, `.rdfp`
/// text), else text.
pub fn binary_input(path: &Path, format: Option<&str>) -> bool {
    match format {
        Some(f) => f == "binary",
        None => sparkles::patch::binary_for_path(path).unwrap_or(false),
    }
}

/// Open a patch file, or standard input for `-`.
pub fn open(path: &Path) -> Result<Box<dyn Read>> {
    if path.as_os_str() == "-" {
        return Ok(Box::new(std::io::stdin().lock()));
    }
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(Box::new(f))
}

#[derive(Default)]
struct Counts {
    adds: u64,
    deletes: u64,
    prefix_adds: u64,
    prefix_deletes: u64,
    tx: u64,
    tc: u64,
    ta: u64,
}

/// `1234567` as `1,234,567`, as Jena prints counts.
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn run(a: RdfpatchArgs) -> Result<()> {
    let stdout = std::io::stdout();
    let mut w = PatchWriter::new(std::io::BufWriter::new(stdout.lock()), a.binary_out);
    let mut failed = false;
    for path in &a.files {
        let binary = binary_input(path, a.format.as_deref());
        let mut c = Counts::default();
        let mut reader = PatchReader::new(open(path)?, binary);
        loop {
            let row = match reader.next_row() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    w.flush()?;
                    eprintln!("{}: {e}", path.display());
                    failed = true;
                    break;
                }
            };
            match &row {
                PatchRow::Add(_) => c.adds += 1,
                PatchRow::Delete(_) => c.deletes += 1,
                PatchRow::PrefixSet(..) => c.prefix_adds += 1,
                PatchRow::PrefixRemove(_) => c.prefix_deletes += 1,
                PatchRow::Begin => c.tx += 1,
                PatchRow::Commit => c.tc += 1,
                PatchRow::Abort => c.ta += 1,
                PatchRow::Header(..) | PatchRow::Segment => {}
            }
            w.row(&row)?;
        }
        if failed {
            break;
        }
        w.flush()?;
        eprintln!(
            "# Data:     Adds={} Deletes={}",
            grouped(c.adds),
            grouped(c.deletes)
        );
        eprintln!(
            "# Prefixes: Adds={} Deletes={}",
            grouped(c.prefix_adds),
            grouped(c.prefix_deletes)
        );
        if c.tx + c.tc + c.ta > 0 {
            eprintln!(
                "# Txn:      TX={}, TC={}, TA={}",
                grouped(c.tx),
                grouped(c.tc),
                grouped(c.ta)
            );
        }
    }
    if failed {
        bail!("the patch could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_are_grouped() {
        assert_eq!(super::grouped(0), "0");
        assert_eq!(super::grouped(1234), "1,234");
        assert_eq!(super::grouped(1234567), "1,234,567");
    }
}
