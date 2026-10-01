//! The test corpora shared by the W3C, property and matrix suites: the W3C SPARQL test
//! files classified by their manifests, the RDF ones ([`rdf`]), the golden inputs, and
//! the list of documented formatter failures (`tests/fmt-known-failures.txt`).
//!
//! The manifests are Turtle. A small reader over the crate's own lexer (whose tokens cover
//! what the manifests use) collects their triples, so classifying the suites depends on
//! neither a reference parser nor the formatter.

#![allow(dead_code)]

pub mod rdf;

use sparkles_fmt::lex::{LexMode, Token, TokenKind, lex};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
const UT: &str = "http://www.w3.org/2009/sparql/tests/test-update#";

/// The `rdf-tests-cg/sparql` directory: `SPARKLES_W3C_DIR`, else the sibling Jena
/// checkout; `None` when absent.
pub fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_W3C_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-arq/testing/rdf-tests-cg/sparql")
        });
    p.exists().then_some(p)
}

/// What the manifests say about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// a positive syntax test, or the query or update of an evaluation test
    Positive,
    /// a negative syntax test: the reference parser must reject it
    Negative,
    /// in no manifest entry
    Unlisted,
}

/// One `.rq` or `.ru` file of the suite.
#[derive(Clone, Debug)]
pub struct Case {
    pub path: PathBuf,
    /// the path under the suite directory, `/`-separated (the key of the known-failure
    /// lists)
    pub rel: String,
    pub kind: Kind,
    /// the short ids of the manifest entries that use it (as the engine's W3C harness
    /// spells them)
    pub ids: Vec<String>,
}

/// Every `.rq` and `.ru` file under `dir`, sorted, classified by every `manifest*.ttl`
/// under `dir`. A file that some entry uses as a negative test is negative.
pub fn cases(dir: &Path) -> Vec<Case> {
    let mut files = Vec::new();
    let mut manifests = Vec::new();
    walk(dir, &mut |p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        match p.extension().and_then(|e| e.to_str()) {
            Some("rq" | "ru") => files.push(p.to_path_buf()),
            Some("ttl") if name.starts_with("manifest") => manifests.push(p.to_path_buf()),
            _ => {}
        }
    });
    files.sort();
    let mut kinds: HashMap<PathBuf, (Kind, Vec<String>)> = HashMap::new();
    for m in &manifests {
        for (file, kind, id) in manifest_entries(m) {
            let e = kinds.entry(file).or_insert((kind, Vec::new()));
            if kind == Kind::Negative {
                e.0 = Kind::Negative;
            }
            e.1.push(id);
        }
    }
    files
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let (kind, ids) = kinds
                .get(&canonical)
                .cloned()
                .unwrap_or((Kind::Unlisted, Vec::new()));
            Case {
                path,
                rel,
                kind,
                ids,
            }
        })
        .collect()
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

/// `(file, kind, short id)` of every syntax and evaluation test in the manifest's
/// `mf:entries`.
fn manifest_entries(path: &Path) -> Vec<(PathBuf, Kind, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let g = TurtleReader::read(&text, path);
    let mut out = Vec::new();
    for list in g.objects_of(&format!("{MF}entries")) {
        for entry in g.list(&list) {
            let Some(ty) = g.object(&entry, &format!("{RDF}type")) else {
                continue;
            };
            let T::Iri(ty) = ty else { continue };
            let kind = match ty.rsplit('#').next().unwrap_or_default() {
                "PositiveSyntaxTest"
                | "PositiveSyntaxTest11"
                | "PositiveUpdateSyntaxTest"
                | "PositiveUpdateSyntaxTest11"
                | "QueryEvaluationTest"
                | "UpdateEvaluationTest" => Kind::Positive,
                "NegativeSyntaxTest"
                | "NegativeSyntaxTest11"
                | "NegativeUpdateSyntaxTest"
                | "NegativeUpdateSyntaxTest11" => Kind::Negative,
                _ => continue,
            };
            let Some(action) = g.object(&entry, &format!("{MF}action")) else {
                continue;
            };
            let file = match action {
                T::Iri(_) => Some(action),
                T::Blank(_) => g
                    .object(&action, &format!("{QT}query"))
                    .or_else(|| g.object(&action, &format!("{UT}request"))),
                T::Lit(_) => None,
            };
            let Some(T::Iri(file)) = file else { continue };
            let Some(file) = file.strip_prefix("file://") else {
                continue;
            };
            let file = PathBuf::from(file);
            let file = std::fs::canonicalize(&file).unwrap_or(file);
            let id = match &entry {
                T::Iri(i) => short_id(i),
                _ => String::new(),
            };
            out.push((file, kind, id));
        }
    }
    out
}

/// A test IRI as the engine's W3C harness abbreviates it in its known-failure list.
fn short_id(id: &str) -> String {
    id.rsplit_once("/sparql/")
        .map(|x| x.1)
        .or_else(|| id.rsplit_once("/data-r2/").map(|x| x.1))
        .or_else(|| id.rsplit_once("/sparql12#").map(|x| x.1))
        .unwrap_or(id)
        .to_string()
}

// --------------------------------------------------------- a small Turtle reader ------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum T {
    Iri(String),
    Blank(usize),
    Lit(String),
}

/// The triples of a Turtle document: IRIs resolved (relative ones against the file),
/// literals kept as written, blank nodes numbered.
pub struct TurtleReader<'a> {
    src: &'a str,
    toks: Vec<Token>,
    i: usize,
    base: PathBuf,
    prefixes: HashMap<String, String>,
    labels: HashMap<String, usize>,
    blanks: usize,
    pub triples: Vec<(T, T, T)>,
}

impl<'a> TurtleReader<'a> {
    pub fn read(src: &'a str, path: &Path) -> TurtleReader<'a> {
        let toks = lex(src, LexMode::Sparql)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .collect();
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut r = TurtleReader {
            src,
            toks,
            i: 0,
            base: path,
            prefixes: HashMap::new(),
            labels: HashMap::new(),
            blanks: 0,
            triples: Vec::new(),
        };
        while r.kind() != TokenKind::Eof {
            let before = r.i;
            r.statement();
            if r.i == before {
                r.i += 1;
            }
        }
        r
    }

    fn kind(&self) -> TokenKind {
        self.toks.get(self.i).map_or(TokenKind::Eof, |t| t.kind)
    }

    fn text(&self) -> &'a str {
        self.toks.get(self.i).map_or("", |t| t.text(self.src))
    }

    fn eat(&mut self, k: TokenKind) -> bool {
        let yes = self.kind() == k;
        if yes {
            self.i += 1;
        }
        yes
    }

    fn statement(&mut self) {
        let word = self.text().to_ascii_lowercase();
        match (self.kind(), word.as_str()) {
            (TokenKind::LangDir, "@prefix") | (TokenKind::Word, "prefix") => {
                self.i += 1;
                let label = self.text().trim_end_matches(':').to_string();
                self.i += 1;
                if let Some(T::Iri(iri)) = self.iri() {
                    self.prefixes.insert(label, iri);
                }
                self.eat(TokenKind::Dot);
            }
            (TokenKind::LangDir, "@base") | (TokenKind::Word, "base") => {
                self.i += 2;
                self.eat(TokenKind::Dot);
            }
            _ => {
                let Some(s) = self.term() else { return };
                if !matches!(self.kind(), TokenKind::Dot) {
                    self.predicate_objects(&s);
                }
                self.eat(TokenKind::Dot);
            }
        }
    }

    fn predicate_objects(&mut self, s: &T) {
        loop {
            if matches!(
                self.kind(),
                TokenKind::Dot | TokenKind::RBracket | TokenKind::Eof
            ) {
                return;
            }
            let p = if self.kind() == TokenKind::Word && self.text() == "a" {
                self.i += 1;
                T::Iri(format!("{RDF}type"))
            } else {
                match self.iri() {
                    Some(p) => p,
                    None => return,
                }
            };
            loop {
                let Some(o) = self.term() else { return };
                self.triples.push((s.clone(), p.clone(), o));
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            if !self.eat(TokenKind::Semicolon) {
                return;
            }
            while self.eat(TokenKind::Semicolon) {}
        }
    }

    fn fresh(&mut self) -> T {
        self.blanks += 1;
        T::Blank(self.blanks)
    }

    fn iri(&mut self) -> Option<T> {
        let text = self.text();
        let iri = match self.kind() {
            TokenKind::IriRef => {
                let iri = &text[1..text.len() - 1];
                if iri.contains(':') {
                    iri.to_string()
                } else if iri.is_empty() {
                    format!("file://{}", self.base.display())
                } else {
                    let dir = self.base.parent().unwrap_or(Path::new("/"));
                    format!("file://{}", dir.join(iri).display())
                }
            }
            TokenKind::PnameLn | TokenKind::PnameNs => {
                let (label, local) = text.split_once(':')?;
                format!("{}{local}", self.prefixes.get(label)?)
            }
            _ => return None,
        };
        self.i += 1;
        Some(T::Iri(iri))
    }

    fn term(&mut self) -> Option<T> {
        let k = self.kind();
        match k {
            TokenKind::IriRef | TokenKind::PnameLn | TokenKind::PnameNs => self.iri(),
            TokenKind::BlankNodeLabel => {
                let label = self.text().to_string();
                self.i += 1;
                let n = match self.labels.get(&label) {
                    Some(&n) => n,
                    None => {
                        let T::Blank(n) = self.fresh() else {
                            unreachable!()
                        };
                        self.labels.insert(label, n);
                        n
                    }
                };
                Some(T::Blank(n))
            }
            TokenKind::Anon => {
                self.i += 1;
                Some(self.fresh())
            }
            TokenKind::LBracket => {
                self.i += 1;
                let b = self.fresh();
                self.predicate_objects(&b);
                self.eat(TokenKind::RBracket);
                Some(b)
            }
            TokenKind::Nil => {
                self.i += 1;
                Some(T::Iri(format!("{RDF}nil")))
            }
            TokenKind::LParen => {
                self.i += 1;
                let mut items = Vec::new();
                while !matches!(self.kind(), TokenKind::RParen | TokenKind::Eof) {
                    let before = self.i;
                    match self.term() {
                        Some(t) => items.push(t),
                        None if self.i == before => self.i += 1,
                        None => {}
                    }
                }
                self.eat(TokenKind::RParen);
                let mut list = T::Iri(format!("{RDF}nil"));
                for item in items.into_iter().rev() {
                    let node = self.fresh();
                    self.triples
                        .push((node.clone(), T::Iri(format!("{RDF}first")), item));
                    self.triples
                        .push((node.clone(), T::Iri(format!("{RDF}rest")), list));
                    list = node;
                }
                Some(list)
            }
            _ if k.is_string() => {
                let lit = self.text().to_string();
                self.i += 1;
                if self.kind() == TokenKind::LangDir {
                    self.i += 1;
                } else if self.eat(TokenKind::HatHat) {
                    self.iri();
                }
                Some(T::Lit(lit))
            }
            _ if k.is_number() || k == TokenKind::Word => {
                let lit = self.text().to_string();
                self.i += 1;
                Some(T::Lit(lit))
            }
            _ => None,
        }
    }

    fn objects_of(&self, p: &str) -> Vec<T> {
        self.triples
            .iter()
            .filter(|(_, pp, _)| *pp == T::Iri(p.to_string()))
            .map(|(_, _, o)| o.clone())
            .collect()
    }

    fn object(&self, s: &T, p: &str) -> Option<T> {
        self.triples
            .iter()
            .find(|(ss, pp, _)| ss == s && *pp == T::Iri(p.to_string()))
            .map(|(_, _, o)| o.clone())
    }

    fn list(&self, head: &T) -> Vec<T> {
        let mut out = Vec::new();
        let mut cur = head.clone();
        while let Some(first) = self.object(&cur, &format!("{RDF}first")) {
            out.push(first);
            match self.object(&cur, &format!("{RDF}rest")) {
                Some(next) => cur = next,
                None => break,
            }
        }
        out
    }
}

// ------------------------------------------------------------- known failures ------

/// `tests/fmt-known-failures.txt`: suite path (or golden name) → reason. Each line is a
/// path, whitespace, and the reason; `#` starts a comment line.
pub fn fmt_known_failures() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fmt-known-failures.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, reason) = line
            .split_once(char::is_whitespace)
            .unwrap_or_else(|| panic!("fmt-known-failures.txt:{}: no reason given", n + 1));
        out.insert(key.to_string(), reason.trim().to_string());
    }
    out
}

/// The engine's `w3c-known-failures.txt`: tests the reference parser itself gets wrong.
pub fn w3c_known_failures() -> BTreeSet<String> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../sparkles/tests/w3c-known-failures.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

// ---------------------------------------------------------------- the corpora ------

/// The golden inputs: `(name, text)` for every `tests/golden/sparql/*.in.rq`.
pub fn golden_inputs() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/sparql");
    let mut v: Vec<(String, String)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let file = p.file_name()?.to_str()?.to_string();
            let (name, _) = file.split_once(".in.")?;
            Some((
                format!("golden/sparql/{name}"),
                std::fs::read_to_string(&p).ok()?,
            ))
        })
        .collect();
    v.sort();
    v
}

/// The documents every property and matrix test draws from: the golden inputs and, when
/// the suite is present, every positive W3C file that is not a documented failure.
pub fn positive_texts() -> &'static [(String, String)] {
    static CORPUS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    CORPUS.get_or_init(|| {
        let known = fmt_known_failures();
        let mut v = golden_inputs();
        if let Some(dir) = suite_dir() {
            for c in cases(&dir) {
                if c.kind != Kind::Positive || known.contains_key(&c.rel) {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&c.path) {
                    v.push((c.rel, text));
                }
            }
        }
        v.retain(|(name, _)| !known.contains_key(name));
        v
    })
}

/// `f` over `items` on every core, results in order.
pub fn par_map<I: Sync, R: Send>(items: &[I], f: impl Fn(&I) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = items.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|c| s.spawn(|| c.iter().map(&f).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a worker"))
            .collect()
    })
}
