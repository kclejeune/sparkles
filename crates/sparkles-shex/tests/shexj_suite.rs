//! The schemas of the shexTest suite (`schemas/`): every ShExJ one reads; its ShExC
//! reads to the same ShExJ (a few suite errata excepted); and every schema, in both
//! forms, compiles except the negative-structure ones. Schemas that import others,
//! that others import (fragments) or that declare EXTERNAL shapes are not compiled.
//!
//! The suite is found like in `shextest.rs` (`SPARKLES_SHEX_TESTS`, or Apache Jena's
//! copy next to the workspace); without it the test is skipped.

use serde_json::Value;
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

/// Schemas whose ShExC and ShExJ in the suite say different things, with the reason.
const ERRATA: &[(&str, &str)] = &[(
    "start2RefS2",
    "the ShExC's triple constraint has <p2>, the ShExJ's <p1>",
)];

fn url(p: &Path) -> String {
    format!("file://{}", p.display())
}

/// ShExJ with numbers compared by value, language tags in lower case, and the default
/// cardinality `{1,1}` dropped.
fn canon(v: &Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
        Value::Object(o) => {
            let one = |k: &str| o.get(k).is_none_or(|x| x.as_f64() == Some(1.0));
            let mut m = serde_json::Map::new();
            for (k, x) in o {
                if matches!(k.as_str(), "min" | "max") && one("min") && one("max") {
                    continue;
                }
                let x = match x {
                    Value::String(t) if matches!(k.as_str(), "language" | "languageTag") => {
                        Value::String(t.to_lowercase())
                    }
                    x => canon(x),
                };
                m.insert(k.clone(), x);
            }
            Value::Object(m)
        }
        _ => v.clone(),
    }
}

#[test]
fn suite_schemas() {
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
    let (mut read, mut same, mut compiled, mut skipped) = (0, 0, 0, 0);
    let mut failures = Vec::new();
    for (f, text) in &files {
        let name = f.file_stem().unwrap().to_string_lossy().into_owned();
        let schema = match Schema::from_shexj_with_base(text, Some(&url(f))) {
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
            .and_then(|t| Schema::parse_shexc(&t, Some(&url(&shexc))).ok());
        if let Some(c) = &from_c {
            let erratum = ERRATA.iter().any(|(n, _)| *n == name);
            match (canon(&c.to_shexj()) == canon(&schema.to_shexj()), erratum) {
                (true, false) => same += 1,
                (false, true) => {}
                (true, true) => failures.push(format!("{name}: listed as an erratum, but agrees")),
                (false, false) => failures.push(format!(
                    "{name}: the ShExC and the ShExJ differ\n{}\n{}",
                    serde_json::to_string_pretty(&c.to_shexj()).unwrap(),
                    serde_json::to_string_pretty(&schema.to_shexj()).unwrap()
                )),
            }
        }
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
        "{} ShExJ schemas: {read} read, {same} equal to their ShExC, {compiled} compiles as \
         expected (both forms), {skipped} with imports, imported or with EXTERNAL shapes",
        files.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
