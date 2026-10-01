//! `.sparklesfmt.toml` (or `sparklesfmt.toml`): found by walking up from each file's
//! directory, the nearest one wins, files are never merged, and the flags override it.
//! Every value goes through [`options::set`], like the flags and the HTTP options.

use sparkles_fmt::Options;
use sparkles_fmt::options::{self, Value};
use std::collections::HashMap;
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
}

impl Config {
    /// Read and check `path`, naming it `shown` in messages.
    pub fn load(path: &Path, shown: &Path) -> Config {
        let options = match std::fs::read_to_string(path) {
            Ok(text) => parse(&text).map_err(|e| e.render(shown)),
            Err(e) => Err(format!(
                "{}: error: {}",
                shown.display(),
                super::report::io(&e)
            )),
        };
        Config {
            path: shown.to_path_buf(),
            options,
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

/// The options of a config file's text, over the defaults.
pub fn parse(text: &str) -> Result<Options, ConfigError> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
        let (line, column) = e
            .span()
            .map_or((0, 0), |s| sparkles_fmt::line_col(text, s.start));
        ConfigError::Toml {
            message: e.message().trim_end().to_string(),
            line,
            column,
        }
    })?;
    let mut o = Options::default();
    for (key, value) in table {
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
