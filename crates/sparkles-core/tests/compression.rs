//! Compressed inputs and backups: every codec loads the same quads, the magic bytes win
//! over a misleading extension, an explicit codec is checked, and backups round-trip.

use sparkles_core::codec::{Codec, Level};
use sparkles_core::io::Source;
use sparkles_core::store::{Store, StoreOptions};
use std::io::{Read, Write};
use std::path::Path;

const TTL: &str = "@prefix ex: <http://ex.org/> .\n\
    ex:a ex:p \"one\" ; ex:q 2 .\n\
    ex:b ex:p \"two\"@en .\n\
    ex:c ex:r ex:a .\n";

fn compress(c: Codec, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = c.writer(&mut out, Some(Level(3)), 1).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
    out
}

fn load(path: &Path, explicit: Option<Codec>) -> sparkles_core::Result<Vec<String>> {
    let store = Store::in_memory(StoreOptions::default());
    let mut s = Source::from_path(path, None)?;
    s.compression = explicit;
    store.load(&[s])?;
    let mut out = Vec::new();
    store.dump_nquads(&mut out)?;
    let mut lines: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    Ok(lines)
}

#[test]
fn every_codec_loads_the_same_quads() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("data.ttl");
    std::fs::write(&plain, TTL).unwrap();
    let expected = load(&plain, None).unwrap();
    assert_eq!(expected.len(), 4);
    for c in Codec::ALL {
        if c == Codec::None || !c.supported() {
            continue;
        }
        let p = dir.path().join(format!("data.ttl{}", c.extension()));
        std::fs::write(&p, compress(c, TTL.as_bytes())).unwrap();
        assert_eq!(load(&p, None).unwrap(), expected, "{c}");
        assert_eq!(load(&p, Some(c)).unwrap(), expected, "{c}");
    }
}

#[test]
fn magic_bytes_win_and_explicit_codecs_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("data.ttl");
    std::fs::write(&plain, TTL).unwrap();
    let expected = load(&plain, None).unwrap();
    // gzip data named like zstd loads as gzip
    let misnamed = dir.path().join("x.ttl.zst");
    std::fs::write(&misnamed, compress(Codec::Gzip, TTL.as_bytes())).unwrap();
    assert_eq!(load(&misnamed, None).unwrap(), expected);
    // an explicit codec that disagrees with the data is an error, before loading
    let e = load(&misnamed, Some(Codec::Brotli))
        .unwrap_err()
        .to_string();
    assert!(e.contains("gzip"), "{e}");
    let e = load(&plain, Some(Codec::Gzip)).unwrap_err().to_string();
    assert!(e.contains("not gzip"), "{e}");
    // a plain file named like gzip is read as it is
    let named = dir.path().join("y.ttl.gz");
    std::fs::write(&named, TTL).unwrap();
    assert_eq!(load(&named, None).unwrap(), expected);
}

#[test]
fn decompressed_size_is_capped() {
    let big: String = (0..20_000)
        .map(|i| format!("<urn:s{i}> <urn:p> \"{i}\" .\n"))
        .collect();
    let store = Store::in_memory(StoreOptions::default());
    let mut s = Source::from_bytes(
        compress(Codec::Gzip, big.as_bytes()),
        sparkles_core::io::RdfFormat::NTriples,
        None,
    );
    s.max_decompressed = Some(10_000);
    let e = store.load(&[s]).unwrap_err();
    assert!(
        matches!(e, sparkles_core::Error::BudgetExceeded(b) if b.kind == sparkles_core::error::BudgetKind::DecompressedBytes),
        "{e}"
    );
    assert_eq!(store.snapshot().len(), 0);
}

#[test]
fn backups_round_trip_in_every_codec() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            TTL.as_bytes().to_vec(),
            sparkles_core::io::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let mut expected = Vec::new();
    store.dump_nquads(&mut expected).unwrap();
    // the default is zstd (gzip in builds without it)
    let p = store.backup(&dir.path().join("b"), "db").unwrap();
    let ext = if Codec::Zstd.supported() {
        ".nq.zst"
    } else {
        ".nq.gz"
    };
    assert!(p.to_string_lossy().ends_with(ext), "{}", p.display());
    for c in Codec::ALL {
        if !c.supported() {
            continue;
        }
        let p = store
            .backup_with(&dir.path().join("b"), &format!("db-{c}"), c, None, 2)
            .unwrap();
        assert!(
            p.to_string_lossy()
                .ends_with(&format!(".nq{}", c.extension())),
            "{}",
            p.display()
        );
        let mut back = Vec::new();
        c.reader(std::fs::File::open(&p).unwrap(), None)
            .unwrap()
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back, expected, "{c}");
        // and it loads back
        let restored = Store::in_memory(StoreOptions::default());
        restored
            .load(&[Source::from_path(&p, None).unwrap()])
            .unwrap();
        assert_eq!(restored.snapshot().len(), store.snapshot().len(), "{c}");
    }
    // no temporary files are left behind
    let stray: Vec<_> = std::fs::read_dir(dir.path().join("b"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(stray.is_empty());
}

#[cfg(all(feature = "text", feature = "zstd"))]
#[test]
fn text_doc_store_uses_zstd_and_lz4_on_request() {
    use sparkles_core::text::{DocstoreCompression, TextConfig};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            TTL.as_bytes().to_vec(),
            sparkles_core::io::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let hits = |s: &Store| {
        let q = "SELECT ?s { ?s <http://jena.apache.org/text#query> \"two\" }";
        let r = sparkles_core::sparql::query(s.snapshot(), q, &Default::default()).unwrap();
        format!("{:?}", r.rows())
    };
    let meta = |root: &Path| std::fs::read_to_string(root.join("text/meta.json")).unwrap();
    store.enable_text(TextConfig::default()).unwrap();
    assert!(
        meta(&root).contains("\"zstd(compression_level=3)\""),
        "{}",
        meta(&root)
    );
    let zstd_hits = hits(&store);
    assert!(zstd_hits.contains("ex.org/b"), "{zstd_hits}");
    store
        .enable_text(TextConfig {
            docstore_compression: Some(DocstoreCompression::Lz4),
            ..TextConfig::default()
        })
        .unwrap();
    assert!(
        meta(&root).contains("\"docstore_compression\": \"lz4\""),
        "{}",
        meta(&root)
    );
    assert_eq!(hits(&store), zstd_hits);
}
