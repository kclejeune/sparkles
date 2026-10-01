//! A nested Turtle writer for ShExR: subjects in the order they were first written,
//! blank nodes used once written in place (`[ … ]`), RDF lists as collections, `a` for
//! `rdf:type`, prefixed names where the prefixes allow, and numbers and booleans bare.

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fmt::Write as _;

const INDENT: &str = "    ";

/// Turtle for `triples`, with `prefixes` declared (those that are used and valid).
pub(super) fn write(triples: &[Triple], prefixes: &[(String, String)]) -> String {
    let mut order: Vec<NamedOrBlankNode> = Vec::new();
    let mut props: FxHashMap<NamedOrBlankNode, Vec<(NamedNode, Term)>> = FxHashMap::default();
    let mut refs: FxHashMap<Term, usize> = FxHashMap::default();
    for t in triples {
        props
            .entry(t.subject.clone())
            .or_insert_with(|| {
                order.push(t.subject.clone());
                Vec::new()
            })
            .push((t.predicate.clone(), t.object.clone()));
        if let Term::BlankNode(_) = &t.object {
            *refs.entry(t.object.clone()).or_default() += 1;
        }
    }
    let prefixes: Vec<(String, String)> = prefixes
        .iter()
        .filter(|(p, ns)| valid_prefix(p) && oxiri::Iri::parse(ns.as_str()).is_ok())
        .cloned()
        .collect();
    let mut w = W {
        props: &props,
        refs: &refs,
        prefixes: &prefixes,
        written: FxHashSet::default(),
        out: String::new(),
    };
    for s in &order {
        // blank nodes used once are written where they are used
        if w.inline(&Term::from(s.clone())) {
            continue;
        }
        w.statement(s);
    }
    // blank nodes used once only from inside a cycle of such nodes
    for s in &order {
        if !w.written.contains(&Term::from(s.clone())) {
            w.statement(s);
        }
    }
    let mut head = String::new();
    for (p, ns) in &prefixes {
        let _ = writeln!(head, "PREFIX {p}: <{ns}>");
    }
    if !head.is_empty() {
        head.push('\n');
    }
    head + &w.out
}

struct W<'a> {
    props: &'a FxHashMap<NamedOrBlankNode, Vec<(NamedNode, Term)>>,
    refs: &'a FxHashMap<Term, usize>,
    prefixes: &'a [(String, String)],
    /// subjects written (or being written)
    written: FxHashSet<Term>,
    out: String,
}

impl W<'_> {
    fn props_of(&self, t: &Term) -> Option<&[(NamedNode, Term)]> {
        let s: NamedOrBlankNode = match t {
            Term::NamedNode(n) => n.clone().into(),
            Term::BlankNode(b) => b.clone().into(),
            _ => return None,
        };
        self.props.get(&s).map(Vec::as_slice)
    }

    /// A blank node used exactly once as an object.
    fn inline(&self, t: &Term) -> bool {
        matches!(t, Term::BlankNode(_)) && self.refs.get(t) == Some(&1)
    }

    /// The items of the list at `t`, if every cell is a blank node used once with just
    /// `rdf:first` and `rdf:rest`.
    fn list_items(&self, t: &Term) -> Option<Vec<Term>> {
        let nil = Term::from(rdf::NIL.into_owned());
        let mut items = Vec::new();
        let mut seen = FxHashSet::default();
        let mut cur = t.clone();
        while cur != nil {
            if !self.inline(&cur) || !seen.insert(cur.clone()) || self.written.contains(&cur) {
                return None;
            }
            let p = self.props_of(&cur)?;
            let [(p1, first), (p2, rest)] = p else {
                return None;
            };
            if p1.as_ref() != rdf::FIRST || p2.as_ref() != rdf::REST {
                return None;
            }
            items.push(first.clone());
            cur = rest.clone();
        }
        Some(items)
    }

    fn statement(&mut self, s: &NamedOrBlankNode) {
        let t = Term::from(s.clone());
        self.written.insert(t.clone());
        let subject = match s {
            NamedOrBlankNode::BlankNode(_) if !self.refs.contains_key(&t) => "[]".to_string(),
            _ => self.term_text(&t),
        };
        self.out.push_str(&subject);
        let props = self.props_of(&t).unwrap_or_default().to_vec();
        self.properties(&props, 1, " ");
        self.out.push_str(" .\n\n");
    }

    /// `p o ; p o , o …` with the first predicate after `first_sep`, the others on their
    /// own lines at `level`.
    fn properties(&mut self, props: &[(NamedNode, Term)], level: usize, first_sep: &str) {
        // the objects of each predicate, in the order the predicates first appear
        let mut groups: Vec<(&NamedNode, Vec<&Term>)> = Vec::new();
        for (p, o) in props {
            match groups.iter_mut().find(|(q, _)| *q == p) {
                Some((_, v)) => v.push(o),
                None => groups.push((p, vec![o])),
            }
        }
        for (i, (p, objects)) in groups.into_iter().enumerate() {
            if i == 0 {
                self.out.push_str(first_sep);
            } else {
                self.out.push_str(" ;\n");
                self.out.push_str(&INDENT.repeat(level));
            }
            let p = if p.as_ref() == rdf::TYPE {
                "a".to_string()
            } else {
                self.iri(p.as_str())
            };
            self.out.push_str(&p);
            for (j, o) in objects.into_iter().enumerate() {
                self.out.push_str(if j == 0 { " " } else { " , " });
                self.object(o, level);
            }
        }
    }

    fn object(&mut self, o: &Term, level: usize) {
        if *o == Term::from(rdf::NIL.into_owned()) {
            self.out.push_str("()");
            return;
        }
        if !self.inline(o) || self.written.contains(o) {
            let t = self.term_text(o);
            self.out.push_str(&t);
            return;
        }
        if let Some(items) = self.list_items(o) {
            let mut cur = o.clone();
            for _ in &items {
                self.written.insert(cur.clone());
                cur = self
                    .props_of(&cur)
                    .and_then(|p| p.get(1))
                    .map_or(cur.clone(), |(_, r)| r.clone());
            }
            let nested = items.iter().any(|x| self.inline(x));
            if !nested {
                self.out.push('(');
                for x in &items {
                    self.out.push(' ');
                    self.object(x, level);
                }
                self.out.push_str(" )");
                return;
            }
            self.out.push('(');
            for x in &items {
                self.out.push('\n');
                self.out.push_str(&INDENT.repeat(level + 1));
                self.object(x, level + 1);
            }
            self.out.push('\n');
            self.out.push_str(&INDENT.repeat(level));
            self.out.push(')');
            return;
        }
        self.written.insert(o.clone());
        let props = self.props_of(o).unwrap_or_default().to_vec();
        if props.is_empty() {
            self.out.push_str("[]");
            return;
        }
        self.out.push('[');
        self.out.push('\n');
        self.out.push_str(&INDENT.repeat(level + 1));
        self.properties(&props, level + 1, "");
        self.out.push('\n');
        self.out.push_str(&INDENT.repeat(level));
        self.out.push(']');
    }

    fn term_text(&self, t: &Term) -> String {
        match t {
            Term::NamedNode(n) => self.iri(n.as_str()),
            Term::BlankNode(b) => format!("_:{}", b.as_str()),
            Term::Literal(l) => self.literal(l),
            #[allow(unreachable_patterns)]
            t => t.to_string(),
        }
    }

    /// A prefixed name when a prefix covers the IRI with a plain local name, else `<…>`.
    fn iri(&self, iri: &str) -> String {
        for (p, ns) in self.prefixes {
            if let Some(local) = iri.strip_prefix(ns.as_str())
                && plain_local(local)
            {
                return format!("{p}:{local}");
            }
        }
        format!("<{}>", escape_iri(iri))
    }

    fn literal(&self, l: &Literal) -> String {
        let v = l.value();
        if let Some(lang) = l.language() {
            return format!("{}@{lang}", quoted(v));
        }
        let dt = l.datatype();
        let bare = (dt == xsd::INTEGER && is_integer(v))
            || (dt == xsd::DECIMAL && is_decimal(v))
            || (dt == xsd::DOUBLE && is_double(v))
            || (dt == xsd::BOOLEAN && matches!(v, "true" | "false"));
        if bare {
            v.to_string()
        } else if dt == xsd::STRING {
            quoted(v)
        } else {
            format!("{}^^{}", quoted(v), self.iri(dt.as_str()))
        }
    }
}

fn valid_prefix(p: &str) -> bool {
    p.is_empty()
        || (p.starts_with(|c: char| c.is_ascii_alphabetic())
            && !p.ends_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
}

/// A local name that needs no escapes: letters, digits, `_`, `-` and inner `.`.
fn plain_local(l: &str) -> bool {
    l.is_empty()
        || (l.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            && !l.ends_with('.')
            && l.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
}

fn escape_iri(iri: &str) -> String {
    let mut s = String::with_capacity(iri.len());
    for c in iri.chars() {
        match c {
            '\0'..=' ' | '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\' => {
                let _ = write!(s, "\\u{:04X}", c as u32);
            }
            c => s.push(c),
        }
    }
    s
}

fn quoted(v: &str) -> String {
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(s, "\\u{:04X}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn unsigned(s: &str) -> &str {
    s.strip_prefix(['+', '-']).unwrap_or(s)
}

fn is_integer(s: &str) -> bool {
    digits(unsigned(s))
}

fn is_decimal(s: &str) -> bool {
    match unsigned(s).split_once('.') {
        Some((a, b)) => (a.is_empty() || digits(a)) && digits(b),
        None => false,
    }
}

fn is_double(s: &str) -> bool {
    let Some((m, e)) = unsigned(s).split_once(['e', 'E']) else {
        return false;
    };
    let mantissa = match m.split_once('.') {
        Some((a, b)) => (digits(a) && (b.is_empty() || digits(b))) || (a.is_empty() && digits(b)),
        None => digits(m),
    };
    mantissa && is_integer(e)
}
