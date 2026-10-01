//! `sparkles fmt`: format SPARQL queries and updates (Turtle, TriG, N-Triples, N-Quads
//! and JSON-LD later). Prettier's modes and exit codes: print to stdout by default,
//! `--check` / `--list-different` exit 1 when something would change, `--write`
//! rewrites in place, and any error exits 2.

use anyhow::{Result, bail};
use clap::Args;
use sparkles_fmt::options::{self, OptionError, Value};
use sparkles_fmt::{Detection, FormatError, Language, Options};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct FmtArgs {
    /// Files or directories. Directories are walked recursively (known extensions only;
    /// .gitignore and .sparklesfmtignore apply). With no PATH, reads stdin and writes
    /// stdout
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Check only; exit 1 if any file would change
    #[arg(short = 'c', long, conflicts_with_all = ["list_different", "write"])]
    pub check: bool,
    /// Print the names of files that would change (stdout); exit 1 if any
    #[arg(short = 'l', long, conflicts_with = "write")]
    pub list_different: bool,
    /// Rewrite files in place (atomically; unchanged files are not touched)
    #[arg(short = 'w', long)]
    pub write: bool,
    /// With --check: print a unified diff per changed file (stdout)
    #[arg(long, requires = "check")]
    pub diff: bool,
    /// Name of the stdin input, for language detection, config and ignore files
    #[arg(long, value_name = "NAME")]
    pub stdin_filepath: Option<PathBuf>,
    /// sparql | turtle | trig | ntriples | nquads | jsonld
    #[arg(long, value_name = "LANG", value_parser = parse_language)]
    pub language: Option<Language>,
    /// Line width, 40 to 400 (default 100)
    #[arg(long, value_name = "N")]
    pub line_width: Option<i64>,
    /// Spaces per indentation level, 1 to 8 (default 2)
    #[arg(long, value_name = "N")]
    pub indent_width: Option<i64>,
    /// Opt-in sorting: Turtle, TriG, JSON-LD terms, N-Triples, N-Quads (default off)
    #[arg(long, overrides_with = "no_sort")]
    pub sort: bool,
    /// Keep the source order (the default)
    #[arg(long, overrides_with = "sort")]
    pub no_sort: bool,
    /// N-Triples/N-Quads: RDFC-1.0 blank node labels, canonical form, no comments
    #[arg(long)]
    pub canonicalize: bool,
    /// Drop prefix declarations nothing uses
    #[arg(long)]
    pub prune_prefixes: bool,
    /// sparql (PREFIX, GRAPH) | turtle (@prefix, no GRAPH); default sparql
    #[arg(long, value_name = "S")]
    pub directive_style: Option<String>,
    /// Comma-separated labels forming one prefix group (repeatable, in order; "" is the
    /// empty prefix)
    #[arg(long, value_name = "LABELS")]
    pub prefix_group: Vec<String>,
    /// rdf:type → a, and a entries first (the default)
    #[arg(long, overrides_with = "no_type_shorthand")]
    pub type_shorthand: bool,
    /// Keep rdf:type and the entry order as written
    #[arg(long, overrides_with = "type_shorthand")]
    pub no_type_shorthand: bool,
    /// Full IRI → prefixed name (the default)
    #[arg(long, overrides_with = "no_compact_iris")]
    pub compact_iris: bool,
    /// Keep full IRIs as written
    #[arg(long, overrides_with = "compact_iris")]
    pub no_compact_iris: bool,
    /// double | preserve (default double)
    #[arg(long, value_name = "S")]
    pub quote_style: Option<String>,
    /// leading | trailing, for every broken operator chain (default leading)
    #[arg(long, value_name = "P")]
    pub operator_position: Option<String>,
    /// diff | conventional (default diff)
    #[arg(long, value_name = "L")]
    pub turtle_layout: Option<String>,
    /// Align multi-variable VALUES rows (default off)
    #[arg(long, overrides_with = "no_align_values")]
    pub align_values: bool,
    /// Do not align VALUES rows (the default)
    #[arg(long, overrides_with = "align_values")]
    pub no_align_values: bool,
    /// Use this config file (no discovery)
    #[arg(long, value_name = "PATH", conflicts_with = "no_config")]
    pub config: Option<PathBuf>,
    /// Ignore config files
    #[arg(long)]
    pub no_config: bool,
    /// Ignore file (repeatable; default ./.sparklesfmtignore)
    #[arg(long, value_name = "PATH")]
    pub ignore_path: Vec<PathBuf>,
    /// Line formats: in-memory sort budget before spilling
    #[arg(long, value_name = "SIZE", default_value = "1GiB")]
    pub sort_memory: String,
    /// Files formatted in parallel (default: available cores)
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,
}

fn parse_language(s: &str) -> Result<Language, String> {
    Language::from_name(s)
        .ok_or_else(|| "expected sparql, turtle, trig, ntriples, nquads or jsonld".to_string())
}

/// `--x` / `--no-x`: the last one given wins (clap's `overrides_with`).
fn either(yes: bool, no: bool) -> Option<bool> {
    match (yes, no) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

impl FmtArgs {
    /// The style keys the flags set, in config-file spelling, for [`options::set`] over
    /// the options of a config file.
    pub fn overrides(&self) -> Result<Vec<(&'static str, Value)>, OptionError> {
        let mut v = Vec::new();
        let mut put = |k: &'static str, value: Option<Value>| {
            if let Some(value) = value {
                v.push((k, value));
            }
        };
        put("line-width", self.line_width.map(Value::Int));
        put("indent-width", self.indent_width.map(Value::Int));
        put("sort", either(self.sort, self.no_sort).map(Value::Bool));
        put(
            "prune-prefixes",
            self.prune_prefixes.then_some(Value::Bool(true)),
        );
        put(
            "directive-style",
            self.directive_style.clone().map(Value::Str),
        );
        put(
            "type-shorthand",
            either(self.type_shorthand, self.no_type_shorthand).map(Value::Bool),
        );
        put(
            "compact-iris",
            either(self.compact_iris, self.no_compact_iris).map(Value::Bool),
        );
        put("quote-style", self.quote_style.clone().map(Value::Str));
        put(
            "operator-position",
            self.operator_position.clone().map(Value::Str),
        );
        put("turtle-layout", self.turtle_layout.clone().map(Value::Str));
        put(
            "align-values",
            either(self.align_values, self.no_align_values).map(Value::Bool),
        );
        if !self.prefix_group.is_empty() {
            let groups = self
                .prefix_group
                .iter()
                .map(|g| options::group_from_list(g))
                .collect::<Result<_, _>>()?;
            v.push(("prefix-groups", Value::Groups(groups)));
        }
        Ok(v)
    }

    /// `base` with the flags applied (each through the same checks as the config file).
    pub fn options_over(&self, base: Options) -> Result<Options, OptionError> {
        let mut o = base;
        for (k, v) in self.overrides()? {
            options::set(&mut o, k, v)?;
        }
        o.canonicalize = self.canonicalize;
        Ok(o)
    }
}

/// The flag of a config key, for messages.
fn flag(key: &str) -> String {
    match key {
        "prefix-groups" => "--prefix-group".into(),
        k => format!("--{k}"),
    }
}

/// Run `sparkles fmt`, exiting with 1 (changes found) or 2 (errors) as Prettier does.
pub fn run(args: FmtArgs) -> Result<()> {
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

fn run_inner(args: &FmtArgs) -> Result<i32> {
    let opts = args
        .options_over(Options::default())
        .map_err(|e| anyhow::anyhow!("{}: {}", flag(&e.key), e.message))?;
    if !args.paths.is_empty() {
        // TODO: walks, config discovery, ignore files, --write, --diff
        bail!("formatting files is not available yet; pipe a document through stdin");
    }
    if args.write {
        bail!("--write needs file paths");
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| anyhow::anyhow!("reading stdin: {e}"))?;
    let name = args
        .stdin_filepath
        .as_deref()
        .map_or_else(|| "<stdin>".to_string(), |p| p.display().to_string());
    let lang = match args.language {
        Some(l) => l,
        None => match language_of(args.stdin_filepath.as_deref(), &text) {
            Ok(l) => l,
            Err(msg) => {
                eprintln!("{name}: error: {msg}");
                return Ok(2);
            }
        },
    };
    let formatted = match sparkles_fmt::format(&text, lang, &opts) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{}", error_line(&name, lang, &e));
            return Ok(2);
        }
    };
    for w in &formatted.warnings {
        match w.line {
            0 => eprintln!("{name}: warning: {}", w.message),
            l => eprintln!("{name}:{l}:{}: warning: {}", w.column, w.message),
        }
    }
    if args.check {
        if formatted.changed {
            eprintln!("[warn] {name}");
            return Ok(1);
        }
        return Ok(0);
    }
    if args.list_different {
        if formatted.changed {
            println!("{name}");
            return Ok(1);
        }
        return Ok(0);
    }
    let mut out = std::io::stdout().lock();
    out.write_all(formatted.text.as_bytes())?;
    out.flush()?;
    Ok(0)
}

/// The language of a document without `--language`.
fn language_of(path: Option<&Path>, text: &str) -> Result<Language, String> {
    match sparkles_fmt::detect(path, text) {
        Detection::Lang(l) => Ok(l),
        Detection::RdfXml => Err(sparkles_fmt::RDF_XML_MESSAGE.to_string()),
        Detection::Compressed => Err("compressed input: decompress first".to_string()),
        Detection::SkipInWalk | Detection::Unknown => {
            Err("cannot tell the language; use --language".to_string())
        }
    }
}

/// `path:LINE:COL: error: …` (or `path: error: …` without a position).
fn error_line(name: &str, lang: Language, e: &FormatError) -> String {
    match e {
        FormatError::Syntax {
            message,
            line,
            column,
            ..
        } => format!(
            "{name}:{line}:{column}: error: {} syntax error: {message}",
            lang.display_name()
        ),
        FormatError::Unsupported {
            message,
            line,
            column,
        } => format!(
            "{name}:{line}:{column}: error: the formatter cannot handle this yet ({message}); input left unchanged; please report"
        ),
        e => format!("{name}: error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: FmtArgs,
    }

    fn parse(argv: &[&str]) -> Result<FmtArgs, clap::Error> {
        Cli::try_parse_from(std::iter::once("fmt").chain(argv.iter().copied())).map(|c| c.args)
    }

    #[test]
    fn flags_set_the_same_options_as_the_config_keys() {
        let a = parse(&[
            "--line-width=80",
            "--indent-width",
            "4",
            "--sort",
            "--no-sort",
            "--prune-prefixes",
            "--directive-style",
            "turtle",
            "--prefix-group",
            "rdf,rdfs",
            "--prefix-group",
            "\"\",ex",
            "--no-type-shorthand",
            "--no-compact-iris",
            "--compact-iris",
            "--quote-style",
            "preserve",
            "--operator-position",
            "trailing",
            "--turtle-layout",
            "conventional",
            "--align-values",
            "--canonicalize",
        ])
        .unwrap();
        let o = a.options_over(Options::default()).unwrap();
        let mut expected = Options::default();
        for (k, v) in [
            ("line-width", Value::Int(80)),
            ("indent-width", Value::Int(4)),
            ("sort", Value::Bool(false)),
            ("prune-prefixes", Value::Bool(true)),
            ("directive-style", Value::Str("turtle".into())),
            (
                "prefix-groups",
                Value::Groups(vec![
                    vec!["rdf".into(), "rdfs".into()],
                    vec!["".into(), "ex".into()],
                ]),
            ),
            ("type-shorthand", Value::Bool(false)),
            ("compact-iris", Value::Bool(true)),
            ("quote-style", Value::Str("preserve".into())),
            ("operator-position", Value::Str("trailing".into())),
            ("turtle-layout", Value::Str("conventional".into())),
            ("align-values", Value::Bool(true)),
        ] {
            options::set(&mut expected, k, v).unwrap();
        }
        expected.canonicalize = true;
        assert_eq!(o, expected);
        // flags override a config file's options, and only the ones given
        let base = Options {
            line_width: 120,
            sort: true,
            ..Options::default()
        };
        let o = parse(&["--indent-width", "3"])
            .unwrap()
            .options_over(base)
            .unwrap();
        assert_eq!((o.line_width, o.indent_width, o.sort), (120, 3, true));
    }

    #[test]
    fn bad_flags() {
        let e = parse(&["--line-width", "500"])
            .unwrap()
            .options_over(Options::default())
            .unwrap_err();
        assert_eq!(e.key, "line-width");
        assert_eq!(flag(&e.key), "--line-width");
        let e = parse(&["--quote-style", "single"])
            .unwrap()
            .options_over(Options::default())
            .unwrap_err();
        assert_eq!(
            e.message,
            "expected \"double\" or \"preserve\", got \"single\""
        );
        assert!(
            parse(&["--prefix-group", "rdf,,x"])
                .unwrap()
                .overrides()
                .is_err()
        );
        assert!(parse(&["--check", "--write"]).is_err());
        assert!(parse(&["-l", "-w"]).is_err());
        assert!(parse(&["--diff"]).is_err());
        assert!(parse(&["--config", "a.toml", "--no-config"]).is_err());
        assert!(parse(&["--language", "rdfxml"]).is_err());
        assert_eq!(
            parse(&["--language", "SPARQL"]).unwrap().language,
            Some(Language::Sparql)
        );
    }

    #[test]
    fn error_lines() {
        let e = FormatError::Syntax {
            message: "expected one of …".into(),
            line: 3,
            column: 14,
            offset: 40,
        };
        assert_eq!(
            error_line("queries/bad.rq", Language::Sparql, &e),
            "queries/bad.rq:3:14: error: SPARQL syntax error: expected one of …"
        );
        let e = FormatError::Unsafe {
            check: sparkles_fmt::Check::Algebra,
        };
        assert_eq!(
            error_line("q.rq", Language::Sparql, &e),
            "q.rq: error: formatter refused its own output (algebra differs); input left unchanged; please report"
        );
        assert_eq!(
            language_of(Some(Path::new("x.owl")), ""),
            Err(sparkles_fmt::RDF_XML_MESSAGE.to_string())
        );
        assert_eq!(
            language_of(Some(Path::new("q.rq")), ""),
            Ok(Language::Sparql)
        );
    }
}
