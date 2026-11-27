//! OWL 2 XML Serialization (`.owx`) writer and reader (W3C OWL 2 XML Serialization,
//! Second Edition, 2012).
//!
//! [`write_document`] streams an ontology header and axioms directly to any
//! [`std::io::Write`] sink with deterministic prefix ordering, XML 1.0 character
//! validation and escaping, and bounded expression/annotation nesting. All 37 OWL 2
//! axiom types, all 18 class expressions, all 6 data ranges, nested annotations,
//! anonymous individuals and repeated `Multiset` operands are written losslessly.
//!
//! [`parse`] reads an OWL/XML document back into an [`crate::ofn_read::Document`],
//! supporting `<Prefix>`, `xml:base`, `abbreviatedIRI`, `IRI`, `nodeID`, `xml:lang`,
//! `datatypeIRI`, CDATA sections, comments and entity references with bounded nesting.

use crate::model::*;
use crate::ofn::{Renderer, STANDARD_PREFIXES};
use crate::ofn_read::{Document, ParseError};
use crate::read::MAX_NESTING;
use crate::validate;
use crate::vocab;
use crate::write_common::{WriteError, WriteReport};
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;
use std::io::Write;

const OWL_XML_NS: &str = "http://www.w3.org/2002/07/owl#";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

// ------------------------------------------------------------------ writing ------

/// Whether `c` is a legal XML 1.0 character (`Char` production in XML 1.0 §2.2).
fn is_xml10_char(c: char) -> bool {
    matches!(
        c as u32,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
    )
}

fn check_xml_text(s: &str, what: &str) -> Result<(), WriteError> {
    if let Some(c) = s.chars().find(|&c| !is_xml10_char(c)) {
        return Err(WriteError::Unsupported(format!(
            "OWL/XML cannot represent XML 1.0 illegal control character U+{:04X} in {what}",
            c as u32
        )));
    }
    Ok(())
}

/// Write `s` as XML text content, escaping `&`, `<`, `>` and `\r` (preserving carriage
/// returns across XML 1.0 end-of-line normalization).
fn write_xml_text(w: &mut dyn Write, s: &str) -> Result<(), WriteError> {
    check_xml_text(s, "text")?;
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let esc = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'\r' => "&#xD;",
            _ => continue,
        };
        if i > last {
            w.write_all(&s.as_bytes()[last..i])?;
        }
        w.write_all(esc.as_bytes())?;
        last = i + 1;
    }
    if last < s.len() {
        w.write_all(&s.as_bytes()[last..])?;
    }
    Ok(())
}

/// Write `s` as a double-quoted XML attribute value, escaping `&`, `<`, `>`, `"`, `\r`,
/// `\n` and `\t`.
fn write_xml_attr(w: &mut dyn Write, s: &str) -> Result<(), WriteError> {
    check_xml_text(s, "attribute")?;
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let esc = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\r' => "&#xD;",
            b'\n' => "&#xA;",
            b'\t' => "&#x9;",
            _ => continue,
        };
        if i > last {
            w.write_all(&s.as_bytes()[last..i])?;
        }
        w.write_all(esc.as_bytes())?;
        last = i + 1;
    }
    if last < s.len() {
        w.write_all(&s.as_bytes()[last..])?;
    }
    Ok(())
}

/// Whether `s` is a safe abbreviatedIRI local name (matching [`Renderer::iri`]).
fn is_local_name(s: &str) -> bool {
    crate::write_common::is_pn_local(s)
}

/// Whether `s` is a valid OWL/XML `<Prefix name="..."/>` name (`""` or a W3C `PN_PREFIX`).
fn is_owx_prefix(s: &str) -> bool {
    s.is_empty() || crate::write_common::is_pn_prefix(s)
}

/// XML 1.0 Namespaces `NCNameStartChar` (`NameStartChar - ':'`).
fn is_ncname_start(c: char) -> bool {
    matches!(
        c as u32,
        0x41..=0x5A
            | 0x5F
            | 0x61..=0x7A
            | 0xC0..=0xD6
            | 0xD8..=0xF6
            | 0xF8..=0x2FF
            | 0x370..=0x37D
            | 0x37F..=0x1FFF
            | 0x200C..=0x200D
            | 0x2070..=0x218F
            | 0x2C00..=0x2FEF
            | 0x3001..=0xD7FF
            | 0xF900..=0xFDCF
            | 0xFDF0..=0xFFFD
            | 0x10000..=0xEFFFF
    )
}

/// XML 1.0 Namespaces `NCNameChar` (`NameChar - ':'`).
fn is_ncname_char(c: char) -> bool {
    is_ncname_start(c)
        || matches!(
            c as u32,
            0x2D | 0x2E | 0x30..=0x39 | 0xB7 | 0x0300..=0x036F | 0x203F..=0x2040
        )
}

/// Whether `s` is a valid XML 1.0 `xsd:NCName` (required for `AnonymousIndividual/@nodeID`).
pub(crate) fn is_xml_ncname(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(is_ncname_start) && cs.all(is_ncname_char)
}

const OWX_BNODE_ESC_PREFIX: &str = "__owx_";

/// Map any anonymous individual label deterministically and injectively to a valid XML
/// `NCName`, preserving ordinary `NCName` labels verbatim.
fn encode_owx_node_id(label: &str) -> Result<String, WriteError> {
    check_xml_text(label, "nodeID")?;
    if is_xml_ncname(label) && !label.starts_with(OWX_BNODE_ESC_PREFIX) {
        return Ok(label.to_string());
    }
    let mut out = String::with_capacity(OWX_BNODE_ESC_PREFIX.len() + label.len() * 2);
    out.push_str(OWX_BNODE_ESC_PREFIX);
    for b in label.as_bytes() {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    Ok(out)
}

struct OwxWriter<'a> {
    w: &'a mut dyn Write,
    /// `(prefix, namespace)` sorted by prefix name for `<Prefix>` declarations
    declared_prefixes: Vec<(String, String)>,
    /// `(prefix, namespace)` longest namespace first for deterministic contraction
    prefixes: Vec<(String, String)>,
    depth: usize,
}

fn abbreviate<'p, 'i>(
    prefixes: &'p [(String, String)],
    iri: &'i str,
) -> Option<(&'p str, &'i str)> {
    for (p, ns) in prefixes {
        if let Some(local) = iri.strip_prefix(ns.as_str())
            && is_local_name(local)
        {
            return Some((p.as_str(), local));
        }
    }
    None
}

impl<'a> OwxWriter<'a> {
    fn new(w: &'a mut dyn Write, renderer: &Renderer) -> Self {
        let mut by_name: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (p, n) in renderer.prefixes() {
            if is_owx_prefix(p) && !n.is_empty() {
                by_name.insert(p.to_string(), n.to_string());
            }
        }
        let declared_prefixes: Vec<(String, String)> = by_name.into_iter().collect();
        let mut prefixes = declared_prefixes.clone();
        prefixes.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        OwxWriter {
            w,
            declared_prefixes,
            prefixes,
            depth: 0,
        }
    }

    fn enter(&mut self) -> Result<(), WriteError> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(WriteError::Unsupported(format!(
                "OWL/XML expression nests deeper than {MAX_NESTING} levels"
            )));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn indent(&mut self, level: usize) -> Result<(), WriteError> {
        for _ in 0..level {
            self.w.write_all(b"    ")?;
        }
        Ok(())
    }

    fn write_entity_tag(&mut self, indent: usize, tag: &str, iri: &str) -> Result<(), WriteError> {
        self.indent(indent)?;
        write!(self.w, "<{tag} ")?;
        if let Some((p, local)) = abbreviate(&self.prefixes, iri) {
            self.w.write_all(b"abbreviatedIRI=\"")?;
            write_xml_attr(self.w, p)?;
            self.w.write_all(b":")?;
            write_xml_attr(self.w, local)?;
            self.w.write_all(b"\"/>\n")?;
        } else {
            self.w.write_all(b"IRI=\"")?;
            write_xml_attr(self.w, iri)?;
            self.w.write_all(b"\"/>\n")?;
        }
        Ok(())
    }

    fn write_iri_elem(&mut self, indent: usize, iri: &str) -> Result<(), WriteError> {
        self.indent(indent)?;
        if let Some((p, local)) = abbreviate(&self.prefixes, iri) {
            self.w.write_all(b"<AbbreviatedIRI>")?;
            write_xml_text(self.w, p)?;
            self.w.write_all(b":")?;
            write_xml_text(self.w, local)?;
            self.w.write_all(b"</AbbreviatedIRI>\n")?;
        } else {
            self.w.write_all(b"<IRI>")?;
            write_xml_text(self.w, iri)?;
            self.w.write_all(b"</IRI>\n")?;
        }
        Ok(())
    }

    fn write_anon_elem(
        &mut self,
        indent: usize,
        a: &AnonymousIndividual,
    ) -> Result<(), WriteError> {
        let node_id = encode_owx_node_id(a.label())?;
        self.indent(indent)?;
        self.w.write_all(b"<AnonymousIndividual nodeID=\"")?;
        write_xml_attr(self.w, &node_id)?;
        self.w.write_all(b"\"/>\n")?;
        Ok(())
    }

    fn write_individual(&mut self, indent: usize, i: &Individual) -> Result<(), WriteError> {
        match i {
            Individual::Named(n) => self.write_entity_tag(indent, "NamedIndividual", n.as_str()),
            Individual::Anonymous(a) => self.write_anon_elem(indent, a),
        }
    }

    fn write_literal(&mut self, indent: usize, l: &Literal) -> Result<(), WriteError> {
        self.indent(indent)?;
        self.w.write_all(b"<Literal")?;
        if let Some(lang) = l.language() {
            self.w.write_all(b" xml:lang=\"")?;
            write_xml_attr(self.w, lang)?;
            self.w.write_all(b"\"")?;
        } else if l.datatype() != vocab::XSD_STRING {
            self.w.write_all(b" datatypeIRI=\"")?;
            write_xml_attr(self.w, l.datatype())?;
            self.w.write_all(b"\"")?;
        }
        self.w.write_all(b">")?;
        write_xml_text(self.w, l.value())?;
        self.w.write_all(b"</Literal>\n")?;
        Ok(())
    }

    fn write_annotation(&mut self, indent: usize, a: &Annotation) -> Result<(), WriteError> {
        self.enter()?;
        self.indent(indent)?;
        self.w.write_all(b"<Annotation>\n")?;
        for nested in &a.annotations {
            self.write_annotation(indent + 1, nested)?;
        }
        self.write_entity_tag(indent + 1, "AnnotationProperty", a.property.as_str())?;
        match &a.value {
            AnnotationValue::Iri(i) => self.write_iri_elem(indent + 1, i.as_str())?,
            AnnotationValue::Literal(l) => self.write_literal(indent + 1, l)?,
            AnnotationValue::Anonymous(an) => self.write_anon_elem(indent + 1, an)?,
        }
        self.indent(indent)?;
        self.w.write_all(b"</Annotation>\n")?;
        self.leave();
        Ok(())
    }

    fn write_ope(&mut self, indent: usize, p: &ObjectPropertyExpression) -> Result<(), WriteError> {
        match p {
            ObjectPropertyExpression::Property(p) => {
                self.write_entity_tag(indent, "ObjectProperty", p.as_str())
            }
            ObjectPropertyExpression::InverseOf(p) => {
                self.indent(indent)?;
                self.w.write_all(b"<ObjectInverseOf>\n")?;
                self.write_entity_tag(indent + 1, "ObjectProperty", p.as_str())?;
                self.indent(indent)?;
                self.w.write_all(b"</ObjectInverseOf>\n")?;
                Ok(())
            }
        }
    }

    fn write_data_range(&mut self, indent: usize, r: &DataRange) -> Result<(), WriteError> {
        self.enter()?;
        let res = (|| {
            match r {
                DataRange::Datatype(d) => self.write_entity_tag(indent, "Datatype", d.as_str())?,
                DataRange::IntersectionOf(rs) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<DataIntersectionOf>\n")?;
                    for sub in rs {
                        self.write_data_range(indent + 1, sub)?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</DataIntersectionOf>\n")?;
                }
                DataRange::UnionOf(rs) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<DataUnionOf>\n")?;
                    for sub in rs {
                        self.write_data_range(indent + 1, sub)?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</DataUnionOf>\n")?;
                }
                DataRange::ComplementOf(sub) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<DataComplementOf>\n")?;
                    self.write_data_range(indent + 1, sub)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</DataComplementOf>\n")?;
                }
                DataRange::OneOf(ls) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<DataOneOf>\n")?;
                    for l in ls {
                        self.write_literal(indent + 1, l)?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</DataOneOf>\n")?;
                }
                DataRange::DatatypeRestriction(d, fs) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<DatatypeRestriction>\n")?;
                    self.write_entity_tag(indent + 1, "Datatype", d.as_str())?;
                    for f in fs {
                        self.indent(indent + 1)?;
                        self.w.write_all(b"<FacetRestriction facet=\"")?;
                        write_xml_attr(self.w, f.facet.as_str())?;
                        self.w.write_all(b"\">\n")?;
                        self.write_literal(indent + 2, &f.value)?;
                        self.indent(indent + 1)?;
                        self.w.write_all(b"</FacetRestriction>\n")?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</DatatypeRestriction>\n")?;
                }
            }
            Ok(())
        })();
        self.leave();
        res
    }

    fn write_class_expression(
        &mut self,
        indent: usize,
        c: &ClassExpression,
    ) -> Result<(), WriteError> {
        use ClassExpression as C;
        self.enter()?;
        let res = (|| {
            match c {
                C::Class(c) => self.write_entity_tag(indent, "Class", c.as_str())?,
                C::ObjectIntersectionOf(cs) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectIntersectionOf>\n")?;
                    for x in cs {
                        self.write_class_expression(indent + 1, x)?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectIntersectionOf>\n")?;
                }
                C::ObjectUnionOf(cs) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectUnionOf>\n")?;
                    for x in cs {
                        self.write_class_expression(indent + 1, x)?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectUnionOf>\n")?;
                }
                C::ObjectComplementOf(x) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectComplementOf>\n")?;
                    self.write_class_expression(indent + 1, x)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectComplementOf>\n")?;
                }
                C::ObjectOneOf(is) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectOneOf>\n")?;
                    for i in is {
                        self.write_individual(indent + 1, i)?;
                    }
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectOneOf>\n")?;
                }
                C::ObjectSomeValuesFrom(p, f) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectSomeValuesFrom>\n")?;
                    self.write_ope(indent + 1, p)?;
                    self.write_class_expression(indent + 1, f)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectSomeValuesFrom>\n")?;
                }
                C::ObjectAllValuesFrom(p, f) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectAllValuesFrom>\n")?;
                    self.write_ope(indent + 1, p)?;
                    self.write_class_expression(indent + 1, f)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectAllValuesFrom>\n")?;
                }
                C::ObjectHasValue(p, i) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectHasValue>\n")?;
                    self.write_ope(indent + 1, p)?;
                    self.write_individual(indent + 1, i)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectHasValue>\n")?;
                }
                C::ObjectHasSelf(p) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<ObjectHasSelf>\n")?;
                    self.write_ope(indent + 1, p)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</ObjectHasSelf>\n")?;
                }
                C::ObjectMinCardinality(n, p, f)
                | C::ObjectMaxCardinality(n, p, f)
                | C::ObjectExactCardinality(n, p, f) => {
                    let tag = c.type_name();
                    self.indent(indent)?;
                    writeln!(self.w, "<{tag} cardinality=\"{n}\">")?;
                    self.write_ope(indent + 1, p)?;
                    if let Some(f) = f {
                        self.write_class_expression(indent + 1, f)?;
                    }
                    self.indent(indent)?;
                    writeln!(self.w, "</{tag}>")?;
                }
                C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
                    let tag = c.type_name();
                    self.indent(indent)?;
                    writeln!(self.w, "<{tag}>")?;
                    for p in ps {
                        self.write_entity_tag(indent + 1, "DataProperty", p.as_str())?;
                    }
                    self.write_data_range(indent + 1, r)?;
                    self.indent(indent)?;
                    writeln!(self.w, "</{tag}>")?;
                }
                C::DataHasValue(p, l) => {
                    self.indent(indent)?;
                    self.w.write_all(b"<DataHasValue>\n")?;
                    self.write_entity_tag(indent + 1, "DataProperty", p.as_str())?;
                    self.write_literal(indent + 1, l)?;
                    self.indent(indent)?;
                    self.w.write_all(b"</DataHasValue>\n")?;
                }
                C::DataMinCardinality(n, p, r)
                | C::DataMaxCardinality(n, p, r)
                | C::DataExactCardinality(n, p, r) => {
                    let tag = c.type_name();
                    self.indent(indent)?;
                    writeln!(self.w, "<{tag} cardinality=\"{n}\">")?;
                    self.write_entity_tag(indent + 1, "DataProperty", p.as_str())?;
                    if let Some(r) = r {
                        self.write_data_range(indent + 1, r)?;
                    }
                    self.indent(indent)?;
                    writeln!(self.w, "</{tag}>")?;
                }
            }
            Ok(())
        })();
        self.leave();
        res
    }

    fn write_axiom(&mut self, ax: &Axiom) -> Result<(), WriteError> {
        use AxiomKind as K;
        let tag = ax.axiom_type().name();
        self.indent(1)?;
        writeln!(self.w, "<{tag}>")?;
        for a in &ax.annotations {
            self.write_annotation(2, a)?;
        }
        match &ax.kind {
            K::Declaration(e) => self.write_entity_tag(2, e.kind().name(), e.iri().as_str())?,
            K::SubClassOf(a, b) => {
                self.write_class_expression(2, a)?;
                self.write_class_expression(2, b)?;
            }
            K::EquivalentClasses(cs) => {
                if cs.len() == 1 {
                    let c = cs.iter().next().expect("len 1");
                    self.write_class_expression(2, c)?;
                    self.write_class_expression(2, c)?;
                } else {
                    for c in cs {
                        self.write_class_expression(2, c)?;
                    }
                }
            }
            K::DisjointClasses(cs) => {
                for c in cs {
                    self.write_class_expression(2, c)?;
                }
            }
            K::DisjointUnion(c, cs) => {
                self.write_entity_tag(2, "Class", c.as_str())?;
                for x in cs {
                    self.write_class_expression(2, x)?;
                }
            }
            K::SubObjectPropertyOf(sub, sup) => {
                match sub {
                    SubObjectPropertyExpression::Expression(p) => self.write_ope(2, p)?,
                    SubObjectPropertyExpression::Chain(ps) => {
                        self.indent(2)?;
                        self.w.write_all(b"<ObjectPropertyChain>\n")?;
                        for p in ps {
                            self.write_ope(3, p)?;
                        }
                        self.indent(2)?;
                        self.w.write_all(b"</ObjectPropertyChain>\n")?;
                    }
                }
                self.write_ope(2, sup)?;
            }
            K::EquivalentObjectProperties(ps) => {
                if ps.len() == 1 {
                    let p = ps.iter().next().expect("len 1");
                    self.write_ope(2, p)?;
                    self.write_ope(2, p)?;
                } else {
                    for p in ps {
                        self.write_ope(2, p)?;
                    }
                }
            }
            K::DisjointObjectProperties(ps) => {
                for p in ps {
                    self.write_ope(2, p)?;
                }
            }
            K::InverseObjectProperties(a, b) => {
                self.write_ope(2, a)?;
                self.write_ope(2, b)?;
            }
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
                self.write_ope(2, p)?;
                self.write_class_expression(2, c)?;
            }
            K::FunctionalObjectProperty(p)
            | K::InverseFunctionalObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
            | K::IrreflexiveObjectProperty(p)
            | K::SymmetricObjectProperty(p)
            | K::AsymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p) => self.write_ope(2, p)?,
            K::SubDataPropertyOf(a, b) => {
                self.write_entity_tag(2, "DataProperty", a.as_str())?;
                self.write_entity_tag(2, "DataProperty", b.as_str())?;
            }
            K::EquivalentDataProperties(ps) => {
                if ps.len() == 1 {
                    let p = ps.iter().next().expect("len 1");
                    self.write_entity_tag(2, "DataProperty", p.as_str())?;
                    self.write_entity_tag(2, "DataProperty", p.as_str())?;
                } else {
                    for p in ps {
                        self.write_entity_tag(2, "DataProperty", p.as_str())?;
                    }
                }
            }
            K::DisjointDataProperties(ps) => {
                for p in ps {
                    self.write_entity_tag(2, "DataProperty", p.as_str())?;
                }
            }
            K::DataPropertyDomain(p, c) => {
                self.write_entity_tag(2, "DataProperty", p.as_str())?;
                self.write_class_expression(2, c)?;
            }
            K::DataPropertyRange(p, r) => {
                self.write_entity_tag(2, "DataProperty", p.as_str())?;
                self.write_data_range(2, r)?;
            }
            K::FunctionalDataProperty(p) => {
                self.write_entity_tag(2, "DataProperty", p.as_str())?;
            }
            K::DatatypeDefinition(d, r) => {
                self.write_entity_tag(2, "Datatype", d.as_str())?;
                self.write_data_range(2, r)?;
            }
            K::HasKey(c, ops, dps) => {
                self.write_class_expression(2, c)?;
                for p in ops {
                    self.write_ope(2, p)?;
                }
                for p in dps {
                    self.write_entity_tag(2, "DataProperty", p.as_str())?;
                }
            }
            K::SameIndividual(is) => {
                if is.len() == 1 {
                    let i = is.iter().next().expect("len 1");
                    self.write_individual(2, i)?;
                    self.write_individual(2, i)?;
                } else {
                    for i in is {
                        self.write_individual(2, i)?;
                    }
                }
            }
            K::DifferentIndividuals(is) => {
                for i in is {
                    self.write_individual(2, i)?;
                }
            }
            K::ClassAssertion(c, i) => {
                self.write_class_expression(2, c)?;
                self.write_individual(2, i)?;
            }
            K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
                self.write_ope(2, p)?;
                self.write_individual(2, a)?;
                self.write_individual(2, b)?;
            }
            K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
                self.write_entity_tag(2, "DataProperty", p.as_str())?;
                self.write_individual(2, a)?;
                self.write_literal(2, l)?;
            }
            K::AnnotationAssertion(p, s, v) => {
                self.write_entity_tag(2, "AnnotationProperty", p.as_str())?;
                match s {
                    AnnotationSubject::Iri(i) => self.write_iri_elem(2, i.as_str())?,
                    AnnotationSubject::Anonymous(an) => self.write_anon_elem(2, an)?,
                }
                match v {
                    AnnotationValue::Iri(i) => self.write_iri_elem(2, i.as_str())?,
                    AnnotationValue::Literal(l) => self.write_literal(2, l)?,
                    AnnotationValue::Anonymous(an) => self.write_anon_elem(2, an)?,
                }
            }
            K::SubAnnotationPropertyOf(a, b) => {
                self.write_entity_tag(2, "AnnotationProperty", a.as_str())?;
                self.write_entity_tag(2, "AnnotationProperty", b.as_str())?;
            }
            K::AnnotationPropertyDomain(p, i) | K::AnnotationPropertyRange(p, i) => {
                self.write_entity_tag(2, "AnnotationProperty", p.as_str())?;
                self.write_iri_elem(2, i.as_str())?;
            }
        }
        self.indent(1)?;
        writeln!(self.w, "</{tag}>")?;
        Ok(())
    }
}

/// Stream an ontology header and axioms as an OWL/XML document directly into `w`.
pub fn write_document<'a>(
    w: &mut dyn Write,
    renderer: &Renderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> Result<WriteReport, WriteError> {
    let base = header.iri.as_ref().map_or(OWL_XML_NS, |i| i.as_str());
    w.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    w.write_all(b"<Ontology xmlns=\"")?;
    write_xml_attr(w, OWL_XML_NS)?;
    w.write_all(b"\"\n     xml:base=\"")?;
    write_xml_attr(w, base)?;
    w.write_all(b"\"\n     xmlns:rdf=\"")?;
    write_xml_attr(w, vocab::RDF)?;
    w.write_all(b"\"\n     xmlns:xml=\"")?;
    write_xml_attr(w, XML_NS)?;
    w.write_all(b"\"\n     xmlns:xsd=\"")?;
    write_xml_attr(w, vocab::XSD)?;
    w.write_all(b"\"\n     xmlns:rdfs=\"")?;
    write_xml_attr(w, vocab::RDFS)?;
    w.write_all(b"\"")?;
    if let Some(iri) = &header.iri {
        w.write_all(b"\n     ontologyIRI=\"")?;
        write_xml_attr(w, iri.as_str())?;
        w.write_all(b"\"")?;
        if let Some(ver) = &header.version_iri {
            w.write_all(b"\n     versionIRI=\"")?;
            write_xml_attr(w, ver.as_str())?;
            w.write_all(b"\"")?;
        }
    }
    w.write_all(b">\n")?;

    let mut out = OwxWriter::new(w, renderer);
    let declared = out.declared_prefixes.clone();
    for (p, n) in &declared {
        out.indent(1)?;
        out.w.write_all(b"<Prefix name=\"")?;
        write_xml_attr(out.w, p)?;
        out.w.write_all(b"\" IRI=\"")?;
        write_xml_attr(out.w, n)?;
        out.w.write_all(b"\"/>\n")?;
    }
    for imp in &header.imports {
        out.indent(1)?;
        out.w.write_all(b"<Import>")?;
        write_xml_text(out.w, imp.as_str())?;
        out.w.write_all(b"</Import>\n")?;
    }
    for ann in &header.annotations {
        out.write_annotation(1, ann)?;
    }

    let mut report = WriteReport::default();
    for ax in axioms {
        out.write_axiom(ax)?;
        report.written_axioms += 1;
    }
    out.w.write_all(b"</Ontology>\n")?;
    Ok(report)
}

/// Format an ontology header and axioms as an OWL/XML string.
pub fn document<'a>(
    renderer: &Renderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> Result<String, WriteError> {
    let mut buf = Vec::new();
    write_document(&mut buf, renderer, header, axioms)?;
    String::from_utf8(buf).map_err(|e| WriteError::Unsupported(e.to_string()))
}

// ------------------------------------------------------------------ reading ------

/// Read an OWL/XML document into a [`Document`].
pub fn parse(text: &str) -> Result<Document, ParseError> {
    let mut r = XmlReader::new(text);
    r.parse_ontology()
}

struct XmlReader<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    col: usize,
    prefixes: FxHashMap<String, String>,
    declared_prefixes: Vec<(String, String)>,
    base: Option<oxiri::Iri<String>>,
    depth: usize,
}

#[derive(Debug)]
enum Event {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        empty: bool,
    },
    End(String),
    Text(String),
    Doctype,
    Eof,
}

impl<'a> XmlReader<'a> {
    fn new(src: &'a str) -> Self {
        let mut prefixes = FxHashMap::default();
        for (p, ns) in STANDARD_PREFIXES {
            prefixes.insert(p.to_string(), ns.to_string());
        }
        XmlReader {
            src,
            pos: 0,
            line: 1,
            col: 1,
            prefixes,
            declared_prefixes: Vec::new(),
            base: None,
            depth: 0,
        }
    }

    fn err(&self, msg: impl Into<String>) -> ParseError {
        ParseError {
            line: self.line,
            column: self.col,
            message: msg.into(),
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.rest().starts_with(s)
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            for _ in s.chars() {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r' | '\n')) {
            self.bump();
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(self.err(format!("OWL/XML nests deeper than {MAX_NESTING} levels")));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn read_name(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, '/' | '>' | '=' | '<' | '"' | '\'') {
                break;
            }
            self.bump();
        }
        if self.pos == start {
            return Err(self.err("expected XML name"));
        }
        Ok(self.src[start..self.pos].to_string())
    }

    fn unescape(&self, raw: &str) -> Result<String, ParseError> {
        let mut out = String::with_capacity(raw.len());
        let bytes = raw.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'&' {
                let semi = raw[i + 1..]
                    .find(';')
                    .ok_or_else(|| self.err("unterminated XML entity reference"))?;
                let ent = &raw[i + 1..i + 1 + semi];
                match ent {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" => out.push('\''),
                    s if s.starts_with("#x") || s.starts_with("#X") => {
                        let cp = u32::from_str_radix(&s[2..], 16)
                            .map_err(|_| self.err(format!("invalid hex char ref &{ent};")))?;
                        let ch = char::from_u32(cp)
                            .ok_or_else(|| self.err(format!("invalid unicode scalar &{ent};")))?;
                        out.push(ch);
                    }
                    s if s.starts_with('#') => {
                        let cp: u32 = s[1..]
                            .parse()
                            .map_err(|_| self.err(format!("invalid decimal char ref &{ent};")))?;
                        let ch = char::from_u32(cp)
                            .ok_or_else(|| self.err(format!("invalid unicode scalar &{ent};")))?;
                        out.push(ch);
                    }
                    _ => return Err(self.err(format!("unknown XML entity &{ent};"))),
                }
                i += semi + 2;
            } else {
                let ch = raw[i..].chars().next().expect("non-empty");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        Ok(out)
    }

    fn read_attr_value(&mut self) -> Result<String, ParseError> {
        let q = match self.bump() {
            Some(c @ ('"' | '\'')) => c,
            _ => return Err(self.err("expected quoted attribute value")),
        };
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == q {
                let raw = &self.src[start..self.pos];
                self.bump();
                return self.unescape(raw);
            }
            self.bump();
        }
        Err(self.err("unterminated XML attribute value"))
    }

    fn next_event(&mut self) -> Result<Event, ParseError> {
        loop {
            if self.pos >= self.src.len() {
                return Ok(Event::Eof);
            }
            if self.starts_with("<!--") {
                self.eat("<!--");
                while !self.starts_with("-->") {
                    if self.bump().is_none() {
                        return Err(self.err("unterminated XML comment"));
                    }
                }
                self.eat("-->");
                continue;
            }
            if self.starts_with("<?") {
                self.eat("<?");
                while !self.starts_with("?>") {
                    if self.bump().is_none() {
                        return Err(self.err("unterminated XML processing instruction"));
                    }
                }
                self.eat("?>");
                continue;
            }
            if self.starts_with("<![CDATA[") {
                self.eat("<![CDATA[");
                let start = self.pos;
                while !self.starts_with("]]>") {
                    if self.bump().is_none() {
                        return Err(self.err("unterminated CDATA section"));
                    }
                }
                let text = self.src[start..self.pos].to_string();
                self.eat("]]>");
                return Ok(Event::Text(text));
            }
            if self.starts_with("<!DOCTYPE") {
                self.eat("<!DOCTYPE");
                let mut depth = 0usize;
                let mut in_quote: Option<char> = None;
                let mut closed = false;
                while self.pos < self.src.len() {
                    if in_quote.is_none() {
                        if self.starts_with("<!--") {
                            self.eat("<!--");
                            while !self.starts_with("-->") {
                                if self.bump().is_none() {
                                    return Err(self.err("unterminated XML comment in <!DOCTYPE>"));
                                }
                            }
                            self.eat("-->");
                            continue;
                        }
                        if self.starts_with("<?") {
                            self.eat("<?");
                            while !self.starts_with("?>") {
                                if self.bump().is_none() {
                                    return Err(self
                                        .err("unterminated processing instruction in <!DOCTYPE>"));
                                }
                            }
                            self.eat("?>");
                            continue;
                        }
                    }
                    let Some(c) = self.bump() else {
                        break;
                    };
                    if let Some(q) = in_quote {
                        if c == q {
                            in_quote = None;
                        }
                        continue;
                    }
                    match c {
                        '\'' | '"' => in_quote = Some(c),
                        '[' => depth += 1,
                        ']' => depth = depth.saturating_sub(1),
                        '>' if depth == 0 => {
                            closed = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !closed {
                    return Err(self.err("unterminated <!DOCTYPE> declaration"));
                }
                return Ok(Event::Doctype);
            }
            if self.starts_with("</") {
                self.eat("</");
                let name = self.read_name()?;
                self.skip_ws();
                if !self.eat(">") {
                    return Err(self.err("expected '>' in closing tag"));
                }
                return Ok(Event::End(local_xml_name(&name).to_string()));
            }
            if self.eat("<") {
                let name = self.read_name()?;
                let mut attrs = Vec::new();
                loop {
                    self.skip_ws();
                    if self.eat("/>") {
                        return Ok(Event::Start {
                            name: local_xml_name(&name).to_string(),
                            attrs,
                            empty: true,
                        });
                    }
                    if self.eat(">") {
                        return Ok(Event::Start {
                            name: local_xml_name(&name).to_string(),
                            attrs,
                            empty: false,
                        });
                    }
                    let aname = self.read_name()?;
                    self.skip_ws();
                    if !self.eat("=") {
                        return Err(self.err(format!("expected '=' after attribute {aname}")));
                    }
                    self.skip_ws();
                    let aval = self.read_attr_value()?;
                    attrs.push((aname, aval));
                }
            }
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c == '<' {
                    break;
                }
                self.bump();
            }
            let raw = &self.src[start..self.pos];
            // XML 1.0 §2.11 normalizes literal \r\n and \r to \n BEFORE entity unescaping
            let normalized = if raw.contains('\r') {
                raw.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                raw.to_string()
            };
            let text = self.unescape(&normalized)?;
            return Ok(Event::Text(text));
        }
    }

    fn next_non_ws_event(&mut self) -> Result<Event, ParseError> {
        loop {
            match self.next_event()? {
                Event::Text(t) if t.chars().all(char::is_whitespace) => continue,
                ev => return Ok(ev),
            }
        }
    }

    fn read_text_until_end(&mut self, tag: &str) -> Result<String, ParseError> {
        let mut out = String::new();
        loop {
            match self.next_event()? {
                Event::Text(t) => out.push_str(&t),
                Event::End(e) if e == tag => return Ok(out),
                ev => {
                    return Err(self.err(format!("expected text or </{tag}>, got {ev:?}")));
                }
            }
        }
    }

    fn expect_end(&mut self, tag: &str) -> Result<(), ParseError> {
        match self.next_non_ws_event()? {
            Event::End(e) if e == tag => Ok(()),
            ev => Err(self.err(format!("expected </{tag}>, got {ev:?}"))),
        }
    }

    fn resolve_iri(&self, raw: &str) -> Result<Iri, ParseError> {
        let resolved = if let Some(base) = &self.base {
            base.resolve(raw)
                .map(|i| i.into_inner())
                .map_err(|e| self.err(format!("invalid IRI '{raw}': {e}")))?
        } else {
            raw.to_string()
        };
        parse_iri(&resolved).map_err(|e| self.err(e.0))
    }

    fn expand_abbreviated(&self, curie: &str) -> Result<Iri, ParseError> {
        let (p, local) = curie
            .split_once(':')
            .ok_or_else(|| self.err(format!("invalid abbreviatedIRI '{curie}'")))?;
        let ns = self
            .prefixes
            .get(p)
            .ok_or_else(|| self.err(format!("undeclared prefix '{p}:'")))?;
        let full = format!("{ns}{local}");
        parse_iri(&full).map_err(|e| self.err(e.0))
    }

    fn iri_from_attrs(&self, attrs: &[(String, String)]) -> Result<Iri, ParseError> {
        if let Some((_, v)) = attrs.iter().find(|(k, _)| k == "IRI") {
            return self.resolve_iri(v);
        }
        if let Some((_, v)) = attrs.iter().find(|(k, _)| k == "abbreviatedIRI") {
            return self.expand_abbreviated(v);
        }
        Err(self.err("expected IRI or abbreviatedIRI attribute"))
    }

    fn parse_ontology(&mut self) -> Result<Document, ParseError> {
        let mut seen_doctype = false;
        let (attrs, empty) = loop {
            match self.next_non_ws_event()? {
                Event::Doctype if !seen_doctype => {
                    seen_doctype = true;
                }
                Event::Start { name, attrs, empty } if name == "Ontology" => break (attrs, empty),
                ev => return Err(self.err(format!("expected <Ontology>, got {ev:?}"))),
            }
        };
        if let Some((_, b)) = attrs.iter().find(|(k, _)| k == "xml:base") {
            self.base = oxiri::Iri::parse(b.clone()).ok();
        }
        let mut header = Header::default();
        if let Some((_, iri)) = attrs.iter().find(|(k, _)| k == "ontologyIRI") {
            header.iri = Some(self.resolve_iri(iri)?);
        }
        if let Some((_, ver)) = attrs.iter().find(|(k, _)| k == "versionIRI") {
            header.version_iri = Some(self.resolve_iri(ver)?);
        }
        let mut axioms = Vec::new();
        if !empty {
            loop {
                match self.next_non_ws_event()? {
                    Event::End(e) if e == "Ontology" => break,
                    Event::Start { name, attrs, empty } => match name.as_str() {
                        "Prefix" => {
                            let pname = attr(&attrs, "name")
                                .ok_or_else(|| self.err("<Prefix> missing name attribute"))?;
                            let piri = attr(&attrs, "IRI")
                                .ok_or_else(|| self.err("<Prefix> missing IRI attribute"))?;
                            if !empty {
                                self.expect_end("Prefix")?;
                            }
                            self.prefixes.insert(pname.to_string(), piri.to_string());
                            if !STANDARD_PREFIXES
                                .iter()
                                .any(|(sp, sn)| *sp == pname && *sn == piri)
                            {
                                self.declared_prefixes
                                    .push((pname.to_string(), piri.to_string()));
                            }
                        }
                        "Import" => {
                            let iri_str = if empty {
                                String::new()
                            } else {
                                self.read_text_until_end("Import")?
                            };
                            header.imports.insert(self.resolve_iri(iri_str.trim())?);
                        }
                        "Annotation" => {
                            if empty {
                                return Err(self.err("empty <Annotation>"));
                            }
                            header.annotations.insert(self.parse_annotation_body()?);
                        }
                        _ => {
                            if empty {
                                return Err(self.err(format!("unexpected empty axiom <{name}/>")));
                            }
                            let ax = self.parse_axiom(&name)?;
                            validate::axiom(&ax).map_err(|e| self.err(e.0))?;
                            axioms.push(ax);
                        }
                    },
                    ev => return Err(self.err(format!("unexpected event in <Ontology>: {ev:?}"))),
                }
            }
        }
        match self.next_non_ws_event()? {
            Event::Eof => {}
            ev => {
                return Err(self.err(format!(
                    "unexpected trailing content after </Ontology>: {ev:?}"
                )));
            }
        }
        Ok(Document {
            prefixes: self.declared_prefixes.clone(),
            header,
            axioms,
            rules: 0,
        })
    }

    fn parse_annotation_body(&mut self) -> Result<Annotation, ParseError> {
        self.enter()?;
        let res = (|| {
            let mut nested = BTreeSet::new();
            let prop: AnnotationProperty;
            loop {
                match self.next_non_ws_event()? {
                    Event::Start {
                        name,
                        attrs: _,
                        empty,
                    } if name == "Annotation" => {
                        if empty {
                            return Err(self.err("empty <Annotation>"));
                        }
                        nested.insert(self.parse_annotation_body()?);
                    }
                    Event::Start { name, attrs, empty } if name == "AnnotationProperty" => {
                        if !empty {
                            self.expect_end("AnnotationProperty")?;
                        }
                        prop = AnnotationProperty(self.iri_from_attrs(&attrs)?);
                        break;
                    }
                    ev => {
                        return Err(self.err(format!(
                            "expected <Annotation> or <AnnotationProperty>, got {ev:?}"
                        )));
                    }
                }
            }
            let val = match self.next_non_ws_event()? {
                Event::Start { name, attrs, empty } => {
                    self.parse_annotation_value(&name, &attrs, empty)?
                }
                ev => return Err(self.err(format!("expected annotation value, got {ev:?}"))),
            };
            self.expect_end("Annotation")?;
            Ok(Annotation {
                property: prop,
                value: val,
                annotations: nested,
            })
        })();
        self.leave();
        res
    }

    fn parse_annotation_value(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<AnnotationValue, ParseError> {
        match name {
            "IRI" => {
                let s = if empty {
                    String::new()
                } else {
                    self.read_text_until_end("IRI")?
                };
                Ok(AnnotationValue::Iri(self.resolve_iri(s.trim())?))
            }
            "AbbreviatedIRI" => {
                let s = if empty {
                    String::new()
                } else {
                    self.read_text_until_end("AbbreviatedIRI")?
                };
                Ok(AnnotationValue::Iri(self.expand_abbreviated(s.trim())?))
            }
            "AnonymousIndividual" => {
                let id = attr(attrs, "nodeID")
                    .ok_or_else(|| self.err("<AnonymousIndividual> missing nodeID"))?;
                if !empty {
                    self.expect_end("AnonymousIndividual")?;
                }
                Ok(AnnotationValue::Anonymous(AnonymousIndividual::new(id)))
            }
            "Literal" => Ok(AnnotationValue::Literal(self.parse_literal(attrs, empty)?)),
            _ => Err(self.err(format!("unexpected annotation value <{name}>"))),
        }
    }

    fn parse_literal(
        &mut self,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<Literal, ParseError> {
        let text = if empty {
            String::new()
        } else {
            self.read_text_until_end("Literal")?
        };
        if let Some(lang) = attr(attrs, "xml:lang").filter(|l| !l.is_empty()) {
            return Literal::lang(text, lang).map_err(|e| self.err(e.0));
        }
        if let Some(dt) = attr(attrs, "datatypeIRI") {
            let dt_iri = self.resolve_iri(dt)?;
            if dt_iri.as_str() == vocab::RDF_PLAIN_LITERAL
                && let Some((v, lang)) = text.rsplit_once('@')
            {
                return if lang.is_empty() {
                    Ok(Literal::simple(v))
                } else {
                    Literal::lang(v, lang).map_err(|e| self.err(e.0))
                };
            }
            return Ok(Literal::typed(text, dt_iri.as_str()));
        }
        Ok(Literal::simple(text))
    }

    fn parse_ope(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<ObjectPropertyExpression, ParseError> {
        match name {
            "ObjectProperty" => {
                if !empty {
                    self.expect_end("ObjectProperty")?;
                }
                Ok(ObjectPropertyExpression::Property(ObjectProperty(
                    self.iri_from_attrs(attrs)?,
                )))
            }
            "ObjectInverseOf" => {
                if empty {
                    return Err(self.err("empty <ObjectInverseOf>"));
                }
                let inner = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } if n == "ObjectProperty" => {
                        if !e {
                            self.expect_end("ObjectProperty")?;
                        }
                        ObjectProperty(self.iri_from_attrs(&a)?)
                    }
                    ev => return Err(self.err(format!("expected <ObjectProperty>, got {ev:?}"))),
                };
                self.expect_end("ObjectInverseOf")?;
                Ok(ObjectPropertyExpression::InverseOf(inner))
            }
            _ => Err(self.err(format!("expected object property expression, got <{name}>"))),
        }
    }

    fn parse_individual(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<Individual, ParseError> {
        match name {
            "NamedIndividual" => {
                if !empty {
                    self.expect_end("NamedIndividual")?;
                }
                Ok(Individual::Named(NamedIndividual(
                    self.iri_from_attrs(attrs)?,
                )))
            }
            "AnonymousIndividual" => {
                let id = attr(attrs, "nodeID")
                    .ok_or_else(|| self.err("<AnonymousIndividual> missing nodeID"))?;
                if !empty {
                    self.expect_end("AnonymousIndividual")?;
                }
                Ok(Individual::Anonymous(AnonymousIndividual::new(id)))
            }
            _ => Err(self.err(format!("expected individual, got <{name}>"))),
        }
    }

    fn parse_data_range(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<DataRange, ParseError> {
        self.enter()?;
        let res = (|| match name {
            "Datatype" => {
                if !empty {
                    self.expect_end("Datatype")?;
                }
                Ok(DataRange::Datatype(Datatype(self.iri_from_attrs(attrs)?)))
            }
            "DataIntersectionOf" | "DataUnionOf" => {
                if empty {
                    return Err(self.err(format!("empty <{name}>")));
                }
                let mut rs = BTreeSet::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == name => break,
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } => {
                            rs.insert(self.parse_data_range(&n, &a, e)?);
                        }
                        ev => return Err(self.err(format!("unexpected {ev:?} in <{name}>"))),
                    }
                }
                if name == "DataIntersectionOf" {
                    Ok(DataRange::IntersectionOf(rs))
                } else {
                    Ok(DataRange::UnionOf(rs))
                }
            }
            "DataComplementOf" => {
                if empty {
                    return Err(self.err("empty <DataComplementOf>"));
                }
                let inner = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_data_range(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected data range, got {ev:?}"))),
                };
                self.expect_end("DataComplementOf")?;
                Ok(DataRange::ComplementOf(Box::new(inner)))
            }
            "DataOneOf" => {
                if empty {
                    return Err(self.err("empty <DataOneOf>"));
                }
                let mut ls = BTreeSet::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == "DataOneOf" => break,
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } if n == "Literal" => {
                            ls.insert(self.parse_literal(&a, e)?);
                        }
                        ev => return Err(self.err(format!("expected <Literal>, got {ev:?}"))),
                    }
                }
                Ok(DataRange::OneOf(ls))
            }
            "DatatypeRestriction" => {
                if empty {
                    return Err(self.err("empty <DatatypeRestriction>"));
                }
                let dt = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } if n == "Datatype" => {
                        if !e {
                            self.expect_end("Datatype")?;
                        }
                        Datatype(self.iri_from_attrs(&a)?)
                    }
                    ev => return Err(self.err(format!("expected <Datatype>, got {ev:?}"))),
                };
                let mut fs = BTreeSet::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == "DatatypeRestriction" => break,
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } if n == "FacetRestriction" => {
                            if e {
                                return Err(self.err("empty <FacetRestriction>"));
                            }
                            let facet_str = attr(&a, "facet")
                                .ok_or_else(|| self.err("<FacetRestriction> missing facet"))?;
                            let facet = self.resolve_iri(facet_str)?;
                            let value = match self.next_non_ws_event()? {
                                Event::Start {
                                    name: ln,
                                    attrs: la,
                                    empty: le,
                                } if ln == "Literal" => self.parse_literal(&la, le)?,
                                ev => {
                                    return Err(self.err(format!(
                                        "expected <Literal> in FacetRestriction, got {ev:?}"
                                    )));
                                }
                            };
                            self.expect_end("FacetRestriction")?;
                            fs.insert(FacetRestriction { facet, value });
                        }
                        ev => {
                            return Err(
                                self.err(format!("expected <FacetRestriction>, got {ev:?}"))
                            );
                        }
                    }
                }
                Ok(DataRange::DatatypeRestriction(dt, fs))
            }
            _ => Err(self.err(format!("unexpected data range <{name}>"))),
        })();
        self.leave();
        res
    }

    fn parse_class_expression(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<ClassExpression, ParseError> {
        use ClassExpression as C;
        self.enter()?;
        let res = (|| match name {
            "Class" => {
                if !empty {
                    self.expect_end("Class")?;
                }
                Ok(C::Class(Class(self.iri_from_attrs(attrs)?)))
            }
            "ObjectIntersectionOf" | "ObjectUnionOf" => {
                if empty {
                    return Err(self.err(format!("empty <{name}>")));
                }
                let mut cs = BTreeSet::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == name => break,
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } => {
                            cs.insert(self.parse_class_expression(&n, &a, e)?);
                        }
                        ev => return Err(self.err(format!("unexpected {ev:?} in <{name}>"))),
                    }
                }
                if name == "ObjectIntersectionOf" {
                    Ok(C::ObjectIntersectionOf(cs))
                } else {
                    Ok(C::ObjectUnionOf(cs))
                }
            }
            "ObjectComplementOf" => {
                if empty {
                    return Err(self.err("empty <ObjectComplementOf>"));
                }
                let inner = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_class_expression(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected class expression, got {ev:?}"))),
                };
                self.expect_end("ObjectComplementOf")?;
                Ok(C::ObjectComplementOf(Box::new(inner)))
            }
            "ObjectOneOf" => {
                if empty {
                    return Err(self.err("empty <ObjectOneOf>"));
                }
                let mut is = BTreeSet::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == "ObjectOneOf" => break,
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } => {
                            is.insert(self.parse_individual(&n, &a, e)?);
                        }
                        ev => return Err(self.err(format!("expected individual, got {ev:?}"))),
                    }
                }
                Ok(C::ObjectOneOf(is))
            }
            "ObjectSomeValuesFrom" | "ObjectAllValuesFrom" => {
                if empty {
                    return Err(self.err(format!("empty <{name}>")));
                }
                let p = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_ope(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected property, got {ev:?}"))),
                };
                let f = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_class_expression(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected class expression, got {ev:?}"))),
                };
                self.expect_end(name)?;
                if name == "ObjectSomeValuesFrom" {
                    Ok(C::ObjectSomeValuesFrom(p, Box::new(f)))
                } else {
                    Ok(C::ObjectAllValuesFrom(p, Box::new(f)))
                }
            }
            "ObjectHasValue" => {
                if empty {
                    return Err(self.err("empty <ObjectHasValue>"));
                }
                let p = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_ope(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected property, got {ev:?}"))),
                };
                let i = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_individual(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected individual, got {ev:?}"))),
                };
                self.expect_end("ObjectHasValue")?;
                Ok(C::ObjectHasValue(p, i))
            }
            "ObjectHasSelf" => {
                if empty {
                    return Err(self.err("empty <ObjectHasSelf>"));
                }
                let p = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_ope(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected property, got {ev:?}"))),
                };
                self.expect_end("ObjectHasSelf")?;
                Ok(C::ObjectHasSelf(p))
            }
            "ObjectMinCardinality" | "ObjectMaxCardinality" | "ObjectExactCardinality" => {
                if empty {
                    return Err(self.err(format!("empty <{name}>")));
                }
                let card: u32 = attr(attrs, "cardinality")
                    .ok_or_else(|| self.err(format!("<{name}> missing cardinality attribute")))?
                    .parse()
                    .map_err(|_| self.err("invalid cardinality"))?;
                let p = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => self.parse_ope(&n, &a, e)?,
                    ev => return Err(self.err(format!("expected property, got {ev:?}"))),
                };
                let filler = match self.next_non_ws_event()? {
                    Event::End(e) if e == name => None,
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => {
                        let f = self.parse_class_expression(&n, &a, e)?;
                        self.expect_end(name)?;
                        Some(Box::new(f))
                    }
                    ev => return Err(self.err(format!("unexpected {ev:?} in <{name}>"))),
                };
                Ok(match name {
                    "ObjectMinCardinality" => C::ObjectMinCardinality(card, p, filler),
                    "ObjectMaxCardinality" => C::ObjectMaxCardinality(card, p, filler),
                    _ => C::ObjectExactCardinality(card, p, filler),
                })
            }
            "DataSomeValuesFrom" | "DataAllValuesFrom" => {
                if empty {
                    return Err(self.err(format!("empty <{name}>")));
                }
                let mut ps = Vec::new();
                let dr = loop {
                    match self.next_non_ws_event()? {
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } if n == "DataProperty" => {
                            if !e {
                                self.expect_end("DataProperty")?;
                            }
                            ps.push(DataProperty(self.iri_from_attrs(&a)?));
                        }
                        Event::Start {
                            name: n,
                            attrs: a,
                            empty: e,
                        } => break self.parse_data_range(&n, &a, e)?,
                        ev => {
                            return Err(self.err(format!(
                                "expected <DataProperty> or data range, got {ev:?}"
                            )));
                        }
                    }
                };
                self.expect_end(name)?;
                if name == "DataSomeValuesFrom" {
                    Ok(C::DataSomeValuesFrom(ps, dr))
                } else {
                    Ok(C::DataAllValuesFrom(ps, dr))
                }
            }
            "DataHasValue" => {
                if empty {
                    return Err(self.err("empty <DataHasValue>"));
                }
                let p = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } if n == "DataProperty" => {
                        if !e {
                            self.expect_end("DataProperty")?;
                        }
                        DataProperty(self.iri_from_attrs(&a)?)
                    }
                    ev => return Err(self.err(format!("expected <DataProperty>, got {ev:?}"))),
                };
                let l = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } if n == "Literal" => self.parse_literal(&a, e)?,
                    ev => return Err(self.err(format!("expected <Literal>, got {ev:?}"))),
                };
                self.expect_end("DataHasValue")?;
                Ok(C::DataHasValue(p, l))
            }
            "DataMinCardinality" | "DataMaxCardinality" | "DataExactCardinality" => {
                if empty {
                    return Err(self.err(format!("empty <{name}>")));
                }
                let card: u32 = attr(attrs, "cardinality")
                    .ok_or_else(|| self.err(format!("<{name}> missing cardinality attribute")))?
                    .parse()
                    .map_err(|_| self.err("invalid cardinality"))?;
                let p = match self.next_non_ws_event()? {
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } if n == "DataProperty" => {
                        if !e {
                            self.expect_end("DataProperty")?;
                        }
                        DataProperty(self.iri_from_attrs(&a)?)
                    }
                    ev => return Err(self.err(format!("expected <DataProperty>, got {ev:?}"))),
                };
                let dr = match self.next_non_ws_event()? {
                    Event::End(e) if e == name => None,
                    Event::Start {
                        name: n,
                        attrs: a,
                        empty: e,
                    } => {
                        let r = self.parse_data_range(&n, &a, e)?;
                        self.expect_end(name)?;
                        Some(r)
                    }
                    ev => return Err(self.err(format!("unexpected {ev:?} in <{name}>"))),
                };
                Ok(match name {
                    "DataMinCardinality" => C::DataMinCardinality(card, p, dr),
                    "DataMaxCardinality" => C::DataMaxCardinality(card, p, dr),
                    _ => C::DataExactCardinality(card, p, dr),
                })
            }
            _ => Err(self.err(format!("unexpected class expression <{name}>"))),
        })();
        self.leave();
        res
    }

    fn parse_axiom(&mut self, tag: &str) -> Result<Axiom, ParseError> {
        use AxiomKind as K;
        let mut annotations = BTreeSet::new();
        // Collect leading <Annotation> elements and stop at the first non-annotation child.
        let first = loop {
            match self.next_non_ws_event()? {
                Event::Start {
                    name,
                    attrs: _,
                    empty,
                } if name == "Annotation" => {
                    if empty {
                        return Err(self.err("empty <Annotation>"));
                    }
                    annotations.insert(self.parse_annotation_body()?);
                }
                ev => break ev,
            }
        };

        type StartTag = (String, Vec<(String, String)>, bool);
        let next_start = |r: &mut Self, ev: Event| -> Result<StartTag, ParseError> {
            match ev {
                Event::Start { name, attrs, empty } => Ok((name, attrs, empty)),
                other => Err(r.err(format!("expected element inside <{tag}>, got {other:?}"))),
            }
        };

        let kind = match tag {
            "Declaration" => {
                let (n, a, e) = next_start(self, first)?;
                if !e {
                    self.expect_end(&n)?;
                }
                let kind = EntityKind::parse(&n)
                    .ok_or_else(|| self.err(format!("unexpected entity <{n}> in Declaration")))?;
                let iri = self.iri_from_attrs(&a)?;
                self.expect_end(tag)?;
                K::Declaration(Entity::of_kind(kind, iri))
            }
            "SubClassOf" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let sub = self.parse_class_expression(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let sup = self.parse_class_expression(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::SubClassOf(sub, sup)
            }
            "EquivalentClasses" => {
                let mut cs = BTreeSet::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            cs.insert(self.parse_class_expression(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::EquivalentClasses(cs)
            }
            "DisjointClasses" => {
                let mut cs = Vec::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            cs.push(self.parse_class_expression(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::DisjointClasses(cs.into())
            }
            "DisjointUnion" => {
                let (n1, a1, e1) = next_start(self, first)?;
                if n1 != "Class" {
                    return Err(self.err(format!("expected <Class> in DisjointUnion, got <{n1}>")));
                }
                if !e1 {
                    self.expect_end("Class")?;
                }
                let head = Class(self.iri_from_attrs(&a1)?);
                let mut cs = Vec::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            cs.push(self.parse_class_expression(&name, &attrs, empty)?);
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::DisjointUnion(head, cs.into())
            }
            "SubObjectPropertyOf" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let sub = if n1 == "ObjectPropertyChain" {
                    if e1 {
                        return Err(self.err("empty <ObjectPropertyChain>"));
                    }
                    let mut chain = Vec::new();
                    loop {
                        match self.next_non_ws_event()? {
                            Event::End(e) if e == "ObjectPropertyChain" => break,
                            Event::Start { name, attrs, empty } => {
                                chain.push(self.parse_ope(&name, &attrs, empty)?);
                            }
                            other => {
                                return Err(self.err(format!(
                                    "unexpected {other:?} in <ObjectPropertyChain>"
                                )));
                            }
                        }
                    }
                    SubObjectPropertyExpression::Chain(chain)
                } else {
                    SubObjectPropertyExpression::Expression(self.parse_ope(&n1, &a1, e1)?)
                };
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let sup = self.parse_ope(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::SubObjectPropertyOf(sub, sup)
            }
            "EquivalentObjectProperties" => {
                let mut ps = BTreeSet::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            ps.insert(self.parse_ope(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::EquivalentObjectProperties(ps)
            }
            "DisjointObjectProperties" => {
                let mut ps = Vec::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            ps.push(self.parse_ope(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::DisjointObjectProperties(ps.into())
            }
            "InverseObjectProperties" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p1 = self.parse_ope(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let p2 = self.parse_ope(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::InverseObjectProperties(p1, p2)
            }
            "ObjectPropertyDomain" | "ObjectPropertyRange" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_ope(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let c = self.parse_class_expression(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                if tag == "ObjectPropertyDomain" {
                    K::ObjectPropertyDomain(p, c)
                } else {
                    K::ObjectPropertyRange(p, c)
                }
            }
            "FunctionalObjectProperty"
            | "InverseFunctionalObjectProperty"
            | "ReflexiveObjectProperty"
            | "IrreflexiveObjectProperty"
            | "SymmetricObjectProperty"
            | "AsymmetricObjectProperty"
            | "TransitiveObjectProperty" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_ope(&n1, &a1, e1)?;
                self.expect_end(tag)?;
                match tag {
                    "FunctionalObjectProperty" => K::FunctionalObjectProperty(p),
                    "InverseFunctionalObjectProperty" => K::InverseFunctionalObjectProperty(p),
                    "ReflexiveObjectProperty" => K::ReflexiveObjectProperty(p),
                    "IrreflexiveObjectProperty" => K::IrreflexiveObjectProperty(p),
                    "SymmetricObjectProperty" => K::SymmetricObjectProperty(p),
                    "AsymmetricObjectProperty" => K::AsymmetricObjectProperty(p),
                    _ => K::TransitiveObjectProperty(p),
                }
            }
            "SubDataPropertyOf" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p1 = self.parse_data_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let p2 = self.parse_data_property(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::SubDataPropertyOf(p1, p2)
            }
            "EquivalentDataProperties" => {
                let mut ps = BTreeSet::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            ps.insert(self.parse_data_property(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::EquivalentDataProperties(ps)
            }
            "DisjointDataProperties" => {
                let mut ps = Vec::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            ps.push(self.parse_data_property(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::DisjointDataProperties(ps.into())
            }
            "DataPropertyDomain" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_data_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let c = self.parse_class_expression(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::DataPropertyDomain(p, c)
            }
            "DataPropertyRange" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_data_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let r = self.parse_data_range(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::DataPropertyRange(p, r)
            }
            "FunctionalDataProperty" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_data_property(&n1, &a1, e1)?;
                self.expect_end(tag)?;
                K::FunctionalDataProperty(p)
            }
            "DatatypeDefinition" => {
                let (n1, a1, e1) = next_start(self, first)?;
                if n1 != "Datatype" {
                    return Err(self.err(format!("expected <Datatype>, got <{n1}>")));
                }
                if !e1 {
                    self.expect_end("Datatype")?;
                }
                let d = Datatype(self.iri_from_attrs(&a1)?);
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let r = self.parse_data_range(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::DatatypeDefinition(d, r)
            }
            "HasKey" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let c = self.parse_class_expression(&n1, &a1, e1)?;
                let mut ops = BTreeSet::new();
                let mut dps = BTreeSet::new();
                loop {
                    match self.next_non_ws_event()? {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } if name == "DataProperty" => {
                            dps.insert(self.parse_data_property(&name, &attrs, empty)?);
                        }
                        Event::Start { name, attrs, empty } => {
                            ops.insert(self.parse_ope(&name, &attrs, empty)?);
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <HasKey>"))),
                    }
                }
                K::HasKey(c, ops, dps)
            }
            "SameIndividual" => {
                let mut is = BTreeSet::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            is.insert(self.parse_individual(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::SameIndividual(is)
            }
            "DifferentIndividuals" => {
                let mut is = Vec::new();
                let mut cur = first;
                loop {
                    match cur {
                        Event::End(e) if e == tag => break,
                        Event::Start { name, attrs, empty } => {
                            is.push(self.parse_individual(&name, &attrs, empty)?);
                            cur = self.next_non_ws_event()?;
                        }
                        other => return Err(self.err(format!("unexpected {other:?} in <{tag}>"))),
                    }
                }
                K::DifferentIndividuals(is.into())
            }
            "ClassAssertion" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let c = self.parse_class_expression(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let i = self.parse_individual(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::ClassAssertion(c, i)
            }
            "ObjectPropertyAssertion" | "NegativeObjectPropertyAssertion" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_ope(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let a = self.parse_individual(&n2, &a2, e2)?;
                let ev3 = self.next_non_ws_event()?;
                let (n3, a3, e3) = next_start(self, ev3)?;
                let b = self.parse_individual(&n3, &a3, e3)?;
                self.expect_end(tag)?;
                if tag == "ObjectPropertyAssertion" {
                    K::ObjectPropertyAssertion(p, a, b)
                } else {
                    K::NegativeObjectPropertyAssertion(p, a, b)
                }
            }
            "DataPropertyAssertion" | "NegativeDataPropertyAssertion" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_data_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let a = self.parse_individual(&n2, &a2, e2)?;
                let ev3 = self.next_non_ws_event()?;
                let (n3, a3, e3) = next_start(self, ev3)?;
                if n3 != "Literal" {
                    return Err(self.err(format!("expected <Literal>, got <{n3}>")));
                }
                let l = self.parse_literal(&a3, e3)?;
                self.expect_end(tag)?;
                if tag == "DataPropertyAssertion" {
                    K::DataPropertyAssertion(p, a, l)
                } else {
                    K::NegativeDataPropertyAssertion(p, a, l)
                }
            }
            "AnnotationAssertion" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_annotation_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let subj = match n2.as_str() {
                    "IRI" => {
                        let s = if e2 {
                            String::new()
                        } else {
                            self.read_text_until_end("IRI")?
                        };
                        AnnotationSubject::Iri(self.resolve_iri(s.trim())?)
                    }
                    "AbbreviatedIRI" => {
                        let s = if e2 {
                            String::new()
                        } else {
                            self.read_text_until_end("AbbreviatedIRI")?
                        };
                        AnnotationSubject::Iri(self.expand_abbreviated(s.trim())?)
                    }
                    "AnonymousIndividual" => {
                        let id = attr(&a2, "nodeID")
                            .ok_or_else(|| self.err("<AnonymousIndividual> missing nodeID"))?;
                        if !e2 {
                            self.expect_end("AnonymousIndividual")?;
                        }
                        AnnotationSubject::Anonymous(AnonymousIndividual::new(id))
                    }
                    _ => return Err(self.err(format!("unexpected annotation subject <{n2}>"))),
                };
                let ev3 = self.next_non_ws_event()?;
                let (n3, a3, e3) = next_start(self, ev3)?;
                let val = self.parse_annotation_value(&n3, &a3, e3)?;
                self.expect_end(tag)?;
                K::AnnotationAssertion(p, subj, val)
            }
            "SubAnnotationPropertyOf" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p1 = self.parse_annotation_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, a2, e2) = next_start(self, ev2)?;
                let p2 = self.parse_annotation_property(&n2, &a2, e2)?;
                self.expect_end(tag)?;
                K::SubAnnotationPropertyOf(p1, p2)
            }
            "AnnotationPropertyDomain" | "AnnotationPropertyRange" => {
                let (n1, a1, e1) = next_start(self, first)?;
                let p = self.parse_annotation_property(&n1, &a1, e1)?;
                let ev2 = self.next_non_ws_event()?;
                let (n2, _a2, e2) = next_start(self, ev2)?;
                let iri = match n2.as_str() {
                    "IRI" => {
                        let s = if e2 {
                            String::new()
                        } else {
                            self.read_text_until_end("IRI")?
                        };
                        self.resolve_iri(s.trim())?
                    }
                    "AbbreviatedIRI" => {
                        let s = if e2 {
                            String::new()
                        } else {
                            self.read_text_until_end("AbbreviatedIRI")?
                        };
                        self.expand_abbreviated(s.trim())?
                    }
                    _ => {
                        return Err(
                            self.err(format!("expected <IRI> or <AbbreviatedIRI>, got <{n2}>"))
                        );
                    }
                };
                self.expect_end(tag)?;
                if tag == "AnnotationPropertyDomain" {
                    K::AnnotationPropertyDomain(p, iri)
                } else {
                    K::AnnotationPropertyRange(p, iri)
                }
            }
            _ => return Err(self.err(format!("unknown OWL/XML axiom <{tag}>"))),
        };
        Ok(Axiom { kind, annotations })
    }

    fn parse_data_property(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<DataProperty, ParseError> {
        if name != "DataProperty" {
            return Err(self.err(format!("expected <DataProperty>, got <{name}>")));
        }
        if !empty {
            self.expect_end("DataProperty")?;
        }
        Ok(DataProperty(self.iri_from_attrs(attrs)?))
    }

    fn parse_annotation_property(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        empty: bool,
    ) -> Result<AnnotationProperty, ParseError> {
        if name != "AnnotationProperty" {
            return Err(self.err(format!("expected <AnnotationProperty>, got <{name}>")));
        }
        if !empty {
            self.expect_end("AnnotationProperty")?;
        }
        Ok(AnnotationProperty(self.iri_from_attrs(attrs)?))
    }
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn local_xml_name(qname: &str) -> &str {
    qname.rsplit_once(':').map_or(qname, |(_, l)| l)
}
