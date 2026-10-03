//! The file-level tools run as the real binary (spec G05): `convert`/`riot`, `qparse`,
//! `uparse`, `compare`, `iri`, `langtag`, `rset`, `load --check`, and `rsparql` and
//! `rupdate` against a running server. Exit statuses are part of each command's contract.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("SPARKLES_SERVER")
        .output()
        .unwrap()
}

fn run_stdin(dir: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut c = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input).unwrap();
    c.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const TTL: &str = r#"@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice foaf:name "Alice"@en ; foaf:knows [ foaf:name "Bob" ] .
"#;

const TRIG: &str =
    "@prefix ex: <http://example.org/> .\nex:g { ex:s ex:p ex:o . }\nex:s2 ex:p ex:o2 .\n";

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.ttl"), TTL).unwrap();
    std::fs::write(d.path().join("g.trig"), TRIG).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(TTL.as_bytes()).unwrap();
    std::fs::write(d.path().join("a.ttl.gz"), gz.finish().unwrap()).unwrap();
    std::fs::write(
        d.path().join("bad.nt"),
        "<http://e/s> <http://e/p> \"a\" .\n<http://e/s> <http://e/p> .\n<http://e/s> <http://e/p> \"b\" .\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("odd.ttl"),
        "<HTTP://example.org/a> <http://example.org/p> \"x\"@zh-yue .\n",
    )
    .unwrap();
    d
}

#[test]
fn convert_streams_between_syntaxes() {
    let d = setup();
    let dir = d.path();
    // N-Quads by default; gzip detected
    let o = run(dir, &["convert", "a.ttl.gz"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o).lines().count(), 3, "{}", out(&o));
    assert!(
        out(&o)
            .contains("<http://example.org/alice> <http://xmlns.com/foaf/0.1/name> \"Alice\"@en .")
    );
    // Turtle keeps the input's prefixes; `riot` is an alias; standard input with --syntax
    let o = run_stdin(
        dir,
        &["riot", "--syntax", "ttl", "--output", "turtle"],
        TTL.as_bytes(),
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("@prefix foaf:") && out(&o).contains("ex:alice"),
        "{}",
        out(&o)
    );
    // named graphs: dropped with a warning for a triple syntax, or merged
    let o = run(dir, &["convert", "g.trig", "--out", "nt"]);
    assert_eq!(out(&o).lines().count(), 1);
    assert!(err(&o).contains("dropped 1 quad"), "{}", err(&o));
    let o = run(dir, &["convert", "g.trig", "--out", "nt", "--merge"]);
    assert_eq!(out(&o).lines().count(), 2);
    let o = run(dir, &["convert", "g.trig"]);
    assert!(out(&o).contains("<http://example.org/g> ."), "{}", out(&o));
    // compressed output
    let o = run(dir, &["convert", "a.ttl", "--compress"]);
    assert_eq!(&o.stdout[..2], &[0x1f, 0x8b]);
    // unknown syntax
    let o = run(dir, &["convert", "a.ttl", "--output", "csv"]);
    assert!(!o.status.success());
}

#[test]
fn convert_counts_and_validates() {
    let d = setup();
    let dir = d.path();
    let o = run(dir, &["convert", "--count", "a.ttl", "g.trig"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "a.ttl: 3 triples\ng.trig: 2 quads\ntotal: 5\n");
    // a syntax error has its position and exit status 1
    let o = run(dir, &["convert", "--validate", "bad.nt"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("bad.nt:2:27: "), "{}", err(&o));
    let o = run(dir, &["convert", "--count", "bad.nt"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("bad.nt: 2 triples"), "{}", out(&o));
    // warnings: printed with --check, fatal with --strict and --validate
    let o = run(dir, &["convert", "--check", "odd.ttl"]);
    assert!(o.status.success());
    assert!(
        err(&o).contains("[scheme-case]") && err(&o).contains("[extlang]"),
        "{}",
        err(&o)
    );
    assert_eq!(out(&o).lines().count(), 1);
    let o = run(dir, &["convert", "--validate", "odd.ttl"]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(dir, &["convert", "--validate", "a.ttl"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "");
}

/// Directories: merged into one output file, or mirrored into an output directory in
/// parallel; files without a known extension by their content, unknown ones skipped.
#[test]
fn convert_directories() {
    let d = setup();
    let dir = d.path();
    let tree = dir.join("tree");
    std::fs::create_dir_all(tree.join("sub/deeper")).unwrap();
    std::fs::write(tree.join("a.ttl"), TTL).unwrap();
    std::fs::write(tree.join("sub/g.trig"), TRIG).unwrap();
    // no extension: N-Triples by its lines
    std::fs::write(
        tree.join("sub/deeper/plain"),
        "<http://e/s> <http://e/p> \"x\" .\n",
    )
    .unwrap();
    std::fs::write(tree.join("people.csv"), "id,name\n7,Ann\n").unwrap();
    std::fs::write(tree.join("README.md"), "Some notes.\n").unwrap();

    // a directory needs --recursive
    let o = run(dir, &["convert", "tree"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--recursive"), "{}", err(&o));

    // merged into one file, its syntax and compression from the extension
    let base = ["--base", "http://e/t/"];
    let o = run(
        dir,
        &[&["convert", "-r", "tree", "-o", "all.nq.gz"][..], &base].concat(),
    );
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    assert!(e.contains("warning: skipping tree/README.md"), "{e}");
    assert!(e.contains("tree/people.csv: 2 triples"), "{e}");
    assert!(e.contains("4 files, 8 statements"), "{e}");
    let o = run(dir, &["convert", "--count", "all.nq.gz"]);
    assert_eq!(out(&o), "all.nq.gz: 8 quads\n", "{}", err(&o));

    // --include and --exclude
    let o = run(
        dir,
        &[
            "convert",
            "-r",
            "tree",
            "--include",
            "*.ttl",
            "--include",
            "**/plain",
            "--count",
        ],
    );
    assert_eq!(
        out(&o),
        "tree/a.ttl: 3 triples\ntree/sub/deeper/plain: 1 triples\ntotal: 4\n",
        "{}",
        err(&o)
    );
    let o = run(
        dir,
        &[
            &[
                "convert",
                "-r",
                "tree",
                "--exclude",
                "sub/**",
                "--exclude",
                "*.md",
                "--count",
            ][..],
            &base,
        ]
        .concat(),
    );
    assert!(out(&o).ends_with("total: 5\n"), "{}{}", out(&o), err(&o));

    // mirrored into a directory
    let o = run(
        dir,
        &[
            &[
                "convert",
                "-r",
                "tree",
                "--out-dir",
                "out",
                "--format",
                "nt",
                "--merge",
                "-j",
                "2",
            ][..],
            &base,
        ]
        .concat(),
    );
    assert!(o.status.success(), "{}", err(&o));
    for f in [
        "out/a.nt",
        "out/sub/g.nt",
        "out/sub/deeper/plain.nt",
        "out/people.nt",
    ] {
        assert!(dir.join(f).is_file(), "{f}: {}", err(&o));
    }
    let g = std::fs::read_to_string(dir.join("out/sub/g.nt")).unwrap();
    assert_eq!(g.lines().count(), 2, "{g}");
    // existing files need --overwrite
    let only_ttl = [
        "convert",
        "-r",
        "tree",
        "--include",
        "*.ttl",
        "--out-dir",
        "out",
        "--format",
        "nt",
    ];
    let o = run(dir, &only_ttl);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("exists (use --overwrite)"), "{}", err(&o));
    let o = run(dir, &[&only_ttl[..], &["--overwrite"]].concat());
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        err(&o).contains("tree/a.ttl -> out/a.nt: 3 triples"),
        "{}",
        err(&o)
    );

    // a file that fails: its position, exit status 1, the others still converted
    std::fs::write(tree.join("broken.nt"), "<http://e/s> <http://e/p> .\n").unwrap();
    let o = run(
        dir,
        &[
            "convert",
            "-r",
            "tree",
            "--include",
            "*.nt",
            "--include",
            "*.ttl",
            "--out-dir",
            "out2",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("tree/broken.nt:1:"), "{e}");
    assert!(e.contains("2 files, 3 statements"), "{e}");
    assert!(e.contains("1 failed"), "{e}");
    assert!(dir.join("out2/a.nq").is_file());
    assert!(!dir.join("out2/broken.nq").exists());
}

/// Content sniffing: standard input and files without a known extension.
#[test]
fn convert_sniffs_the_syntax() {
    let d = setup();
    let dir = d.path();
    let cases: &[(&str, &[u8], &str)] = &[
        ("t1", TTL.as_bytes(), "3 triples"),
        ("t2", TRIG.as_bytes(), "2 quads"),
        (
            "t3",
            b"<http://e/s> <http://e/p> <http://e/o> <http://e/g> .\n",
            "1 quads",
        ),
        (
            "t4",
            b"<?xml version=\"1.0\"?>\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
              <rdf:Description rdf:about=\"http://e/s\"><rdf:value>1</rdf:value></rdf:Description></rdf:RDF>",
            "1 triples",
        ),
        ("t5", br#"{"@id": "http://e/s", "http://e/p": "x"}"#, "1 quads"),
        (
            "t6",
            br#"{"http://e/s": {"http://e/p": [{"type": "literal", "value": "x"}]}}"#,
            "1 triples",
        ),
        ("t7", b"id\tname\n7\tAnn\n", "2 triples"),
    ];
    for (name, body, want) in cases {
        std::fs::write(dir.join(name), body).unwrap();
        let o = run(dir, &["convert", "--count", "--base", "http://e/t/", name]);
        assert_eq!(out(&o), format!("{name}: {want}\n"), "{name}: {}", err(&o));
        let o = run_stdin(dir, &["convert", "--count", "--base", "http://e/t/"], body);
        assert_eq!(out(&o), format!("stdin: {want}\n"), "{name}: {}", err(&o));
    }
    // TriX, RDF Thrift and RDF Protobuf written by convert, read back without --syntax
    for fmt in ["trix", "rt", "rpb"] {
        let o = run(dir, &["convert", "g.trig", "--output", fmt]);
        assert!(o.status.success(), "{}", err(&o));
        let o = run_stdin(dir, &["convert", "--count"], &o.stdout);
        assert_eq!(out(&o), "stdin: 2 quads\n", "{fmt}: {}", err(&o));
    }
    // compressed standard input is decompressed before it is sniffed
    let o = run(dir, &["convert", "a.ttl.gz", "--output", "ttl"]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&o.stdout).unwrap();
    let o = run_stdin(dir, &["convert", "--count"], &gz.finish().unwrap());
    assert_eq!(out(&o), "stdin: 3 triples\n", "{}", err(&o));
    // an extension or --syntax wins over the content
    std::fs::write(dir.join("lies.nt"), TTL).unwrap();
    let o = run(dir, &["convert", "--count", "lies.nt"]);
    assert_eq!(o.status.code(), Some(1));
    let o = run_stdin(
        dir,
        &["convert", "--count", "--syntax", "nt"],
        TTL.as_bytes(),
    );
    assert_eq!(o.status.code(), Some(1));
    // ambiguous and unknown content
    std::fs::write(dir.join("empty"), "{}").unwrap();
    let o = run(dir, &["convert", "empty"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("JSON-LD or RDF/JSON"), "{}", err(&o));
    std::fs::write(dir.join("notes"), "hello world\n").unwrap();
    let o = run(dir, &["convert", "notes"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("unknown RDF syntax"), "{}", err(&o));
}

/// Jena's syntaxes in `convert` and `load`: TriG to TriX and back, TriX to RDF Thrift
/// and back, standard input, and a load of a compressed TriX file.
#[test]
fn convert_and_load_take_jena_syntaxes() {
    let d = setup();
    let dir = d.path();
    let o = run(dir, &["convert", "g.trig", "--output", "trix"]);
    assert!(o.status.success(), "{}", err(&o));
    let trix = out(&o);
    assert!(
        trix.starts_with("<trix xmlns=\"http://www.w3.org/2004/03/trix/trix-1/\">"),
        "{trix}"
    );
    std::fs::write(dir.join("g.trix"), &trix).unwrap();
    let o = run(dir, &["compare", "g.trig", "g.trix"]);
    assert!(o.status.success(), "{}", err(&o));
    let o = run(dir, &["convert", "g.trix", "--output", "rt"]);
    assert!(o.status.success(), "{}", err(&o));
    std::fs::write(dir.join("g.rt"), &o.stdout).unwrap();
    let o = run(dir, &["compare", "g.rt", "g.trig"]);
    assert!(o.status.success(), "{}", err(&o));
    let o = run_stdin(
        dir,
        &["convert", "--syntax", "trix", "--count"],
        trix.as_bytes(),
    );
    assert_eq!(out(&o), "stdin: 2 quads\n", "{}", err(&o));
    // a broken document is an error with the reader's message
    let o = run_stdin(dir, &["convert", "--syntax", "trix"], b"<trix><graph>");
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("TriX:"), "{}", err(&o));

    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(trix.as_bytes()).unwrap();
    std::fs::write(dir.join("g.trix.gz"), gz.finish().unwrap()).unwrap();
    let o = run(dir, &["load", "--loc", "db", "g.trix.gz"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("loaded 2 quads"), "{}", err(&o));
    // CONSTRUCT results in TriX
    let o = run(
        dir,
        &[
            "query",
            "--loc",
            "db",
            "--results",
            "trix",
            "CONSTRUCT WHERE { ?s ?p ?o }",
        ],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("<uri>http://example.org/o2</uri>"),
        "{}",
        out(&o)
    );
}

#[test]
fn load_checks_terms_first() {
    let d = setup();
    let dir = d.path();
    let o = run(
        dir,
        &["load", "--loc", "db", "--check", "--strict", "odd.ttl"],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("nothing was loaded"), "{}", err(&o));
    assert!(!dir.join("db").exists());
    let o = run(dir, &["load", "--loc", "db", "--check", "odd.ttl", "a.ttl"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(err(&o).contains("[scheme-case]"), "{}", err(&o));
    assert!(err(&o).contains("loaded 4 quads"), "{}", err(&o));
}

#[test]
fn qparse_and_uparse() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let q =
        "PREFIX ex: <http://example.org/> SELECT ?s (COUNT(*) AS ?n) { ?s ex:p ?o } GROUP BY ?s";
    let o = run(dir, &["qparse", q]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("(COUNT(*) AS ?n)") && out(&o).contains("ex:p"),
        "{}",
        out(&o)
    );
    let o = run(dir, &["qparse", "--print", "algebra", q]);
    assert!(out(&o).contains("(group (?s)"), "{}", out(&o));
    assert!(out(&o).contains("((count) ?.0)"), "{}", out(&o));
    let o = run(dir, &["qparse", "--print", "op,plan", q]);
    assert!(out(&o).contains("(bgp"), "{}", out(&o));
    assert!(out(&o).contains("GroupBy"), "{}", out(&o));
    let o = run(dir, &["qparse", "SELECT * { ?s ?p }"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("1:"), "{}", err(&o));
    let o = run_stdin(
        dir,
        &["uparse", "--print", "algebra"],
        b"INSERT DATA { <http://e/a> <http://e/b> 1 }",
    );
    assert!(out(&o).contains("(insertData"), "{}", out(&o));
    let o = run(dir, &["uparse", "INSERT { ?s }"]);
    assert_eq!(o.status.code(), Some(1));
    // Jena ARQ's syntax by default, strict SPARQL with --syntax sparql (spec G06)
    let arq = "SELECT * { ?s <http://e/p>{2} ?o LATERAL { ?o <http://e/q> ?x } }";
    let o = run(dir, &["qparse", "--print", "algebra", arq]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("(pathN 2 ") && out(&o).contains("(lateral"),
        "{}",
        out(&o)
    );
    let o = run(dir, &["qparse", "--syntax", "sparql", arq]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("1:"), "{}", err(&o));
    let o = run(
        dir,
        &["qparse", "--syntax", "SPARQL_11", "SELECT * { ?s ?p ?o }"],
    );
    assert!(o.status.success(), "{}", err(&o));
}

#[test]
fn compare_exit_statuses() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(
        dir.join("a.nt"),
        "_:x <http://e/p> _:y .\n_:y <http://e/q> \"1\" .\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b.ttl"),
        "@prefix e: <http://e/> .\n[] e:p [ e:q \"1\" ] .\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("c.nt"),
        "_:x <http://e/p> _:y .\n_:y <http://e/q> \"2\" .\n",
    )
    .unwrap();
    let o = run(dir, &["compare", "a.nt", "b.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("isomorphic"));
    let o = run(dir, &["rdfdiff", "a.nt", "c.nt"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        out(&o).contains("< _:c14n0 <http://e/q> \"1\" ."),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("> _:c14n2 <http://e/q> \"2\" ."),
        "{}",
        out(&o)
    );
    let o = run(dir, &["rdfcompare", "-q", "a.nt", "c.nt"]);
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(1), ""));
    let o = run(dir, &["compare", "a.nt", "missing.nt"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn iri_and_langtag() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let o = run(dir, &["iri", "<http://Example.org:80/a/../b>"]);
    assert!(o.status.success());
    assert!(
        out(&o).contains("normalized:   <http://example.org/b>"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("[default-port]"), "{}", out(&o));
    let o = run(dir, &["iri", "--strict", "http://Example.org/"]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(dir, &["iri", "http://example.org/a b"]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(
        dir,
        &[
            "iri",
            "--format",
            "json",
            "../x",
            "--base",
            "http://example.org/a/b",
        ],
    );
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j[0]["resolved"], "http://example.org/x");
    let o = run(dir, &["langtag", "zh-yue-hk", "en--ltr"]);
    assert!(o.status.success(), "{}", out(&o));
    assert!(out(&o).contains("canonical:    zh-yue-HK"), "{}", out(&o));
    assert!(out(&o).contains("[extlang]"), "{}", out(&o));
    let o = run(dir, &["langtag", "en-"]);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn rset_converts() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(
        dir.join("r.srj"),
        r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"http://e/a"}}]}}"#,
    )
    .unwrap();
    let o = run(dir, &["rset", "r.srj"]);
    assert!(out(&o).contains("| <http://e/a> |"), "{}", out(&o));
    let o = run(dir, &["rset", "r.srj", "--results", "tsv"]);
    assert_eq!(out(&o), "?s\n<http://e/a>\n");
}

/// A free port in 5380–5399.
#[cfg(feature = "auth")]
fn port() -> u16 {
    (5380..5400)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("a free port in 5380-5399")
}

#[cfg(feature = "auth")]
#[test]
fn rsparql_and_rupdate_against_an_endpoint() {
    use std::time::{Duration, Instant};
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let port = port();
    let mut child = Command::new(BIN)
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--mem",
            "ds",
            "--idle-release-ms",
            "0",
        ])
        .arg("--data")
        .arg(dir.join("data"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let up = format!("{base}/ds/update");
    let q = format!("{base}/ds/sparql");
    let o = run(
        dir,
        &[
            "rupdate",
            "--service",
            &up,
            "INSERT DATA { <http://e/a> <http://e/p> \"x\"@en }",
        ],
    );
    assert!(o.status.success(), "{}", err(&o));
    let o = run(
        dir,
        &["rsparql", "--service", &q, "SELECT ?s ?o { ?s ?p ?o }"],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("| <http://e/a> | \"x\"@en |"),
        "{}",
        out(&o)
    );
    let o = run(
        dir,
        &[
            "rsparql",
            "--service",
            &q,
            "--results",
            "csv",
            "--post",
            "SELECT ?s { ?s ?p ?o }",
        ],
    );
    assert_eq!(out(&o), "s\r\nhttp://e/a\r\n");
    let o = run(
        dir,
        &["rsparql", "--service", &q, "CONSTRUCT WHERE { ?s ?p ?o }"],
    );
    assert!(out(&o).contains("<http://e/a>"), "{}", out(&o));
    let o = run(dir, &["rsparql", "--service", &q, "ASK { ?s ?p 1 }"]);
    assert_eq!(out(&o), "no\n");
    let o = run(dir, &["rsparql", "--service", &q, "SELECT * { ?s ?p }"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("400"), "{}", err(&o));
    // credentials are never sent over plain http to another host
    let o = run(
        dir,
        &[
            "rsparql",
            "--service",
            "http://example.org/sparql",
            "--user",
            "a:b",
            "ASK {}",
        ],
    );
    assert!(err(&o).contains("plain http"), "{}", err(&o));
    let _ = child.kill();
    let _ = child.wait();
}
