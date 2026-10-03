//! A configuration file as a graph, with the lookups the converter needs: objects of a
//! property, RDF lists, typed values and file references.

use anyhow::{Context, Result, bail};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const FUSEKI: &str = "http://jena.apache.org/fuseki#";
pub const JA: &str = "http://jena.hpl.hp.com/2005/11/Assembler#";
pub const TDB2: &str = "http://jena.apache.org/2016/tdb#";
pub const TDB1: &str = "http://jena.hpl.hp.com/2008/tdb#";
pub const TEXT: &str = "http://jena.apache.org/text#";
pub const GEO: &str = "http://jena.apache.org/geosparql#";
pub const ACCESS: &str = "http://jena.apache.org/access#";
pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// One parsed configuration file.
pub struct ConfigGraph {
    /// the file, for messages and relative paths
    pub path: PathBuf,
    triples: Vec<Triple>,
    by_subject: HashMap<NamedOrBlankNode, Vec<usize>>,
}

/// The RDF syntax of a configuration file, from its extension.
pub fn format_of(path: &Path) -> Option<oxrdfio::RdfFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    oxrdfio::RdfFormat::from_extension(&ext)
}

impl ConfigGraph {
    pub fn read(path: &Path) -> Result<ConfigGraph> {
        let format = format_of(path)
            .with_context(|| format!("{}: not an RDF file extension", path.display()))?;
        let body = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let base = file_iri(&abs);
        Self::parse(&body, format, &base, path)
    }

    pub fn parse(
        body: &[u8],
        format: oxrdfio::RdfFormat,
        base: &str,
        path: &Path,
    ) -> Result<ConfigGraph> {
        let mut triples = Vec::new();
        let parser = oxrdfio::RdfParser::from_format(format)
            .with_base_iri(base)
            .unwrap_or_else(|_| {
                oxrdfio::RdfParser::from_format(format)
                    .with_base_iri("file:///")
                    .expect("a valid base IRI")
            });
        for q in parser.for_slice(body) {
            let q = q.with_context(|| format!("{}: not valid RDF", path.display()))?;
            triples.push(Triple::from(q));
        }
        let mut by_subject: HashMap<NamedOrBlankNode, Vec<usize>> = HashMap::new();
        for (i, t) in triples.iter().enumerate() {
            by_subject.entry(t.subject.clone()).or_default().push(i);
        }
        Ok(ConfigGraph {
            path: path.to_path_buf(),
            triples,
            by_subject,
        })
    }

    /// The directory relative paths of this file are resolved against.
    pub fn dir(&self) -> PathBuf {
        let abs = std::path::absolute(&self.path).unwrap_or_else(|_| self.path.clone());
        abs.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    /// The triples of `s`.
    pub fn about<'a>(&'a self, s: &NamedOrBlankNode) -> impl Iterator<Item = &'a Triple> + 'a {
        self.by_subject
            .get(s)
            .into_iter()
            .flatten()
            .map(|&i| &self.triples[i])
    }

    pub fn objects(&self, s: &NamedOrBlankNode, p: &str) -> Vec<&Term> {
        self.about(s)
            .filter(|t| t.predicate.as_str() == p)
            .map(|t| &t.object)
            .collect()
    }

    /// The one value of `p`, if any; a second one is an error.
    pub fn one(&self, s: &NamedOrBlankNode, p: &str) -> Result<Option<&Term>> {
        let v = self.objects(s, p);
        if v.len() > 1 {
            bail!("{} has more than one {}", self.show(s), short(p));
        }
        Ok(v.into_iter().next())
    }

    pub fn types(&self, s: &NamedOrBlankNode) -> Vec<String> {
        self.objects(s, RDF_TYPE)
            .into_iter()
            .filter_map(|t| match t {
                Term::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            })
            .collect()
    }

    pub fn has_type(&self, s: &NamedOrBlankNode, t: &str) -> bool {
        self.types(s).iter().any(|x| x == t)
    }

    /// The subjects with `rdf:type t`, in file order.
    pub fn of_type(&self, t: &str) -> Vec<NamedOrBlankNode> {
        let mut out: Vec<NamedOrBlankNode> = Vec::new();
        for tr in &self.triples {
            if tr.predicate.as_str() == RDF_TYPE
                && matches!(&tr.object, Term::NamedNode(n) if n.as_str() == t)
                && !out.contains(&tr.subject)
            {
                out.push(tr.subject.clone());
            }
        }
        out
    }

    /// The members of the RDF list at `head`, or `None` when `head` is not a list.
    pub fn list(&self, head: &Term) -> Option<Vec<Term>> {
        let nil = format!("{RDF}nil");
        let first = format!("{RDF}first");
        let rest = format!("{RDF}rest");
        let mut out = Vec::new();
        let mut cur = head.clone();
        let mut seen = 0;
        loop {
            match &cur {
                Term::NamedNode(n) if n.as_str() == nil => return Some(out),
                Term::NamedNode(_) | Term::BlankNode(_) => {}
                _ => return None,
            }
            let s = as_subject(&cur)?;
            let f = self.objects(&s, &first);
            let r = self.objects(&s, &rest);
            let ([f], [r]) = (f.as_slice(), r.as_slice()) else {
                return None;
            };
            out.push((*f).clone());
            cur = (*r).clone();
            seen += 1;
            if seen > 100_000 {
                return None;
            }
        }
    }

    /// Fuseki's "one or a list": the values of `p`, with list values expanded.
    pub fn values(&self, s: &NamedOrBlankNode, p: &str) -> Vec<Term> {
        let mut out = Vec::new();
        for o in self.objects(s, p) {
            match self.list(o) {
                Some(members) => out.extend(members),
                None => out.push(o.clone()),
            }
        }
        out
    }

    /// A resource for messages: its IRI made relative to the file, or `[]`.
    pub fn show(&self, s: &NamedOrBlankNode) -> String {
        match s {
            NamedOrBlankNode::NamedNode(n) => {
                let iri = n.as_str();
                match iri.split_once('#') {
                    Some((doc, frag)) if doc.starts_with("file:") => format!("<#{frag}>"),
                    _ => short(iri),
                }
            }
            NamedOrBlankNode::BlankNode(_) => "[]".into(),
        }
    }

    /// A file named by a literal or an IRI, resolved against this file's directory.
    pub fn file_ref(&self, t: &Term) -> Option<PathBuf> {
        let raw = match t {
            Term::Literal(l) => l.value().to_string(),
            Term::NamedNode(n) => n.as_str().to_string(),
            _ => return None,
        };
        Some(resolve_file(&raw, &self.dir()))
    }
}

/// A path for messages: relative to the working directory when it is under it.
pub fn shown(p: &Path) -> String {
    match std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(Path::to_path_buf))
    {
        Some(r) if !r.as_os_str().is_empty() => r.display().to_string(),
        _ => p.display().to_string(),
    }
}

/// A `file:` IRI or a plain path, made absolute against `dir`.
pub fn resolve_file(raw: &str, dir: &Path) -> PathBuf {
    let path = match raw.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None => match raw.strip_prefix("file:") {
            Some(rest) => percent_decode(rest),
            None => raw.to_string(),
        },
    };
    let p = PathBuf::from(path);
    if p.is_absolute() { p } else { dir.join(p) }
}

fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

/// The `file:` IRI of an absolute path, as a base IRI.
pub fn file_iri(p: &Path) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'[')
        .add(b']')
        .add(b'\\')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');
    let s = p.to_string_lossy();
    let enc = percent_encoding::utf8_percent_encode(&s, SET).to_string();
    if enc.starts_with('/') {
        format!("file://{enc}")
    } else {
        format!("file:///{enc}")
    }
}

pub fn as_subject(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(n.clone().into()),
        Term::BlankNode(b) => Some(b.clone().into()),
        _ => None,
    }
}

/// The lexical form of a literal.
pub fn lexical(t: &Term) -> Option<&str> {
    match t {
        Term::Literal(l) => Some(l.value()),
        _ => None,
    }
}

/// A boolean literal (`true`, `"true"`, `1`).
pub fn boolean(t: &Term) -> Option<bool> {
    match lexical(t)?.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

pub fn iri(t: &Term) -> Option<&str> {
    match t {
        Term::NamedNode(n) => Some(n.as_str()),
        _ => None,
    }
}

/// An IRI with the configuration prefixes, for messages.
pub fn short(iri: &str) -> String {
    for (p, ns) in [
        ("fuseki:", FUSEKI),
        ("ja:", JA),
        ("tdb2:", TDB2),
        ("tdb:", TDB1),
        ("text:", TEXT),
        ("geosparql:", GEO),
        ("access:", ACCESS),
        ("rdfs:", RDFS),
        ("rdf:", RDF),
    ] {
        if let Some(l) = iri.strip_prefix(ns) {
            return format!("{p}{l}");
        }
    }
    format!("<{iri}>")
}

/// A term for messages.
pub fn show_term(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => short(n.as_str()),
        t => t.to_string(),
    }
}
