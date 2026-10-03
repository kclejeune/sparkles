//! The acceptance examples of spec C05 and the malformed inputs it names.

use super::*;
use std::io::Cursor;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

fn run(csv: &str, opts: &Options) -> Result<Vec<String>> {
    let mut out = Vec::new();
    convert(Cursor::new(csv.as_bytes().to_vec()), opts, &mut |t| {
        out.push(t.to_string());
        Ok(())
    })?;
    out.sort();
    Ok(out)
}

fn default_opts(key: Option<&str>, base: &str) -> Options {
    let mut o = Options::new(
        Mapping::Default {
            key: key.map(str::to_string),
        },
        "people.csv",
    );
    o.base = Some(base.into());
    o
}

fn csvw(meta: &str, name: &str) -> Options {
    let m = csvw::parse(meta, Some("file:///data/meta.json")).unwrap();
    Options::new(Mapping::Csvw(Arc::new(m)), name)
}

fn template(q: &str, meta: Option<&str>) -> Options {
    let t = Template::parse(q, None).unwrap();
    let metadata = meta.map(|m| Arc::new(csvw::parse(m, Some("file:///data/meta.json")).unwrap()));
    Options::new(
        Mapping::Template {
            template: Arc::new(t),
            metadata,
        },
        "people.csv",
    )
}

/// T1: a key column and a namespace.
#[test]
fn default_mapping_with_key() {
    let got = run(
        "id,name\n7,Ann\n8,Bob\n",
        &default_opts(Some("id"), "http://example.org/p/"),
    )
    .unwrap();
    assert_eq!(
        got,
        [
            "<http://example.org/p/7> <http://example.org/p/id> \"7\"",
            "<http://example.org/p/7> <http://example.org/p/name> \"Ann\"",
            "<http://example.org/p/8> <http://example.org/p/id> \"8\"",
            "<http://example.org/p/8> <http://example.org/p/name> \"Bob\"",
        ]
    );
    // an empty key would merge rows; a missing key column is named
    let e = run("id,name\n,Ann\n", &default_opts(Some("id"), "http://e/")).unwrap_err();
    assert!(
        e.to_string()
            .contains("row 2, column 1 (id): the value is required"),
        "{e}"
    );
    let e = run("id,name\n1,Ann\n", &default_opts(Some("nope"), "http://e/")).unwrap_err();
    assert!(e.to_string().contains("no column \"nope\""), "{e}");
}

/// T2: without a key, rows are named by their RFC 7111 fragment in the file.
#[test]
fn default_mapping_rows_and_titles() {
    let mut o = Options::new(Mapping::Default { key: None }, "people.csv");
    o.file_url = Some("file:///data/people.csv".into());
    let got = run("Person ID,name\n7,Ann\n8,\n", &o).unwrap();
    assert_eq!(
        got,
        [
            "<file:///data/people.csv#row=2> <file:///data/people.csv#Person%20ID> \"7\"",
            "<file:///data/people.csv#row=2> <file:///data/people.csv#name> \"Ann\"",
            "<file:///data/people.csv#row=3> <file:///data/people.csv#Person%20ID> \"8\"",
        ]
    );
    // no base and no file: nothing to name the subjects with
    let o = Options::new(Mapping::Default { key: None }, "body");
    assert!(run("a\n1\n", &o).is_err());
}

/// `sparkles csv mapping` prints metadata that maps a table as the default mapping does.
#[test]
fn default_metadata_matches_the_default_mapping() {
    let csv = "id,full name\n7,Ann Lee\n8,Bob\n";
    let mut o = default_opts(Some("id"), "http://example.org/p/");
    o.file_url = Some("file:///data/people.csv".into());
    let m = default_metadata(Cursor::new(csv), &o, Some("id")).unwrap();
    let meta = csvw::parse(&m.to_string(), None).unwrap();
    let mut o2 = Options::new(Mapping::Csvw(Arc::new(meta)), "people.csv");
    o2.file_url = o.file_url.clone();
    assert_eq!(run(csv, &o).unwrap(), run(csv, &o2).unwrap());
    let o = default_opts(None, "http://example.org/p/");
    let m = default_metadata(Cursor::new(csv), &o, None).unwrap();
    let meta = csvw::parse(&m.to_string(), None).unwrap();
    let o2 = Options::new(Mapping::Csvw(Arc::new(meta)), "people.csv");
    assert_eq!(run(csv, &o).unwrap(), run(csv, &o2).unwrap());
}

const META: &str = r#"{
  "@context": "http://www.w3.org/ns/csvw",
  "url": "people.csv",
  "tableSchema": {
    "aboutUrl": "http://example.org/person/{id}",
    "columns": [
      { "name": "id", "titles": "ID", "datatype": "integer",
        "propertyUrl": "http://example.org/id" },
      { "name": "name", "titles": "Name", "propertyUrl": "schema:name", "lang": "en" },
      { "name": "born", "titles": "Born",
        "datatype": { "base": "date", "format": "dd/MM/yyyy" },
        "propertyUrl": "schema:birthDate" },
      { "name": "tags", "titles": "Tags", "separator": ";",
        "propertyUrl": "schema:keywords" },
      { "name": "employer", "titles": "Employer", "null": ["", "n/a"],
        "propertyUrl": "schema:worksFor",
        "valueUrl": "http://example.org/org/{employer}" },
      { "virtual": true, "propertyUrl": "rdf:type", "valueUrl": "schema:Person" }
    ]
  }
}"#;

/// T3: the metadata of spec §3.
#[test]
fn csvw_metadata() {
    let got = run(
        "ID,Name,Born,Tags,Employer\n7,Ann,03/04/1990,a;b,n/a\n8,Bob,01/01/2000,,Acme Corp\n",
        &csvw(META, "people.csv"),
    )
    .unwrap();
    let p7 = "<http://example.org/person/7>";
    let p8 = "<http://example.org/person/8>";
    let s = "http://schema.org/";
    let mut want = vec![
        format!("{p7} <http://example.org/id> \"7\"^^<{XSD}integer>"),
        format!("{p7} <{s}name> \"Ann\"@en"),
        format!("{p7} <{s}birthDate> \"1990-04-03\"^^<{XSD}date>"),
        format!("{p7} <{s}keywords> \"a\""),
        format!("{p7} <{s}keywords> \"b\""),
        format!("{p7} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{s}Person>"),
        format!("{p8} <http://example.org/id> \"8\"^^<{XSD}integer>"),
        format!("{p8} <{s}name> \"Bob\"@en"),
        format!("{p8} <{s}birthDate> \"2000-01-01\"^^<{XSD}date>"),
        format!("{p8} <{s}worksFor> <http://example.org/org/Acme%20Corp>"),
        format!("{p8} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{s}Person>"),
    ];
    want.sort();
    assert_eq!(got, want);
}

/// T4: a value that does not match its datatype names the file, row and column.
#[test]
fn invalid_values_name_their_cell() {
    let e = run(
        "ID,Name,Born,Tags,Employer\n7,Ann,31/02/1990,,\n",
        &csvw(META, "people.csv"),
    )
    .unwrap_err();
    let m = e.to_string();
    assert!(
        m.contains("people.csv: row 2, column 3 (born): \"31/02/1990\" is not a valid date"),
        "{m}"
    );
    let e = run(
        "ID,Name,Born,Tags,Employer\nseven,Ann,03/04/1990,,\n",
        &csvw(META, "people.csv"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("column 1 (id)"), "{e}");
    // a quoted cell with a line break: the line is named too
    let e = run(
        "ID,Name,Born,Tags,Employer\n\"7\",\"A\nnn\",x,,\n",
        &csvw(META, "people.csv"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("row 2, column 3 (born)"), "{e}");
}

/// T5: rows of the wrong length, unclosed quotes, bad UTF-8, too many columns.
#[test]
fn malformed_tables() {
    let o = default_opts(None, "http://e/");
    let e = run("a,b\n1,2\n1,2,3\n", &o).unwrap_err();
    assert!(
        e.to_string()
            .contains("row 3 has 3 cells and the table has 2 columns"),
        "{e}"
    );
    let e = run("a,b\n1,2\n1\n", &o).unwrap_err();
    assert!(e.to_string().contains("row 3 has 1 cells"), "{e}");
    // an unclosed quote reads on until the record limit
    let mut o2 = default_opts(None, "http://e/");
    o2.limits.max_record_bytes = 1000;
    let mut csv = String::from("a,b\n1,\"open\n");
    for i in 0..100_000 {
        csv.push_str(&format!("{i},x\n"));
    }
    let e = run(&csv, &o2).unwrap_err();
    assert!(
        e.to_string().contains("row 2: a record is longer than"),
        "{e}"
    );
    // under the limit, an unclosed quote ends at the end of the file, as in the csv crate
    let got = run("a,b\n1,\"open\n2,x\n", &o).unwrap();
    assert_eq!(got[1], "<http://e/row=2> <http://e/b> \"open\\n2,x\"");
    let mut bad = b"a,b\n1,".to_vec();
    bad.extend([0xff, 0xfe]);
    bad.extend(b"\n");
    let mut out = Vec::new();
    let e = convert(Cursor::new(bad), &o, &mut |t| {
        out.push(t);
        Ok(())
    })
    .unwrap_err();
    assert!(
        e.to_string()
            .contains("row 2, field 2: the table is not UTF-8"),
        "{e}"
    );
    let mut o3 = default_opts(None, "http://e/");
    o3.limits.max_columns = 2;
    assert!(run("a,b,c\n1,2,3\n", &o3).is_err());
    // an empty file and a header alone give nothing
    assert!(run("", &o).unwrap().is_empty());
    assert!(run("a,b\n", &o).unwrap().is_empty());
    // a byte-order mark is not part of the first title
    let got = run("\u{feff}a\n1\n", &o).unwrap();
    assert_eq!(got, ["<http://e/row=2> <http://e/a> \"1\""]);
}

/// T6: the CONSTRUCT template of spec §5.1 gives the triples of the metadata.
#[test]
fn templates() {
    let q = r#"
        PREFIX schema: <http://schema.org/>
        PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
        CONSTRUCT {
          ?person a schema:Person ; schema:name ?name ; schema:birthDate ?born ; schema:position ?ROWNUM .
        }
        WHERE {
          BIND (IRI(CONCAT("http://example.org/person/", ?id)) AS ?person)
          BIND (xsd:date(?born_on) AS ?born)
        }"#;
    let got = run(
        "id,name,born on\n7,Ann,1990-04-03\n8,,2000-01-01\n",
        &template(q, None),
    )
    .unwrap();
    let s = "http://schema.org/";
    let mut want = vec![
        format!(
            "<http://example.org/person/7> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{s}Person>"
        ),
        format!("<http://example.org/person/7> <{s}name> \"Ann\""),
        format!("<http://example.org/person/7> <{s}birthDate> \"1990-04-03\"^^<{XSD}date>"),
        format!("<http://example.org/person/7> <{s}position> \"1\"^^<{XSD}integer>"),
        format!(
            "<http://example.org/person/8> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{s}Person>"
        ),
        format!("<http://example.org/person/8> <{s}birthDate> \"2000-01-01\"^^<{XSD}date>"),
        format!("<http://example.org/person/8> <{s}position> \"2\"^^<{XSD}integer>"),
    ];
    want.sort();
    assert_eq!(got, want);
}

/// Templates with a mapping see typed values; without a header the columns are `?a`,
/// `?b`; FILTER and OPTIONAL see the row.
#[test]
fn templates_with_mappings_and_without_headers() {
    let meta = r#"{"@context": "http://www.w3.org/ns/csvw",
        "dialect": {"header": false},
        "tableSchema": {"columns": [
            {"datatype": "integer"},
            {"name": "org", "valueUrl": "http://e/org/{org}"}]}}"#;
    let q = "CONSTRUCT { ?s <http://e/n> ?n ; <http://e/org> ?org } WHERE { \
             BIND(IRI(CONCAT('http://e/', STR(?a))) AS ?s) BIND(?a + 1 AS ?n) FILTER(?a != 2) }";
    let got = run("1,x\n2,y\n3,z\n", &template(q, Some(meta))).unwrap();
    assert_eq!(
        got,
        [
            format!("<http://e/1> <http://e/n> \"2\"^^<{XSD}integer>"),
            "<http://e/1> <http://e/org> <http://e/org/x>".to_string(),
            format!("<http://e/3> <http://e/n> \"4\"^^<{XSD}integer>"),
            "<http://e/3> <http://e/org> <http://e/org/z>".to_string(),
        ]
    );
    // without a mapping the cells are strings, and empty cells are unbound
    let q = "CONSTRUCT { <http://e/s> <http://e/p> ?v } WHERE { OPTIONAL { BIND(1 AS ?unused) } FILTER(BOUND(?v)) }";
    let got = run("v\nx\n\ny\n\"\"\n", &template(q, None)).unwrap();
    assert_eq!(
        got,
        [
            "<http://e/s> <http://e/p> \"x\"",
            "<http://e/s> <http://e/p> \"y\""
        ]
    );
}

/// Blank nodes of a template are new per row, also across batches.
#[test]
fn template_blank_nodes_per_row() {
    let q = "CONSTRUCT { _:n <http://e/v> ?v } WHERE {}";
    let mut o = template(q, None);
    o.limits.batch_rows = 2;
    let mut subjects = std::collections::HashSet::new();
    let mut n = 0;
    convert(Cursor::new("v\n1\n2\n3\n4\n5\n"), &o, &mut |t| {
        subjects.insert(t.subject.to_string());
        n += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!((n, subjects.len()), (5, 5));
}

/// T10: TSV has no quoting.
#[test]
fn tsv() {
    let mut o = default_opts(None, "http://e/");
    o.tsv = true;
    let got = run("a\tb\n\"x\ty \"z\"\n", &o).unwrap();
    assert_eq!(
        got,
        [
            "<http://e/row=2> <http://e/a> \"\\\"x\"",
            "<http://e/row=2> <http://e/b> \"y \\\"z\\\"\""
        ]
    );
    assert_eq!(tabular_kind(Path::new("a.TSV.gz")), Some(TabularKind::Tsv));
    assert_eq!(tabular_kind(Path::new("a.csv.zst")), Some(TabularKind::Csv));
    assert_eq!(tabular_kind(Path::new("a.ttl")), None);
    assert_eq!(
        tabular_media_type("text/csv; charset=utf-8"),
        Some(TabularKind::Csv)
    );
}

/// Dialects: skipped rows, two header rows, comments, other delimiters, no trimming,
/// skipped columns and blank rows.
#[test]
fn dialects() {
    let meta = r##"{"@context": ["http://www.w3.org/ns/csvw", {"@base": "http://e/"}],
        "url": "t.csv",
        "dialect": {"delimiter": ";", "skipRows": 1, "headerRowCount": 2,
                    "commentPrefix": "#", "trim": false, "skipColumns": 1,
                    "skipBlankRows": true}}"##;
    let csv = "junk line\nx;a;b\nx;A;B\n# a comment\nx; 1 ;2\nx;;\n";
    let got = run(csv, &csvw(meta, "t.csv")).unwrap();
    assert_eq!(
        got,
        [
            "_:t0r4 <http://e/t.csv#a> \" 1 \"",
            "_:t0r4 <http://e/t.csv#b> \"2\"",
        ]
    );
}

/// Lists, ordered lists, `default`, `_row` and `_column` in templates, suppressed
/// columns, number formats.
#[test]
fn csvw_details() {
    let meta = r##"{"@context": "http://www.w3.org/ns/csvw", "url": "http://e/t.csv",
        "tableSchema": {"aboutUrl": "#r{_row}", "columns": [
            {"name": "n", "datatype": {"base": "decimal", "format": {"groupChar": ",", "decimalChar": "."}},
             "propertyUrl": "http://e/n"},
            {"name": "l", "separator": " ", "ordered": true, "datatype": "integer",
             "propertyUrl": "http://e/l"},
            {"name": "d", "default": "none", "propertyUrl": "http://e/c{_column}"},
            {"name": "s", "suppressOutput": true}
        ]}}"##;
    let got = run("n,l,d,s\n\"1,234.50\",1 2,,x\n", &csvw(meta, "t.csv")).unwrap();
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let mut want = vec![
        format!("<http://e/t.csv#r1> <http://e/n> \"1234.5\"^^<{XSD}decimal>"),
        "<http://e/t.csv#r1> <http://e/l> _:t0r2c2l0".to_string(),
        format!("_:t0r2c2l0 <{rdf}first> \"1\"^^<{XSD}integer>"),
        "_:t0r2c2l0 <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> _:t0r2c2l1".to_string(),
        format!("_:t0r2c2l1 <{rdf}first> \"2\"^^<{XSD}integer>"),
        format!("_:t0r2c2l1 <{rdf}rest> <{rdf}nil>"),
        "<http://e/t.csv#r1> <http://e/c3> \"none\"".to_string(),
    ];
    want.sort();
    assert_eq!(got, want);
}

/// A table group's tables are matched by file name; metadata errors are clear.
#[test]
fn table_groups_and_bad_metadata() {
    let meta = r#"{"@context": "http://www.w3.org/ns/csvw", "tables": [
        {"url": "a.csv", "tableSchema": {"aboutUrl": "http://e/a/{x}", "columns": [{"name": "x", "propertyUrl": "http://e/x"}]}},
        {"url": "dir/b.csv", "tableSchema": {"aboutUrl": "http://e/b/{y}", "columns": [{"name": "y", "propertyUrl": "http://e/y"}]}}
    ]}"#;
    let got = run("y\n1\n", &csvw(meta, "/tmp/b.csv")).unwrap();
    assert_eq!(got, ["<http://e/b/1> <http://e/y> \"1\""]);
    assert!(run("y\n1\n", &csvw(meta, "c.csv")).is_err());
    for (bad, why) in [
        (r#"{"tableSchema": {}}"#, "@context"),
        (
            r#"{"@context": "http://www.w3.org/ns/csvw", "tableSchema": "s.json"}"#,
            "inline",
        ),
        (
            r#"{"@context": "http://www.w3.org/ns/csvw", "tableSchema": {"columns": [{"name": "_x"}]}}"#,
            "reserves",
        ),
        (
            r#"{"@context": "http://www.w3.org/ns/csvw", "tableSchema": {"columns": [{"virtual": true}]}}"#,
            "valueUrl",
        ),
        (
            r#"{"@context": "http://www.w3.org/ns/csvw", "dialect": {"encoding": "latin1"}}"#,
            "utf-8",
        ),
        (
            r#"{"@context": "http://www.w3.org/ns/csvw", "tableSchema": {"aboutUrl": "{x"}}"#,
            "unclosed",
        ),
    ] {
        let e = csvw::parse(bad, None).unwrap_err();
        assert!(e.contains(why), "{bad}: {e}");
    }
    let m = csvw::parse(
        r#"{"@context": "http://www.w3.org/ns/csvw", "dc:title": "x", "notes": [], "frob": 1}"#,
        None,
    )
    .unwrap();
    assert_eq!(m.warnings, ["table property \"frob\" is ignored"]);
    // the header must match the metadata's columns in number
    let e = run("ID,Name\n1,a\n", &csvw(META, "people.csv")).unwrap_err();
    assert!(e.to_string().contains("the header has 2 columns"), "{e}");
    // relative IRIs need a table URL
    let meta = r#"{"@context": "http://www.w3.org/ns/csvw",
        "tableSchema": {"aboutUrl": "{x}", "columns": [{"name": "x", "propertyUrl": "http://e/x"}]}}"#;
    let m = csvw::parse(meta, None).unwrap();
    let o = Options::new(Mapping::Csvw(Arc::new(m)), "upload");
    let e = run("x\n1\n", &o).unwrap_err();
    assert!(
        e.to_string().contains("relative and the table has no URL"),
        "{e}"
    );
}

/// The serialized forms and the output limit.
#[test]
fn writing() {
    let o = default_opts(Some("id"), "http://example.org/p/");
    let mut out = Vec::new();
    let g = NamedNode::new_unchecked("urn:g");
    let s = write(
        Cursor::new("id,name\n7,Ann\n"),
        &o,
        &mut out,
        RdfFormat::NQuads,
        Some(&g),
    )
    .unwrap();
    assert_eq!((s.rows, s.triples), (1, 2));
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("<http://example.org/p/7> <http://example.org/p/name> \"Ann\" <urn:g> ."),
        "{text}"
    );
    let mut o = csvw(META, "people.csv");
    let mut out = Vec::new();
    write(
        Cursor::new("ID,Name,Born,Tags,Employer\n7,Ann,03/04/1990,,\n"),
        &o,
        &mut out,
        RdfFormat::Turtle,
        None,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("@prefix schema: <http://schema.org/>"),
        "{text}"
    );
    assert!(text.contains("schema:name \"Ann\"@en"), "{text}");
    o.limits.max_output_bytes = Some(100);
    let e = write(
        Cursor::new("ID,Name,Born,Tags,Employer\n7,Ann,03/04/1990,,\n8,Bob,03/04/1990,,\n"),
        &o,
        std::io::sink(),
        RdfFormat::NTriples,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(e, Error::BudgetExceeded(b) if b.kind == BudgetKind::DecompressedBytes),
        "{e}"
    );
}

/// A generated table that is never held in memory.
struct Generated {
    rows: u64,
    next: u64,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
            if self.next == 0 {
                self.buf.extend_from_slice(b"id,name,score\n");
            }
            while self.buf.len() < 64 << 10 && self.next < self.rows {
                self.next += 1;
                let n = self.next;
                self.buf.extend_from_slice(
                    format!("{n},\"name {n}, the {}\",{}.5\n", n % 7, n % 100).as_bytes(),
                );
            }
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let k = out.len().min(self.buf.len() - self.pos);
        out[..k].copy_from_slice(&self.buf[self.pos..self.pos + k]);
        self.pos += k;
        Ok(k)
    }
}

/// T8, scaled down: the conversion streams.
#[test]
fn large_tables_stream() {
    let rows = 200_000;
    let o = default_opts(Some("id"), "http://example.org/p/");
    let mut n = 0u64;
    let s = convert(
        Generated {
            rows,
            next: 0,
            buf: Vec::new(),
            pos: 0,
        },
        &o,
        &mut |_| {
            n += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!((s.rows, s.triples, n), (rows, 3 * rows, 3 * rows));
}

/// The loader path: a temporary N-Triples file loaded in one commit; a failing table
/// leaves the store as it was.
#[test]
fn loads_into_a_store() {
    let store = crate::store::Store::in_memory(Default::default());
    let o = default_opts(Some("id"), "http://example.org/p/");
    let (path, stats) = to_ntriples_file(Cursor::new("id,name\n7,Ann\n8,Bob\n"), &o, None).unwrap();
    assert_eq!(stats.triples, 4);
    let g = NamedNode::new_unchecked("urn:g");
    let src = source(&path, Some(g), "people.csv");
    let n = store.load(&[src]).unwrap();
    assert_eq!(n, 4);
    let r = crate::sparql::query(
        store.snapshot(),
        "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <urn:g> { ?s ?p ?o } }",
        &Default::default(),
    )
    .unwrap();
    assert_eq!(
        r.rows()[0][0].as_ref().unwrap().to_string(),
        format!("\"4\"^^<{XSD}integer>")
    );
    // the same table again adds nothing
    let (path, _) = to_ntriples_file(Cursor::new("id,name\n7,Ann\n8,Bob\n"), &o, None).unwrap();
    assert_eq!(
        store
            .load(&[source(&path, Some(NamedNode::new_unchecked("urn:g")), "x")])
            .unwrap(),
        0
    );
    let e = to_ntriples_file(Cursor::new("id,name\n9,Cy\n9\n"), &o, None).unwrap_err();
    assert!(e.to_string().contains("row 3"), "{e}");
    assert_eq!(store.snapshot().len(), 4);
}

/// Compressed tables are decompressed as a stream, within the decompressed-size limit.
#[test]
fn compressed_tables() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.csv.gz");
    let mut w = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut csv = String::from("id,name\n");
    for i in 0..1000 {
        csv.push_str(&format!("{i},name {i}\n"));
    }
    std::io::Write::write_all(&mut w, csv.as_bytes()).unwrap();
    std::fs::write(&p, w.finish().unwrap()).unwrap();
    assert_eq!(tabular_kind(&p), Some(TabularKind::Csv));
    let o = default_opts(Some("id"), "http://e/");
    let s = convert(open(&p, None).unwrap(), &o, &mut |_| Ok(())).unwrap();
    assert_eq!(s.triples, 2000);
    let e = convert(open(&p, Some(1000)).unwrap(), &o, &mut |_| Ok(())).unwrap_err();
    assert!(
        matches!(e, Error::BudgetExceeded(b) if b.kind == BudgetKind::DecompressedBytes),
        "{e}"
    );
    // the limit is on decompression: a plain file is read whole
    let plain = dir.path().join("t.csv");
    std::fs::write(&plain, &csv).unwrap();
    assert!(convert(open(&plain, Some(1000)).unwrap(), &o, &mut |_| Ok(())).is_ok());
}
