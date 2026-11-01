//! The catalog described in DCAT 3, PROV-O, SPDX, VANN and VoID.
//!
//! The catalog is `urn:x-sparkles:resources`, a `dcat:Catalog`. A resource is
//! `urn:x-sparkles:resource:{id}`, a `dcat:Dataset` whose `dct:type` names its kind, and a
//! version is `urn:x-sparkles:resource:{id}/{version}`, linked with `dcat:hasVersion`,
//! `dct:isVersionOf`, `dcat:hasCurrentVersion` and `dcat:previousVersion`. Imports and
//! graph sources are `prov:wasDerivedFrom` of the pinned version.

use super::{CatalogFile, Resource, Version};
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfSerializer};

const DCAT: &str = "http://www.w3.org/ns/dcat#";
const DCT: &str = "http://purl.org/dc/terms/";
const PROV: &str = "http://www.w3.org/ns/prov#";
const SPDX: &str = "http://spdx.org/rdf/terms#";
const VANN: &str = "http://purl.org/vocab/vann/";
const VOID: &str = "http://rdfs.org/ns/void#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// The catalog's IRI.
pub const CATALOG: &str = "urn:x-sparkles:resources";

pub fn resource_iri(id: &str) -> String {
    format!("urn:x-sparkles:resource:{id}")
}

pub fn version_iri(id: &str, version: &str) -> String {
    format!("urn:x-sparkles:resource:{id}/{version}")
}

fn kind_iri(k: super::Kind) -> String {
    format!("urn:x-sparkles:resource-kind:{}", k.as_str())
}

fn dataset_iri(name: &str) -> String {
    format!("urn:x-sparkles:dataset:{name}")
}

fn n(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

fn t(ns: &str, local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{ns}{local}"))
}

struct Out(Vec<Triple>);

impl Out {
    fn add(&mut self, s: impl Into<NamedOrBlankNode>, p: NamedNode, o: impl Into<Term>) {
        self.0.push(Triple::new(s, p, o));
    }
    fn lit(&mut self, s: impl Into<NamedOrBlankNode>, p: NamedNode, v: &str) {
        self.add(s, p, Literal::new_simple_literal(v));
    }
    fn typed(&mut self, s: impl Into<NamedOrBlankNode>, p: NamedNode, v: &str, dt: &str) {
        self.add(s, p, Literal::new_typed_literal(v, t(XSD, dt)));
    }
    /// An IRI that a check accepted; anything else is written as a literal.
    fn iri_or_lit(&mut self, s: impl Into<NamedOrBlankNode>, p: NamedNode, v: &str) {
        match NamedNode::new(v) {
            Ok(o) => self.add(s, p, o),
            Err(_) => self.lit(s, p, v),
        }
    }
}

fn date_type(v: &str) -> &'static str {
    if v.len() == 10 { "date" } else { "dateTime" }
}

fn licence(out: &mut Out, s: &NamedNode, l: super::license::Licence) {
    if let Some(iri) = l.spdx_iri() {
        out.add(s.clone(), t(DCT, "license"), n(&iri));
        return;
    }
    let Some(e) = l.expression else { return };
    let b = BlankNode::default();
    out.add(s.clone(), t(DCT, "license"), b.clone());
    out.add(b.clone(), t(RDF, "type"), t(DCT, "LicenseDocument"));
    out.lit(b.clone(), t(RDFS, "label"), e);
    if let Some(u) = l.url {
        out.iri_or_lit(b, t(RDFS, "seeAlso"), u);
    }
}

fn version(out: &mut Out, r: &Resource, v: &Version, prev: Option<&Version>) {
    let me = n(&version_iri(&r.id, &v.input.version));
    let res = n(&resource_iri(&r.id));
    out.add(res.clone(), t(DCAT, "hasVersion"), me.clone());
    out.add(me.clone(), t(DCT, "isVersionOf"), res);
    out.add(me.clone(), t(RDF, "type"), t(DCAT, "Dataset"));
    out.lit(me.clone(), t(DCAT, "version"), &v.input.version);
    if let Some(p) = prev {
        out.add(
            me.clone(),
            t(DCAT, "previousVersion"),
            n(&version_iri(&r.id, &p.input.version)),
        );
    }
    if let Some(d) = &v.input.issued {
        out.typed(me.clone(), t(DCT, "issued"), d, date_type(d));
    }
    if let Some(notes) = &v.input.notes {
        out.lit(me.clone(), t(DCT, "description"), notes);
    }
    out.typed(me.clone(), t(DCT, "created"), &v.added.at, "dateTime");
    if v.input.license.is_some() || v.input.license_class.is_some() {
        licence(out, &me, r.licence_of(Some(v)));
    }
    for d in &v.input.distributions {
        let b = BlankNode::default();
        out.add(me.clone(), t(DCAT, "distribution"), b.clone());
        out.add(b.clone(), t(RDF, "type"), t(DCAT, "Distribution"));
        out.iri_or_lit(b.clone(), t(DCAT, "downloadURL"), &d.url);
        if let Some(m) = &d.media_type {
            out.iri_or_lit(
                b.clone(),
                t(DCAT, "mediaType"),
                &format!("https://www.iana.org/assignments/media-types/{m}"),
            );
        }
        if let Some(sz) = d.byte_size {
            out.typed(
                b.clone(),
                t(DCAT, "byteSize"),
                &sz.to_string(),
                "nonNegativeInteger",
            );
        }
        if let Some(h) = &d.sha256 {
            let c = BlankNode::default();
            out.add(b, t(SPDX, "checksum"), c.clone());
            out.add(c.clone(), t(RDF, "type"), t(SPDX, "Checksum"));
            out.add(
                c.clone(),
                t(SPDX, "algorithm"),
                t(SPDX, "checksumAlgorithm_sha256"),
            );
            out.typed(c, t(SPDX, "checksumValue"), h, "hexBinary");
        }
    }
    for p in &v.input.imports {
        out.add(
            me.clone(),
            t(PROV, "wasDerivedFrom"),
            n(&version_iri(&p.resource, &p.version)),
        );
    }
    for g in &v.input.graphs {
        let graph = n(&g.graph);
        out.add(me.clone(), t(DCT, "hasPart"), graph.clone());
        for p in &g.sources {
            out.add(
                graph.clone(),
                t(PROV, "wasDerivedFrom"),
                n(&version_iri(&p.resource, &p.version)),
            );
        }
    }
}

fn resource(out: &mut Out, r: &Resource) {
    let me = n(&resource_iri(&r.id));
    out.add(n(CATALOG), t(DCAT, "dataset"), me.clone());
    out.add(me.clone(), t(RDF, "type"), t(DCAT, "Dataset"));
    out.add(me.clone(), t(DCT, "type"), n(&kind_iri(r.input.kind)));
    out.lit(me.clone(), t(DCT, "identifier"), &r.id);
    out.lit(me.clone(), t(DCT, "title"), &r.input.title);
    if let Some(d) = &r.input.description {
        out.lit(me.clone(), t(DCT, "description"), d);
    }
    if let Some(h) = &r.input.homepage {
        out.iri_or_lit(me.clone(), t(DCAT, "landingPage"), h);
    }
    licence(out, &me, r.licence_of(None));
    if let Some(a) = &r.input.attribution {
        let b = BlankNode::default();
        out.add(me.clone(), t(DCT, "rights"), b.clone());
        out.add(b.clone(), t(RDF, "type"), t(DCT, "RightsStatement"));
        out.lit(b, t(RDFS, "label"), a);
    }
    out.typed(me.clone(), t(DCT, "created"), &r.created.at, "dateTime");
    out.typed(me.clone(), t(DCT, "modified"), &r.modified.at, "dateTime");
    for ns in &r.input.namespaces {
        out.lit(me.clone(), t(VANN, "preferredNamespacePrefix"), &ns.prefix);
        out.typed(
            me.clone(),
            t(VANN, "preferredNamespaceUri"),
            &ns.iri,
            "anyURI",
        );
    }
    for l in &r.input.graphs {
        let b = BlankNode::default();
        out.add(me.clone(), t(DCT, "hasPart"), b.clone());
        out.add(b.clone(), t(RDF, "type"), t(VOID, "Dataset"));
        out.add(b.clone(), t(VOID, "inDataset"), n(&dataset_iri(&l.dataset)));
        out.typed(b, t(DCT, "identifier"), &l.graph, "anyURI");
    }
    if let Some(d) = &r.input.dataset {
        out.add(me.clone(), t(DCT, "source"), n(&dataset_iri(d)));
    }
    let mut prev = None;
    for v in &r.versions {
        version(out, r, v, prev);
        prev = Some(v);
    }
    if let Some(cur) = r.current() {
        out.add(
            me,
            t(DCAT, "hasCurrentVersion"),
            n(&version_iri(&r.id, &cur.input.version)),
        );
    }
}

/// The triples that describe `resources`, inside the catalog.
pub fn triples<'a>(resources: impl IntoIterator<Item = &'a Resource>) -> Vec<Triple> {
    let mut out = Out(Vec::new());
    out.add(n(CATALOG), t(RDF, "type"), t(DCAT, "Catalog"));
    out.lit(n(CATALOG), t(DCT, "title"), "Resource catalog");
    for r in resources {
        resource(&mut out, r);
    }
    out.0
}

/// The description of the whole catalog, or of one resource, in `format`.
pub fn describe(c: &CatalogFile, only: Option<&str>, format: RdfFormat) -> String {
    let triples = triples(
        c.resources
            .iter()
            .filter(|r| only.is_none_or(|id| r.id == id)),
    );
    let prefixes = [
        ("dcat", DCAT),
        ("dcterms", DCT),
        ("prov", PROV),
        ("spdx", SPDX),
        ("vann", VANN),
        ("void", VOID),
        ("rdf", RDF),
        ("rdfs", RDFS),
        ("xsd", XSD),
    ]
    .into_iter()
    .map(|(p, ns)| (p.to_string(), ns.to_string()));
    let ser = sparkles::io::with_prefixes(RdfSerializer::from_format(format), prefixes);
    let mut w = ser.for_writer(Vec::new());
    for t in &triples {
        w.serialize_triple(t)
            .expect("writing to memory does not fail");
    }
    String::from_utf8(w.finish().expect("writing to memory does not fail"))
        .expect("RDF syntaxes are UTF-8")
}
