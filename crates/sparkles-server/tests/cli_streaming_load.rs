//! Real CLI imports: mode overrides, native xz detection and ordered JSON-LD.
use sparkles::codec::Codec;
use std::io::Write;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

#[test]
fn cli_load_modes_and_jsonld_profile() {
    let dir = tempfile::tempdir().unwrap();
    let mut compressed = Vec::new();
    let mut writer = Codec::Xz.writer(&mut compressed, None, 1).unwrap();
    writer
        .write_all(b"@prefix ex: <urn:> . ex:s ex:p ex:o .")
        .unwrap();
    writer.finish().unwrap();
    std::fs::write(dir.path().join("data.ttl.xz"), compressed).unwrap();
    for mode in ["auto", "streaming", "buffered"] {
        let out = Command::new(BIN)
            .current_dir(dir.path())
            .args([
                "load",
                "--loc",
                mode,
                "--parse-mode",
                mode,
                "--auto-buffer-bytes",
                "1024",
                "data.ttl.xz",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let store =
            sparkles::store::Store::open(&dir.path().join(mode), Default::default()).unwrap();
        assert_eq!(store.snapshot().len(), 1);
    }
    std::fs::write(
        dir.path().join("late.jsonld"),
        r#"{"@id":"urn:s","p":"value","@context":{"p":"urn:p"}}"#,
    )
    .unwrap();
    let out = Command::new(BIN)
        .current_dir(dir.path())
        .args([
            "load",
            "--loc",
            "json",
            "--parse-mode",
            "streaming",
            "--jsonld-streaming",
            "late.jsonld",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let store = sparkles::store::Store::open(&dir.path().join("json"), Default::default()).unwrap();
    assert_eq!(store.snapshot().len(), 0);
}
