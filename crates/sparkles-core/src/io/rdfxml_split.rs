//! Cutting an RDF/XML document between the top-level elements of its `rdf:RDF` root, so
//! that the pieces can be parsed on several threads.
//!
//! Each piece is the document's prolog and root start tag, a run of the root's children
//! and the root's end tag, so it is a document of its own that declares the same
//! namespaces, base, language and entities. A piece parses to the triples that the run
//! gives in the whole document, since RDF/XML ties a triple to the elements around it and
//! to the root's attributes only. `rdf:nodeID` labels are kept as written, so references
//! between pieces hold. Two things span the document: the check that an `rdf:ID` is used
//! once, so a document with an `ID` attribute is not cut, and the order of the triples,
//! which the caller keeps by joining the pieces in order.
//!
//! The scanner reads the markup of well-formed XML: the prolog with its document type
//! declaration, comments, CDATA sections, processing instructions and quoted attribute
//! values, and counts the depth of the elements. Anything it does not expect at the top
//! level, such as text beside the root's children, an end tag that does not close the
//! root last or content after the root, refuses the cut. A malformed element inside a
//! piece makes that piece fail to parse, and the caller then parses the whole document
//! as one, which gives its answer or its error.

/// A document cut into pieces: the bytes before the root's children, the ranges of the
/// runs of children, and where the root's end tag starts.
pub(super) struct Split {
    head: usize,
    runs: Vec<(usize, usize)>,
    end: usize,
}

const RDF_NS: &[u8] = b"http://www.w3.org/1999/02/22-rdf-syntax-ns#";

impl Split {
    /// The pieces as documents, in order: the head, a run and an end tag, with the
    /// document's own end on the last one.
    pub(super) fn pieces(&self, doc: &[u8]) -> Vec<Vec<u8>> {
        let last = self.runs.len().saturating_sub(1);
        self.runs
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| {
                let tail: &[u8] = if i == last {
                    &doc[self.end..]
                } else {
                    b"</rdf:RDF>"
                };
                let mut v = Vec::with_capacity(self.head + (b - a) + tail.len());
                v.extend_from_slice(&doc[..self.head]);
                v.extend_from_slice(&doc[a..b]);
                v.extend_from_slice(tail);
                v
            })
            .collect()
    }
}

/// Cut `doc` into at most `n` pieces of whole top-level elements, or `None` when the
/// document is not one the scanner can cut or has fewer than two pieces.
pub(super) fn split(doc: &[u8], n: usize) -> Option<Split> {
    if n < 2 {
        return None;
    }
    let mut s = Scanner { b: doc, i: 0 };
    s.prolog()?;
    // the root start tag: rdf:RDF with the RDF namespace bound to rdf
    let (name, attrs, empty) = s.start_tag()?;
    if name != b"rdf:RDF"
        || empty
        || !attrs
            .iter()
            .any(|(k, v)| *k == b"xmlns:rdf" && *v == RDF_NS)
    {
        return None;
    }
    if attrs.iter().any(|(k, _)| is_id(k)) {
        return None;
    }
    let head = s.i;
    // the boundaries between the root's children: after each top-level element
    let mut ends: Vec<usize> = Vec::new();
    let end = loop {
        s.space();
        match s.peek_markup()? {
            Markup::Comment => s.comment()?,
            Markup::Pi => s.pi()?,
            Markup::End => {
                let at = s.i;
                if s.end_tag()? != b"rdf:RDF" {
                    return None;
                }
                break at;
            }
            Markup::Start => {
                s.element()?;
                ends.push(s.i);
            }
            Markup::Cdata | Markup::Doctype | Markup::Text => return None,
        }
    };
    // after the root, only white space, comments and processing instructions
    loop {
        s.space();
        if s.i == doc.len() {
            break;
        }
        match s.peek_markup()? {
            Markup::Comment => s.comment()?,
            Markup::Pi => s.pi()?,
            _ => return None,
        }
    }
    if ends.len() < 2 {
        return None;
    }
    // runs of about equal size, cut after whole elements
    let size = (end - head).div_ceil(n);
    let mut runs = Vec::new();
    let mut start = head;
    for &e in &ends {
        if e - start >= size {
            runs.push((start, e));
            start = e;
        }
    }
    // the rest up to the root's end tag: a run of its own when it holds an element, else
    // the end of the last run
    let last_element = ends.last().copied().unwrap_or(head);
    match runs.last_mut() {
        Some(last) if start >= last_element => last.1 = end,
        _ => runs.push((start, end)),
    }
    (runs.len() >= 2).then_some(Split { head, runs, end })
}

/// Whether an attribute name is `ID` in some namespace, as `rdf:ID` is.
fn is_id(name: &[u8]) -> bool {
    name == b"ID" || name.ends_with(b":ID")
}

enum Markup {
    Start,
    End,
    Comment,
    Pi,
    Cdata,
    Doctype,
    Text,
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

type Attrs<'a> = Vec<(&'a [u8], &'a [u8])>;

impl<'a> Scanner<'a> {
    fn rest(&self) -> &'a [u8] {
        &self.b[self.i.min(self.b.len())..]
    }

    fn space(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.i += 1;
        }
    }

    /// Move past the next `pat`, or fail.
    fn past(&mut self, pat: &[u8]) -> Option<()> {
        let at = memchr::memmem::find(self.rest(), pat)?;
        self.i += at + pat.len();
        Some(())
    }

    fn peek_markup(&self) -> Option<Markup> {
        let r = self.rest();
        if r.first() != Some(&b'<') {
            return Some(Markup::Text);
        }
        Some(if r.starts_with(b"<!--") {
            Markup::Comment
        } else if r.starts_with(b"<![CDATA[") {
            Markup::Cdata
        } else if r.starts_with(b"<!") {
            Markup::Doctype
        } else if r.starts_with(b"<?") {
            Markup::Pi
        } else if r.starts_with(b"</") {
            Markup::End
        } else {
            Markup::Start
        })
    }

    fn comment(&mut self) -> Option<()> {
        self.i += 4;
        self.past(b"-->")
    }

    fn pi(&mut self) -> Option<()> {
        self.i += 2;
        self.past(b"?>")
    }

    fn cdata(&mut self) -> Option<()> {
        self.i += 9;
        self.past(b"]]>")
    }

    /// The XML declaration, comments, processing instructions and the document type
    /// declaration before the root, which is next.
    fn prolog(&mut self) -> Option<()> {
        if self.rest().starts_with(b"\xEF\xBB\xBF") {
            self.i += 3;
        }
        loop {
            self.space();
            match self.peek_markup()? {
                Markup::Comment => self.comment()?,
                Markup::Pi => self.pi()?,
                Markup::Doctype => self.doctype()?,
                Markup::Start => return Some(()),
                _ => return None,
            }
        }
    }

    /// `<!DOCTYPE …>`, with an internal subset in brackets whose declarations may quote
    /// `>` and `]`.
    fn doctype(&mut self) -> Option<()> {
        if !self.rest().starts_with(b"<!DOCTYPE") {
            return None;
        }
        self.i += 9;
        let mut subset = false;
        loop {
            let c = *self.b.get(self.i)?;
            match c {
                b'"' | b'\'' => {
                    self.i += 1;
                    self.past(&[c])?;
                    continue;
                }
                b'[' if !subset => subset = true,
                b']' if subset => subset = false,
                b'<' if subset && self.rest().starts_with(b"<!--") => {
                    self.comment()?;
                    continue;
                }
                b'>' if !subset => {
                    self.i += 1;
                    return Some(());
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// A start tag of an element under the root, past its `>`: whether it is empty
    /// (`/>`), or `None` when it has an `ID` attribute or is not closed.
    fn tag(&mut self) -> Option<bool> {
        let start = self.i;
        loop {
            let at = memchr::memchr3(b'>', b'"', b'\'', self.rest())?;
            let c = self.b[self.i + at];
            self.i += at + 1;
            if c == b'>' {
                break;
            }
            let close = memchr::memchr(c, self.rest())?;
            self.i += close + 1;
        }
        let tag = &self.b[start..self.i];
        if has_id(tag) {
            return None;
        }
        Some(tag.ends_with(b"/>"))
    }

    /// A start tag: its name, its attributes and whether it is empty (`/>`).
    fn start_tag(&mut self) -> Option<(&'a [u8], Attrs<'a>, bool)> {
        self.i += 1;
        let name = self.name()?;
        let mut attrs = Vec::new();
        loop {
            self.space();
            match *self.b.get(self.i)? {
                b'>' => {
                    self.i += 1;
                    return Some((name, attrs, false));
                }
                b'/' if self.b.get(self.i + 1) == Some(&b'>') => {
                    self.i += 2;
                    return Some((name, attrs, true));
                }
                _ => {
                    let k = self.name()?;
                    self.space();
                    if self.b.get(self.i) != Some(&b'=') {
                        return None;
                    }
                    self.i += 1;
                    self.space();
                    let q = *self.b.get(self.i)?;
                    if q != b'"' && q != b'\'' {
                        return None;
                    }
                    self.i += 1;
                    let len = memchr::memchr(q, self.rest())?;
                    let v = &self.b[self.i..self.i + len];
                    self.i += len + 1;
                    attrs.push((k, v));
                }
            }
        }
    }

    fn end_tag(&mut self) -> Option<&'a [u8]> {
        self.i += 2;
        let name = self.name()?;
        self.space();
        if self.b.get(self.i) != Some(&b'>') {
            return None;
        }
        self.i += 1;
        Some(name)
    }

    fn name(&mut self) -> Option<&'a [u8]> {
        let start = self.i;
        while self.b.get(self.i).is_some_and(|&c| {
            !matches!(
                c,
                b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/' | b'=' | b'<' | b'"' | b'\''
            )
        }) {
            self.i += 1;
        }
        (self.i > start).then(|| &self.b[start..self.i])
    }

    /// One element and everything in it, from its start tag, by the depth of the tags.
    fn element(&mut self) -> Option<()> {
        let mut depth = 0usize;
        loop {
            match self.peek_markup()? {
                Markup::Comment => self.comment()?,
                Markup::Pi => self.pi()?,
                Markup::Cdata => self.cdata()?,
                Markup::End => {
                    self.past(b">")?;
                    depth = depth.checked_sub(1)?;
                }
                Markup::Start => {
                    if !self.tag()? {
                        depth += 1;
                    }
                }
                Markup::Doctype | Markup::Text => return None,
            }
            if depth == 0 {
                return Some(());
            }
            // text up to the next markup
            self.i += memchr::memchr(b'<', self.rest())?;
        }
    }
}

/// Whether a start tag has an attribute whose local name is `ID` or `bagID`, in any
/// namespace, as `rdf:ID` and `rdf:bagID` are: a name before an `=` outside the quoted
/// values, with any white space between the two.
fn has_id(tag: &[u8]) -> bool {
    let mut quote = None;
    for (i, &c) in tag.iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'=' => {
                let name = tag[..i].trim_ascii_end();
                let at = name
                    .iter()
                    .rposition(|b| b.is_ascii_whitespace() || *b == b':')
                    .map_or(0, |j| j + 1);
                if matches!(&name[at..], b"ID" | b"bagID") {
                    return true;
                }
            }
            None => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [
  <!ENTITY obo "http://purl.obolibrary.org/obo/" >
  <!-- a comment with ] and > -->
]>
<rdf:RDF xmlns="http://ex.org/#" xml:base="http://ex.org/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about=""/>
  <!-- between > elements -->
  <owl:Class rdf:about="&obo;A"><rdf:type rdf:nodeID="n1"/></owl:Class>
  <owl:Class rdf:about="B">
    <x xml:lang="en">a &lt; b > c <![CDATA[ </owl:Class> ]]></x>
    <y rdf:parseType="Literal"><p>text <b>bold</b></p></y>
  </owl:Class>
  <?pi data > ?>
  <rdf:Description rdf:nodeID="n1"><owl:sameAs rdf:resource="C"/></rdf:Description>
</rdf:RDF>
<!-- after -->
"#;

    #[test]
    fn a_document_cuts_between_top_level_elements() {
        let doc = DOC.as_bytes();
        let s = split(doc, 3).expect("a cut");
        let pieces = s.pieces(doc);
        assert!(pieces.len() >= 2);
        for p in &pieces {
            let p = std::str::from_utf8(p).unwrap();
            assert!(p.starts_with("<?xml"), "{p}");
            assert!(p.trim_end().ends_with("</rdf:RDF>") || p.ends_with("<!-- after -->\n"));
        }
        // the runs cover the children once, in order
        let joined: Vec<u8> = s
            .runs
            .iter()
            .flat_map(|&(a, b)| doc[a..b].to_vec())
            .collect();
        assert_eq!(&joined[..], &doc[s.head..s.end]);
    }

    #[test]
    fn what_the_scanner_does_not_expect_is_not_cut() {
        let refused = [
            DOC.replace("rdf:about=\"B\"", "rdf:ID=\"B\""),
            DOC.replace("rdf:about=\"B\"", "rdf:ID\t=\"B\""),
            DOC.replace("rdf:about=\"B\"", "r:ID\n =\r\"B\""),
            DOC.replace("rdf:about=\"B\"", "rdf:bagID=\"B\""),
            DOC.replace("<rdf:RDF ", "<RDF ")
                .replace("</rdf:RDF>", "</RDF>"),
            DOC.replace("<!-- after -->", "<x/>"),
            DOC.replace("</rdf:RDF>", ""),
            DOC.replace("<!-- between > elements -->", "text"),
            DOC.replace(
                "xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"",
                "xmlns:rdf=\"http://ex.org/other#\"",
            ),
        ];
        for d in &refused {
            assert_ne!(d, DOC);
            assert!(split(d.as_bytes(), 3).is_none(), "{d}");
        }
        assert!(split(DOC.as_bytes(), 1).is_none());
    }

    /// The triples with their blank nodes named by first appearance, which two parses
    /// of one document share whatever labels the parser gave its anonymous nodes.
    fn canonical(ts: &[oxrdf::Triple]) -> Vec<String> {
        let mut names = std::collections::HashMap::new();
        let mut name = |b: &oxrdf::BlankNode| {
            let n = names.len();
            names.entry(b.as_str().to_string()).or_insert(n).to_string()
        };
        ts.iter()
            .map(|t| {
                let s = match &t.subject {
                    oxrdf::NamedOrBlankNode::BlankNode(b) => format!("_:{}", name(b)),
                    s => s.to_string(),
                };
                let o = match &t.object {
                    oxrdf::Term::BlankNode(b) => format!("_:{}", name(b)),
                    o => o.to_string(),
                };
                format!("{s} {} {o}", t.predicate)
            })
            .collect()
    }

    fn document(n: usize) -> String {
        let mut d = String::from(
            r#"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF [ <!ENTITY obo "http://purl.obolibrary.org/obo/" > ]>
<rdf:RDF xmlns="http://ex.org/#" xml:base="http://ex.org/base" xml:lang="en"
     xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
     xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"
     xmlns:owl="http://www.w3.org/2002/07/owl#">
  <owl:Ontology rdf:about=""/>
"#,
        );
        for i in 0..n {
            d.push_str(&format!(
                r##"  <owl:Class rdf:about="&obo;C{i}">
    <rdfs:label>class {i}</rdfs:label>
    <rdfs:subClassOf>
      <owl:Restriction>
        <owl:onProperty rdf:resource="#p"/>
        <owl:someValuesFrom rdf:nodeID="n{}"/>
      </owl:Restriction>
    </rdfs:subClassOf>
    <rdfs:comment rdf:parseType="Literal"><b>bold {i}</b></rdfs:comment>
  </owl:Class>
  <rdf:Description rdf:nodeID="n{i}"><rdf:type rdf:resource="http://www.w3.org/2002/07/owl#Class"/></rdf:Description>
  <!-- {i} -->
"##,
                (i + 7) % n.max(1)
            ));
        }
        d.push_str("</rdf:RDF>\n");
        d
    }

    /// the canonical triples and the prefixes of a parse
    type Parsed = (Vec<String>, Vec<(String, String)>);

    fn parse(doc: &str, threads: usize) -> crate::Result<Parsed> {
        let src = super::super::Source::from_bytes(
            doc.as_bytes().to_vec(),
            oxrdfio::RdfFormat::RdfXml,
            None,
        );
        let (t, p) = super::super::parse_triples(&src, threads)?;
        Ok((canonical(&t), p.into_iter().collect()))
    }

    #[test]
    fn a_cut_document_parses_to_the_whole_documents_triples() {
        for n in [3, 40, 300] {
            let d = document(n);
            let want = parse(&d, 1).unwrap();
            assert!(want.0.len() >= 7 * n);
            if n >= 40 {
                assert!(split(d.as_bytes(), 4).is_some());
            }
            for threads in [2, 4, 8] {
                assert_eq!(parse(&d, threads).unwrap(), want, "{n} {threads}");
            }
        }
    }

    #[test]
    fn a_repeated_id_in_two_pieces_fails_as_in_the_whole_document() {
        let d = document(300);
        for spacing in ["", "\t", "\n ", "\r\n"] {
            let b = d
                .replacen(
                    "<owl:Class rdf:about=\"&obo;C10\">",
                    &format!("<owl:Class rdf:ID{spacing}=\"same\">"),
                    1,
                )
                .replacen(
                    "<owl:Class rdf:about=\"&obo;C250\">",
                    &format!("<owl:Class rdf:ID{spacing}=\"same\">"),
                    1,
                );
            let want = parse(&b, 1).unwrap_err().to_string();
            assert_eq!(parse(&b, 8).unwrap_err().to_string(), want, "{spacing:?}");
        }
        assert!(!has_id(b"<x a=\"rdf:ID=\" b='ID = 1'>"));
        assert!(has_id(b"<x a=\"1\" ID =\"2\">"));
    }

    #[test]
    fn a_cut_document_fails_with_the_whole_documents_error() {
        let d = document(300);
        let broken = [
            d.replacen("</rdfs:label>", "</rdfs:labell>", 1),
            d.replacen(
                "rdf:resource=\"#p\"",
                "rdf:resource=\"#p\" rdf:resource=\"#q\"",
                1,
            ),
            d.replacen("&obo;C150", "&nope;C150", 1),
            d.replacen(
                "<rdf:Description rdf:nodeID=\"n200\">",
                "<rdf:Description rdf:nodeID=\"n200\" rdf:about=\"x\">",
                1,
            ),
        ];
        for b in &broken {
            assert_ne!(b, &d);
            let want = parse(b, 1).unwrap_err().to_string();
            assert_eq!(parse(b, 8).unwrap_err().to_string(), want);
        }
    }
}
