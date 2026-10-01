//! The files `sparkles fmt` formats: explicit paths as given, directories walked with
//! the gitignore rules, and `.sparklesfmtignore` (or `--ignore-path`) over both.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sparkles_fmt::{Detection, detect_path};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// The ignore file read from the current directory without `--ignore-path`.
pub const IGNORE_FILE: &str = ".sparklesfmtignore";

/// Directories a walk never enters.
const NEVER_ENTER: [&str; 3] = [".git", "node_modules", "target"];

/// `p` made absolute against `cwd`, with `.` and `..` resolved lexically (symbolic links
/// are not resolved, so a pattern matches the path as written).
pub fn absolute(cwd: &Path, p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in cwd.join(p).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// The ignore files (gitignore syntax; patterns are relative to each file's directory).
#[derive(Default)]
pub struct Ignores {
    files: Vec<(PathBuf, Gitignore)>,
}

impl Ignores {
    /// `--ignore-path` files (each must exist), or `./.sparklesfmtignore` if there is one.
    pub fn load(cwd: &Path, given: &[PathBuf]) -> Result<Ignores, String> {
        let default = [PathBuf::from(IGNORE_FILE)];
        let explicit = !given.is_empty();
        let mut files = Vec::new();
        for p in if explicit { given } else { &default[..] } {
            let abs = absolute(cwd, p);
            if !abs.is_file() {
                if explicit {
                    return Err(format!("{}: error: no such ignore file", p.display()));
                }
                continue;
            }
            let root = abs.parent().unwrap_or(Path::new("/")).to_path_buf();
            let mut b = GitignoreBuilder::new(&root);
            if let Some(e) = b.add(&abs) {
                return Err(format!("{}: error: {e}", p.display()));
            }
            let gi = b
                .build()
                .map_err(|e| format!("{}: error: {e}", p.display()))?;
            files.push((root, gi));
        }
        Ok(Ignores { files })
    }

    /// Whether an ignore file matches `abs` (an absolute path) or a directory above it.
    pub fn ignored(&self, abs: &Path, is_dir: bool) -> bool {
        self.files.iter().any(|(root, gi)| {
            abs.starts_with(root)
                && abs != root
                && gi.matched_path_or_any_parents(abs, is_dir).is_ignore()
        })
    }
}

/// A file to format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// the path as given, or as the walk built it from the directory given
    pub path: PathBuf,
    /// absolute, for the config and ignore lookups
    pub abs: PathBuf,
    /// named on the command line, so formatted whatever its extension
    pub explicit: bool,
}

/// Whether a walk formats `path`: only the extensions of the languages this build formats.
pub fn walk_takes(path: &Path) -> bool {
    matches!(detect_path(path), Some(Detection::Lang(l)) if l.is_implemented())
}

/// The files under `paths`, each once, in command-line order (directories sorted by
/// path), and the errors (`path: error: …`) of paths that cannot be read.
pub fn collect(paths: &[PathBuf], cwd: &Path, ignores: &Arc<Ignores>) -> (Vec<Input>, Vec<String>) {
    let mut inputs = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |inputs: &mut Vec<Input>, input: Input| {
        if seen.insert(input.abs.clone()) {
            inputs.push(input);
        }
    };
    for p in paths {
        let abs = absolute(cwd, p);
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{}: error: {}", p.display(), super::report::io(&e)));
                continue;
            }
        };
        // an explicit path an ignore file matches is skipped silently, as in Prettier
        if ignores.ignored(&abs, meta.is_dir()) {
            continue;
        }
        if !meta.is_dir() {
            add(
                &mut inputs,
                Input {
                    path: p.clone(),
                    abs,
                    explicit: true,
                },
            );
            continue;
        }
        let mut found = false;
        let filter = ignores.clone();
        let walk = ignore::WalkBuilder::new(&abs)
            .hidden(false)
            .require_git(false)
            .sort_by_file_path(|a, b| a.cmp(b))
            .filter_entry(move |e| {
                if e.depth() == 0 {
                    return true;
                }
                let is_dir = e.file_type().is_some_and(|t| t.is_dir());
                if is_dir && NEVER_ENTER.iter().any(|n| e.file_name() == *n) {
                    return false;
                }
                !filter.ignored(e.path(), is_dir)
            })
            .build();
        for entry in walk {
            match entry {
                Ok(e) if e.file_type().is_some_and(|t| t.is_file()) && walk_takes(e.path()) => {
                    found = true;
                    let rel = e.path().strip_prefix(&abs).unwrap_or(e.path());
                    let path = match p.as_os_str() == "." {
                        true => rel.to_path_buf(),
                        false => p.join(rel),
                    };
                    let abs = e.into_path();
                    add(
                        &mut inputs,
                        Input {
                            path,
                            abs,
                            explicit: false,
                        },
                    );
                }
                Ok(_) => {}
                Err(e) => errors.push(walk_error(p, &abs, &e)),
            }
        }
        if !found {
            errors.push(format!(
                "{}: error: no files to format in this directory (formatted extensions: {})",
                p.display(),
                EXTENSIONS.join(", ")
            ));
        }
    }
    (inputs, errors)
}

/// The extensions a walk formats, for messages.
const EXTENSIONS: [&str; 3] = [".rq", ".ru", ".sparql"];

/// `path: error: …` with the path the walk failed at, shown under the directory given.
fn walk_error(dir: &Path, abs: &Path, e: &ignore::Error) -> String {
    let mut at = None;
    let mut e = e;
    loop {
        match e {
            ignore::Error::WithPath { path, err } => {
                at.get_or_insert(path);
                e = err;
            }
            ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
                e = err;
            }
            _ => break,
        }
    }
    let shown = match at {
        Some(path) => dir.join(path.strip_prefix(abs).unwrap_or(path)),
        None => dir.to_path_buf(),
    };
    format!("{}: error: {e}", shown.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_fmt::Language;

    #[test]
    fn absolute_paths_are_lexical() {
        let cwd = Path::new("/w/proj");
        assert_eq!(
            absolute(cwd, Path::new("a/./b.rq")),
            Path::new("/w/proj/a/b.rq")
        );
        assert_eq!(
            absolute(cwd, Path::new("../x/q.rq")),
            Path::new("/w/x/q.rq")
        );
        assert_eq!(
            absolute(cwd, Path::new("/abs/q.rq")),
            Path::new("/abs/q.rq")
        );
    }

    #[test]
    fn walks_take_only_implemented_languages() {
        for (p, takes) in [
            ("q.rq", true),
            ("u.RU", true),
            ("q.sparql", true),
            ("d.ttl", Language::Turtle.is_implemented()),
            ("d.rdf", false),
            ("d.ttl.gz", false),
            ("d.json", false),
            ("README.md", false),
            ("noext", false),
        ] {
            assert_eq!(walk_takes(Path::new(p)), takes, "{p}");
        }
    }

    #[test]
    fn walks_and_ignores() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        for f in [
            "q/a.rq",
            "q/b.ru",
            "q/data.ttl",
            "q/x.rdf",
            "q/notes.txt",
            "q/skip/c.rq",
            "q/gen/d.rq",
            "q/.hidden/e.rq",
            "q/node_modules/f.rq",
            "q/target/g.rq",
            "q/.git/h.rq",
            "q/kept.rq",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "ASK {}").unwrap();
        }
        std::fs::write(root.join("q/.gitignore"), "gen/\n").unwrap();
        std::fs::write(root.join(IGNORE_FILE), "skip/\nkept.rq\n").unwrap();
        let ignores = Arc::new(Ignores::load(root, &[]).unwrap());
        let (inputs, errors) = collect(&[PathBuf::from("q")], root, &ignores);
        assert!(errors.is_empty(), "{errors:?}");
        let names: Vec<_> = inputs.iter().map(|i| i.path.clone()).collect();
        let mut walked: Vec<&str> = vec!["q/.hidden/e.rq", "q/a.rq", "q/b.ru"];
        if Language::Turtle.is_implemented() {
            walked.push("q/data.ttl");
        }
        assert_eq!(names, walked.iter().map(PathBuf::from).collect::<Vec<_>>());
        // explicit files: any extension, unless the ignore file matches; .gitignore does
        // not apply
        let (inputs, errors) = collect(
            &[
                "q/data.ttl",
                "q/kept.rq",
                "q/gen/d.rq",
                "q/a.rq",
                "q/../q/a.rq",
                "missing.rq",
            ]
            .map(PathBuf::from),
            root,
            &ignores,
        );
        let names: Vec<_> = inputs.iter().map(|i| i.path.clone()).collect();
        assert_eq!(
            names,
            ["q/data.ttl", "q/gen/d.rq", "q/a.rq"]
                .map(PathBuf::from)
                .to_vec()
        );
        assert!(inputs.iter().all(|i| i.explicit && i.abs.is_absolute()));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("missing.rq: error: "), "{errors:?}");
        // a directory whose files were all named before is not empty
        let (inputs, errors) = collect(&["q/a.rq", "q"].map(PathBuf::from), root, &ignores);
        assert_eq!((inputs.len(), errors.len()), (walked.len(), 0));
        // a directory with nothing to format is an error
        let (_, errors) = collect(&[PathBuf::from("q/gen")], root, &Arc::default());
        assert!(errors.is_empty());
        std::fs::create_dir(root.join("empty")).unwrap();
        let (_, errors) = collect(&[PathBuf::from("empty")], root, &ignores);
        assert!(errors[0].starts_with("empty: error: no files to format"));
        // --ignore-path replaces the default and must exist
        assert!(Ignores::load(root, &[PathBuf::from("nope")]).is_err());
        let custom = Ignores::load(root, &[PathBuf::from("q/.gitignore")]).unwrap();
        assert!(custom.ignored(&root.join("q/gen/d.rq"), false));
        assert!(!custom.ignored(&root.join("q/skip/c.rq"), false));
        assert!(!custom.ignored(&root.join("gen/d.rq"), false));
    }
}
