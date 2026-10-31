//! Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON load through the loader like the W3C
//! syntaxes, with no rewritten copy: `Source` reads them, SPARQL `LOAD` reads them from
//! files and from HTTP, byte budgets and blank node scopes hold, and malformed input
//! fails with a message that names the source, the syntax and a position.

use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Term};
use sparkles_core::codec::{Codec, Level};
use sparkles_core::io::{RdfFormat, RdfSyntax, Source};
use sparkles_core::jena_formats::{JenaFormat, RdfWriter};
use sparkles_core::outbound::OutboundPolicy;
use sparkles_core::sparql::{FileLoads, QueryOptions};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{Budget, BudgetKind, Error};
use std::io::{Read, Write};
use std::net::TcpListener;

const ALL: [JenaFormat; 4] = [
    JenaFormat::TriX,
    JenaFormat::Thrift,
    JenaFormat::Protobuf,
    JenaFormat::RdfJson,
];

fn n(s: &str) -> NamedNode {
    NamedNode::new(format!("http://ex.org/{s}")).unwrap()
}

/// Quads for `format`: RDF/JSON holds one graph, the others a named graph as well.
fn fixture(format: JenaFormat) -> Vec<Quad> {
    let b = BlankNode::new("b0").unwrap();
    let mut quads = vec![
        Quad::new(
            n("a"),
            n("p"),
            Literal::new_simple_literal("one"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            n("a"),
            n("p"),
            Literal::new_language_tagged_literal("deux", "fr").unwrap(),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            n("a"),
            n("q"),
            Literal::new_typed_literal("3", oxrdf::vocab::xsd::INTEGER),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            b.clone(),
            n("p"),
            Term::from(n("a")),
            GraphName::DefaultGraph,
        ),
        Quad::new(b, n("q"), Term::from(n("c")), GraphName::DefaultGraph),
    ];
    if format.quads() {
        quads.push(Quad::new(
            n("c"),
            n("p"),
            Term::from(n("a")),
            GraphName::NamedNode(n("g")),
        ));
    }
    quads
}

fn encode(format: JenaFormat, quads: &[Quad]) -> Vec<u8> {
    let mut w = RdfWriter::new(format, Vec::new());
    for q in quads {
        w.quad(q).unwrap();
    }
    w.finish().unwrap()
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Codec::Gzip.writer(&mut out, Some(Level(1)), 1).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
    out
}

/// N-Quads of the store with blank node labels renamed, sorted: two isomorphic stores
/// of one blank node per label agree.
fn canonical(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    store.dump_nquads(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let mut labels = std::collections::BTreeMap::new();
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            l.split(' ')
                .map(|t| match t.strip_prefix("_:") {
                    Some(label) => {
                        let next = labels.len();
                        format!("_:b{}", labels.entry(label.to_owned()).or_insert(next))
                    }
                    None => t.to_owned(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    lines.sort();
    lines
}

fn expected(quads: &[Quad]) -> Vec<String> {
    let reference = Store::in_memory(StoreOptions::default());
    let mut nq = String::new();
    for q in quads {
        nq += &format!("{q} .\n");
    }
    reference
        .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
        .unwrap();
    canonical(&reference)
}

fn nonempty() -> Store {
    // a store that holds a quad loads through a transaction, and an empty one through the
    // bulk loader: both paths read the syntax
    let store = Store::in_memory(StoreOptions::default());
    let seed = "<http://ex.org/seed> <http://ex.org/p> <http://ex.org/o> .\n";
    store
        .load(&[Source::from_bytes(
            seed.as_bytes().to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    store
}

#[test]
fn every_syntax_loads_as_a_source_through_both_loaders() {
    for format in ALL {
        let quads = fixture(format);
        let want = expected(&quads);
        for compressed in [false, true] {
            let bytes = encode(format, &quads);
            let bytes = if compressed { gzip(&bytes) } else { bytes };
            // bulk loader, from bytes
            let store = Store::in_memory(StoreOptions::default());
            let src = Source::from_bytes(bytes.clone(), format, None);
            assert_eq!(
                store.load(&[src]).unwrap(),
                quads.len() as u64,
                "{format:?}"
            );
            assert_eq!(canonical(&store), want, "{format:?} gz={compressed}");
            // transaction path, from a file by its extension
            let dir = tempfile::tempdir().unwrap();
            let name = format!(
                "d.{}{}",
                format.file_extension(),
                if compressed { ".gz" } else { "" }
            );
            let path = dir.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            let store = nonempty();
            let src = Source::from_path(&path, None).unwrap();
            assert_eq!(src.format, RdfSyntax::Jena(format));
            store.load(&[src]).unwrap();
            let mut with_seed = want.clone();
            with_seed.push("<http://ex.org/seed> <http://ex.org/p> <http://ex.org/o> .".into());
            with_seed.sort();
            assert_eq!(
                canonical(&store),
                with_seed,
                "{format:?} gz={compressed} file"
            );
        }
    }
}

#[test]
fn triples_of_a_jena_source_go_to_its_graph_and_named_graphs_stay() {
    for format in ALL {
        let quads = fixture(format);
        let store = Store::in_memory(StoreOptions::default());
        let src = Source::from_bytes(encode(format, &quads), format, Some(n("target")));
        store.load(&[src]).unwrap();
        let text = canonical(&store).join("\n");
        assert!(text.contains("<http://ex.org/target> ."), "{format:?}");
        assert!(
            !text.contains("\"one\" ."),
            "{format:?} left a default-graph triple"
        );
        assert_eq!(
            text.contains("<http://ex.org/g> ."),
            format.quads(),
            "{format:?}"
        );
    }
}

#[test]
fn a_single_graph_source_refuses_a_named_graph() {
    for format in ALL.into_iter().filter(|f| f.quads()) {
        let bytes = encode(format, &fixture(format));
        let mut src = Source::from_bytes(bytes.clone(), format, None);
        src.triples_only = true;
        let store = Store::in_memory(StoreOptions::default());
        let e = store.load(&[src]).unwrap_err();
        assert!(
            matches!(&e, Error::RdfParse(m) if m.contains("holds a quad in graph") && m.contains(format.name())),
            "{format:?}: {e:?}"
        );
        assert_eq!(store.snapshot().len(), 0, "{format:?}");
        // without named graphs the body loads
        let default_only: Vec<Quad> = fixture(format)
            .into_iter()
            .filter(|q| q.graph_name == GraphName::DefaultGraph)
            .collect();
        let mut src = Source::from_bytes(encode(format, &default_only), format, Some(n("g")));
        src.triples_only = true;
        assert_eq!(
            store.load(&[src]).unwrap(),
            default_only.len() as u64,
            "{format:?}"
        );
    }
}

#[test]
fn blank_nodes_are_scoped_to_each_source() {
    for format in ALL {
        let bytes = encode(format, &fixture(format));
        for store in [Store::in_memory(StoreOptions::default()), nonempty()] {
            let a = Source::from_bytes(bytes.clone(), format, None);
            let b = Source::from_bytes(bytes.clone(), format, None);
            store.load(&[a, b]).unwrap();
            let text = canonical(&store).join("\n");
            // `b0` of the first file and of the second are two blank nodes
            assert!(text.contains("_:b1 "), "{format:?}\n{text}");
        }
    }
}

#[test]
fn a_byte_budget_stops_a_jena_source() {
    for format in ALL {
        let bytes = encode(format, &fixture(format));
        let mut src = Source::from_bytes(bytes.clone(), format, None);
        src.max_decompressed = Some(bytes.len() as u64 / 2);
        let store = Store::in_memory(StoreOptions::default());
        let e = store.load(&[src]).unwrap_err();
        assert!(
            matches!(
                e,
                Error::BudgetExceeded(Budget {
                    kind: BudgetKind::DecompressedBytes,
                    ..
                })
            ),
            "{format:?}: {e:?}"
        );
        assert_eq!(store.snapshot().len(), 0, "{format:?}");
        // the budget counts decompressed bytes
        let mut src = Source::from_bytes(gzip(&bytes), format, None);
        src.max_decompressed = Some(bytes.len() as u64 / 2);
        let e = Store::in_memory(StoreOptions::default())
            .load(&[src])
            .unwrap_err();
        assert!(matches!(e, Error::BudgetExceeded(_)), "{format:?}: {e:?}");
    }
}

fn varint(mut v: usize, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A protobuf field holding `payload`.
fn field(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    varint(payload.len(), &mut out);
    out.extend_from_slice(payload);
    out
}

/// A Protobuf body with one prefix of `prefix_len` bytes and `statements` statements that
/// each use it for their subject and object. It is small and expands a thousand-fold.
fn prefix_bomb(prefix_len: usize, statements: usize) -> Vec<u8> {
    let iri = format!("http://ex.org/{}/", "x".repeat(prefix_len));
    let mut prefix = field(0x0a, b"p");
    prefix.extend(field(0x12, iri.as_bytes()));
    let mut body = Vec::new();
    let mut row = |row: Vec<u8>| {
        varint(row.len(), &mut body);
        body.extend(row);
    };
    row(field(0x0a, &prefix));
    let mut pname = field(0x0a, b"p");
    pname.extend(field(0x12, b"a"));
    let pname = field(0x22, &pname);
    let predicate = field(0x0a, &field(0x0a, b"http://ex.org/p"));
    let mut statement = field(0x0a, &pname);
    statement.extend(field(0x12, &predicate));
    statement.extend(field(0x1a, &pname));
    for _ in 0..statements {
        row(field(0x12, &statement));
    }
    body
}

#[test]
fn a_prefix_that_expands_the_body_is_held_to_the_byte_budget() {
    // 100 statements of three 64 KiB terms from a body of about 64 KiB
    let body = prefix_bomb(64 << 10, 100);
    assert!(body.len() < 80 << 10, "{}", body.len());
    let limit = 1 << 20;
    // the input is far under the limit, and the decoded quads are far over it
    let mut src = Source::from_bytes(body.clone(), JenaFormat::Protobuf, None);
    src.max_decompressed = Some(limit);
    let store = Store::in_memory(StoreOptions::default());
    let e = store.load(&[src]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                ..
            })
        ),
        "{e:?}"
    );
    assert_eq!(store.snapshot().len(), 0);
    // the transaction path holds it too
    let mut src = Source::from_bytes(body.clone(), JenaFormat::Protobuf, None);
    src.max_decompressed = Some(limit);
    let e = sparkles_core::io::parse_source_into(&src, |_| Ok(())).unwrap_err();
    assert!(matches!(e, Error::BudgetExceeded(_)), "{e:?}");
    // without a limit the body loads: the cap is what stops it
    let src = Source::from_bytes(body, JenaFormat::Protobuf, None);
    let mut n = 0;
    sparkles_core::io::parse_source_into(&src, |_| {
        n += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(n, 100);
}

/// A Protobuf body of one statement whose object is the literal `lexical`.
fn proto_literal_body(lexical: &str) -> Vec<u8> {
    let iri = |s: &[u8]| field(0x0a, &field(0x0a, s));
    let mut statement = field(0x0a, &iri(b"http://ex.org/s"));
    statement.extend(field(0x12, &iri(b"http://ex.org/p")));
    statement.extend(field(0x1a, &field(0x1a, &field(0x0a, lexical.as_bytes()))));
    let row = field(0x12, &statement);
    // the row is length-delimited in the stream
    let mut framed = Vec::new();
    varint(row.len(), &mut framed);
    framed.extend(row);
    framed
}

fn load_with_cap(body: &[u8], cap: u64) -> sparkles_core::Result<u64> {
    let mut src = Source::from_bytes(body.to_vec(), JenaFormat::Protobuf, None);
    src.max_decompressed = Some(cap);
    Store::in_memory(StoreOptions::default()).load(&[src])
}

#[test]
fn the_expansion_count_follows_the_escapes_of_nquads() {
    let over = |r: sparkles_core::Result<u64>| {
        matches!(
            r,
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                ..
            }))
        )
    };
    // U+FFFE and U+FFFF are three bytes in the body and a six-byte escape in the
    // N-Quads line, so 600 KB of them are 1.2 MB of output
    for c in ['\u{fffe}', '\u{ffff}'] {
        let body = proto_literal_body(&c.to_string().repeat(200_000));
        assert!(body.len() < 700_000, "{}", body.len());
        assert!(over(load_with_cap(&body, 1 << 20)), "{c:?} over the cap");
        // under a cap above the line, the body loads
        assert!(
            load_with_cap(&body, 1_300_000).is_ok(),
            "{c:?} under the cap"
        );
    }
    // backspace and form feed are two bytes in the line, as they are in the body
    // (200 KB in, 400 KB out), where six bytes each would be 1.2 MB
    let body = proto_literal_body(&"\u{8}\u{c}".repeat(100_000));
    assert!(body.len() < 250_000, "{}", body.len());
    assert!(
        load_with_cap(&body, 512 << 10).is_ok(),
        "backspace and form feed"
    );
    assert!(
        over(load_with_cap(&body, 300_000)),
        "backspace and form feed over a cap of 300 kB"
    );
    // the four together
    let body = proto_literal_body(&"\u{8}\u{fffe}\u{c}\u{ffff}".repeat(70_000));
    assert!(over(load_with_cap(&body, 1 << 20)), "mixed");
    assert!(
        load_with_cap(&body, 1_500_000).is_ok(),
        "mixed under the cap"
    );
}

/// A malformed body of each syntax, with what its message names.
fn malformed() -> Vec<(JenaFormat, Vec<u8>, &'static str)> {
    let thrift = encode(JenaFormat::Thrift, &fixture(JenaFormat::Thrift));
    let proto = encode(JenaFormat::Protobuf, &fixture(JenaFormat::Protobuf));
    vec![
        (JenaFormat::Thrift, thrift[..thrift.len() - 5].to_vec(), "after"),
        (JenaFormat::Protobuf, proto[..proto.len() - 5].to_vec(), "after"),
        (
            JenaFormat::TriX,
            b"<trix xmlns=\"http://www.w3.org/2004/03/trix/trix-1/\"><graph>\n<triple><uri>urn:x</uri></graph></trix>"
                .to_vec(),
            "at byte",
        ),
        (JenaFormat::RdfJson, b"{ \"http://ex.org/a\": { \"http://ex.org/p\": [ { \"type\": ".to_vec(), "line"),
    ]
}

#[test]
fn malformed_input_fails_with_the_source_the_syntax_and_a_position() {
    for (format, bytes, position) in malformed() {
        for first in [true, false] {
            let store = if first {
                Store::in_memory(StoreOptions::default())
            } else {
                nonempty()
            };
            let before = store.snapshot().len();
            let mut src = Source::from_bytes(bytes.clone(), format, None);
            src.name = "broken.in".into();
            let e = store.load(&[src]).unwrap_err();
            let Error::RdfParse(m) = &e else {
                panic!("{format:?}: {e:?}")
            };
            assert!(m.contains("broken.in"), "{format:?}: {m}");
            assert!(m.contains(format.name()), "{format:?}: {m}");
            assert!(!m.contains("TriX: TriX"), "{m}");
            assert!(m.contains(position), "{format:?}: {m}");
            // a failed load adds nothing
            assert_eq!(store.snapshot().len(), before, "{format:?}");
        }
    }
}

// ---------------------------------------------------------------- SPARQL LOAD ----

fn update(
    store: &Store,
    text: &str,
) -> sparkles_core::Result<sparkles_core::sparql::update::UpdateStats> {
    let opts = QueryOptions {
        file_loads: FileLoads::Anywhere,
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    };
    sparkles_core::sparql::update::update(store, text, &opts)
}

#[test]
fn load_reads_a_file_of_each_syntax_with_and_without_compression() {
    for format in ALL {
        let quads = fixture(format);
        let want = expected(&quads);
        for ext in ["", ".gz"] {
            let bytes = encode(format, &quads);
            let bytes = if ext.is_empty() { bytes } else { gzip(&bytes) };
            let dir = tempfile::tempdir().unwrap();
            let path = dir
                .path()
                .join(format!("d.{}{ext}", format.file_extension()));
            std::fs::write(&path, bytes).unwrap();
            let store = Store::in_memory(StoreOptions::default());
            update(&store, &format!("LOAD <file://{}>", path.display())).unwrap();
            assert_eq!(canonical(&store), want, "{format:?}{ext}");
            // into a graph
            let store = Store::in_memory(StoreOptions::default());
            update(
                &store,
                &format!(
                    "LOAD <file://{}> INTO GRAPH <http://ex.org/t>",
                    path.display()
                ),
            )
            .unwrap();
            let text = canonical(&store).join("\n");
            assert!(
                text.contains("\"one\" <http://ex.org/t> ."),
                "{format:?}{ext}\n{text}"
            );
        }
    }
}

/// Serve `body` as `content_type` once per request for `/d`, on a loopback port.
fn serve(content_type: &'static str, body: Vec<u8>, requests: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for _ in 0..requests {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && s.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let accept = String::from_utf8_lossy(&head).to_ascii_lowercase();
            assert!(
                accept.contains("application/rdf+thrift")
                    && accept.contains("application/rdf+protobuf")
                    && accept.contains("application/rdf+json")
                    && accept.contains("application/trix+xml"),
                "the Accept header names every syntax: {accept}"
            );
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    port
}

#[test]
fn load_reads_each_syntax_over_http_by_media_type_with_and_without_compression() {
    for format in ALL {
        let quads = fixture(format);
        let want = expected(&quads);
        for compressed in [false, true] {
            let bytes = encode(format, &quads);
            let bytes = if compressed { gzip(&bytes) } else { bytes };
            let port = serve(format.media_type(), bytes, 1);
            let store = Store::in_memory(StoreOptions::default());
            update(&store, &format!("LOAD <http://127.0.0.1:{port}/d>")).unwrap();
            assert_eq!(canonical(&store), want, "{format:?} gz={compressed}");
        }
    }
}

#[test]
fn load_picks_the_syntax_from_the_url_when_the_media_type_says_nothing() {
    let format = JenaFormat::Thrift;
    let port = serve(
        "application/octet-stream",
        encode(format, &fixture(format)),
        1,
    );
    let store = Store::in_memory(StoreOptions::default());
    update(&store, &format!("LOAD <http://127.0.0.1:{port}/d.rt>")).unwrap();
    assert_eq!(canonical(&store), expected(&fixture(format)));
}

#[test]
fn load_over_http_holds_a_body_that_expands_to_the_response_ceiling() {
    let body = prefix_bomb(64 << 10, 100);
    let port = serve(JenaFormat::Protobuf.media_type(), body, 1);
    let store = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            max_response_bytes: 1 << 20,
            ..Default::default()
        },
        ..Default::default()
    };
    let e = sparkles_core::sparql::update::update(
        &store,
        &format!("LOAD <http://127.0.0.1:{port}/d>"),
        &opts,
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                ..
            })
        ),
        "{e:?}"
    );
    assert_eq!(store.snapshot().len(), 0);
}

#[test]
fn load_of_a_broken_body_fails_and_load_silent_leaves_nothing_behind() {
    for (format, bytes, position) in malformed() {
        let port = serve(format.media_type(), bytes.clone(), 2);
        let url = format!("http://127.0.0.1:{port}/d");
        let store = nonempty();
        let before = store.snapshot().len();
        let e = update(&store, &format!("LOAD <{url}>")).unwrap_err();
        let Error::RdfParse(m) = &e else {
            panic!("{format:?}: {e:?}")
        };
        assert!(
            m.contains(&url) && m.contains(format.name()) && m.contains(position),
            "{m}"
        );
        assert_eq!(store.snapshot().len(), before, "{format:?}");
        let stats = update(&store, &format!("LOAD SILENT <{url}>")).unwrap();
        assert_eq!(stats.inserted, 0, "{format:?}");
        assert_eq!(store.snapshot().len(), before, "{format:?}");
    }
}

#[test]
fn load_of_a_broken_file_names_the_file() {
    for (format, bytes, _) in malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("bad.{}", format.file_extension()));
        std::fs::write(&path, bytes).unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let e = update(&store, &format!("LOAD <file://{}>", path.display())).unwrap_err();
        assert!(
            matches!(&e, Error::RdfParse(m) if m.contains("bad.") && m.contains(format.name())),
            "{e:?}"
        );
        assert_eq!(store.snapshot().len(), 0);
    }
}
