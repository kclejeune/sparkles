//! The W3C SPARQL test suites (sparql10, sparql11, sparql12) through the formatter:
//! every query and update either formats and passes the checks, or is a syntax error;
//! nothing panics, and nothing is refused.
//!
//! Set `SPARKLES_W3C_DIR` to the `rdf-tests-cg/sparql` directory (as for the engine's
//! suite); without it the default sibling checkout is used, and the test is skipped when
//! that is absent.

use sparkles_fmt::{FormatError, Language, Options, format};
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

/// Every `.rq` and `.ru` file under `dir`, sorted.
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
fn corpus_formats_or_is_a_syntax_error() {
    let Some(dir) = suite_dir() else {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return;
    };
    let files = sparql_files(&dir);
    assert!(!files.is_empty(), "no queries under {}", dir.display());
    let (mut formatted, mut syntax, mut failures) = (0, 0, Vec::new());
    for path in &files {
        // a few negative tests are not UTF-8
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .display()
            .to_string();
        match std::panic::catch_unwind(|| format(&text, Language::Sparql, &Options::default())) {
            Ok(Ok(_)) => formatted += 1,
            Ok(Err(FormatError::Syntax { .. })) => syntax += 1,
            Ok(Err(e)) => failures.push(format!("{rel}: {e}")),
            Err(_) => failures.push(format!("{rel}: panicked")),
        }
    }
    eprintln!(
        "W3C SPARQL: {formatted} formatted, {syntax} syntax errors, {} failures of {} files",
        failures.len(),
        files.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
