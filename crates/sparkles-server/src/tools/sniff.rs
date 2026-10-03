//! The syntax of RDF or tabular content from its first bytes (after decompression), for
//! `sparkles convert` when neither `--syntax` nor the file extension names it.
//!
//! The checks, in order:
//!
//! * Binary content is RDF Thrift or RDF Protobuf when its first row has the shape
//!   Jena's writers give it.
//! * XML is TriX when its root element is `TriX`, and RDF/XML when the root is `rdf:RDF`
//!   or the document declares the RDF namespace.
//! * JSON is RDF/JSON when its top-level object maps a subject to an object of
//!   predicates, and JSON-LD otherwise.
//! * Text whose statements are lines of IRIs, blank nodes and literals is N-Quads, or
//!   N-Triples when no line names a graph. Other Turtle-family text is TriG when it has a
//!   `GRAPH` keyword or a `{` block, and Turtle otherwise.
//! * Text whose lines hold the same number of tabs, or of commas, is TSV or CSV.

use super::convert::Syntax;
use crate::http::jena_formats::JenaFormat;
use oxrdfio::RdfFormat;
use sparkles::tabular::TabularKind;

/// How many decompressed bytes [`sniff`] looks at.
pub const HEAD_BYTES: usize = 8192;

/// What the content looks like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Rdf(Syntax),
    Table(TabularKind),
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Rdf(s) => s.name(),
            Kind::Table(TabularKind::Csv) => "CSV",
            Kind::Table(TabularKind::Tsv) => "TSV",
        }
    }
}

/// The outcome of [`sniff`].
#[derive(Clone, Debug, PartialEq)]
pub enum Sniffed {
    Found(Kind),
    /// Nothing recognizable.
    Unknown,
    /// More than one syntax fits.
    Ambiguous(Vec<Kind>),
}

const fn rdf(f: RdfFormat) -> Sniffed {
    Sniffed::Found(Kind::Rdf(Syntax::Rdf(f)))
}

const fn jena(j: JenaFormat) -> Sniffed {
    Sniffed::Found(Kind::Rdf(Syntax::Jena(j)))
}

/// The syntax of content that starts with `head`. `complete` says that `head` is the
/// whole content, so that its last line is not cut short.
pub fn sniff(head: &[u8], complete: bool) -> Sniffed {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    if head.iter().all(u8::is_ascii_whitespace) {
        return Sniffed::Unknown;
    }
    if is_binary(head) {
        return binary(head);
    }
    let text = match std::str::from_utf8(head) {
        Ok(t) => t,
        // a character cut at the end of the head
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&head[..e.valid_up_to()]).expect("valid up to there")
        }
        Err(_) => return Sniffed::Unknown,
    };
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return json(trimmed);
    }
    if let Some(s) = xml(trimmed) {
        return s;
    }
    // the lines that are whole
    let lines: Vec<&str> = {
        let mut v: Vec<&str> = text.lines().collect();
        if !complete && !text.ends_with('\n') && v.len() > 1 {
            v.pop();
        }
        v
    };
    if let Some(s) = turtle_family(text, &lines) {
        return s;
    }
    table(&lines)
}

/// Control characters other than whitespace mark binary content.
fn is_binary(head: &[u8]) -> bool {
    head.iter()
        .any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C))
}

/// RDF Thrift: rows in the Thrift compact protocol, the first a prefix declaration
/// (field 1), a triple (2) or a quad (3), whose first field is a string (the prefix) or
/// a term struct. RDF Protobuf: length-delimited rows whose first field is one of those,
/// itself length-delimited and shorter than the row.
fn binary(h: &[u8]) -> Sniffed {
    let thrift = h.len() >= 3
        && matches!(h[0], 0x1C | 0x2C | 0x3C)
        && (h[1] == 0x18
            || (h[1] == 0x1C && h[2] & 0x0F == 0x0C && (1..=9).contains(&(h[2] >> 4))));
    let protobuf = (|| {
        let (len, n) = varint(h)?;
        let tag = *h.get(n)?;
        if !matches!(tag, 0x0A | 0x12 | 0x1A) {
            return None;
        }
        let (inner, m) = varint(&h[n + 1..])?;
        let first = *h.get(n + 1 + m)?;
        (inner < len && matches!(first, 0x0A | 0x12)).then_some(())
    })()
    .is_some();
    match (thrift, protobuf) {
        (true, false) => jena(JenaFormat::Thrift),
        (false, true) => jena(JenaFormat::Protobuf),
        (true, true) => Sniffed::Ambiguous(vec![
            Kind::Rdf(Syntax::Jena(JenaFormat::Thrift)),
            Kind::Rdf(Syntax::Jena(JenaFormat::Protobuf)),
        ]),
        (false, false) => Sniffed::Unknown,
    }
}

fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &x) in b.iter().enumerate().take(10) {
        v |= u64::from(x & 0x7F) << (7 * i);
        if x & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// XML by its root element, or `None` when the text is not XML.
fn xml(t: &str) -> Option<Sniffed> {
    let prolog = t.starts_with("<?xml") || t.starts_with("<!");
    let mut rest = t;
    // skip the declaration, comments, processing instructions and the doctype
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = &r[r.find("-->")? + 3..];
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = &rest[rest.find('>')? + 1..];
        } else {
            break;
        }
    }
    let tag = rest.strip_prefix('<')?;
    let end = tag.find('>').unwrap_or(tag.len());
    let open = &tag[..end];
    let name: &str = open
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or_default();
    // a Turtle or N-Triples IRI also starts with `<`: XML has a prolog, or namespaces
    if !prolog && !open.contains("xmlns") {
        return None;
    }
    let local = name.rsplit(':').next().unwrap_or(name);
    Some(if local.eq_ignore_ascii_case("trix") {
        jena(JenaFormat::TriX)
    } else if local == "RDF" || t.contains("http://www.w3.org/1999/02/22-rdf-syntax-ns#") {
        rdf(RdfFormat::RdfXml)
    } else {
        Sniffed::Unknown
    })
}

/// JSON-LD or RDF/JSON. RDF/JSON is an object of subjects, each an object of predicates
/// whose values are arrays of objects; JSON-LD is an array, or an object with `@`
/// keywords or property values.
fn json(t: &str) -> Sniffed {
    let jsonld = rdf(RdfFormat::JsonLd {
        profile: oxrdfio::JsonLdProfileSet::empty(),
    });
    if t.starts_with('[') {
        return jsonld;
    }
    let mut s = t[1..].trim_start();
    if s.starts_with('}') {
        return Sniffed::Ambiguous(vec![
            Kind::Rdf(Syntax::Rdf(RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            })),
            Kind::Rdf(Syntax::Jena(JenaFormat::RdfJson)),
        ]);
    }
    // the first key, then the first key of its value
    let Some((key, r)) = json_key(s) else {
        return Sniffed::Unknown;
    };
    if key.starts_with('@') {
        return jsonld;
    }
    s = r;
    let Some(r) = s.strip_prefix('{') else {
        return jsonld;
    };
    match json_key(r.trim_start()) {
        Some((k, r)) if !k.starts_with('@') && r.starts_with('[') => jena(JenaFormat::RdfJson),
        // `{}`: a subject without predicates
        None if r.trim_start().starts_with('}') => jena(JenaFormat::RdfJson),
        _ => jsonld,
    }
}

/// A JSON string key and the text after its colon.
fn json_key(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix('"')?;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => {
                let rest = s[i + 1..].trim_start().strip_prefix(':')?;
                return Some((&s[..i], rest.trim_start()));
            }
            _ => escaped = false,
        }
    }
    None
}

/// The Turtle family, or `None` when the text does not look like it.
fn turtle_family(text: &str, lines: &[&str]) -> Option<Sniffed> {
    let statements: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let first = statements.first()?;
    let directive = |l: &str| {
        let lower = l.to_ascii_lowercase();
        lower.starts_with("@prefix")
            || lower.starts_with("@base")
            || lower.starts_with("prefix ")
            || lower.starts_with("base ")
            || lower.starts_with("graph ")
            || lower.starts_with("graph<")
    };
    let rdfish =
        |l: &str| l.starts_with('<') || l.starts_with("_:") || l.starts_with('[') || directive(l);
    let prefixed = |l: &str| {
        let word = l.split(char::is_whitespace).next().unwrap_or_default();
        word.contains(':')
            && !word.contains(',')
            && word
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == ':')
    };
    if !rdfish(first) && !prefixed(first) {
        return None;
    }
    // line-based: every statement is one line of terms ending with `.`
    let mut quads = false;
    let line_based = statements.iter().all(|l| match nquads_terms(l) {
        Some(3) => true,
        Some(4) => {
            quads = true;
            true
        }
        _ => false,
    });
    if line_based {
        return Some(rdf(if quads {
            RdfFormat::NQuads
        } else {
            RdfFormat::NTriples
        }));
    }
    let code = without_strings_and_iris(text);
    let trig = code
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w.eq_ignore_ascii_case("graph"))
        || code.contains('{');
    Some(rdf(if trig {
        RdfFormat::TriG
    } else {
        RdfFormat::Turtle
    }))
}

/// The number of terms of an N-Triples or N-Quads line (`None` if it is not one).
fn nquads_terms(line: &str) -> Option<usize> {
    let mut s = line.trim();
    s = s.strip_suffix('.')?.trim_end();
    let mut n = 0;
    while !s.is_empty() {
        let len = if s.starts_with('<') {
            let end = s.find('>')?;
            if s[1..end].contains(char::is_whitespace) {
                return None;
            }
            end + 1
        } else if let Some(r) = s.strip_prefix("_:") {
            2 + r.find(char::is_whitespace).unwrap_or(r.len())
        } else if s.starts_with('"') {
            let b = s.as_bytes();
            let mut i = 1;
            loop {
                match b.get(i)? {
                    b'\\' => i += 2,
                    b'"' => break,
                    _ => i += 1,
                }
            }
            i += 1;
            let r = &s[i..];
            if let Some(dt) = r.strip_prefix("^^<") {
                i += 3 + dt.find('>')? + 1;
            } else if r.starts_with('@') {
                i += r.find(char::is_whitespace).unwrap_or(r.len());
            }
            i
        } else {
            return None;
        };
        n += 1;
        s = s[len..].trim_start();
    }
    (3..=4).contains(&n).then_some(n)
}

/// The text with string literals, IRIs and comments blanked out.
fn without_strings_and_iris(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let q = c;
                while let Some(d) = chars.next() {
                    if d == '\\' {
                        chars.next();
                    } else if d == q {
                        break;
                    }
                }
                out.push(' ');
            }
            '<' => {
                for d in chars.by_ref() {
                    if d == '>' || d.is_whitespace() {
                        break;
                    }
                }
                out.push(' ');
            }
            '#' => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
                out.push('\n');
            }
            c => out.push(c),
        }
    }
    out
}

/// CSV or TSV: the header and the rows after it hold the same number of tabs (or of
/// commas), at least one.
fn table(lines: &[&str]) -> Sniffed {
    let lines: Vec<&str> = lines.iter().copied().filter(|l| !l.is_empty()).collect();
    let consistent = |sep: char| {
        let n = lines[0].matches(sep).count();
        n > 0 && lines.iter().all(|l| l.matches(sep).count() == n)
    };
    if lines.is_empty() {
        return Sniffed::Unknown;
    }
    // a quoted comma breaks the count, so a CSV header alone decides when rows differ
    let tsv = consistent('\t');
    let csv = consistent(',') || (!tsv && lines[0].contains(',') && !lines[0].contains('\t'));
    match (tsv, csv) {
        (true, false) => Sniffed::Found(Kind::Table(TabularKind::Tsv)),
        (false, true) => Sniffed::Found(Kind::Table(TabularKind::Csv)),
        (true, true) => Sniffed::Ambiguous(vec![
            Kind::Table(TabularKind::Csv),
            Kind::Table(TabularKind::Tsv),
        ]),
        (false, false) => Sniffed::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(s: &str) -> Kind {
        match sniff(s.as_bytes(), true) {
            Sniffed::Found(k) => k,
            other => panic!("{s:?}: {other:?}"),
        }
    }

    fn rdf_kind(f: RdfFormat) -> Kind {
        Kind::Rdf(Syntax::Rdf(f))
    }

    fn jena_kind(j: JenaFormat) -> Kind {
        Kind::Rdf(Syntax::Jena(j))
    }

    #[test]
    fn xml_by_root_element() {
        assert_eq!(
            found(
                "<?xml version=\"1.0\"?>\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"/>"
            ),
            rdf_kind(RdfFormat::RdfXml)
        );
        assert_eq!(
            found(
                "<!-- c -->\n<TriX xmlns=\"http://www.w3.org/2004/03/trix/trix-1/\"><graph/></TriX>"
            ),
            jena_kind(JenaFormat::TriX)
        );
        // a typed node as the root, with the RDF namespace
        assert_eq!(
            found(
                "<ex:Person xmlns:ex=\"http://e/\" xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"/>"
            ),
            rdf_kind(RdfFormat::RdfXml)
        );
        assert_eq!(
            sniff(b"<?xml version=\"1.0\"?><html/>", true),
            Sniffed::Unknown
        );
    }

    #[test]
    fn json_by_shape() {
        let jsonld = rdf_kind(RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
        });
        assert_eq!(found(r#"{"@context": {}, "@id": "http://e/a"}"#), jsonld);
        assert_eq!(found(r#"[{"@id": "http://e/a"}]"#), jsonld);
        assert_eq!(found(r#"{"http://schema.org/name": "x"}"#), jsonld);
        assert_eq!(
            found(
                r#"{ "http://e/a" : { "http://e/p" : [ {"type": "uri", "value": "http://e/b"} ] } }"#
            ),
            jena_kind(JenaFormat::RdfJson)
        );
        assert!(matches!(sniff(b"{}", true), Sniffed::Ambiguous(k) if k.len() == 2));
    }

    #[test]
    fn turtle_family_by_statements() {
        assert_eq!(
            found(
                "<http://e/a> <http://e/p> \"x y\"@en .\n_:b <http://e/p> \"1\"^^<http://e/t> .\n"
            ),
            rdf_kind(RdfFormat::NTriples)
        );
        assert_eq!(
            found("# c\n<http://e/a> <http://e/p> <http://e/o> <http://e/g> .\n"),
            rdf_kind(RdfFormat::NQuads)
        );
        assert_eq!(
            found("@prefix ex: <http://e/> .\nex:a ex:p 1, 2 .\n"),
            rdf_kind(RdfFormat::Turtle)
        );
        assert_eq!(
            found("PREFIX ex: <http://e/>\nGRAPH ex:g { ex:a ex:p \"{\" }\n"),
            rdf_kind(RdfFormat::TriG)
        );
        assert_eq!(
            found("<http://e/g> { <http://e/a> <http://e/p> 1 }\n"),
            rdf_kind(RdfFormat::TriG)
        );
        // a brace inside a string is not a block
        assert_eq!(found("ex:a ex:p \"{x}\" .\n"), rdf_kind(RdfFormat::Turtle));
        // a truncated last line is left out
        assert_eq!(
            sniff(
                b"<http://e/a> <http://e/p> <http://e/o> .\n<http://e/a> <http://e/p",
                false
            ),
            Sniffed::Found(rdf_kind(RdfFormat::NTriples))
        );
    }

    #[test]
    fn tables_by_delimiter() {
        assert_eq!(
            found("id,name\n7,Ann\n8,\"B, C\"\n"),
            Kind::Table(TabularKind::Csv)
        );
        assert_eq!(found("id\tname\n7\tAnn\n"), Kind::Table(TabularKind::Tsv));
        assert!(matches!(
            sniff(b"a,b\tc\n1,2\t3\n", true),
            Sniffed::Ambiguous(_)
        ));
        assert_eq!(sniff(b"hello world\n", true), Sniffed::Unknown);
        assert_eq!(sniff(b"  \n", true), Sniffed::Unknown);
    }

    #[test]
    fn binary_rows() {
        let quad = oxrdf::Quad::new(
            oxrdf::NamedNode::new_unchecked("http://e/a"),
            oxrdf::NamedNode::new_unchecked("http://e/p"),
            oxrdf::Literal::new_simple_literal("x"),
            oxrdf::GraphName::DefaultGraph,
        );
        for j in [JenaFormat::Thrift, JenaFormat::Protobuf] {
            let mut w = crate::http::jena_formats::RdfWriter::new(j, Vec::new());
            w.quad(&quad).unwrap();
            let bytes = w.finish().unwrap();
            assert_eq!(sniff(&bytes, true), Sniffed::Found(jena_kind(j)), "{j:?}");
        }
        assert_eq!(sniff(&[0x00, 0x01, 0x02, 0x03], true), Sniffed::Unknown);
    }
}
