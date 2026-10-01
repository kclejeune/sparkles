//! Golden files: `tests/golden/<language>/<name>.in.<ext>` formats to
//! `<name>.out.<ext>`, which formats to itself. Variants: `<name>.w40.out.<ext>` (line
//! width 40), and `<name>.<variant>.out.<ext>` with the options of
//! `<name>.<variant>.toml` (config file keys).
//!
//! `SPARKLES_FMT_BLESS=1` writes the outputs instead of comparing them. Names listed in
//! `tests/golden/pending.txt` (outputs written by hand ahead of the printers) are left
//! out of the default run and never blessed; `-- --ignored` checks them.

use sparkles_fmt::options::{self, Value};
use sparkles_fmt::{Language, Options, format};
use std::path::{Path, PathBuf};

fn bless() -> bool {
    std::env::var_os("SPARKLES_FMT_BLESS").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Options from a config file's text, through the same key checks as the CLI.
fn options_from_toml(text: &str) -> Result<Options, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut o = Options::default();
    for (k, v) in table {
        let value = match v {
            toml::Value::Boolean(b) => Value::Bool(b),
            toml::Value::Integer(i) => Value::Int(i),
            toml::Value::String(s) => Value::Str(s),
            toml::Value::Array(groups) => Value::Groups(
                groups
                    .into_iter()
                    .map(|g| match g {
                        toml::Value::Array(labels) => labels
                            .into_iter()
                            .map(|l| l.as_str().map(str::to_string).ok_or("a label"))
                            .collect::<Result<Vec<_>, _>>(),
                        _ => Err("an array"),
                    })
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("{k}: expected {e}"))?,
            ),
            other => return Err(format!("{k}: unexpected {other}")),
        };
        options::set(&mut o, &k, value).map_err(|e| e.to_string())?;
    }
    Ok(o)
}

/// The variant's options: `w40` is a line width, anything else names a `.toml` file.
fn variant_options(dir: &Path, name: &str, variant: &str) -> Result<Options, String> {
    if variant.is_empty() {
        return Ok(Options::default());
    }
    if let Some(w) = variant
        .strip_prefix('w')
        .and_then(|w| w.parse::<u16>().ok())
    {
        return Ok(Options {
            line_width: w,
            ..Options::default()
        });
    }
    let path = dir.join(format!("{name}.{variant}.toml"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    options_from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `(input, name, extension)` of every `*.in.*` file in `dir`.
fn inputs(dir: &Path) -> Vec<(PathBuf, String, String)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            let file = p.file_name()?.to_str()?.to_string();
            let (name, ext) = file.split_once(".in.")?;
            Some((p.clone(), name.to_string(), ext.to_string()))
        })
        .collect();
    v.sort();
    v
}

/// The variants of `name`: `""` (the default output) and every `<variant>` that has an
/// `.out` file or a `.toml` file.
fn variants(dir: &Path, name: &str, ext: &str) -> Vec<String> {
    let mut v = vec![String::new()];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = file.strip_prefix(&format!("{name}.")) else {
            continue;
        };
        let variant = rest
            .strip_suffix(&format!(".out.{ext}"))
            .or_else(|| rest.strip_suffix(".toml"));
        if let Some(variant) = variant
            && !variant.is_empty()
            && !variant.contains('.')
            && !v.iter().any(|x| x == variant)
        {
            v.push(variant.to_string());
        }
    }
    v.sort();
    v
}

/// `tests/golden/pending.txt`: golden names (`<language>/<name>`, every variant, or
/// `<language>/<name>.<variant>`, that variant only) whose outputs were written by hand
/// ahead of the printers, with the reason. The default run skips them;
/// `cargo test -p sparkles-fmt --test golden -- --ignored` checks them.
fn pending() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/pending.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (name, reason) = l.split_once(char::is_whitespace).unwrap_or((l, ""));
            assert!(
                !reason.trim().is_empty(),
                "golden/pending.txt: {name}: no reason"
            );
            name.to_string()
        })
        .collect()
}

/// Whether the golden check `key` (`<language>/<name>` for the default output,
/// `<language>/<name>.<variant>` for a variant) is pending, by its name or by itself.
fn is_pending(pending: &[String], key: &str) -> bool {
    let name = match key.split_once('.') {
        Some((name, _)) => name,
        None => key,
    };
    pending.iter().any(|p| p == key || p == name)
}

/// Check the golden files `select` picks (by `<language>/<name>` for the default output,
/// `<language>/<name>.<variant>` for a variant): `(checks, failures, keys that passed
/// their checks)`. `bless` writes the outputs instead of comparing.
fn run_golden(select: &dyn Fn(&str) -> bool, bless: bool) -> (usize, Vec<String>, Vec<String>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut failures = Vec::new();
    let mut passed = Vec::new();
    let mut checked = 0;
    let mut langs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    langs.sort_by_key(|e| e.file_name());
    for lang_dir in langs {
        let dir = lang_dir.path();
        let lang_name = lang_dir.file_name().to_string_lossy().into_owned();
        let lang = Language::from_name(&lang_name)
            .unwrap_or_else(|| panic!("tests/golden/{lang_name}: not a language"));
        for (input, name, ext) in inputs(&dir) {
            let key = |variant: &str| match variant {
                "" => format!("{lang_name}/{name}"),
                v => format!("{lang_name}/{name}.{v}"),
            };
            let selected: Vec<String> = variants(&dir, &name, &ext)
                .into_iter()
                .filter(|v| select(&key(v)))
                .collect();
            if selected.is_empty() {
                continue;
            }
            let text = std::fs::read_to_string(&input).unwrap();
            for variant in selected {
                let before = failures.len();
                checked += 1;
                let label = match variant.as_str() {
                    "" => format!("{lang_name}/{name}"),
                    v => format!("{lang_name}/{name} ({v})"),
                };
                let opts = match variant_options(&dir, &name, &variant) {
                    Ok(o) => o,
                    Err(e) => {
                        failures.push(format!("{label}: {e}"));
                        continue;
                    }
                };
                let out_path = match variant.as_str() {
                    "" => dir.join(format!("{name}.out.{ext}")),
                    v => dir.join(format!("{name}.{v}.out.{ext}")),
                };
                let out = match format(&text, lang, &opts) {
                    Ok(f) => f.text,
                    Err(e) => {
                        failures.push(format!("{label}: {e}"));
                        continue;
                    }
                };
                if bless {
                    std::fs::write(&out_path, &out).unwrap();
                } else {
                    let expected = std::fs::read_to_string(&out_path).unwrap_or_default();
                    if out != expected {
                        failures.push(format!(
                            "{label}: output differs from {}\n--- got ---\n{out}--- end ---",
                            out_path.display()
                        ));
                        continue;
                    }
                }
                // the output is a fixpoint
                match format(&out, lang, &opts) {
                    Ok(f) if f.text == out => {}
                    Ok(f) => failures.push(format!(
                        "{label}: the output formats differently\n--- got ---\n{}--- end ---",
                        f.text
                    )),
                    Err(e) => failures.push(format!("{label}: formatting the output: {e}")),
                }
                if failures.len() == before {
                    passed.push(key(&variant));
                }
            }
        }
    }
    (checked, failures, passed)
}

#[test]
fn golden_files() {
    let pending = pending();
    let (checked, failures, _) = run_golden(&|key| !is_pending(&pending, key), bless());
    assert!(checked > 0, "no golden files");
    assert!(
        failures.is_empty(),
        "{} of {checked} golden checks failed (SPARKLES_FMT_BLESS=1 rewrites the outputs):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The pending golden files, never blessed: their outputs are the specification.
#[test]
#[ignore = "golden outputs written ahead of the printers (tests/golden/pending.txt)"]
fn pending_golden_files() {
    let pending = pending();
    let (checked, failures, passed) = run_golden(&|key| is_pending(&pending, key), false);
    eprintln!(
        "pending golden files: {} names, {checked} checks, {} failures",
        pending.len(),
        failures.len()
    );
    if !passed.is_empty() {
        eprintln!(
            "passing now (remove from tests/golden/pending.txt): {}",
            passed.join(", ")
        );
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} pending golden checks failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Every expected SPARQL output, pending ones included, parses to the algebra of its
/// input and keeps its comments: the hand-written outputs pass the formatter's own
/// safety checks before any printer produces them.
#[test]
fn sparql_outputs_mean_what_their_inputs_mean() {
    use sparkles_fmt::check::{comments, sparql_equivalent, sparql_reference};
    use sparkles_fmt::lex::{LexMode, lex};
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/sparql");
    let mut failures = Vec::new();
    let mut checked = 0;
    for (input, name, ext) in inputs(&dir) {
        let text = std::fs::read_to_string(&input).unwrap();
        let r = sparql_reference(&text, &lex(&text, LexMode::Sparql))
            .unwrap_or_else(|e| panic!("sparql/{name}: the input: {e}"));
        for variant in variants(&dir, &name, &ext) {
            let out_path = match variant.as_str() {
                "" => dir.join(format!("{name}.out.{ext}")),
                v => dir.join(format!("{name}.{v}.out.{ext}")),
            };
            let Ok(out) = std::fs::read_to_string(&out_path) else {
                continue;
            };
            checked += 1;
            if let Err(e) = sparql_equivalent(&r, &out)
                .and_then(|()| comments::same(&text, &out, LexMode::Sparql))
            {
                failures.push(format!("{}: {e}", out_path.display()));
            }
        }
    }
    assert!(checked > 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every expected output of the RDF syntaxes, pending ones included, parses to a graph or
/// dataset isomorphic to its input's and keeps its comments, and every expected JSON-LD
/// output gives its input's dataset: like the SPARQL ones, the hand-written outputs pass
/// the safety checks before any printer produces them.
#[test]
fn rdf_outputs_mean_what_their_inputs_mean() {
    use sparkles_fmt::check::{comments, graph};
    use sparkles_fmt::lex::LexMode;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut failures = Vec::new();
    let mut checked = 0;
    let jsonld = |text: &str| -> Result<Vec<oxrdf::Quad>, String> {
        oxjsonld::JsonLdParser::new()
            .for_slice(text)
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())
    };
    for lang in [
        Language::Turtle,
        Language::TriG,
        Language::NTriples,
        Language::NQuads,
        Language::JsonLd,
    ] {
        let dir = root.join(lang.name());
        if !dir.exists() {
            continue;
        }
        for (input, name, ext) in inputs(&dir) {
            let text = std::fs::read_to_string(&input).unwrap();
            let reference = match lang {
                Language::JsonLd => None,
                _ => Some(
                    graph::rdf_reference(&text, lang)
                        .unwrap_or_else(|e| panic!("{}/{name}: the input: {e}", lang.name())),
                ),
            };
            for variant in variants(&dir, &name, &ext) {
                let out_path = match variant.as_str() {
                    "" => dir.join(format!("{name}.out.{ext}")),
                    v => dir.join(format!("{name}.{v}.out.{ext}")),
                };
                let Ok(out) = std::fs::read_to_string(&out_path) else {
                    continue;
                };
                checked += 1;
                let result = match &reference {
                    Some(r) => graph::rdf_equivalent(r, &out)
                        .and_then(|()| comments::same(&text, &out, LexMode::Turtle))
                        .map_err(|e| e.to_string()),
                    None => jsonld(&text).and_then(|a| {
                        let b = jsonld(&out)?;
                        match graph::isomorphic(&a, &b) {
                            true => Ok(()),
                            false => Err("graph differs".to_string()),
                        }
                    }),
                };
                if let Err(e) = result {
                    failures.push(format!("{}: {e}", out_path.display()));
                }
            }
        }
    }
    assert!(checked > 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn toml_options_use_the_config_keys() {
    let o = options_from_toml(
        "line-width = 80\noperator-position = \"trailing\"\nprefix-groups = [[\"rdf\", \"xsd\"], [\"\"]]\n",
    )
    .unwrap();
    assert_eq!(o.line_width, 80);
    assert_eq!(o.prefix_groups, [vec!["rdf", "xsd"], vec![""]]);
    assert!(
        options_from_toml("line-widht = 80")
            .unwrap_err()
            .contains("unknown option")
    );
}
