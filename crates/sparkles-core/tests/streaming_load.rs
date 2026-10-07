//! Reader and slice loading agree across syntaxes, codecs and source boundaries.
use sparkles_core::codec::{Codec, Level};
use sparkles_core::io::{ParseMode, RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{Error, Result};
use std::io::Write;

fn documents() -> Vec<(RdfFormat, &'static str)> {
    vec![
        (
            RdfFormat::NTriples,
            "<urn:s> <urn:p> \"one\" .\n_:b <urn:p> <urn:s> .\n_:b <urn:q> <urn:o> .\n",
        ),
        (
            RdfFormat::NQuads,
            "<urn:s> <urn:p> \"one\" <urn:g> .\n_:b <urn:p> <urn:s> .\n_:b <urn:q> <urn:o> .\n",
        ),
        (
            RdfFormat::Turtle,
            "@prefix ex: <urn:> . ex:s ex:p \"one\" . _:b ex:p ex:s ; ex:q ex:o .",
        ),
        (
            RdfFormat::TriG,
            "@prefix ex: <urn:> . ex:g { ex:s ex:p \"one\" . } _:b ex:p ex:s ; ex:q ex:o .",
        ),
        (
            RdfFormat::N3,
            "@prefix ex: <urn:> . ex:s ex:p \"one\" . _:b ex:p ex:s ; ex:q ex:o .",
        ),
        (
            RdfFormat::RdfXml,
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ex="urn:"><rdf:Description rdf:about="urn:s"><ex:p>one</ex:p></rdf:Description><rdf:Description rdf:nodeID="b"><ex:p rdf:resource="urn:s"/><ex:q rdf:resource="urn:o"/></rdf:Description></rdf:RDF>"#,
        ),
        (
            RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
            r#"[{"@id":"urn:s","p":"one","@context":{"p":"urn:p"}},{"@id":"_:b","urn:p":{"@id":"urn:s"},"urn:q":{"@id":"urn:o"}}]"#,
        ),
    ]
}

fn compressed(codec: Codec, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut writer = codec.writer(&mut out, Some(Level(1)), 1).unwrap();
    writer.write_all(data).unwrap();
    writer.finish().unwrap();
    out
}

fn load(src: Source) -> Result<(Vec<String>, std::collections::BTreeMap<String, String>)> {
    let store = Store::in_memory(StoreOptions::default());
    store.load(&[src])?;
    let mut dump = Vec::new();
    store.dump_nquads(&mut dump)?;
    let mut lines: Vec<String> = String::from_utf8(dump)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    Ok((lines, store.prefixes()))
}

#[test]
fn every_syntax_and_codec_agree_in_all_parse_modes() {
    for (format, document) in documents() {
        let mut baseline = Source::from_bytes(document.as_bytes().to_vec(), format, None);
        baseline.parse_mode = ParseMode::Buffered;
        let expected = load(baseline).unwrap();
        assert_eq!(expected.0.len(), 3, "{format}");
        for codec in Codec::ALL.into_iter().filter(|c| c.supported()) {
            for mode in [ParseMode::Auto, ParseMode::Streaming, ParseMode::Buffered] {
                let cutoffs: &[Option<usize>] = if mode == ParseMode::Auto {
                    &[None, Some(13)]
                } else {
                    &[None]
                };
                for &cutoff in cutoffs {
                    let mut src =
                        Source::from_bytes(compressed(codec, document.as_bytes()), format, None);
                    // Brotli has no identifying magic; byte inputs need an explicit codec.
                    src.compression = Some(codec);
                    src.parse_mode = mode;
                    src.auto_buffer_bytes = cutoff;
                    assert_eq!(
                        load(src).unwrap(),
                        expected,
                        "{format} {codec} {mode:?} cutoff={cutoff:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn streamed_load_preserves_base_named_graphs_and_blank_node_scope() {
    for mode in [ParseMode::Auto, ParseMode::Streaming, ParseMode::Buffered] {
        let store = Store::in_memory(StoreOptions::default());
        let mut sources = Vec::new();
        for name in ["a", "b"] {
            let mut src = Source::from_bytes(
                b"@prefix ex: <http://e/> . _:same ex:p <relative> ; ex:q \"x\" .".to_vec(),
                RdfFormat::Turtle,
                Some(oxrdf::NamedNode::new(format!("urn:{name}")).unwrap()),
            );
            src.base = Some(format!("http://e/{name}/"));
            src.parse_mode = mode;
            sources.push(src);
        }
        assert_eq!(store.load(&sources).unwrap(), 4);
        let mut dump = Vec::new();
        store.dump_nquads(&mut dump).unwrap();
        let text = String::from_utf8(dump).unwrap();
        assert!(text.contains("<http://e/a/relative>"), "{text}");
        assert!(text.contains("<http://e/b/relative>"), "{text}");
        let nodes: std::collections::HashSet<_> = text
            .lines()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(nodes.len(), 2, "{text}");
    }
}

#[test]
fn late_parse_failure_rolls_back_streamed_transaction() {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            b"<urn:old> <urn:p> <urn:o> .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let head = store.snapshot().commit;
    let vocab_before = store.snapshot().generation.dvocab.len();
    let mut source = Source::from_bytes(
        compressed(
            Codec::Xz,
            b"<urn:new> <urn:p> <urn:o> .\n<urn:broken> <urn:p> .",
        ),
        RdfFormat::Turtle,
        None,
    );
    source.parse_mode = ParseMode::Streaming;
    assert!(matches!(store.load(&[source]), Err(Error::RdfParse(_))));
    assert_eq!(store.snapshot().len(), 1);
    assert_eq!(store.snapshot().commit, head);
    assert_eq!(store.snapshot().generation.dvocab.len(), vocab_before);
}

#[test]
fn failed_multi_source_load_truncates_persistent_vocabulary_and_remains_writable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            b"<urn:old> <urn:p> <urn:o> .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let before = store.snapshot().generation.dvocab.len();
    let head = store.snapshot().commit;
    // Force the append buffer to spill, then fail in a subsequent source.
    let mut valid = Source::from_bytes(
        format!("_:b <urn:new> \"{}\" .", "z".repeat(100_000)).into_bytes(),
        RdfFormat::Turtle,
        None,
    );
    valid.parse_mode = ParseMode::Streaming;
    let mut invalid =
        Source::from_bytes(b"<urn:bad> <urn:new> .".to_vec(), RdfFormat::Turtle, None);
    invalid.parse_mode = ParseMode::Streaming;
    assert!(matches!(
        store.load(&[valid, invalid]),
        Err(Error::RdfParse(_))
    ));
    assert_eq!(store.snapshot().generation.dvocab.len(), before);
    assert_eq!(store.snapshot().commit, head);
    assert_eq!(
        store
            .load(&[Source::from_bytes(
                b"<urn:after> <urn:p> <urn:o> .".to_vec(),
                RdfFormat::Turtle,
                None
            )])
            .unwrap(),
        1
    );
    drop(store);
    let reopened = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(reopened.snapshot().len(), 2);
}

#[test]
fn plain_mapped_input_obeys_byte_limit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.ttl");
    std::fs::write(&file, "<urn:s> <urn:p> <urn:o> .").unwrap();
    for mode in [ParseMode::Auto, ParseMode::Streaming, ParseMode::Buffered] {
        let mut src = Source::from_path(&file, None).unwrap();
        src.parse_mode = mode;
        src.max_decompressed = Some(10);
        assert!(
            matches!(load(src), Err(Error::BudgetExceeded(_))),
            "{mode:?}"
        );
    }
}

#[test]
fn streaming_emits_before_the_whole_document_is_decompressed() {
    use sparkles_core::io::{QuadSink, parse_source};
    struct Stop;
    impl QuadSink for Stop {
        fn quad(&mut self, _: oxrdf::Quad) -> Result<()> {
            Err(Error::Cancelled)
        }
        fn finish(self) -> Result<()> {
            panic!("sink must stop at the first quad")
        }
    }
    let triples = "<urn:s> <urn:p> <urn:o> .\n".repeat(50_000);
    let xml_nodes =
        "<rdf:Description rdf:about=\"urn:s\"><ex:p rdf:resource=\"urn:o\"/></rdf:Description>"
            .repeat(20_000);
    let json_nodes = "{\"@id\":\"urn:s\",\"urn:p\":{\"@id\":\"urn:o\"}},".repeat(20_000);
    let formats = [
        (RdfFormat::NTriples, triples.clone()),
        (RdfFormat::NQuads, triples.clone()),
        (RdfFormat::Turtle, triples.clone()),
        (RdfFormat::TriG, format!("<urn:g> {{ {triples} }}")),
        (RdfFormat::N3, triples),
        (
            RdfFormat::RdfXml,
            format!(
                "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:ex=\"urn:\">{xml_nodes}</rdf:RDF>"
            ),
        ),
        (
            RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
            format!("[{}]", json_nodes.trim_end_matches(',')),
        ),
    ];
    for (format, document) in formats {
        for codec in [Codec::None, Codec::Gzip, Codec::Xz, Codec::Bzip2] {
            let mut source =
                Source::from_bytes(compressed(codec, document.as_bytes()), format, None);
            source.parse_mode = ParseMode::Streaming;
            source.max_decompressed = Some(64 << 10);
            // Whole-file decompression fails its byte budget before the sink can run.
            // A reader parser emits the first quad and honours downstream cancellation.
            assert!(
                matches!(parse_source(&source, 2, || Stop), Err(Error::Cancelled)),
                "{format} {codec}"
            );
        }
    }
}

#[test]
fn jsonld_profile_keeps_general_key_order_and_explicit_streaming_validation() {
    let general = RdfFormat::JsonLd {
        profile: oxrdfio::JsonLdProfileSet::empty(),
    };
    let streaming = sparkles_core::io::format_for_media_type(
        "application/ld+json; charset=utf-8; profile=\"http://www.w3.org/ns/json-ld#streaming\"",
    )
    .unwrap();
    let late = br#"{"@id":"urn:s","p":"value","@context":{"p":"urn:p"}}"#;
    let ordered = br#"{"@context":{"p":"urn:p"},"@id":"urn:s","p":"value"}"#;
    for mode in [ParseMode::Auto, ParseMode::Streaming, ParseMode::Buffered] {
        let mut src = Source::from_bytes(late.to_vec(), general, None);
        src.parse_mode = mode;
        assert_eq!(load(src).unwrap().0.len(), 1);
        let mut src = Source::from_bytes(late.to_vec(), streaming, None);
        src.parse_mode = mode;
        assert!(matches!(load(src), Err(Error::RdfParse(_))));
        let mut src = Source::from_bytes(ordered.to_vec(), streaming, None);
        src.parse_mode = mode;
        assert_eq!(load(src).unwrap().0.len(), 1);
    }
}

#[test]
fn auto_switches_large_compressed_documents_without_reading_their_tail() {
    use sparkles_core::io::{QuadSink, parse_source};
    struct Stop;
    impl QuadSink for Stop {
        fn quad(&mut self, _: oxrdf::Quad) -> Result<()> {
            Err(Error::Cancelled)
        }
        fn finish(self) -> Result<()> {
            panic!("sink must stop at the first quad")
        }
    }
    let triples = "<urn:s> <urn:p> <urn:o> .\n".repeat(500_000);
    let xml_nodes =
        "<rdf:Description rdf:about=\"urn:s\"><ex:p rdf:resource=\"urn:o\"/></rdf:Description>"
            .repeat(150_000);
    let json_nodes = "{\"@id\":\"urn:s\",\"urn:p\":{\"@id\":\"urn:o\"}},".repeat(300_000);
    for (format, document) in [
        (RdfFormat::Turtle, triples.clone()),
        (RdfFormat::TriG, format!("<urn:g> {{ {triples} }}")),
        (RdfFormat::N3, triples),
        (
            RdfFormat::RdfXml,
            format!(
                "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:ex=\"urn:\">{xml_nodes}</rdf:RDF>"
            ),
        ),
        (
            RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
            format!("[{}]", json_nodes.trim_end_matches(',')),
        ),
    ] {
        assert!(document.len() > 9 << 20);
        let mut source =
            Source::from_bytes(compressed(Codec::Gzip, document.as_bytes()), format, None);
        source.max_decompressed = Some(9 << 20);
        source.auto_buffer_bytes = Some(8 << 20);
        assert!(
            matches!(parse_source(&source, 2, || Stop), Err(Error::Cancelled)),
            "{format}"
        );
    }
}

#[test]
fn configurable_auto_cutoff_replays_prefix_and_honours_cancellation() {
    use sparkles_core::io::{QuadSink, parse_source};
    struct Stop;
    impl QuadSink for Stop {
        fn quad(&mut self, _: oxrdf::Quad) -> Result<()> {
            Err(Error::Cancelled)
        }
        fn finish(self) -> Result<()> {
            panic!("cancelled sink must not finish")
        }
    }
    let triples = "<urn:s> <urn:p> <urn:o> .\n".repeat(20_000);
    for format in [RdfFormat::Turtle, RdfFormat::TriG] {
        for cutoff in [0, 1000, 4096, 128 << 10] {
            let mut source =
                Source::from_bytes(compressed(Codec::Gzip, triples.as_bytes()), format, None);
            source.auto_buffer_bytes = Some(cutoff);
            source.max_decompressed = Some((cutoff + 4096) as u64);
            assert!(
                matches!(parse_source(&source, 2, || Stop), Err(Error::Cancelled)),
                "{format} cutoff={cutoff}"
            );
            // Explicit buffering must still reject the complete over-budget input.
            source.parse_mode = ParseMode::Buffered;
            assert!(matches!(
                parse_source(&source, 2, || Stop),
                Err(Error::BudgetExceeded(_))
            ));
        }
    }
}

#[test]
fn auto_turtle_trig_default_buffers_beyond_eight_mib() {
    use sparkles_core::io::{QuadSink, parse_source};
    struct Stop;
    impl QuadSink for Stop {
        fn quad(&mut self, _: oxrdf::Quad) -> Result<()> {
            Err(Error::Cancelled)
        }
        fn finish(self) -> Result<()> {
            panic!("cancelled sink must not finish")
        }
    }
    let document = "<urn:s> <urn:p> <urn:o> .\n".repeat(500_000);
    for format in [RdfFormat::Turtle, RdfFormat::TriG, RdfFormat::N3] {
        let mut source =
            Source::from_bytes(compressed(Codec::Gzip, document.as_bytes()), format, None);
        source.max_decompressed = Some(9 << 20);
        let result = parse_source(&source, 2, || Stop);
        // Turtle/TriG keep probing towards 128 MiB; N3 switches at 8 MiB.
        if format == RdfFormat::N3 {
            assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        } else {
            assert!(
                matches!(result, Err(Error::BudgetExceeded(_))),
                "{result:?}"
            );
        }
    }
}

#[test]
fn streaming_preserves_upstream_incomplete_token_limit() {
    use sparkles_core::io::parse_source_into;
    for format in [RdfFormat::Turtle, RdfFormat::TriG] {
        let mut source = Source::from_bytes(
            format!("<urn:s> <urn:p> \"{}\" .", "x".repeat(17 << 20)).into_bytes(),
            format,
            None,
        );
        source.parse_mode = ParseMode::Streaming;
        let mut emitted = 0;
        let error = parse_source_into(&source, |_| {
            emitted += 1;
            Ok(())
        })
        .unwrap_err();
        assert!(
            matches!(error, Error::Io(ref e) if e.kind() == std::io::ErrorKind::OutOfMemory),
            "{format}: {error}"
        );
        assert_eq!(emitted, 0);
    }
}

#[test]
fn streaming_cancels_after_several_reads_before_its_byte_budget() {
    use sparkles_core::io::parse_source_into;
    let document = "<urn:s> <urn:p> <urn:o> .\n".repeat(50_000);
    for codec in [Codec::None, Codec::Gzip, Codec::Xz, Codec::Bzip2] {
        let mut source = Source::from_bytes(
            compressed(codec, document.as_bytes()),
            RdfFormat::Turtle,
            None,
        );
        source.parse_mode = ParseMode::Streaming;
        source.max_decompressed = Some(200 << 10);
        let mut emitted = 0;
        let result = parse_source_into(&source, |_| {
            emitted += 1;
            if emitted == 6000 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(Error::Cancelled)),
            "{codec}: {result:?}"
        );
        assert_eq!(emitted, 6000);
    }
}
