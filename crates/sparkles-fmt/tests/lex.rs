//! The lexer is lossless on the W3C SPARQL suites: the token texts of every query and
//! update concatenate to the file (minus a BOM), tokens are contiguous, and nothing
//! panics. Skipped without the suite (`SPARKLES_W3C_DIR`, as in `tests/w3c.rs`).

use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use std::path::{Path, PathBuf};

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_W3C_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-arq/testing/rdf-tests-cg/sparql")
        });
    p.exists().then_some(p)
}

fn sparql_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if matches!(p.extension().and_then(|e| e.to_str()), Some("rq" | "ru")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn lossless_on_the_corpus() {
    let Some(dir) = suite_dir() else {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return;
    };
    let mut n = 0;
    for path in sparql_files(&dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let tokens = lex(&text, LexMode::Sparql);
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let mut at = bom;
        for t in &tokens {
            assert_eq!(
                t.start as usize,
                at,
                "{}: a gap before {t:?}",
                path.display()
            );
            at = t.end();
        }
        assert_eq!(at, text.len(), "{}", path.display());
        assert_eq!(tokens.last().map(|t| t.kind), Some(TokenKind::Eof));
        let joined: String = tokens.iter().map(|t| t.text(&text)).collect();
        assert_eq!(joined, text[bom..], "{}", path.display());
        n += 1;
    }
    assert!(n > 1000, "only {n} files");
}
