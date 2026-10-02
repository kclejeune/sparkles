//! Jena's text table of a result set (`ResultSetFormatter.out`), for `query`, `rsparql`
//! and `rset`.

use oxrdf::Term;
use std::collections::BTreeMap;
use std::io::Write;

/// A term as the table shows it: an IRI under one of `prefixes` as a prefixed name.
fn show(t: Option<&Term>, prefixes: &BTreeMap<String, String>) -> String {
    match t {
        None => String::new(),
        Some(Term::NamedNode(n)) => {
            for (p, ns) in prefixes {
                if let Some(l) = n.as_str().strip_prefix(ns.as_str())
                    && l.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    return format!("{p}:{l}");
                }
            }
            format!("<{}>", n.as_str())
        }
        Some(t) => t.to_string(),
    }
}

/// Write the table of `vars` and `rows` (one optional term per variable).
pub fn write_table(
    vars: &[String],
    rows: &[Vec<Option<Term>>],
    prefixes: &BTreeMap<String, String>,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|t| show(t.as_ref(), prefixes)).collect())
        .collect();
    let mut widths: Vec<usize> = vars.iter().map(|v| v.chars().count() + 1).collect();
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line: String = widths
        .iter()
        .map(|w| "-".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("-");
    writeln!(out, "-{line}-")?;
    let hdr: Vec<String> = vars
        .iter()
        .enumerate()
        .map(|(i, v)| format!(" {:w$} ", format!("?{v}"), w = widths[i]))
        .collect();
    writeln!(out, "|{}|", hdr.join("|"))?;
    writeln!(out, "={}=", "=".repeat(line.chars().count()))?;
    for row in &rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| format!(" {:w$} ", c, w = widths[i]))
            .collect();
        writeln!(out, "|{}|", cells.join("|"))?;
    }
    writeln!(out, "-{line}-")?;
    Ok(())
}

/// The answer of an ASK query as the table shows it.
pub fn write_boolean(b: bool, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "{}", if b { "yes" } else { "no" })
}
