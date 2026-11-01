//! Licence records of the resource catalog (spec C30, phase 1): SPDX licence expressions,
//! the four licence classes and the unknown class, and the licence gate's verdict.
//!
//! An expression follows the SPDX License Expression syntax: identifiers, `LicenseRef-`
//! references, `AND`, `OR`, `WITH`, a trailing `+` and parentheses. A built-in table
//! classifies the common data and software licences. Any other identifier, and every
//! `LicenseRef-`, needs the class given with the record.

use serde::{Deserialize, Serialize};

/// How freely data under a licence may be redistributed, from most to least free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// no conditions on redistribution
    Open,
    /// redistribution with attribution
    Attribution,
    /// redistribution under the same licence
    ShareAlike,
    /// no redistribution, such as non-commercial or no-derivatives terms
    Restricted,
    /// no licence is recorded, which means no redistribution
    Unknown,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Open => "open",
            Class::Attribution => "attribution",
            Class::ShareAlike => "share-alike",
            Class::Restricted => "restricted",
            Class::Unknown => "unknown",
        }
    }

    /// Whether data of this class may be redistributed, with its obligations.
    pub fn redistributable(self) -> bool {
        matches!(self, Class::Open | Class::Attribution | Class::ShareAlike)
    }
}

/// The built-in table of SPDX identifiers and their classes.
const TABLE: &[(&str, Class)] = &[
    ("CC0-1.0", Class::Open),
    ("PDDL-1.0", Class::Open),
    ("CC-PDDC", Class::Open),
    ("Unlicense", Class::Open),
    ("0BSD", Class::Open),
    ("CC-BY-1.0", Class::Attribution),
    ("CC-BY-2.0", Class::Attribution),
    ("CC-BY-2.5", Class::Attribution),
    ("CC-BY-3.0", Class::Attribution),
    ("CC-BY-4.0", Class::Attribution),
    ("ODC-By-1.0", Class::Attribution),
    ("CDLA-Permissive-1.0", Class::Attribution),
    ("CDLA-Permissive-2.0", Class::Attribution),
    ("OGL-UK-3.0", Class::Attribution),
    ("MIT", Class::Attribution),
    ("Apache-2.0", Class::Attribution),
    ("BSD-2-Clause", Class::Attribution),
    ("BSD-3-Clause", Class::Attribution),
    ("ISC", Class::Attribution),
    ("W3C", Class::Attribution),
    ("W3C-20150513", Class::Attribution),
    ("Zlib", Class::Attribution),
    ("CC-BY-SA-1.0", Class::ShareAlike),
    ("CC-BY-SA-2.0", Class::ShareAlike),
    ("CC-BY-SA-2.5", Class::ShareAlike),
    ("CC-BY-SA-3.0", Class::ShareAlike),
    ("CC-BY-SA-4.0", Class::ShareAlike),
    ("ODbL-1.0", Class::ShareAlike),
    ("CDLA-Sharing-1.0", Class::ShareAlike),
    ("GFDL-1.3-only", Class::ShareAlike),
    ("GFDL-1.3-or-later", Class::ShareAlike),
    ("GPL-2.0-only", Class::ShareAlike),
    ("GPL-2.0-or-later", Class::ShareAlike),
    ("GPL-3.0-only", Class::ShareAlike),
    ("GPL-3.0-or-later", Class::ShareAlike),
    ("LGPL-2.1-only", Class::ShareAlike),
    ("LGPL-2.1-or-later", Class::ShareAlike),
    ("LGPL-3.0-only", Class::ShareAlike),
    ("LGPL-3.0-or-later", Class::ShareAlike),
    ("AGPL-3.0-only", Class::ShareAlike),
    ("AGPL-3.0-or-later", Class::ShareAlike),
    ("MPL-2.0", Class::ShareAlike),
    ("EPL-2.0", Class::ShareAlike),
    ("CC-BY-NC-2.0", Class::Restricted),
    ("CC-BY-NC-3.0", Class::Restricted),
    ("CC-BY-NC-4.0", Class::Restricted),
    ("CC-BY-NC-SA-2.0", Class::Restricted),
    ("CC-BY-NC-SA-3.0", Class::Restricted),
    ("CC-BY-NC-SA-4.0", Class::Restricted),
    ("CC-BY-ND-2.0", Class::Restricted),
    ("CC-BY-ND-3.0", Class::Restricted),
    ("CC-BY-ND-4.0", Class::Restricted),
    ("CC-BY-NC-ND-2.0", Class::Restricted),
    ("CC-BY-NC-ND-3.0", Class::Restricted),
    ("CC-BY-NC-ND-4.0", Class::Restricted),
];

/// The class of an SPDX identifier in the built-in table. SPDX identifiers match
/// case-insensitively.
pub fn listed(id: &str) -> Option<Class> {
    TABLE
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(id))
        .map(|&(_, c)| c)
}

/// The canonical spelling of a listed identifier, for its SPDX licence page.
pub fn canonical(id: &str) -> Option<&'static str> {
    TABLE
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(id))
        .map(|&(k, _)| k)
}

/// The longest licence expression, in bytes.
pub const MAX_EXPRESSION_BYTES: usize = 512;

/// A parsed licence expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// an identifier or `LicenseRef-`, with whether it ends in `+`
    Id(String, bool),
    With(Box<Expr>, String),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

/// Whether `s` is an operator of the expression syntax.
fn operator(s: &str) -> bool {
    matches!(s, "AND" | "OR" | "WITH")
}

/// Whether `s` is an SPDX idstring: letters, digits, `.` and `-`.
fn idstring(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' | ')' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

struct Parser {
    toks: Vec<String>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.at).map(String::as_str)
    }
    fn next(&mut self) -> Option<String> {
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }
    // or := and ("OR" and)*
    fn or(&mut self) -> Result<Expr, String> {
        let mut e = self.and()?;
        while self.peek() == Some("OR") {
            self.at += 1;
            e = Expr::Or(Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }
    // and := with ("AND" with)*
    fn and(&mut self) -> Result<Expr, String> {
        let mut e = self.with()?;
        while self.peek() == Some("AND") {
            self.at += 1;
            e = Expr::And(Box::new(e), Box::new(self.with()?));
        }
        Ok(e)
    }
    // with := atom ("WITH" exception)?, where only a simple licence takes WITH
    fn with(&mut self) -> Result<Expr, String> {
        let simple = self.peek() != Some("(");
        let e = self.atom()?;
        if self.peek() == Some("WITH") {
            if !simple {
                return Err("WITH follows a single licence, not a parenthesized expression".into());
            }
            self.at += 1;
            let x = self
                .next()
                .ok_or("WITH needs an exception identifier after it")?;
            if !idstring(&x) || operator(&x) {
                return Err(format!("{x:?} is not an exception identifier"));
            }
            return Ok(Expr::With(Box::new(e), x));
        }
        Ok(e)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            None => Err("the expression ends where a licence was expected".into()),
            Some(t) if t == "(" => {
                self.depth += 1;
                if self.depth > 16 {
                    return Err("parentheses nest more than 16 deep".into());
                }
                let e = self.or()?;
                if self.next().as_deref() != Some(")") {
                    return Err("a parenthesis is not closed".into());
                }
                self.depth -= 1;
                Ok(e)
            }
            Some(t) if matches!(t.as_str(), ")" | "AND" | "OR" | "WITH") => {
                Err(format!("{t} stands where a licence was expected"))
            }
            Some(t) => {
                let (id, plus) = match t.strip_suffix('+') {
                    Some(id) => (id.to_string(), true),
                    None => (t, false),
                };
                let ok = match id.split_once(':') {
                    // DocumentRef-x:LicenseRef-y
                    Some((doc, r)) => {
                        doc.starts_with("DocumentRef-")
                            && idstring(doc)
                            && r.starts_with("LicenseRef-")
                            && idstring(r)
                    }
                    None => idstring(&id),
                };
                if !ok {
                    return Err(format!("{id:?} is not an SPDX licence identifier"));
                }
                Ok(Expr::Id(id, plus))
            }
        }
    }
}

/// Parse an SPDX licence expression.
pub fn parse(s: &str) -> Result<Expr, String> {
    if s.len() > MAX_EXPRESSION_BYTES {
        return Err(format!(
            "the licence expression is longer than {MAX_EXPRESSION_BYTES} bytes"
        ));
    }
    let mut p = Parser {
        toks: tokens(s),
        at: 0,
        depth: 0,
    };
    if p.toks.is_empty() {
        return Err("the licence expression is empty".into());
    }
    let e = p.or()?;
    if let Some(t) = p.peek() {
        return Err(format!("{t:?} follows a complete licence expression"));
    }
    Ok(e)
}

/// Whether an identifier means that no licence is known.
fn no_assertion(id: &str) -> bool {
    id.eq_ignore_ascii_case("NOASSERTION") || id.eq_ignore_ascii_case("NONE")
}

impl Expr {
    /// The class of the expression from the built-in table, or the first identifier it
    /// cannot classify.
    pub fn class(&self) -> Result<Class, String> {
        match self {
            Expr::Id(id, _) if no_assertion(id) => Ok(Class::Unknown),
            Expr::Id(id, _) => listed(id).ok_or_else(|| id.clone()),
            Expr::With(e, _) => e.class(),
            Expr::And(a, b) => Ok(a.class()?.max(b.class()?)),
            Expr::Or(a, b) => Ok(a.class()?.min(b.class()?)),
        }
    }

    /// The single listed identifier the expression is, if it is one.
    pub fn single(&self) -> Option<&'static str> {
        match self {
            Expr::Id(id, false) => canonical(id),
            _ => None,
        }
    }
}

/// A licence as a record carries it: an expression, a class when the expression needs
/// one, and a URL for licences outside the SPDX list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Licence<'a> {
    pub expression: Option<&'a str>,
    pub class: Option<Class>,
    pub url: Option<&'a str>,
}

impl Licence<'_> {
    /// The licence's class. A given class overrides the table, and an expression the
    /// table cannot classify needs one.
    pub fn class(&self) -> Result<Class, String> {
        let Some(e) = self.expression else {
            return Ok(self.class.unwrap_or(Class::Unknown));
        };
        let expr = parse(e)?;
        match (expr.class(), self.class) {
            // the table classifies the expression, and NOASSERTION and NONE are unknown,
            // so a given class may only restate it
            (Ok(t), Some(c)) if t != c => {
                return Err(format!(
                    "the built-in table classifies {e} as {}; leave licenseClass out or give {}",
                    t.as_str(),
                    t.as_str()
                ));
            }
            (Ok(t), _) => return Ok(t),
            (Err(_), Some(c)) => return Ok(c),
            (Err(_), None) => {}
        }
        expr.class().map_err(|id| {
            format!(
                "the licence {id} is not in the built-in table; give its class in licenseClass (open, attribution, share-alike or restricted)"
            )
        })
    }

    /// The page that describes the licence: the URL given, else the SPDX licence page of
    /// a single listed identifier.
    pub fn page(&self) -> Option<String> {
        if let Some(u) = self.url {
            return Some(u.to_string());
        }
        let e = parse(self.expression?).ok()?;
        e.single()
            .map(|id| format!("https://spdx.org/licenses/{id}.html"))
    }

    /// The SPDX licence IRI of a single listed identifier.
    pub fn spdx_iri(&self) -> Option<String> {
        let e = parse(self.expression?).ok()?;
        e.single()
            .map(|id| format!("http://spdx.org/licenses/{id}"))
    }
}

/// The operator's licence policy (`serve --license-policy`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// report data that may not be redistributed, and refuse nothing
    #[default]
    Warn,
    /// refuse to redistribute data that may not be redistributed
    Enforce,
}

/// What the licence gate says of redistributing data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Warn,
    Refuse,
}

/// The gate's verdict on redistributing data of class `class` under `policy`.
pub fn verdict(policy: Policy, class: Class) -> Verdict {
    match (class.redistributable(), policy) {
        (true, _) => Verdict::Allow,
        (false, Policy::Warn) => Verdict::Warn,
        (false, Policy::Enforce) => Verdict::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(s: &str) -> Result<Class, String> {
        parse(s).unwrap().class()
    }

    #[test]
    fn expressions_parse_and_classify() {
        assert_eq!(class("CC-BY-4.0"), Ok(Class::Attribution));
        assert_eq!(class("cc0-1.0"), Ok(Class::Open));
        assert_eq!(class("ODbL-1.0 AND CC-BY-4.0"), Ok(Class::ShareAlike));
        assert_eq!(class("CC-BY-NC-4.0 OR CC-BY-4.0"), Ok(Class::Attribution));
        assert_eq!(
            class("(CC-BY-NC-4.0 OR CC0-1.0) AND CC-BY-SA-4.0"),
            Ok(Class::ShareAlike)
        );
        assert_eq!(
            class("GPL-2.0-or-later WITH Classpath-exception-2.0"),
            Ok(Class::ShareAlike)
        );
        assert_eq!(class("NOASSERTION"), Ok(Class::Unknown));
        assert_eq!(class("NONE AND CC0-1.0"), Ok(Class::Unknown));
        assert_eq!(class("LicenseRef-imdb"), Err("LicenseRef-imdb".into()));
        assert_eq!(class("Apache-1.0+"), Err("Apache-1.0".into()));
        for bad in [
            "",
            "AND",
            "MIT AND",
            "(MIT",
            "MIT)",
            "MIT WITH",
            "M I T",
            "a/b",
            "DocumentRef-x:MIT",
            "(MIT OR Apache-2.0) WITH Classpath-exception-2.0",
            "MIT WITH AND",
            "MIT WITH OR",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(parse("DocumentRef-spdx:LicenseRef-x").is_ok());
        assert!(parse(&"(".repeat(20)).is_err());
        assert!(parse(&"MIT OR ".repeat(100)).is_err());
    }

    #[test]
    fn classes_order_from_free_to_unknown() {
        assert!(Class::Open < Class::Attribution);
        assert!(Class::Attribution < Class::ShareAlike);
        assert!(Class::ShareAlike < Class::Restricted);
        assert!(Class::Restricted < Class::Unknown);
        assert!(Class::ShareAlike.redistributable());
        assert!(!Class::Restricted.redistributable());
        assert!(!Class::Unknown.redistributable());
    }

    #[test]
    fn records_need_a_class_outside_the_table() {
        let r = Licence {
            expression: Some("LicenseRef-imdb"),
            ..Default::default()
        };
        assert!(r.class().unwrap_err().contains("licenseClass"));
        let r = Licence {
            class: Some(Class::Restricted),
            ..r
        };
        assert_eq!(r.class(), Ok(Class::Restricted));
        assert_eq!(Licence::default().class(), Ok(Class::Unknown));
        // a given class cannot change what the table or a sentinel says
        for (e, c) in [
            ("NONE", Class::Open),
            ("NOASSERTION", Class::Open),
            ("CC-BY-4.0", Class::Open),
        ] {
            let l = Licence {
                expression: Some(e),
                class: Some(c),
                url: None,
            };
            assert!(l.class().unwrap_err().contains("built-in table"), "{e}");
        }
        let same = Licence {
            expression: Some("CC-BY-4.0"),
            class: Some(Class::Attribution),
            url: None,
        };
        assert_eq!(same.class(), Ok(Class::Attribution));
        let cc = Licence {
            expression: Some("cc-by-4.0"),
            ..Default::default()
        };
        assert_eq!(
            cc.page().as_deref(),
            Some("https://spdx.org/licenses/CC-BY-4.0.html")
        );
        assert_eq!(
            cc.spdx_iri().as_deref(),
            Some("http://spdx.org/licenses/CC-BY-4.0")
        );
    }

    #[test]
    fn the_gate_warns_unless_enforced() {
        assert_eq!(verdict(Policy::Warn, Class::Attribution), Verdict::Allow);
        assert_eq!(verdict(Policy::Warn, Class::Restricted), Verdict::Warn);
        assert_eq!(verdict(Policy::Warn, Class::Unknown), Verdict::Warn);
        assert_eq!(verdict(Policy::Enforce, Class::Unknown), Verdict::Refuse);
        assert_eq!(verdict(Policy::Enforce, Class::Open), Verdict::Allow);
        assert_eq!(Policy::default(), Policy::Warn);
    }
}
