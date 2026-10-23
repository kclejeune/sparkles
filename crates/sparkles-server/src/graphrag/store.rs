//! Reports as triples in the reports graph (spec C22 §4).
//!
//! A community is `gr:Community` with its level, parent, members and counts, and links to
//! its current report. A report holds its title, summary, rating, status, findings,
//! source commit, the activity that wrote it, and a link to the disclosure footprint of
//! the build that wrote it (§6.2). A finding holds its text, the entities it is about,
//! its citations and the names the heuristic found unmatched. A cited fact is a triple
//! term, plus its reifier when it has one, and a cited child finding is that finding's
//! IRI.
//!
//! The functions here only build quads and the SPARQL that reads them back. The build
//! writes them in commits of kind `graphrag`.

use super::guard::{DISCLOSURE_GRAPH, gr};
use super::report::{Checked, Item, Rendered};
use oxrdf::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
pub const SKOS_BROADER: &str = "http://www.w3.org/2004/02/skos/core#broader";
pub const PROV: &str = "http://www.w3.org/ns/prov#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
/// The principal that writes reports.
pub const WRITER: &str = "urn:x-sparkles:principal:graphrag";
/// The vocabulary of the server's own annotations.
pub const SPK: &str = "urn:x-sparkles:";

/// A report's review status (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Unreviewed,
    Reviewed,
    Rejected,
    Stale,
    Failed,
}

impl Status {
    pub const ALL: [Status; 5] = [
        Status::Unreviewed,
        Status::Reviewed,
        Status::Rejected,
        Status::Stale,
        Status::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unreviewed => "unreviewed",
            Status::Reviewed => "reviewed",
            Status::Rejected => "rejected",
            Status::Stale => "stale",
            Status::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// A community as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunityRecord {
    pub iri: String,
    pub level: u32,
    pub parent: Option<String>,
    /// canonical N-Triples terms of the members
    pub members: Vec<String>,
    /// the facts the principal's view holds about the members
    pub fact_count: u64,
}

/// The activity that wrote a report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    /// `provider/model`
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// RFC 3339
    pub started_at: String,
}

/// A report to store.
#[derive(Clone, Debug, PartialEq)]
pub struct NewReport {
    pub iri: String,
    pub community: String,
    pub status: Status,
    pub source_commit: u64,
    pub footprint: String,
    pub language: String,
    pub activity: Option<Activity>,
    /// the report this one replaces
    pub revision_of: Option<String>,
    pub body: Body,
}

/// What a report says.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// a checked report, with the input whose numbering its findings use
    Written { checked: Checked, input: Rendered },
    /// a report that could not be written, with the reason
    Failed(String),
}

fn nn(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

/// A term from its canonical N-Triples form, when it is an IRI or a literal.
pub fn parse_term(nt: &str) -> Option<Term> {
    if let Some(iri) = nt.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        return NamedNode::new(iri).ok().map(Term::NamedNode);
    }
    let doc = format!("<urn:x> <urn:x> {nt} .");
    oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::NTriples)
        .for_slice(doc.as_bytes())
        .next()
        .and_then(Result::ok)
        .map(|q| q.object)
}

fn subject_of(nt: &str) -> Option<NamedOrBlankNode> {
    match parse_term(nt)? {
        Term::NamedNode(n) => Some(n.into()),
        _ => None,
    }
}

/// The IRI of finding `n` (from 1) of a report.
pub fn finding_iri(report: &str, n: usize) -> String {
    format!("{report}#f{n}")
}

/// The IRI of the activity that wrote a report.
pub fn activity_iri(report: &str) -> String {
    format!("{report}#activity")
}

/// A new report IRI.
pub fn new_report_iri() -> String {
    format!("urn:uuid:{}", uuid::Uuid::new_v4())
}

struct Out<'a> {
    graph: &'a GraphName,
    quads: Vec<Quad>,
}

impl Out<'_> {
    fn add(&mut self, s: &str, p: &str, o: impl Into<Term>) {
        self.quads
            .push(Quad::new(nn(s), nn(p), o.into(), self.graph.clone()));
    }

    fn lit(&mut self, s: &str, p: &str, v: &str) {
        self.add(s, p, Literal::new_simple_literal(v));
    }

    fn text(&mut self, s: &str, p: &str, v: &str, lang: &str) {
        let l = Literal::new_language_tagged_literal(v, lang)
            .unwrap_or_else(|_| Literal::new_simple_literal(v));
        self.add(s, p, l);
    }

    fn int(&mut self, s: &str, p: &str, v: u64) {
        self.add(
            s,
            p,
            Literal::new_typed_literal(v.to_string(), nn(&format!("{XSD}integer"))),
        );
    }
}

/// The quads of a community, without its report link.
pub fn community_quads(c: &CommunityRecord, graph: &str) -> Vec<Quad> {
    let g = GraphName::NamedNode(nn(graph));
    let mut o = Out {
        graph: &g,
        quads: Vec::new(),
    };
    o.add(&c.iri, RDF_TYPE, nn(&gr("Community")));
    o.int(&c.iri, &gr("level"), c.level.into());
    if let Some(p) = &c.parent {
        o.add(&c.iri, SKOS_BROADER, nn(p));
    }
    for m in &c.members {
        if let Some(Term::NamedNode(n)) = parse_term(m) {
            o.add(&c.iri, &gr("member"), n);
        }
    }
    o.int(&c.iri, &gr("memberCount"), c.members.len() as u64);
    o.int(&c.iri, &gr("factCount"), c.fact_count);
    o.quads
}

/// The quads of a report, its findings and its activity, and the community's link to it.
pub fn report_quads(r: &NewReport, graph: &str) -> Vec<Quad> {
    let g = GraphName::NamedNode(nn(graph));
    let mut o = Out {
        graph: &g,
        quads: Vec::new(),
    };
    let id = r.iri.as_str();
    o.add(&r.community, &gr("report"), nn(id));
    o.add(id, RDF_TYPE, nn(&gr("Report")));
    o.lit(id, &gr("status"), r.status.as_str());
    o.int(id, &gr("sourceCommit"), r.source_commit);
    o.add(id, &gr("disclosure"), nn(&r.footprint));
    o.add(id, &gr("community"), nn(&r.community));
    if let Some(old) = &r.revision_of {
        o.add(id, &format!("{PROV}wasRevisionOf"), nn(old));
    }
    if let Some(a) = &r.activity {
        let act = activity_iri(id);
        o.add(id, &format!("{PROV}wasGeneratedBy"), nn(&act));
        o.add(&act, RDF_TYPE, nn(&format!("{PROV}Activity")));
        o.add(&act, &format!("{PROV}wasAssociatedWith"), nn(WRITER));
        o.lit(&act, &format!("{SPK}model"), &a.model);
        o.lit(
            &act,
            &format!("{SPK}promptVersion"),
            super::report::PROMPT_VERSION,
        );
        o.int(&act, &format!("{SPK}inputTokens"), a.input_tokens);
        o.int(&act, &format!("{SPK}outputTokens"), a.output_tokens);
        o.add(
            &act,
            &format!("{PROV}startedAtTime"),
            Literal::new_typed_literal(&a.started_at, nn(&format!("{XSD}dateTime"))),
        );
    }
    match &r.body {
        Body::Failed(reason) => {
            o.lit(id, &gr("failure"), reason);
        }
        Body::Written { checked, input } => {
            o.text(id, RDFS_LABEL, &checked.title, &r.language);
            o.text(id, &gr("summary"), &checked.summary, &r.language);
            o.int(id, &gr("rating"), checked.rating.into());
            let invalid = checked
                .dropped
                .iter()
                .filter(|d| **d == super::report::Dropped::InvalidCitation)
                .count();
            o.int(id, &gr("droppedCitations"), invalid as u64);
            o.int(id, &gr("droppedUnsupported"), checked.unsupported() as u64);
            let mut children: Vec<&str> = Vec::new();
            for (i, f) in checked.findings.iter().enumerate() {
                let fid = finding_iri(id, i + 1);
                o.add(id, &gr("finding"), nn(&fid));
                o.int(&fid, &gr("order"), (i + 1) as u64);
                o.text(&fid, &gr("text"), &f.text, &r.language);
                for e in &f.entities {
                    if let Some(Term::NamedNode(n)) = parse_term(e) {
                        o.add(&fid, &gr("about"), n);
                    }
                }
                for n in &f.cites {
                    let Some(item) = input.items.get(n - 1) else {
                        continue;
                    };
                    match item {
                        Item::Fact(fact) | Item::Edge(fact) => {
                            if let (Some(s), Some(Term::NamedNode(p)), Some(ob)) = (
                                subject_of(&fact.s),
                                parse_term(&fact.p),
                                parse_term(&fact.o),
                            ) {
                                o.add(&fid, &gr("cites"), Triple::new(s, p, ob));
                            }
                            if let Some(rf) = fact.reifier.as_deref().and_then(subject_of)
                                && let NamedOrBlankNode::NamedNode(rf) = rf
                            {
                                o.add(&fid, &gr("cites"), rf);
                            }
                        }
                        Item::ChildFinding { report, index, .. } => {
                            o.add(&fid, &gr("cites"), nn(&finding_iri(report, *index + 1)));
                            children.push(report);
                        }
                    }
                }
                for u in &f.unmatched {
                    o.lit(&fid, &gr("unmatchedName"), u);
                }
            }
            children.sort_unstable();
            children.dedup();
            for c in children {
                o.add(id, &gr("citesReport"), nn(c));
            }
        }
    }
    o.quads
}

/// The SPARQL update that removes every triple whose subject is one of `subjects`, or
/// is a finding or the activity of one of them, from `graph`.
pub fn delete_subjects(graph: &str, subjects: &[String]) -> Option<String> {
    if subjects.is_empty() {
        return None;
    }
    let values: Vec<String> = subjects.iter().map(|s| format!("<{s}>")).collect();
    Some(format!(
        "DELETE {{ GRAPH <{graph}> {{ ?x ?p ?o }} }} WHERE {{ VALUES ?r {{ {} }} GRAPH <{graph}> {{ ?x ?p ?o }} FILTER(?x = ?r || STRSTARTS(STR(?x), CONCAT(STR(?r), \"#\"))) }}",
        values.join(" ")
    ))
}

/// The SPARQL update that removes footprints of `graph` that no report links to.
pub fn delete_unlinked_footprints(graph: &str) -> String {
    format!(
        "DELETE {{ GRAPH <{DISCLOSURE_GRAPH}> {{ ?f ?p ?o }} }} WHERE {{ GRAPH <{DISCLOSURE_GRAPH}> {{ ?f <{covers}> <{graph}> ; ?p ?o }} FILTER NOT EXISTS {{ GRAPH <{graph}> {{ ?r <{disclosure}> ?f }} }} }}",
        covers = gr("covers"),
        disclosure = gr("disclosure"),
    )
}

#[cfg(test)]
mod tests;
