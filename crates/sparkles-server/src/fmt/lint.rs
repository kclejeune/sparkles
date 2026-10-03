//! `sparkles lint` (spec X03): lint SPARQL queries and updates, Turtle and TriG. Paths
//! and directories are walked as `sparkles fmt` walks them (the same ignore files), each
//! file gets the `[lint]` table of its nearest `.sparklesfmt.toml`, and `--rule` overrides
//! it. The findings are printed as `path:line:column: severity [rule] message`, or as
//! JSON. `--fix` applies the safe fixes in place (to stdout for stdin).
//!
//! Exit status: 0 without an error-level finding, 1 with one (or with a warning under
//! `--strict`), 2 when a file, flag or config file cannot be used.

use super::{config, parse_language, parse_size, report, walk};
use anyhow::{Context, Result, bail};
use clap::Args;
use rayon::prelude::*;
use serde_json::{Value as J, json};
use sparkles_fmt::lint::{self, Diagnostic, LintOptions};
use sparkles_fmt::{Detection, Language};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Args, Debug)]
pub struct LintArgs {
    /// Files or directories. Directories are walked recursively (.rq, .ru, .sparql, .ttl
    /// and .trig; .gitignore and .sparklesfmtignore apply). With no PATH, reads stdin
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// sparql | turtle | trig (default: from the extension, else the content)
    #[arg(long, value_name = "LANG", value_parser = parse_language)]
    pub language: Option<Language>,
    /// Name of the stdin input, for language detection, config and ignore files
    #[arg(long, value_name = "NAME", conflicts_with = "paths")]
    pub stdin_filepath: Option<PathBuf>,
    /// A rule's severity: RULE=error|warning|info|hint|off (repeatable; over the config
    /// file's [lint] table)
    #[arg(long = "rule", value_name = "RULE=LEVEL")]
    pub rules: Vec<String>,
    /// Apply the safe fixes: rewrite files in place, or print the fixed stdin
    #[arg(long)]
    pub fix: bool,
    /// Exit 1 on warnings too
    #[arg(long)]
    pub strict: bool,
    /// text | json
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub format: String,
    /// List the rules with their default severities, and exit
    #[arg(long)]
    pub list_rules: bool,
    /// Use this config file (no discovery)
    #[arg(long, value_name = "PATH", conflicts_with = "no_config")]
    pub config: Option<PathBuf>,
    /// Ignore config files
    #[arg(long)]
    pub no_config: bool,
    /// Ignore file (repeatable; default ./.sparklesfmtignore)
    #[arg(long, value_name = "PATH")]
    pub ignore_path: Vec<PathBuf>,
    /// The largest file linted (bytes, or with a KiB, MiB, GiB or TiB suffix)
    #[arg(long, value_name = "SIZE", default_value = "256MiB", value_parser = parse_size)]
    pub max_bytes: u64,
}

impl LintArgs {
    /// `base` with the `--rule` flags applied.
    fn options_over(&self, mut base: LintOptions) -> Result<LintOptions, String> {
        for r in &self.rules {
            let (rule, level) = r.split_once('=').ok_or_else(|| {
                format!("--rule {r}: expected RULE=LEVEL, such as unused-prefix=off")
            })?;
            base.set(rule.trim(), level.trim())
                .map_err(|e| format!("--rule {e}"))?;
        }
        Ok(base)
    }
}

/// Whether a directory walk lints `path`.
fn walk_takes(path: &Path) -> bool {
    matches!(sparkles_fmt::detect_path(path), Some(Detection::Lang(l)) if lint::lints(l))
}

const EXTENSIONS: [&str; 5] = [".rq", ".ru", ".sparql", ".ttl", ".trig"];

/// Run `sparkles lint`.
pub fn run(args: LintArgs) -> Result<()> {
    let code = match run_inner(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// One document.
struct Job {
    name: String,
    file: Option<PathBuf>,
    lang_path: Option<PathBuf>,
    text: Option<String>,
    opts: Result<Arc<LintOptions>, ()>,
}

/// What linting one document found.
struct Outcome {
    name: String,
    language: Option<Language>,
    diagnostics: Vec<Diagnostic>,
    /// the fixes applied (`--fix`)
    fixed: usize,
    /// a failure: the file could not be read, linted or written
    error: Option<String>,
    /// the fixed text of stdin
    stdout: Option<String>,
}

fn run_inner(args: &LintArgs) -> Result<i32> {
    if args.list_rules {
        list_rules();
        return Ok(0);
    }
    let json = match args.format.as_str() {
        "text" => false,
        "json" => true,
        f => bail!("--format {f}: expected text or json"),
    };
    // the flags alone, so a bad one stops the run before any file
    if let Err(e) = args.options_over(LintOptions::default()) {
        bail!(e);
    }
    let cwd = std::env::current_dir().context("the current directory")?;
    let mut configs = match (&args.config, args.no_config) {
        (_, true) => config::Source::None,
        (Some(path), _) => {
            let c = config::Config::load(path, path);
            if let Err(e) = &c.lint {
                eprintln!("{e}");
                return Ok(2);
            }
            config::Source::Fixed(Arc::new(c))
        }
        (None, false) => config::Source::Discover {
            cwd: cwd.clone(),
            cache: HashMap::new(),
        },
    };
    let ignores = match walk::Ignores::load(&cwd, &args.ignore_path) {
        Ok(i) => Arc::new(i),
        Err(e) => {
            eprintln!("{e}");
            return Ok(2);
        }
    };
    let mut errors = 0;
    let mut broken = HashSet::new();
    let mut options_in = |dir: &Path| -> Result<Arc<LintOptions>, ()> {
        let base = match configs.for_dir(dir) {
            None => LintOptions::default(),
            Some(c) => match &c.lint {
                Ok(o) => o.clone(),
                Err(e) => {
                    if broken.insert(c.path.clone()) {
                        eprintln!("{e}");
                    }
                    return Err(());
                }
            },
        };
        args.options_over(base).map(Arc::new).map_err(|_| ())
    };
    let mut jobs = Vec::new();
    if args.paths.is_empty() {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(args.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .context("reading stdin")?;
        if bytes.len() as u64 > args.max_bytes {
            bail!("stdin is larger than --max-bytes");
        }
        let text = String::from_utf8(bytes).context("reading stdin: not UTF-8 text")?;
        let abs = args
            .stdin_filepath
            .as_deref()
            .map(|p| walk::absolute(&cwd, p));
        if abs.as_deref().is_some_and(|a| ignores.ignored(a, false)) {
            if args.fix {
                print!("{text}");
            }
            return Ok(0);
        }
        let dir = abs
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(&cwd)
            .to_path_buf();
        jobs.push(Job {
            name: args
                .stdin_filepath
                .as_deref()
                .map_or_else(|| "<stdin>".to_string(), |p| p.display().to_string()),
            file: None,
            lang_path: args.stdin_filepath.clone(),
            text: Some(text),
            opts: options_in(&dir),
        });
    } else {
        let (inputs, walk_errors) = walk::collect_with(
            &args.paths,
            &cwd,
            &ignores,
            walk_takes,
            ("lint", "linted"),
            &EXTENSIONS,
        );
        for e in &walk_errors {
            eprintln!("{e}");
        }
        errors += walk_errors.len();
        for input in inputs {
            let dir = input.abs.parent().unwrap_or(&cwd).to_path_buf();
            jobs.push(Job {
                name: input.path.display().to_string(),
                lang_path: Some(input.path.clone()),
                file: Some(input.path),
                text: None,
                opts: options_in(&dir),
            });
        }
    }

    let outcomes: Vec<Outcome> = jobs.par_iter().map(|j| process(j, args)).collect();

    let mut counts = [0usize; 4];
    let mut fixed = (0, 0);
    let mut out = std::io::stdout().lock();
    let mut files = Vec::new();
    for o in &outcomes {
        if let Some(e) = &o.error {
            eprintln!("{e}");
            errors += 1;
            continue;
        }
        if o.fixed > 0 {
            fixed.0 += o.fixed;
            fixed.1 += 1;
        }
        for d in &o.diagnostics {
            counts[d.severity as usize] += 1;
        }
        if json {
            files.push(json!({
                "path": o.name,
                "language": o.language.map(Language::name),
                "fixed": o.fixed,
                "diagnostics": o.diagnostics.iter().map(diagnostic_json).collect::<Vec<J>>(),
            }));
            continue;
        }
        // the findings go to stderr when stdout carries the fixed text
        let lines: Vec<String> = o
            .diagnostics
            .iter()
            .map(|d| {
                format!(
                    "{}:{}:{}: {} [{}] {}",
                    o.name,
                    d.line,
                    d.column,
                    d.severity.name(),
                    d.rule,
                    d.message
                )
            })
            .collect();
        match &o.stdout {
            Some(text) => {
                for l in &lines {
                    eprintln!("{l}");
                }
                out.write_all(text.as_bytes())?;
            }
            None => {
                for l in &lines {
                    writeln!(out, "{l}")?;
                }
            }
        }
    }
    if json {
        let doc = json!({
            "files": files,
            "summary": {
                "errors": counts[0],
                "warnings": counts[1],
                "info": counts[2],
                "hints": counts[3],
                "fixed": fixed.0,
                "failures": errors,
            },
        });
        match outcomes.first().and_then(|o| o.stdout.as_ref()) {
            // stdout carries the fixed text: the report goes to stderr
            Some(text) => {
                eprintln!("{doc}");
                out.write_all(text.as_bytes())?;
            }
            None => writeln!(out, "{doc}")?,
        }
    }
    out.flush()?;
    drop(out);
    if !json {
        let total: usize = counts.iter().sum();
        if total > 0 {
            eprintln!(
                "{total} {} ({} {}, {} {}, {} info, {} {})",
                plural(total, "problem"),
                counts[0],
                plural(counts[0], "error"),
                counts[1],
                plural(counts[1], "warning"),
                counts[2],
                counts[3],
                plural(counts[3], "hint")
            );
        }
        if fixed.0 > 0 {
            eprintln!(
                "fixed {} {} in {} {}",
                fixed.0,
                plural(fixed.0, "problem"),
                fixed.1,
                plural(fixed.1, "file")
            );
        }
    }
    Ok(if errors > 0 {
        2
    } else if counts[0] > 0 || (args.strict && counts[1] > 0) {
        1
    } else {
        0
    })
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

fn diagnostic_json(d: &Diagnostic) -> J {
    json!({
        "rule": d.rule,
        "severity": d.severity.name(),
        "message": d.message,
        "line": d.line,
        "column": d.column,
        "endLine": d.end_line,
        "endColumn": d.end_column,
        "fixable": d.fix.is_some() && lint::rule(d.rule).is_some_and(|r| r.safe_fix),
    })
}

/// Lint (and with `--fix`, fix) one document.
fn process(j: &Job, args: &LintArgs) -> Outcome {
    let mut o = Outcome {
        name: j.name.clone(),
        language: None,
        diagnostics: Vec::new(),
        fixed: 0,
        error: None,
        stdout: None,
    };
    let fail = |mut o: Outcome, m: String| {
        o.error = Some(format!("{}: error: {m}", j.name));
        o
    };
    let Ok(opts) = &j.opts else {
        // the broken config file was reported
        return fail(o, "its config file is broken".to_string());
    };
    let text = match (&j.text, &j.file) {
        (Some(t), _) => t.clone(),
        (None, Some(p)) => {
            match std::fs::metadata(p) {
                Ok(m) if m.len() > args.max_bytes => {
                    return fail(o, "larger than --max-bytes".to_string());
                }
                Err(e) => return fail(o, report::io(&e)),
                _ => {}
            }
            match std::fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) => return fail(o, report::io(&e)),
            }
        }
        (None, None) => String::new(),
    };
    let lang = match args.language {
        Some(l) => l,
        None => match sparkles_fmt::detect(j.lang_path.as_deref(), &text) {
            Detection::Lang(l) => l,
            Detection::RdfXml => return fail(o, "RDF/XML is not linted".to_string()),
            _ => return fail(o, "cannot tell the language; use --language".to_string()),
        },
    };
    o.language = Some(lang);
    if !lint::lints(lang) {
        return fail(
            o,
            format!(
                "{} is not linted: lint takes SPARQL, Turtle and TriG",
                lang.display_name()
            ),
        );
    }
    if args.fix {
        match lint::fix(&text, lang, opts) {
            Ok(f) => {
                o.fixed = f.applied;
                o.diagnostics = f.diagnostics;
                match &j.file {
                    Some(p) if f.applied > 0 => {
                        if let Err(e) = super::write_atomically(p, &f.text) {
                            return fail(o, report::io(&e));
                        }
                    }
                    Some(_) => {}
                    None => o.stdout = Some(f.text),
                }
            }
            Err(e) => return fail(o, e.to_string()),
        }
    } else {
        match lint::lint(&text, lang, opts) {
            Ok(l) => o.diagnostics = l.diagnostics,
            Err(e) => return fail(o, e.to_string()),
        }
    }
    o
}

/// `--list-rules`: each rule with its default severity, its languages and whether
/// `--fix` fixes it.
fn list_rules() {
    println!(
        "{:<24} {:<8} {:<14} {:<4} summary",
        "rule", "default", "languages", "fix"
    );
    for r in lint::RULES {
        let langs = match (r.sparql, r.turtle) {
            (true, true) => "sparql, turtle",
            (true, false) => "sparql",
            _ => "turtle",
        };
        println!(
            "{:<24} {:<8} {:<14} {:<4} {}",
            r.id,
            r.default.name(),
            langs,
            if r.safe_fix { "yes" } else { "" },
            r.summary
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_fmt::lint::Severity;

    #[test]
    fn rule_flags() {
        let a = LintArgs {
            paths: Vec::new(),
            language: None,
            stdin_filepath: None,
            rules: vec![
                "unused-prefix=off".into(),
                "cartesian-product = error".into(),
            ],
            fix: false,
            strict: false,
            format: "text".into(),
            list_rules: false,
            config: None,
            no_config: false,
            ignore_path: Vec::new(),
            max_bytes: 1,
        };
        let o = a.options_over(LintOptions::default()).unwrap();
        assert_eq!(o.level("unused-prefix"), None);
        assert_eq!(o.level("cartesian-product"), Some(Severity::Error));
        let bad = LintArgs {
            rules: vec!["unused-prefix".into()],
            ..a
        };
        assert!(bad.options_over(LintOptions::default()).is_err());
    }
}
