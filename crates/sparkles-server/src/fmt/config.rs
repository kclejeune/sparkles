//! `.sparklesfmt.toml` (or `sparklesfmt.toml`): found by walking up from each file's
//! directory, the nearest one wins, files are never merged, and the flags override it.
//! Every value goes through [`options::set`], like the flags and the HTTP options. The
//! file's `[lint]` table sets the severity of `sparkles lint`'s rules
//! ([`LintOptions::set`]). Its `[prefixes]` and `[lsp]` tables are read by `sparkles lsp`
//! for completion ([`Editor`], spec C20 §8), and the formatter only checks them.

use sparkles_fmt::Options;
use sparkles_fmt::lint::LintOptions;
use sparkles_fmt::options::{self, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The names of a config file, in order of preference within one directory.
pub const NAMES: [&str; 2] = [".sparklesfmt.toml", "sparklesfmt.toml"];

/// A config file and what it says: its options, or the error that names the file (and
/// the key, for a bad option).
#[derive(Debug)]
pub struct Config {
    /// the path in messages
    pub path: PathBuf,
    pub options: Result<Options, String>,
    /// the `[lint]` table's rule levels
    pub lint: Result<LintOptions, String>,
    /// the `[prefixes]` and `[lsp]` tables
    pub editor: Result<Editor, String>,
}

/// What `sparkles lsp` reads from a config file besides the options: the `[prefixes]`
/// table, and the dataset of the `[lsp]` table whose prefixes it asks a server for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Editor {
    /// prefix name to IRI
    pub prefixes: BTreeMap<String, String>,
    /// the `[lsp]` table's `server` and `dataset`
    pub server: Option<ServerSource>,
}

/// A server's dataset whose effective prefixes the language server reads (C20 §8.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerSource {
    pub server: String,
    pub dataset: String,
}

impl Config {
    /// Read and check `path`, naming it `shown` in messages.
    pub fn load(path: &Path, shown: &Path) -> Config {
        let (options, lint, editor) = match std::fs::read_to_string(path) {
            Ok(text) => (
                parse(&text).map_err(|e| e.render(shown)),
                parse_lint(&text).map_err(|e| e.render(shown)),
                parse_editor(&text).map_err(|e| e.render(shown)),
            ),
            Err(e) => {
                let m = format!("{}: error: {}", shown.display(), super::report::io(&e));
                (Err(m.clone()), Err(m.clone()), Err(m))
            }
        };
        Config {
            path: shown.to_path_buf(),
            options,
            lint,
            editor,
        }
    }
}

/// Where the options of each file come from.
pub enum Source {
    /// `--no-config`: the defaults
    None,
    /// `--config FILE`: one file for every input
    Fixed(Arc<Config>),
    /// the nearest config file above each input (named relative to `cwd` in messages),
    /// remembered per directory
    Discover {
        cwd: PathBuf,
        cache: HashMap<PathBuf, Option<Arc<Config>>>,
    },
}

impl Source {
    /// The config of a file in `dir` (an absolute directory), or `None` for the defaults.
    pub fn for_dir(&mut self, dir: &Path) -> Option<Arc<Config>> {
        match self {
            Source::None => None,
            Source::Fixed(c) => Some(c.clone()),
            Source::Discover { cwd, cache } => discover(cwd, cache, dir),
        }
    }
}

/// The options of a file in `dir` (an absolute directory) from the nearest config file,
/// read afresh (so an edited file takes effect at once), or the defaults when there is
/// none. The error names the file by its absolute path. For `sparkles lsp`.
pub(crate) fn options_for_dir(dir: &Path) -> Result<Options, String> {
    match discover(Path::new(""), &mut HashMap::new(), dir) {
        None => Ok(Options::default()),
        Some(c) => c.options.clone(),
    }
}

/// The lint rule levels of a file in `dir`, as [`options_for_dir`] finds its options.
pub(crate) fn lint_for_dir(dir: &Path) -> Result<LintOptions, String> {
    match discover(Path::new(""), &mut HashMap::new(), dir) {
        None => Ok(LintOptions::default()),
        Some(c) => c.lint.clone(),
    }
}

/// The `[prefixes]` and `[lsp]` tables for a file in `dir`, as [`options_for_dir`] finds
/// its options.
pub(crate) fn editor_for_dir(dir: &Path) -> Result<Editor, String> {
    match discover(Path::new(""), &mut HashMap::new(), dir) {
        None => Ok(Editor::default()),
        Some(c) => c.editor.clone(),
    }
}

/// The nearest config file at or above `dir`, remembering the answer for every directory
/// on the way up.
fn discover(
    cwd: &Path,
    cache: &mut HashMap<PathBuf, Option<Arc<Config>>>,
    dir: &Path,
) -> Option<Arc<Config>> {
    let mut seen = Vec::new();
    let mut found = None;
    let mut at = Some(dir);
    while let Some(d) = at {
        if let Some(hit) = cache.get(d) {
            found = hit.clone();
            break;
        }
        seen.push(d.to_path_buf());
        if let Some(path) = NAMES.iter().map(|n| d.join(n)).find(|p| p.is_file()) {
            let shown = path.strip_prefix(cwd).unwrap_or(&path);
            found = Some(Arc::new(Config::load(&path, shown)));
            break;
        }
        at = d.parent();
    }
    for d in seen {
        cache.insert(d, found.clone());
    }
    found
}

/// A config file's error: a TOML syntax error with its position, or a bad key or value.
#[derive(Debug, PartialEq)]
pub enum ConfigError {
    Toml {
        message: String,
        line: u32,
        column: u32,
    },
    Option(options::OptionError),
}

impl ConfigError {
    /// `file:L:C: error: …` or `file: error: key: …`
    pub fn render(&self, path: &Path) -> String {
        let p = path.display();
        match self {
            ConfigError::Toml {
                message, line: 0, ..
            } => format!("{p}: error: invalid TOML: {message}"),
            ConfigError::Toml {
                message,
                line,
                column,
            } => format!("{p}:{line}:{column}: error: invalid TOML: {message}"),
            ConfigError::Option(e) => format!("{p}: error: {}: {}", e.key, e.message),
        }
    }
}

/// A config file's text as a TOML table.
fn toml_table(text: &str) -> Result<toml::Table, ConfigError> {
    text.parse().map_err(|e: toml::de::Error| {
        let (line, column) = e
            .span()
            .map_or((0, 0), |s| sparkles_fmt::line_col(text, s.start));
        ConfigError::Toml {
            message: e.message().trim_end().to_string(),
            line,
            column,
        }
    })
}

/// The options of a config file's text, over the defaults. The `[prefixes]` and `[lsp]`
/// tables are checked here too, so that a mistake in them fails `sparkles fmt` as it
/// fails the language server.
pub fn parse(text: &str) -> Result<Options, ConfigError> {
    let table = toml_table(text)?;
    editor_of(&table)?;
    let mut o = Options::default();
    for (key, value) in table {
        // the tables of `sparkles lint` ([`parse_lint`]) and of the language server
        // ([`parse_editor`])
        if matches!(key.as_str(), "lint" | "prefixes" | "lsp") && value.is_table() {
            continue;
        }
        let bad = |message: String| {
            ConfigError::Option(options::OptionError {
                key: key.clone(),
                message,
            })
        };
        match options::kebab(&key) {
            Some(k) if k == key => {}
            // a camelCase name, as the HTTP options spell it
            Some(k) => return Err(bad(format!("unknown option (did you mean {k}?)"))),
            None => return Err(bad("unknown option".to_string())),
        }
        let value = match value {
            toml::Value::Boolean(b) => Value::Bool(b),
            toml::Value::Integer(i) => Value::Int(i),
            toml::Value::String(s) => Value::Str(s),
            toml::Value::Array(groups) if key == "prefix-groups" => {
                Value::Groups(prefix_groups(groups).map_err(bad)?)
            }
            other => {
                return Err(bad(format!("{} is not a valid value", type_name(&other))));
            }
        };
        options::set(&mut o, &key, value).map_err(ConfigError::Option)?;
    }
    Ok(o)
}

/// The `[lint]` table of a config file's text: `rule = "error" | "warning" | "info" |
/// "hint" | "off"`, over the rules' defaults.
pub fn parse_lint(text: &str) -> Result<LintOptions, ConfigError> {
    let table = toml_table(text)?;
    let mut o = LintOptions::default();
    let Some(lint) = table.get("lint") else {
        return Ok(o);
    };
    let bad = |key: &str, message: String| {
        ConfigError::Option(options::OptionError {
            key: key.to_string(),
            message,
        })
    };
    let lint = lint
        .as_table()
        .ok_or_else(|| bad("lint", "expected a table of rule levels".into()))?;
    for (rule, level) in lint {
        let key = format!("lint.{rule}");
        let level = level.as_str().ok_or_else(|| {
            bad(
                &key,
                "expected \"error\", \"warning\", \"info\", \"hint\" or \"off\"".into(),
            )
        })?;
        o.set(rule, level).map_err(|e| {
            // the rule name leads the message already
            let message = e
                .split_once(": ")
                .map_or(e.as_str(), |(_, m)| m)
                .to_string();
            bad(&key, message)
        })?;
    }
    Ok(o)
}

/// The `[prefixes]` and `[lsp]` tables of a config file's text (C20 §8).
pub fn parse_editor(text: &str) -> Result<Editor, ConfigError> {
    editor_of(&toml_table(text)?)
}

fn editor_of(table: &toml::Table) -> Result<Editor, ConfigError> {
    let bad = |key: &str, message: &str| {
        ConfigError::Option(options::OptionError {
            key: key.to_string(),
            message: message.to_string(),
        })
    };
    let mut e = Editor::default();
    if let Some(p) = table.get("prefixes") {
        let p = p.as_table().ok_or_else(|| {
            bad(
                "prefixes",
                "expected a table of prefix names and IRIs, such as ex = \"http://example.org/\"",
            )
        })?;
        for (name, iri) in p {
            let key = format!("prefixes.{name}");
            if !valid_prefix_name(name) {
                return Err(bad(
                    &key,
                    "not a prefix name, which is a letter followed by letters, digits, _, - or . and does not end in .",
                ));
            }
            let iri = iri
                .as_str()
                .ok_or_else(|| bad(&key, "expected an IRI as a string"))?;
            if let Err(err) = oxiri::Iri::parse(iri) {
                return Err(bad(&key, &format!("not an absolute IRI: {err}")));
            }
            e.prefixes.insert(name.clone(), iri.to_string());
        }
    }
    if let Some(l) = table.get("lsp") {
        let l = l
            .as_table()
            .ok_or_else(|| bad("lsp", "expected a table with server and dataset"))?;
        let (mut server, mut dataset) = (None, None);
        for (k, v) in l {
            let key = format!("lsp.{k}");
            let v = v
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| bad(&key, "expected a non-empty string"))?;
            match k.as_str() {
                "server" => server = Some(v.to_string()),
                "dataset" => dataset = Some(v.to_string()),
                _ => return Err(bad(&key, "unknown key, the table holds server and dataset")),
            }
        }
        e.server = match (server, dataset) {
            (Some(server), Some(dataset)) => Some(ServerSource { server, dataset }),
            (None, None) => None,
            (Some(_), None) => return Err(bad("lsp.dataset", "missing, server needs a dataset")),
            (None, Some(_)) => return Err(bad("lsp.server", "missing, dataset needs a server")),
        };
    }
    Ok(e)
}

/// A SPARQL `PN_PREFIX` in its ASCII subset, or the empty name, as a dataset's prefixes
/// accept.
pub(crate) fn valid_prefix_name(p: &str) -> bool {
    let b = p.as_bytes();
    p.is_empty()
        || (b[0].is_ascii_alphabetic()
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
            && !p.ends_with('.'))
}

/// `[["rdf", "rdfs"], ["ex"]]`
fn prefix_groups(groups: Vec<toml::Value>) -> Result<Vec<Vec<String>>, String> {
    let expected = || {
        "expected an array of arrays of prefix labels, such as [[\"rdf\", \"rdfs\", \"xsd\"]]"
            .to_string()
    };
    groups
        .into_iter()
        .map(|g| match g {
            toml::Value::Array(labels) => labels
                .into_iter()
                .map(|l| match l {
                    toml::Value::String(s) => Ok(s),
                    _ => Err(expected()),
                })
                .collect(),
            _ => Err(expected()),
        })
        .collect()
}

fn type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a boolean",
        toml::Value::Datetime(_) => "a date-time",
        toml::Value::Array(_) => "an array",
        toml::Value::Table(_) => "a table",
    }
}

/// A bad entry of a JSON `options` object: the option's camelCase name (`None` when the
/// object itself is wrong) and what is wrong with it.
pub(crate) struct JsonOptionError {
    pub option: Option<String>,
    pub message: String,
}

/// Apply a JSON `options` object with camelCase keys (`POST /$/format`, the MCP `format`
/// tool) to `o`. `null` values are skipped.
pub(crate) fn json_options(v: &serde_json::Value, o: &mut Options) -> Result<(), JsonOptionError> {
    use serde_json::Value as J;
    let Some(obj) = v.as_object() else {
        return Err(JsonOptionError {
            option: None,
            message: "`options` must be an object".into(),
        });
    };
    let bad = |name: &str, message: String| JsonOptionError {
        option: Some(name.to_string()),
        message,
    };
    for (name, v) in obj {
        let Some(key) = options::KEYS
            .iter()
            .find(|(_, camel)| camel == name)
            .map(|(kebab, _)| *kebab)
        else {
            return Err(bad(name, "unknown option".into()));
        };
        let value = match v {
            J::Null => continue,
            J::Bool(b) => Value::Bool(*b),
            J::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Str(n.to_string()),
            },
            J::String(s) => Value::Str(s.clone()),
            J::Array(groups) => {
                let groups: Option<Vec<Vec<String>>> = groups
                    .iter()
                    .map(|g| {
                        g.as_array()?
                            .iter()
                            .map(|l| l.as_str().map(str::to_string))
                            .collect()
                    })
                    .collect();
                match groups {
                    Some(g) => Value::Groups(g),
                    None => {
                        return Err(bad(
                            name,
                            "expected an array of arrays of prefix labels".into(),
                        ));
                    }
                }
            }
            J::Object(_) => Value::Str(v.to_string()),
        };
        options::set(o, key, value).map_err(|e| bad(name, e.message))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        parse(text)
            .unwrap_err()
            .render(Path::new("dir/.sparklesfmt.toml"))
    }

    #[test]
    fn every_key() {
        let o = parse(
            r#"
            line-width = 80
            indent-width = 4
            sort = true
            prune-prefixes = true
            directive-style = "turtle"
            prefix-groups = [["rdf", "rdfs"], ["", "ex"]]
            type-shorthand = false
            compact-iris = false
            quote-style = "preserve"
            operator-position = "trailing"
            turtle-layout = "conventional"
            align-values = true
            "#,
        )
        .unwrap();
        let mut expected = Options::default();
        for (k, v) in [
            ("line-width", Value::Int(80)),
            ("indent-width", Value::Int(4)),
            ("sort", Value::Bool(true)),
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
            ("compact-iris", Value::Bool(false)),
            ("quote-style", Value::Str("preserve".into())),
            ("operator-position", Value::Str("trailing".into())),
            ("turtle-layout", Value::Str("conventional".into())),
            ("align-values", Value::Bool(true)),
        ] {
            options::set(&mut expected, k, v).unwrap();
        }
        assert_eq!(o, expected);
        assert_eq!(parse("").unwrap(), Options::default());
        assert_eq!(parse("prefix-groups = []").unwrap(), Options::default());
    }

    #[test]
    fn errors_name_the_file_and_the_key() {
        assert_eq!(
            err("line-widht = 80"),
            "dir/.sparklesfmt.toml: error: line-widht: unknown option"
        );
        assert_eq!(
            err("lineWidth = 80"),
            "dir/.sparklesfmt.toml: error: lineWidth: unknown option (did you mean line-width?)"
        );
        assert_eq!(
            err("quote-style = \"singel\""),
            "dir/.sparklesfmt.toml: error: quote-style: expected \"double\" or \"preserve\", got \"singel\""
        );
        assert!(
            err("prefix-groups = [[\"rdf\"], [\"rdfs\", \"rdf\"]]")
                .starts_with("dir/.sparklesfmt.toml: error: prefix-groups: ")
        );
        assert!(err("prefix-groups = [\"rdf\"]").starts_with(
            "dir/.sparklesfmt.toml: error: prefix-groups: expected an array of arrays"
        ));
        assert_eq!(
            err("line-width = 1.5"),
            "dir/.sparklesfmt.toml: error: line-width: a float is not a valid value"
        );
        assert!(err("line-width = 500").contains("line-width: expected an integer from 40 to 400"));
        assert!(err("sort = \"yes\"").contains("sort: expected true or false, got a string"));
        // a syntax error has a position
        assert!(
            err("line-width = 80\nsort = \n").starts_with("dir/.sparklesfmt.toml:2:"),
            "{}",
            err("line-width = 80\nsort = \n")
        );
    }

    #[test]
    fn the_lint_table() {
        use sparkles_fmt::lint::Severity;
        let text =
            "line-width = 80\n[lint]\nunused-prefix = \"error\"\ncartesian-product = \"off\"\n";
        // the formatter skips the table, and the lint reads only it
        assert_eq!(parse(text).unwrap().line_width, 80);
        let l = parse_lint(text).unwrap();
        assert_eq!(l.level("unused-prefix"), Some(Severity::Error));
        assert_eq!(l.level("cartesian-product"), None);
        assert_eq!(l.level("filter-scope"), Some(Severity::Warning));
        assert_eq!(parse_lint("").unwrap(), LintOptions::default());
        let lint_err = |t: &str| {
            parse_lint(t)
                .unwrap_err()
                .render(Path::new("dir/.sparklesfmt.toml"))
        };
        assert_eq!(
            lint_err("[lint]\nno-such-rule = \"off\""),
            "dir/.sparklesfmt.toml: error: lint.no-such-rule: unknown lint rule"
        );
        assert!(lint_err("[lint]\nunused-prefix = 1").contains("lint.unused-prefix: expected"));
        assert!(lint_err("lint = 3").contains("lint: expected a table"));
        // a `lint` key that is not a table is still the formatter's error
        assert!(parse("lint = 3").is_err());
    }

    #[test]
    fn the_prefixes_and_lsp_tables() {
        let text = "line-width = 80\n[prefixes]\nkclj = \"https://kclj.io/sparkles/\"\n\"\" = \"http://example.org/\"\n[lsp]\nserver = \"https://sparkles.example.org\"\ndataset = \"slurp\"\n";
        // the formatter and the lint skip the tables
        assert_eq!(parse(text).unwrap().line_width, 80);
        assert_eq!(parse_lint(text).unwrap(), LintOptions::default());
        let e = parse_editor(text).unwrap();
        assert_eq!(e.prefixes["kclj"], "https://kclj.io/sparkles/");
        assert_eq!(e.prefixes[""], "http://example.org/");
        assert_eq!(
            e.server,
            Some(ServerSource {
                server: "https://sparkles.example.org".into(),
                dataset: "slurp".into()
            })
        );
        assert_eq!(parse_editor("").unwrap(), Editor::default());
        let err = |t: &str| {
            parse(t)
                .unwrap_err()
                .render(Path::new("dir/.sparklesfmt.toml"))
        };
        assert!(
            err("[prefixes]\n\"bad name\" = \"http://x/\"")
                .starts_with("dir/.sparklesfmt.toml: error: prefixes.bad name: not a prefix name")
        );
        assert!(err("[prefixes]\nex = \"relative/\"").contains("prefixes.ex: not an absolute IRI"));
        assert!(err("[prefixes]\nex = 1").contains("prefixes.ex: expected an IRI"));
        assert!(err("prefixes = 1").contains("prefixes: expected a table"));
        assert!(err("[lsp]\nserver = \"https://x\"").contains("lsp.dataset: missing"));
        assert!(err("[lsp]\nport = \"1\"").contains("lsp.port: unknown key"));
    }

    #[test]
    fn nearest_wins() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let deep = root.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.join(".sparklesfmt.toml"), "line-width = 80").unwrap();
        std::fs::write(root.join("a/b/sparklesfmt.toml"), "line-width = 60").unwrap();
        let mut s = Source::Discover {
            cwd: PathBuf::new(),
            cache: HashMap::new(),
        };
        let width = |s: &mut Source, dir: &Path| {
            s.for_dir(dir)
                .map(|c| c.options.as_ref().unwrap().line_width)
        };
        assert_eq!(width(&mut s, &deep), Some(60));
        assert_eq!(width(&mut s, &root.join("a/b")), Some(60));
        assert_eq!(width(&mut s, &root.join("a")), Some(80));
        assert_eq!(width(&mut s, root), Some(80));
        // the dotfile wins within one directory
        std::fs::write(root.join("a/.sparklesfmt.toml"), "line-width = 70").unwrap();
        let mut s = Source::Discover {
            cwd: PathBuf::new(),
            cache: HashMap::new(),
        };
        std::fs::write(root.join("a/sparklesfmt.toml"), "line-width = 90").unwrap();
        assert_eq!(width(&mut s, &root.join("a")), Some(70));
        assert_eq!(width(&mut Source::None, &deep), None);
    }
}
