//! `sparkles fmt`: format SPARQL queries and updates (Turtle, TriG, N-Triples, N-Quads
//! and JSON-LD later). Prettier's modes and exit codes: print to stdout by default,
//! `--check` / `--list-different` exit 1 when something would change, `--write`
//! rewrites in place, and any error exits 2.

pub(crate) mod config;
pub(crate) mod report;
mod walk;

use anyhow::{Context, Result, bail};
use clap::Args;
use rayon::prelude::*;
use sparkles_fmt::options::{self, OptionError, Value};
use sparkles_fmt::{Detection, FormatError, Language, LinesConfig, Options, Warning};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    #[arg(long, value_name = "NAME", conflicts_with = "paths")]
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
    /// Line formats: in-memory sort budget before spilling (bytes, or with a KiB, MiB, GiB
    /// or TiB suffix)
    #[arg(long, value_name = "SIZE", default_value = "1GiB", value_parser = parse_size)]
    pub sort_memory: u64,
    /// Line formats: the most quads --canonicalize holds in memory
    #[arg(long, value_name = "N", default_value_t = 20_000_000)]
    pub max_canonicalize_quads: u64,
    /// The largest document formatted in memory: every language but N-Triples and
    /// N-Quads, which stream (bytes, or with a KiB, MiB, GiB or TiB suffix)
    #[arg(long, value_name = "SIZE", default_value = "256MiB", value_parser = parse_size)]
    pub max_bytes: u64,
    /// Files formatted in parallel (default: available cores)
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,
}

fn parse_language(s: &str) -> Result<Language, String> {
    Language::from_name(s)
        .ok_or_else(|| "expected sparql, turtle, trig, ntriples, nquads or jsonld".to_string())
}

/// A size: bytes, or a number with a binary suffix (`K`/`KiB`, `M`/`MiB`, `G`/`GiB`,
/// `T`/`TiB`; ASCII case-insensitive, a space allowed before it).
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n: u64 = s[..digits]
        .parse()
        .map_err(|_| format!("expected a size like 512MiB, not '{s}'"))?;
    let shift = match s[digits..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "k" | "kib" => 10,
        "m" | "mib" => 20,
        "g" | "gib" => 30,
        "t" | "tib" => 40,
        _ => return Err(format!("expected a size like 512MiB, not '{s}'")),
    };
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("'{s}' is too large"))
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

    /// What the line formats' streaming needs: the sort budget, the canonicalization
    /// limit and the threads, spilling to the system's temporary directory.
    #[allow(
        dead_code,
        reason = "the streaming of N-Triples and N-Quads calls it once they format"
    )]
    pub fn lines_config(&self) -> LinesConfig {
        LinesConfig {
            sort_memory: self.sort_memory,
            max_canonicalize_quads: self.max_canonicalize_quads,
            threads: self.threads.unwrap_or(0),
            ..LinesConfig::default()
        }
    }
}

/// The flag of a config key, for messages.
fn flag(key: &str) -> String {
    match key {
        "prefix-groups" => "--prefix-group".into(),
        k => format!("--{k}"),
    }
}

/// What a run does with each formatted document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// print the formatted text to stdout
    Print,
    /// `--check` (with `--diff`: a unified diff per changed file on stdout)
    Check { diff: bool },
    /// `--list-different`
    List,
    /// `--write`
    Write,
}

impl Mode {
    fn of(args: &FmtArgs) -> Mode {
        if args.check {
            Mode::Check { diff: args.diff }
        } else if args.list_different {
            Mode::List
        } else if args.write {
            Mode::Write
        } else {
            Mode::Print
        }
    }
}

/// One document to format.
struct Job {
    /// the name in messages: the path as given, or `<stdin>` / `--stdin-filepath`
    name: String,
    /// the file to read and write (`None`: stdin)
    file: Option<PathBuf>,
    /// the path whose extension names the language
    lang_path: Option<PathBuf>,
    /// stdin's text
    text: Option<String>,
    /// stdin whose `--stdin-filepath` an ignore file matches: passed through as it is
    ignored: bool,
    /// `None`: its config file is broken (reported once, before formatting)
    options: Option<Arc<Options>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Status {
    #[default]
    Unchanged,
    Changed,
    Error,
}

/// What formatting one document printed, collected so parallel runs print in order.
#[derive(Default)]
struct Outcome {
    status: Status,
    stdout: String,
    /// errors and `[warn]` lines, after the warnings
    stderr: Vec<String>,
    warnings: Vec<Warning>,
}

impl Outcome {
    fn error(line: Option<String>) -> Outcome {
        Outcome {
            status: Status::Error,
            stderr: line.into_iter().collect(),
            ..Outcome::default()
        }
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
    // the flags alone, so a bad one stops the run before any file
    args.options_over(Options::default())
        .map_err(|e| anyhow::anyhow!("{}: {}", flag(&e.key), e.message))?;
    if args.write && args.paths.is_empty() {
        bail!("--write needs file paths");
    }
    let mode = Mode::of(args);
    let cwd = std::env::current_dir().context("the current directory")?;
    let mut configs = match (&args.config, args.no_config) {
        (_, true) => config::Source::None,
        (Some(path), _) => {
            let c = config::Config::load(path, path);
            if let Err(e) = &c.options {
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
    if matches!(mode, Mode::Check { .. }) {
        eprintln!("Checking formatting...");
    }
    let mut errors = 0;
    let mut broken = HashSet::new();
    // the options of a file in `dir`; a broken config file is reported once, and every
    // file under it fails
    let mut options_in = |dir: &Path| -> Option<Arc<Options>> {
        let base = match configs.for_dir(dir) {
            None => Options::default(),
            Some(c) => match &c.options {
                Ok(o) => o.clone(),
                Err(e) => {
                    if broken.insert(c.path.clone()) {
                        eprintln!("{e}");
                    }
                    return None;
                }
            },
        };
        // the flags passed the same checks over the defaults above
        args.options_over(base).ok().map(Arc::new)
    };
    let mut jobs = Vec::new();
    if args.paths.is_empty() {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading stdin")?;
        let abs = args
            .stdin_filepath
            .as_deref()
            .map(|p| walk::absolute(&cwd, p));
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
            ignored: abs.is_some_and(|a| ignores.ignored(&a, false)),
            options: options_in(&dir),
        });
    } else {
        let (inputs, walk_errors) = walk::collect(&args.paths, &cwd, &ignores);
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
                ignored: false,
                options: options_in(&dir),
            });
        }
    }

    let outcomes: Vec<Outcome> = if jobs.len() > 1 {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(args.threads.unwrap_or(0))
            .build()
            .context("starting the formatting threads")?;
        pool.install(|| jobs.par_iter().map(|j| process(j, args, mode)).collect())
    } else {
        jobs.iter().map(|j| process(j, args, mode)).collect()
    };

    let mut changed = 0;
    let mut once = HashSet::new();
    let mut out = std::io::stdout().lock();
    for (job, o) in jobs.iter().zip(&outcomes) {
        for w in &o.warnings {
            // the same for every file: said once
            if w.code == "option-not-implemented" {
                if once.insert(w.message.clone()) {
                    eprintln!("warning: {}", w.message);
                }
                continue;
            }
            eprintln!("{}", report::warning_line(&job.name, w));
        }
        for l in &o.stderr {
            eprintln!("{l}");
        }
        match o.status {
            Status::Unchanged => {}
            Status::Changed => changed += 1,
            Status::Error => errors += 1,
        }
        if let Err(e) = out.write_all(o.stdout.as_bytes()) {
            // a closed pipe (`| head`) ends the output, not the run's verdict
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(e).context("writing stdout");
            }
        }
    }
    let _ = out.flush();
    if let Mode::Check { .. } = mode
        && let Some(line) = report::check_summary(changed, errors)
    {
        eprintln!("{line}");
    }
    Ok(match mode {
        _ if errors > 0 => 2,
        Mode::Check { .. } | Mode::List if changed > 0 => 1,
        _ => 0,
    })
}

/// Format one document and do with it what `mode` says.
fn process(job: &Job, args: &FmtArgs, mode: Mode) -> Outcome {
    let name = job.name.as_str();
    let fail = |message: &str| Outcome::error(Some(format!("{name}: error: {message}")));
    let Some(opts) = job.options.as_deref() else {
        return Outcome::error(None);
    };
    // the language by name first, so files that are never formatted are not read
    let by_name = match language_by_name(args.language, job.lang_path.as_deref()) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    if let Some(l) = by_name
        && !l.is_implemented()
    {
        return fail(&FormatError::unsupported_language(l).to_string());
    }
    // documents formatted in memory have a size limit; the line formats stream
    let over_limit = |len: u64, lang: Option<Language>| {
        (len > args.max_bytes && !lang.is_some_and(Language::is_line_format))
            .then(|| fail(&too_large_message(len, args.max_bytes, lang)))
    };
    if let (None, Some(path)) = (&job.text, &job.file)
        && let Ok(meta) = std::fs::metadata(path)
        && let Some(o) = over_limit(meta.len(), by_name)
    {
        return o;
    }
    let read;
    let text = match (&job.text, &job.file) {
        (Some(t), _) => t.as_str(),
        (None, Some(path)) => match std::fs::read(path).map(String::from_utf8) {
            Ok(Ok(t)) => {
                read = t;
                read.as_str()
            }
            Ok(Err(_)) => return fail("not UTF-8 text"),
            Err(e) => return fail(&report::io(&e)),
        },
        (None, None) => return Outcome::error(None),
    };
    if job.ignored {
        return Outcome {
            stdout: match mode {
                Mode::Print => text.to_string(),
                _ => String::new(),
            },
            ..Outcome::default()
        };
    }
    let lang = match by_name {
        Some(l) => l,
        None => match sniffed(text) {
            Ok(l) => l,
            Err(e) => return fail(&e),
        },
    };
    if let Some(o) = over_limit(text.len() as u64, Some(lang)) {
        return o;
    }
    let f = match sparkles_fmt::format(text, lang, opts) {
        Ok(f) => f,
        Err(e) => return Outcome::error(Some(report::error_line(name, lang, &e))),
    };
    let mut o = Outcome {
        status: match f.changed {
            true => Status::Changed,
            false => Status::Unchanged,
        },
        warnings: f.warnings,
        ..Outcome::default()
    };
    match mode {
        Mode::Print => o.stdout = f.text,
        Mode::Check { diff } if f.changed => {
            o.stderr.push(format!("[warn] {name}"));
            if diff {
                o.stdout = report::diff(name, text, &f.text);
            }
        }
        Mode::List if f.changed => o.stdout = format!("{name}\n"),
        Mode::Write if f.changed => {
            if let Some(path) = &job.file
                && let Err(e) = write_atomically(path, &f.text)
            {
                return fail(&format!("writing the formatted file: {}", report::io(&e)));
            }
        }
        _ => {}
    }
    o
}

/// The message for a document over `--max-bytes`.
fn too_large_message(len: u64, max: u64, lang: Option<Language>) -> String {
    let hint = match lang {
        Some(Language::Turtle | Language::TriG | Language::JsonLd) => {
            "; convert it to N-Triples or N-Quads, which stream, to format it"
        }
        _ => "",
    };
    format!("{len} bytes is more than --max-bytes ({max}) allows to format in memory{hint}")
}

/// The language from `--language` or the path's extension; `Ok(None)` when the content
/// must tell. Compressed files are refused even with `--language`.
fn language_by_name(
    flag: Option<Language>,
    path: Option<&Path>,
) -> Result<Option<Language>, String> {
    let by_path = path.and_then(sparkles_fmt::detect_path);
    if by_path == Some(Detection::Compressed) {
        return Err("compressed input: decompress first".to_string());
    }
    if flag.is_some() {
        return Ok(flag);
    }
    match by_path {
        Some(Detection::Lang(l)) => Ok(Some(l)),
        Some(Detection::RdfXml) => Err(sparkles_fmt::RDF_XML_MESSAGE.to_string()),
        Some(Detection::SkipInWalk) => Err(format!(
            "cannot tell the language of a .{} file; use --language",
            path.and_then(Path::extension)
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default()
        )),
        _ => Ok(None),
    }
}

/// The language of a document by its content (stdin without a name, or an unknown
/// extension).
fn sniffed(text: &str) -> Result<Language, String> {
    match sparkles_fmt::detect(None, text) {
        Detection::Lang(l) if l.is_implemented() => Ok(l),
        Detection::Lang(l) => Err(FormatError::unsupported_language(l).to_string()),
        Detection::RdfXml => Err(sparkles_fmt::RDF_XML_MESSAGE.to_string()),
        Detection::Compressed | Detection::SkipInWalk | Detection::Unknown => {
            Err("cannot tell the language; use --language".to_string())
        }
    }
}

/// Replace `path` (through symbolic links) with `text`: a temporary file in the same
/// directory with the same permissions, synced, then renamed over it, so a reader sees
/// the old file or the new one, never a partial one.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let dir = target.parent().unwrap_or(Path::new("."));
    let permissions = std::fs::metadata(&target)?.permissions();
    let mut tmp = tempfile::Builder::new()
        .prefix(".sparklesfmt-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().set_permissions(permissions)?;
    tmp.as_file().sync_all()?;
    tmp.persist(&target).map_err(|e| e.error)?;
    // the rename itself
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
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
    fn sizes_and_the_line_formats_config() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("1GiB"), Ok(1 << 30));
        assert_eq!(parse_size("256 MiB"), Ok(256 << 20));
        assert_eq!(parse_size("64k"), Ok(64 << 10));
        assert_eq!(parse_size("2T"), Ok(2 << 40));
        assert!(parse_size("1.5GiB").is_err());
        assert!(parse_size("1GB").is_err());
        assert!(parse_size("99999999999TiB").is_err());
        let a = parse(&[]).unwrap();
        assert_eq!((a.sort_memory, a.max_bytes), (1 << 30, 256 << 20));
        assert_eq!(a.lines_config(), LinesConfig::default());
        let a = parse(&[
            "--sort-memory=64MiB",
            "--max-canonicalize-quads=10",
            "--threads=3",
        ])
        .unwrap();
        let c = a.lines_config();
        assert_eq!(
            (c.sort_memory, c.max_canonicalize_quads, c.threads),
            (64 << 20, 10, 3)
        );
        assert!(parse(&["--max-bytes=lots"]).is_err());
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
    fn languages() {
        let l = |flag, p: &str| language_by_name(flag, Some(Path::new(p)));
        assert_eq!(l(None, "q.rq"), Ok(Some(Language::Sparql)));
        assert_eq!(l(None, "Q.SPARQL"), Ok(Some(Language::Sparql)));
        assert_eq!(l(None, "d.ttl"), Ok(Some(Language::Turtle)));
        assert_eq!(l(None, "q.txt"), Ok(None));
        assert_eq!(
            l(None, "x.owl"),
            Err(sparkles_fmt::RDF_XML_MESSAGE.to_string())
        );
        assert_eq!(
            l(Some(Language::Sparql), "x.owl"),
            Ok(Some(Language::Sparql))
        );
        assert_eq!(
            l(None, "x.json"),
            Err("cannot tell the language of a .json file; use --language".to_string())
        );
        assert_eq!(
            l(Some(Language::JsonLd), "x.json"),
            Ok(Some(Language::JsonLd))
        );
        assert!(
            l(Some(Language::Sparql), "q.rq.gz")
                .unwrap_err()
                .contains("decompress")
        );
        assert_eq!(language_by_name(None, None), Ok(None));
        assert_eq!(sniffed("# c\nSELECT * {}"), Ok(Language::Sparql));
        assert_eq!(
            sniffed("<?xml version=\"1.0\"?>"),
            Err(sparkles_fmt::RDF_XML_MESSAGE.to_string())
        );
        assert_eq!(sniffed("<a> <b> <c> ."), Ok(Language::Turtle));
        assert!(sniffed("# only a comment").is_err());
    }
}
