//! Characterization goldens for every result writer (item 828).
//!
//! Each body that the eager writers and the cursor writers produce for a fixed dataset is
//! compared with a file under `tests/goldens/writers/`, generated on the code before the
//! writers were unified. A cursor body must be the same at every batch size, with and without
//! decoding ahead. `manifest-*.txt` record which eager and cursor bodies are equal, and that
//! equality must hold too. Native JSON bodies keep their rows byte for byte and have the
//! timing, memory, execution-progress figures and plan of their `meta` normalized, since those change
//! per run and with the batch size.
//!
//! `UPDATE_GOLDENS=1` writes the files instead of comparing them.

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::jena_formats::{JenaFormat, ThriftResults};
use sparkles_core::sparql::results::{self, SolutionsFormat};
use sparkles_core::sparql::{
    CursorOptions, FallbackPolicy, Optimizations, QueryKind, QueryOptions, graph_cursor, query,
    select_cursor,
};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const DATA: &str = r#"
@prefix ex: <urn:ex:> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:s1 ex:p "plain", "quote\" back\\ nl\n tab\t cr\r end", "ctl\u0001\u001f\u007f",
    "]]> <&> ' \"x\"", "😀 é ü", "", "  lead and trail  ", "   ", "en"@en, "rtl"@ar--rtl,
    "42"^^xsd:integer, "-0.50"^^xsd:decimal, "1.0E3"^^xsd:double, "abc"^^xsd:integer,
    "+7"^^xsd:integer, "true"^^xsd:boolean, "2024-01-01"^^xsd:date, "x"^^ex:dt, ex:o, _:b1,
    <<( ex:a ex:b "t,\"q\"" )>> .
_:b1 ex:q ex:s1 .
ex:o ex:q "after\no" .
ex:s2 ex:p "LONG8", "LONG70" .
ex:g { ex:s1 ex:p "in g" . ex:s3 ex:q "g2" . }
"#;

fn data() -> String {
    let long = |n: usize| {
        let mut s = String::with_capacity(n);
        while s.len() < n {
            s.push_str("abc\"\\<&>é\n");
        }
        s
    };
    DATA.replace(
        "\"LONG8\"",
        &format!(
            "\"\"\"{}\"\"\"",
            long(9_000).replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
    .replace(
        "\"LONG70\"",
        &format!(
            "\"\"\"{}\"\"\"",
            long(70_000).replace('\\', "\\\\").replace('"', "\\\"")
        ),
    )
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        data().into_bytes(),
        RdfFormat::TriG,
        None,
    )])
    .expect("load the golden dataset");
    s
}

fn big_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..70_000)
        .map(|i| format!("<urn:b:{i:06}> <urn:p> \"v{i} \\\"q\\\" é\" .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load the large dataset");
    s
}

const SELECTS: &[(&str, &str)] = &[
    (
        "all",
        "SELECT ?s ?p ?o WHERE { ?s ?p ?o FILTER(!isLiteral(?o) || STRLEN(STR(?o)) < 100) } ORDER BY ?s ?p ?o",
    ),
    (
        "long",
        "SELECT ?s ?o WHERE { ?s ?p ?o FILTER(isLiteral(?o) && STRLEN(STR(?o)) >= 100) } ORDER BY STRLEN(?o)",
    ),
    (
        "optional",
        "SELECT ?s ?élan ?y WHERE { ?s <urn:ex:p> ?élan OPTIONAL { ?élan <urn:ex:q> ?y } } ORDER BY ?s ?élan ?y",
    ),
    ("none", "SELECT ?s WHERE { ?s <urn:ex:none> ?o }"),
    ("novars", "SELECT * WHERE { }"),
    (
        "graphs",
        "SELECT ?g ?s ?o WHERE { GRAPH ?g { ?s ?p ?o } } ORDER BY ?g ?s ?o",
    ),
];

const ASKS: &[(&str, &str)] = &[
    ("ask-true", "ASK { ?s ?p ?o }"),
    ("ask-false", "ASK { ?s <urn:ex:none> ?o }"),
];

const GRAPHS: &[(&str, &str)] = &[
    (
        "construct",
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o FILTER(!isLiteral(?o) || STRLEN(STR(?o)) < 100) }",
    ),
    (
        "construct-mixed",
        "CONSTRUCT { ?s ?p ?o . GRAPH ?g { ?s <urn:ex:in> ?o } } WHERE { GRAPH ?g { ?s ?p ?o } }",
    ),
    (
        "construct-none",
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s <urn:ex:none> ?o }",
    ),
    ("describe", "DESCRIBE <urn:ex:o>"),
];

const SOLUTION_FORMATS: &[(&str, SolutionsFormat)] = &[
    ("json", SolutionsFormat::Json),
    ("xml", SolutionsFormat::Xml),
    ("csv", SolutionsFormat::Csv),
    ("tsv", SolutionsFormat::Tsv),
    ("native", SolutionsFormat::Sparkles),
];

fn rdf_formats() -> Vec<(&'static str, RdfFormat)> {
    vec![
        ("nt", RdfFormat::NTriples),
        ("nq", RdfFormat::NQuads),
        ("ttl", RdfFormat::Turtle),
        ("trig", RdfFormat::TriG),
        ("rdfxml", RdfFormat::RdfXml),
        ("n3", RdfFormat::N3),
    ]
}

const JENA_FORMATS: &[(&str, JenaFormat)] = &[
    ("rt", JenaFormat::Thrift),
    ("pb", JenaFormat::Protobuf),
    ("rj", JenaFormat::RdfJson),
    ("trix", JenaFormat::TriX),
];

fn prefixes() -> BTreeMap<String, String> {
    BTreeMap::from([("ex".to_string(), "urn:ex:".to_string())])
}

fn options(decode_ahead: bool) -> QueryOptions {
    QueryOptions {
        optimizations: Some(Optimizations {
            decode_ahead,
            ..Optimizations::ALL
        }),
        ..Default::default()
    }
}

fn cursor_options(rows: usize) -> CursorOptions {
    CursorOptions {
        batch_rows: rows,
        fallback: FallbackPolicy::AllowMaterialization,
        ..Default::default()
    }
}

/// A body, or the error that ended it after the bytes it wrote.
fn body(write: impl FnOnce(&mut Vec<u8>) -> sparkles_core::Result<()>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Err(e) = write(&mut out) {
        out.extend_from_slice(format!("\n!ERROR: {e}\n").as_bytes());
    }
    out
}

/// Native JSON with the per-run figures of its `meta` replaced: the rows stay byte for byte.
fn normalize_native(bytes: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8(bytes).expect("native JSON is UTF-8");
    let Some(at) = text.rfind(",\"meta\":") else {
        return text.into_bytes();
    };
    let (rows, rest) = text.split_at(at);
    let meta_end = rest.rfind('}').expect("a closed document");
    let mut meta: serde_json::Value = match serde_json::from_str(&rest[8..meta_end]) {
        Ok(meta) => meta,
        Err(_) => return text.into_bytes(),
    };
    fn scrub(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    let key = k.to_ascii_lowercase();
                    if key.ends_with("ms")
                        || key.ends_with("bytes")
                        || key.contains("timing")
                        || key.contains("memory")
                        // how far execution ran past a `send` depends on the batch size, and
                        // a plan holds generated variable names and per-operator progress
                        || key == "rowsproduced"
                        || key == "plan"
                    {
                        *v = serde_json::Value::String("<n>".into());
                    } else {
                        scrub(v);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(scrub),
            _ => {}
        }
    }
    scrub(&mut meta);
    format!("{rows},\"meta\":{}{}", meta, &rest[meta_end..]).into_bytes()
}

struct Goldens {
    dir: PathBuf,
    update: bool,
    seen: BTreeSet<String>,
    failures: Vec<String>,
    /// for the manifest: (name, eager body equals cursor body)
    pairs: BTreeMap<String, bool>,
}

impl Goldens {
    fn new() -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/writers");
        let update = std::env::var_os("UPDATE_GOLDENS").is_some();
        if update {
            std::fs::create_dir_all(&dir).unwrap();
        }
        Goldens {
            dir,
            update,
            seen: BTreeSet::new(),
            failures: Vec::new(),
            pairs: BTreeMap::new(),
        }
    }

    fn check(&mut self, name: &str, bytes: &[u8]) {
        assert!(
            self.seen.insert(name.to_string()),
            "duplicate golden {name}"
        );
        let (file, content) = if bytes.len() > 64 << 10 {
            use sha2::Digest;
            let digest = sha2::Sha256::digest(bytes);
            let hex = digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            (
                format!("{name}.sha256"),
                format!("{} {hex}\n", bytes.len()).into_bytes(),
            )
        } else {
            (name.to_string(), bytes.to_vec())
        };
        let path = self.dir.join(&file);
        if self.update {
            std::fs::write(&path, &content).unwrap();
            return;
        }
        match std::fs::read(&path) {
            Ok(expected) if expected == content => {}
            Ok(_) => self.failures.push(format!("{file} differs")),
            Err(e) => self.failures.push(format!("{file}: {e}")),
        }
    }

    /// Record a mismatch when `got`, the body `what` wrote, differs from the body that the
    /// golden `name` checked.
    fn same(&mut self, name: &str, what: &str, expected: &[u8], got: &[u8]) {
        if expected != got {
            self.failures
                .push(format!("{name}: the {what} body differs from the golden's"));
        }
    }

    fn pair(&mut self, name: &str, eager: &[u8], cursor: &[u8]) {
        self.pairs.insert(name.to_string(), eager == cursor);
    }

    /// Check the manifest `file` of eager and cursor equality, one per test since the
    /// tests run in parallel, and report every mismatch.
    fn finish(mut self, file: &str) {
        let manifest = self
            .pairs
            .iter()
            .map(|(name, equal)| format!("{name} {}\n", if *equal { "equal" } else { "differ" }))
            .collect::<String>();
        let path = self.dir.join(file);
        if self.update {
            std::fs::write(&path, manifest).unwrap();
        } else {
            let expected = std::fs::read_to_string(&path).unwrap_or_default();
            let expected: BTreeMap<&str, &str> =
                expected.lines().filter_map(|l| l.split_once(' ')).collect();
            for (name, equal) in &self.pairs {
                // a pair characterized as equal must stay equal; a pair that became equal
                // is reported so that the manifest is updated by a reviewed change
                let now = if *equal { "equal" } else { "differ" };
                if expected.get(name.as_str()) != Some(&now) {
                    self.failures.push(format!(
                        "manifest: {name} is {now}, expected {:?}",
                        expected.get(name.as_str())
                    ));
                }
            }
        }
        assert!(
            self.failures.is_empty(),
            "{} golden mismatches:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

fn eager_thrift(r: &sparkles_core::sparql::QueryResult, send: Option<usize>) -> Vec<u8> {
    // the server's `write_thrift_results`, which phase 4 moves into the library
    body(|w| {
        let mut out = ThriftResults::new(w, r.variables())?;
        let n = send.map_or(r.len(), |s| s.min(r.len()));
        for i in 0..n {
            let row = r.row(i)?;
            out.row(row.values())?;
        }
        out.finish()?;
        Ok(())
    })
}

/// SPARQL Results Thrift through the library's writer, from a complete result or a cursor.
/// The rows counted must be the rows written.
fn thrift_body(source: results::Source<'_>, send: Option<usize>, expect_rows: usize) -> Vec<u8> {
    let rows = results::RowCount::new();
    let bytes = body(|w| {
        let options = results::WriteOptions {
            send,
            ..results::WriteOptions::new(results::Format::ResultsThrift, &rows)
        };
        results::write(source, options, w).map(drop)
    });
    assert_eq!(rows.get(), expect_rows as u64, "Thrift rows counted");
    bytes
}

/// The cursor body of `write`, asserted equal at every batch size and with and without
/// decoding ahead.
fn cursor_body(
    name: &str,
    mut write: impl FnMut(&QueryOptions, &CursorOptions) -> Vec<u8>,
    normalize: bool,
) -> Vec<u8> {
    let mut first: Option<Vec<u8>> = None;
    for rows in [1, 2, 3, 4096] {
        for ahead in [true, false] {
            let mut bytes = write(&options(ahead), &cursor_options(rows));
            if normalize {
                bytes = normalize_native(bytes);
            }
            match &first {
                None => first = Some(bytes),
                Some(first) => assert!(
                    *first == bytes,
                    "{name}: the cursor body at batch size {rows}, decode ahead {ahead}, differs from batch size 1:\n{}\n{}",
                    String::from_utf8_lossy(first),
                    String::from_utf8_lossy(&bytes)
                ),
            }
        }
    }
    first.unwrap()
}

#[test]
fn every_writer_matches_its_golden() {
    let s = store();
    let mut g = Goldens::new();
    let sends: [(&str, Option<usize>); 2] = [("", None), (".send2", Some(2))];
    for &(qname, q) in SELECTS {
        for &(suffix, send) in &sends {
            let eager =
                query(s.snapshot(), q, &options(true)).unwrap_or_else(|e| panic!("{q}: {e}"));
            assert_eq!(eager.kind, QueryKind::Select);
            for &(fname, fmt) in SOLUTION_FORMATS {
                let native = fmt == SolutionsFormat::Sparkles;
                let mut e = body(|w| results::write_solutions(&eager, fmt, w, send));
                if native {
                    e = normalize_native(e);
                }
                let name = format!("{qname}{suffix}.{fname}");
                let c = cursor_body(
                    &name,
                    |o, c| {
                        let mut cursor = select_cursor(s.snapshot(), q, o, c).unwrap();
                        body(|w| {
                            results::write_cursor_solutions(&mut cursor, fmt, w, send).map(drop)
                        })
                    },
                    native,
                );
                g.check(&format!("{name}.eager"), &e);
                g.check(&format!("{name}.cursor"), &c);
                g.pair(&name, &e, &c);
            }
            let name = format!("{qname}{suffix}.thrift");
            let n = send.map_or(eager.len(), |s| s.min(eager.len()));
            let e = eager_thrift(&eager, send);
            g.check(&format!("{name}.eager"), &e);
            // the library's writer, from the result and from a cursor at every batch size,
            // writes the bytes the server wrote before it
            let lib = thrift_body(results::Source::Result(&eager), send, n);
            g.same(&name, "library eager", &e, &lib);
            let c = cursor_body(
                &name,
                |o, c| {
                    let mut cursor = select_cursor(s.snapshot(), q, o, c).unwrap();
                    thrift_body(results::Source::Select(&mut cursor), send, n)
                },
                false,
            );
            g.same(&name, "cursor", &e, &c);
        }
    }
    for &(qname, q) in ASKS {
        let eager = query(s.snapshot(), q, &options(true)).unwrap();
        assert_eq!(eager.kind, QueryKind::Ask);
        for &(fname, fmt) in SOLUTION_FORMATS {
            let mut e = body(|w| results::write_solutions(&eager, fmt, w, None));
            if fmt == SolutionsFormat::Sparkles {
                e = normalize_native(e);
            }
            g.check(&format!("{qname}.{fname}.eager"), &e);
        }
    }
    let prefixes = prefixes();
    let prefix_list = prefixes
        .iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect::<Vec<_>>();
    for &(qname, q) in GRAPHS {
        for &(suffix, send) in &sends {
            let eager =
                query(s.snapshot(), q, &options(true)).unwrap_or_else(|e| panic!("{q}: {e}"));
            for (fname, fmt) in rdf_formats() {
                let name = format!("{qname}{suffix}.{fname}");
                // the eager graph writers have no `send`
                let e = body(|w| results::write_graph(&eager, fmt, &prefixes, w));
                let c = cursor_body(
                    &name,
                    |o, c| {
                        let mut cursor = graph_cursor(s.snapshot(), q, o, c).unwrap();
                        body(|w| {
                            sparkles_core::sparql::cursor::graph::write_cursor_graph(
                                &mut cursor,
                                fmt,
                                w,
                                send,
                                &prefix_list,
                            )
                            .map(drop)
                        })
                    },
                    false,
                );
                g.check(&format!("{name}.eager"), &e);
                g.check(&format!("{name}.cursor"), &c);
                g.pair(&name, &e, &c);
            }
            for &(fname, fmt) in JENA_FORMATS {
                let name = format!("{qname}{suffix}.{fname}");
                let e = body(|w| results::write_jena_graph(&eager, fmt, w));
                let c = cursor_body(
                    &name,
                    |o, c| {
                        let mut cursor = graph_cursor(s.snapshot(), q, o, c).unwrap();
                        body(|w| {
                            results::write_cursor_jena_graph(&mut cursor, fmt, w, send).map(drop)
                        })
                    },
                    false,
                );
                g.check(&format!("{name}.eager"), &e);
                g.check(&format!("{name}.cursor"), &c);
                g.pair(&name, &e, &c);
            }
            let name = format!("{qname}{suffix}.native");
            let e = normalize_native(body(|w| results::write_native_json(&eager, w, send, None)));
            let c = cursor_body(
                &name,
                |o, c| {
                    let mut cursor = graph_cursor(s.snapshot(), q, o, c).unwrap();
                    body(|w| {
                        results::write_cursor_graph_native_json(&mut cursor, w, send, None)
                            .map(drop)
                    })
                },
                true,
            );
            g.check(&format!("{name}.eager"), &e);
            g.check(&format!("{name}.cursor"), &c);
            g.pair(&name, &e, &c);
        }
    }
    g.finish("manifest-small.txt");
}

#[test]
fn large_answers_match_their_golden() {
    // more rows than one eager decode chunk (65,536), so the eager writer takes its chunked
    // path with the next chunk decoded ahead
    let s = big_store();
    let mut g = Goldens::new();
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?s";
    for &(suffix, send) in &[
        ("", None),
        (".send65537", Some(65_537)),
        (".send65535", Some(65_535)),
    ] {
        let eager = query(s.snapshot(), q, &options(true)).unwrap();
        for &(fname, fmt) in SOLUTION_FORMATS {
            let native = fmt == SolutionsFormat::Sparkles;
            let mut e = body(|w| results::write_solutions(&eager, fmt, w, send));
            let mut cursor =
                select_cursor(s.snapshot(), q, &options(true), &cursor_options(4096)).unwrap();
            let mut c =
                body(|w| results::write_cursor_solutions(&mut cursor, fmt, w, send).map(drop));
            if native {
                e = normalize_native(e);
                c = normalize_native(c);
            }
            let name = format!("large{suffix}.{fname}");
            g.check(&format!("{name}.eager"), &e);
            g.check(&format!("{name}.cursor"), &c);
            g.pair(&name, &e, &c);
        }
        let n = send.map_or(eager.len(), |s| s.min(eager.len()));
        let name = format!("large{suffix}.thrift");
        let e = eager_thrift(&eager, send);
        g.check(&format!("{name}.eager"), &e);
        let lib = thrift_body(results::Source::Result(&eager), send, n);
        g.same(&name, "library eager", &e, &lib);
        let mut cursor =
            select_cursor(s.snapshot(), q, &options(true), &cursor_options(4096)).unwrap();
        let c = thrift_body(results::Source::Select(&mut cursor), send, n);
        g.same(&name, "cursor", &e, &c);
    }
    g.finish("manifest-large.txt");
}
