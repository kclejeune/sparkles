//! Deterministic adapters that read the memory and instruction files of coding agents
//! (spec C18 §8.10) and turn each file into a source with structural facts, without a
//! model.
//!
//! The adapters read Claude Code's memory files, its `MEMORY.md` index and its
//! instruction files, Codex's `AGENTS.md` files and its generated memories, and a
//! generic set of Markdown instruction files (Gemini CLI, Cursor, GitHub Copilot and any
//! file given by path). Every text passes [`redact`] before it is parsed, so no fact and
//! no quote carries a secret the patterns recognize. Nothing here talks to a server: the
//! command line sends the facts with `POST /{ds}/facts`.
//!
//! Every adapter reads only below its roots, follows an `@path` import only inside the
//! project or the harness's user directory, and skips any file whose first line is the
//! generated marker of the brief ([`MARKER`]).

pub mod frontmatter;
pub mod ids;
pub mod project;
pub mod redact;
pub mod vocab;

mod adapters;

pub use adapters::{Request, Roots, Scan, Skipped, scan};
pub use ids::Ctx;
pub use project::Project;

/// The first line of a file that `sparkles memory brief --write` generates. Every
/// adapter skips a file that starts with it.
pub const MARKER: &str = "<!-- sparkles:generated brief; do not edit; not for import -->";

/// The most bytes of one file the import reads.
pub const MAX_FILE_BYTES: u64 = 2 << 20;

/// Whether a text starts with the generated marker.
pub fn is_generated(text: &str) -> bool {
    text.lines().next().is_some_and(|l| l.trim() == MARKER)
}

/// An adapter, by the name the command line uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Adapter {
    ClaudeCode,
    Codex,
    Generic,
}

impl Adapter {
    pub const ALL: [Adapter; 3] = [Adapter::ClaudeCode, Adapter::Codex, Adapter::Generic];

    pub fn parse(s: &str) -> Option<Adapter> {
        match s {
            "claude-code" | "claude" => Some(Adapter::ClaudeCode),
            "codex" => Some(Adapter::Codex),
            "generic" => Some(Adapter::Generic),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Adapter::ClaudeCode => "claude-code",
            Adapter::Codex => "codex",
            Adapter::Generic => "generic",
        }
    }
}

/// The harness a file belongs to; it names the graph segment and `mem:harness`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Harness {
    ClaudeCode,
    Codex,
    GeminiCli,
    Cursor,
    Generic,
}

impl Harness {
    pub fn segment(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::GeminiCli => "gemini-cli",
            Harness::Cursor => "cursor",
            Harness::Generic => "generic",
        }
    }

    pub fn iri(self) -> String {
        vocab::mem(match self {
            Harness::ClaudeCode => "ClaudeCode",
            Harness::Codex => "Codex",
            Harness::GeminiCli => "GeminiCli",
            Harness::Cursor => "Cursor",
            Harness::Generic => "Generic",
        })
    }

    pub fn parse(s: &str) -> Option<Harness> {
        [
            Harness::ClaudeCode,
            Harness::Codex,
            Harness::GeminiCli,
            Harness::Cursor,
            Harness::Generic,
        ]
        .into_iter()
        .find(|h| h.segment() == s)
    }
}

/// What a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Memory,
    Index,
    Instructions,
}

impl FileKind {
    pub fn name(self) -> &'static str {
        match self {
            FileKind::Memory => "memory",
            FileKind::Index => "index",
            FileKind::Instructions => "instructions",
        }
    }
}

/// The scope of an instruction file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    Local,
    Rule,
    Managed,
}

impl Scope {
    pub fn iri(self) -> String {
        vocab::mem(match self {
            Scope::User => "UserScope",
            Scope::Project => "ProjectScope",
            Scope::Local => "LocalScope",
            Scope::Rule => "RuleScope",
            Scope::Managed => "ManagedScope",
        })
    }
}

/// The object of a fact.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Obj {
    Iri(String),
    Literal {
        value: String,
        datatype: Option<String>,
    },
}

impl Obj {
    pub fn lit(v: impl Into<String>) -> Obj {
        Obj::Literal {
            value: v.into(),
            datatype: None,
        }
    }

    pub fn typed(v: impl Into<String>, dt: &str) -> Obj {
        Obj::Literal {
            value: v.into(),
            datatype: Some(dt.to_string()),
        }
    }

    /// The term in SPARQL syntax, as `assert_facts` reads it.
    pub fn sparql(&self) -> String {
        match self {
            Obj::Iri(i) => format!("<{i}>"),
            Obj::Literal { value, datatype } => {
                let mut s = quoted(value);
                if let Some(d) = datatype {
                    s.push_str(&format!("^^<{d}>"));
                }
                s
            }
        }
    }
}

/// A string literal with SPARQL escapes.
pub fn quoted(v: &str) -> String {
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if c.is_control() => s.push_str(&format!("\\u{:04X}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// One structural fact: subject and predicate IRIs, an object, and the line it came
/// from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fact {
    pub s: String,
    pub p: String,
    pub o: Obj,
    pub quote: Option<String>,
}

/// One file, read and turned into facts.
#[derive(Clone, Debug)]
pub struct FileImport {
    pub adapter: Adapter,
    pub harness: Harness,
    pub kind: FileKind,
    pub scope: Option<Scope>,
    /// the file, absolute
    pub path: std::path::PathBuf,
    /// `mem:filePath`: relative to the harness's root for its scope
    pub rel_path: String,
    /// the file's key in its graph's IRI
    pub key: String,
    /// the file's graph, which is also its source's IRI
    pub graph: String,
    /// the memory or instruction entity (none for an index)
    pub entity: Option<String>,
    /// `sha256:<hex>` of the file's bytes
    pub digest: String,
    /// `sha256:<hex>` of the bytes after the frontmatter, which rename detection compares
    pub body_digest: String,
    pub title: String,
    /// the pattern name of each redaction
    pub redactions: Vec<String>,
    /// the structural facts, the source's description included
    pub facts: Vec<Fact>,
    /// the file's text after redaction: the text `register_source` receives
    pub text: String,
}

/// The first line of a file that `sparkles memory export --sources` converted from
/// another source: `<!-- sparkles:copy-of <IRI> exported DATE -->`. An import of the
/// file records `mem:copyOf` that IRI, so the copy does not count as a second source.
pub const COPY_OF: &str = "<!-- sparkles:copy-of ";

/// The sources a converted file names in its leading copy comments, with each line. The
/// comments come first, or right after the frontmatter of a file that has one.
pub fn copies_of(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim_end() == "---") {
        lines.next();
        for l in lines.by_ref() {
            if l.trim_end() == "---" {
                break;
            }
        }
    }
    for line in lines {
        let t = line.trim();
        let Some(rest) = t.strip_prefix(COPY_OF) else {
            break;
        };
        if let Some(r) = rest.strip_prefix('<')
            && let Some((iri, _)) = r.split_once('>')
            && !iri.is_empty()
            && !iri.contains(char::is_whitespace)
        {
            out.push((iri.to_string(), t.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_comments() {
        let t = "<!-- sparkles:copy-of <urn:a> exported 2026-10-10 -->\n<!-- sparkles:copy-of <urn:b> exported 2026-10-10 -->\n## a\n<!-- sparkles:copy-of <urn:c> -->\n";
        let c: Vec<String> = copies_of(t).into_iter().map(|(i, _)| i).collect();
        assert_eq!(c, ["urn:a", "urn:b"]);
        let t = "---\nname: x\n---\n<!-- sparkles:copy-of <urn:a> exported 2026-10-10 -->\nbody\n";
        assert_eq!(copies_of(t).len(), 1);
        assert!(copies_of("body\n<!-- sparkles:copy-of <urn:a> -->\n").is_empty());
    }
}
