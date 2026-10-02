//! `sparkles rset`: a SPARQL result set converted between formats (spec G05 §3.5), and
//! the result rendering of `rsparql`.

use anyhow::{Context, Result, bail};
use sparesults::{
    QueryResultsFormat, QueryResultsParser, QueryResultsSerializer, ReaderQueryResultsParserOutput,
};
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct RsetArgs {
    /// The result file (`-` or none: standard input)
    file: Option<PathBuf>,
    /// Input format: json, xml or tsv (default: from the extension, JSON for standard
    /// input). CSV loses term types and cannot be read
    #[arg(long = "in", value_name = "FMT")]
    input: Option<String>,
    /// Output format: text, json, xml, csv or tsv
    #[arg(long, default_value = "text", value_name = "FMT")]
    results: String,
}

/// A result format by name (`json`, `srj`, `xml`, `srx`, `csv`, `tsv`) or media type.
pub fn results_format(name: &str) -> Option<QueryResultsFormat> {
    let n = name.to_ascii_lowercase();
    QueryResultsFormat::from_extension(&n).or_else(|| QueryResultsFormat::from_media_type(&n))
}

/// Read a result set in `format` from `r` and write it to `out` as `to` (`text` is
/// Jena's text table).
pub fn convert_results(
    format: QueryResultsFormat,
    r: impl Read,
    to: &str,
    out: &mut impl Write,
) -> Result<()> {
    let parsed = QueryResultsParser::from_format(format)
        .for_reader(r)
        .context("reading the results")?;
    if to == "text" {
        match parsed {
            ReaderQueryResultsParserOutput::Boolean(b) => super::table::write_boolean(b, out)?,
            ReaderQueryResultsParserOutput::Solutions(s) => {
                let vars: Vec<String> = s
                    .variables()
                    .iter()
                    .map(|v| v.as_str().to_string())
                    .collect();
                let names = s.variables().to_vec();
                let mut rows = Vec::new();
                for sol in s {
                    let sol = sol.context("reading the results")?;
                    rows.push(names.iter().map(|v| sol.get(v).cloned()).collect());
                }
                super::table::write_table(&vars, &rows, &Default::default(), out)?;
            }
        }
        return Ok(());
    }
    let Some(target) = results_format(to) else {
        bail!("unknown result format '{to}' (text, json, xml, csv or tsv)");
    };
    let ser = QueryResultsSerializer::from_format(target);
    match parsed {
        ReaderQueryResultsParserOutput::Boolean(b) => {
            ser.serialize_boolean_to_writer(&mut *out, b)?;
        }
        ReaderQueryResultsParserOutput::Solutions(s) => {
            let mut w = ser.serialize_solutions_to_writer(&mut *out, s.variables().to_vec())?;
            for sol in s {
                let sol = sol.context("reading the results")?;
                w.serialize(&sol)?;
            }
            w.finish()?;
        }
    }
    if matches!(target, QueryResultsFormat::Json | QueryResultsFormat::Xml) {
        writeln!(out)?;
    }
    Ok(())
}

pub fn run(a: RsetArgs) -> Result<()> {
    let path = a.file.filter(|f| f.as_os_str() != "-");
    let format = match (&a.input, &path) {
        (Some(f), _) => {
            results_format(f).with_context(|| format!("unknown result format '{f}'"))?
        }
        (None, Some(p)) => p
            .extension()
            .and_then(|e| e.to_str())
            .and_then(results_format)
            .with_context(|| format!("{}: unknown result format (use --in)", p.display()))?,
        (None, None) => QueryResultsFormat::Json,
    };
    let reader: Box<dyn Read> = match &path {
        Some(p) => {
            Box::new(std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?)
        }
        None => Box::new(std::io::stdin().lock()),
    };
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    convert_results(format, reader, &a.results, &mut out)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"head":{"vars":["s","n"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://e/a"},"n":{"type":"literal","value":"1","datatype":"http://www.w3.org/2001/XMLSchema#integer"}},{"s":{"type":"bnode","value":"b0"}}]}}"#;

    fn conv(to: &str) -> String {
        let mut out = Vec::new();
        convert_results(QueryResultsFormat::Json, JSON.as_bytes(), to, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn conversions() {
        let t = conv("text");
        assert!(t.contains("| <http://e/a> |"), "{t}");
        assert!(t.contains("?n"), "{t}");
        let csv = conv("csv");
        assert!(csv.starts_with("s,n\r\nhttp://e/a,1\r\n"), "{csv:?}");
        let tsv = conv("tsv");
        assert!(tsv.contains("<http://e/a>\t1"), "{tsv}");
        let xml = conv("xml");
        assert!(xml.contains("<uri>http://e/a</uri>"), "{xml}");
        // and back
        let mut out = Vec::new();
        convert_results(QueryResultsFormat::Xml, xml.as_bytes(), "json", &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("\"http://e/a\""));
        let mut out = Vec::new();
        convert_results(
            QueryResultsFormat::Json,
            br#"{"head":{},"boolean":true}"#.as_slice(),
            "text",
            &mut out,
        )
        .unwrap();
        assert_eq!(out, b"yes\n");
        assert!(
            results_format("srj").is_some()
                && results_format("application/sparql-results+xml").is_some()
        );
    }
}
