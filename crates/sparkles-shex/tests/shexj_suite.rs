//! The schemas of the shexTest suite (`schemas/`): every ShExJ one reads, and every
//! schema (ShExJ and ShExC) compiles except the negative-structure ones. Schemas that import others, that others import (fragments)
//! or that declare EXTERNAL shapes are only read.
//!
//! The suite is found like in `shextest.rs` (`SPARKLES_SHEX_TESTS`, or Apache Jena's
//! copy next to the workspace); without it the test is skipped.

use sparkles_shex::{NoImports, Schema, ShapeExpr, compile, shexj};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHEX_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shex/src/test/files/spec")
        });
    p.exists().then_some(p)
}

/// Schemas of the suite with a negated reference cycle (`schemas/Cycle2Extra` has none,
/// unlike `negativeStructure/Cycle2Extra`).
const NEGATIVE: &[&str] = &["TwoNegation"];

#[test]
fn suite_schemas_read_and_compile() {
    let Some(dir) = suite_dir() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("schemas"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    // other JSON files of the directory (manifests, the coverage list) are not schemas
    let files: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|f| {
            let text = std::fs::read_to_string(&f).unwrap();
            (f, text)
        })
        .filter(|(_, text)| shexj::is_shexj(text))
        .collect();
    let imported: HashSet<String> = files
        .iter()
        .filter_map(|(_, text)| Schema::from_shexj(text).ok())
        .flat_map(|s| s.imports)
        .map(|i| i.rsplit('/').next().unwrap().to_string())
        .collect();
    let (mut read, mut compiled, mut skipped) = (0, 0, 0);
    let mut failures = Vec::new();
    for (f, text) in &files {
        let name = f.file_stem().unwrap().to_string_lossy().into_owned();
        let schema = match Schema::from_shexj(text) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{name}: read: {e}"));
                continue;
            }
        };
        read += 1;
        let shexc = f.with_extension("shex");
        let from_c = std::fs::read_to_string(&shexc)
            .ok()
            .and_then(|t| Schema::parse_shexc(&t, None).ok());
        let external = schema
            .shapes
            .iter()
            .any(|d| matches!(d.expr, ShapeExpr::External));
        if !schema.imports.is_empty() || external || imported.contains(&name) {
            skipped += 1;
            continue;
        }
        let forms = [Some(schema), from_c];
        for (form, s) in ["ShExJ", "ShExC"].iter().zip(forms) {
            let Some(s) = s else { continue };
            match (compile(&s, &NoImports), NEGATIVE.contains(&name.as_str())) {
                (Ok(_), false) | (Err(_), true) => compiled += 1,
                (Ok(_), true) => {
                    failures.push(format!("{name} ({form}): compiles, but should not"))
                }
                (Err(e), false) => failures.push(format!("{name} ({form}): compile: {e}")),
            }
        }
    }
    eprintln!(
        "{} ShExJ schemas: {read} read, {compiled} compiles as \
         expected (both forms), {skipped} with imports, imported or with EXTERNAL shapes",
        files.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
