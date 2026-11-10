//! The JSON forms of the ontology handle's answers, which the HTTP routes, the MCP tool
//! and the Python, Node.js and JVM bindings share, and [`call`], which reads a request
//! in the same form and answers it.
//!
//! A request is a JSON object. `graph` names the graph read, as a string or a list
//! (`"default"` or an IRI), `imports` is a boolean, `mode` is `lenient` or `strict`,
//! `at` names a state as queries do (`commit:N`, a snapshot or a time), and
//! `inferences` reads the materialized inferences with the ontology. The other members
//! depend on the operation and are listed on [`call`]. An axiom is given in its JSON form
//! ([`sparkles_owl::json`]) or as a string in functional-style syntax, with the dataset's
//! prefixes and the standard ones.

use super::browse;
use super::classify::{Classified, ClassifyRequest};
use super::dl_query::{DlAnswers, DlQueryAnswer, DlQueryRequest, DlReasoning};
use super::search;
use super::{
    CheckRequest, EditOutcome, EditRequest, OntologyDiff, OntologyRequest, Opened, ReadView,
    check_opened, parse_axiom_types, parse_profiles, report_view_json,
};
use crate::Dataset;
use crate::error::{Error, Result};
use crate::history::At;
use crate::sparql::QueryOptions;
use serde_json::{Map, Value as J, json};
use sparkles_owl::ofn::Renderer;
use sparkles_owl::profiles::{Profile, Report};
use sparkles_owl::{
    Axiom, AxiomKind, AxiomType, ClassExpression, DataRange, Entity, EntityKind, Individual,
    ObjectPropertyExpression, SubObjectPropertyExpression, read,
};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};
use std::collections::BTreeSet;

/// The most axioms one page lists.
pub const MAX_LIMIT: usize = 10_000;
/// The axioms a page lists by default.
pub const DEFAULT_LIMIT: usize = 100;
/// The diagnostics a summary lists.
pub const SUMMARY_DIAGNOSTICS: usize = 100;
/// The axioms each side of a diff lists by default.
pub const DIFF_LIMIT: usize = 1000;

/// A renderer with the dataset's prefixes, for functional-style output.
pub fn renderer(ds: &Dataset) -> Renderer {
    Renderer::new(ds.prefixes())
}

fn header_json(h: &sparkles_owl::Header, r: &Renderer) -> J {
    json!({
        "iri": h.iri.as_ref().map(|i| i.as_str()),
        "versionIri": h.version_iri.as_ref().map(|i| i.as_str()),
        "imports": h.imports.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
        "annotations": h.annotations.iter().map(|a| r.annotation(a)).collect::<Vec<_>>(),
    })
}

/// The members every answer about an open ontology carries: the dataset, the commit,
/// the mode, the graphs read, the imports, the warnings of the read and, for an inferred
/// view, the inferences read.
pub fn opened_json(name: &str, o: &Opened, req: &OntologyRequest) -> J {
    read_json(name, &o.view(), req)
}

/// [`opened_json`] for the read `o` describes.
pub fn read_json(name: &str, o: &ReadView<'_>, req: &OntologyRequest) -> J {
    let mut j = json!({
        "dataset": name,
        "commit": o.commit,
        "mode": req.mode.as_str(),
        "graphs": o.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "imports": o.imports.iter().map(|i| json!({"iri": i.iri, "graph": i.graph})).collect::<Vec<_>>(),
        "warnings": o.warnings,
    });
    if req.inferences {
        j["inferences"] = match o.inferences {
            None => json!({ "included": false }),
            Some(i) => json!({
                "included": true,
                "graph": o.graph_name(i.graph),
                "profile": i.profile,
                "stale": i.stale,
                "commitsSince": i.commits_since,
                "reason": i.reason,
            }),
        };
    }
    j
}

/// The JSON of the summary of an open ontology, as `GET /$/ontology/{ds}` answers it.
pub fn summary_body(name: &str, o: &Opened, req: &OntologyRequest, r: &Renderer) -> J {
    let s = o.summary();
    let mut j = opened_json(name, o, req);
    j["header"] = header_json(&s.header, r);
    j["axioms"] = json!(
        s.axioms
            .iter()
            .map(|(t, n)| (t.name().to_string(), json!(*n)))
            .collect::<Map<String, J>>()
    );
    j["axiomCount"] = json!(s.axioms.values().sum::<usize>());
    j["entities"] = json!(
        s.entities
            .iter()
            .map(|(k, n)| (k.name().to_string(), json!(*n)))
            .collect::<Map<String, J>>()
    );
    j["triples"] = json!(s.triples);
    j["unmapped"] = json!(s.unmapped);
    j["diagnostics"] = json!({
        "warnings": s.warnings,
        "errors": s.errors,
        "list": o.ontology.diagnostics.iter().take(SUMMARY_DIAGNOSTICS).map(|d| json!({
            "severity": d.severity.as_str(),
            "code": d.code,
            "message": d.message,
        })).collect::<Vec<_>>(),
    });
    j["closure"] = closure_json(&o.closure);
    j["cached"] = json!(o.cached);
    j
}

/// The edges of an imports closure as the summary answers them (spec U12, import
/// resolution): the importing graph, the IRI, the graphs it resolved to, the source that
/// resolved it and whether it closes a cycle.
pub fn closure_json(edges: &[super::ImportEdge]) -> J {
    J::Array(
        edges
            .iter()
            .map(|e| {
                json!({
                    "from": e.from.as_str(),
                    "iri": e.iri,
                    "graphs": e.graphs,
                    "via": e.via.map(|v| v.as_str()),
                    "cycle": e.cycle,
                })
            })
            .collect(),
    )
}

/// What a listing of axioms keeps.
#[derive(Clone, Debug)]
pub struct AxiomQuery {
    pub types: Option<BTreeSet<AxiomType>>,
    pub about: Option<String>,
    pub assertions: bool,
    pub limit: usize,
    pub offset: usize,
}

impl Default for AxiomQuery {
    fn default() -> AxiomQuery {
        AxiomQuery {
            types: None,
            about: None,
            assertions: false,
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

/// One page of axioms, with the graph each was read from, and the total.
pub struct Page {
    pub items: Vec<(Axiom, Option<u32>)>,
    pub total: usize,
    pub offset: usize,
}

/// The page `q` asks for: the terminology's axioms in order, then the assertions the
/// store holds when `q.assertions` is set.
pub fn axiom_page(o: &Opened, q: &AxiomQuery, opts: &QueryOptions) -> Result<Page> {
    let keep = |ax: &Axiom| {
        q.types
            .as_ref()
            .is_none_or(|t| t.contains(&ax.axiom_type()))
            && q.about.as_deref().is_none_or(|a| ax.mentions(a))
    };
    let mut items: Vec<(Axiom, Option<u32>)> = Vec::new();
    let mut total = 0usize;
    for (ax, origins) in &o.ontology.axioms {
        if keep(ax) {
            if total >= q.offset && items.len() < q.limit {
                items.push((ax.clone(), origins.first().map(|x| x.graph)));
            }
            total += 1;
        }
    }
    let wants_assertions = q
        .types
        .as_ref()
        .is_none_or(|t| t.iter().any(|t| t.is_assertion()));
    if q.assertions && wants_assertions {
        o.assertions(opts, |ax, _, g| {
            if let Some(ax) = ax
                && keep(&ax)
            {
                if total >= q.offset && items.len() < q.limit {
                    items.push((ax, Some(g)));
                }
                total += 1;
            }
            Ok(true)
        })?;
    }
    Ok(Page {
        items,
        total,
        offset: q.offset,
    })
}

/// The JSON of one axiom of a listing or a diff: its type, its functional-style form,
/// its JSON form and the graph it is in.
pub fn axiom_json(ax: &Axiom, graph: Option<&str>, r: &Renderer) -> J {
    let mut j = json!({
        "type": ax.axiom_type().name(),
        "ofn": r.axiom(ax),
        "axiom": sparkles_owl::json::axiom(ax),
    });
    if let Some(g) = graph {
        j["graph"] = g.into();
    }
    j
}

/// The JSON of a page of axioms.
pub fn page_body(name: &str, o: &Opened, req: &OntologyRequest, r: &Renderer, page: &Page) -> J {
    let next = page.offset + page.items.len();
    let list: Vec<J> = page
        .items
        .iter()
        .map(|(ax, g)| axiom_json(ax, g.map(|g| o.graph_name(g)), r))
        .collect();
    let mut j = opened_json(name, o, req);
    j["axioms"] = J::Array(list);
    j["total"] = json!(page.total);
    j["offset"] = json!(page.offset);
    j["next"] = if next < page.total {
        json!(next)
    } else {
        J::Null
    };
    j
}

/// The JSON of the reports of a check.
pub fn check_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &Renderer,
    reports: &[Report],
    assertions: bool,
) -> J {
    reports_body(name, &o.view(), req, r, reports, assertions)
}

/// [`check_body`] for the read `o` describes.
pub fn reports_body(
    name: &str,
    o: &ReadView<'_>,
    req: &OntologyRequest,
    r: &Renderer,
    reports: &[Report],
    assertions: bool,
) -> J {
    let mut j = read_json(name, o, req);
    j["conforms"] = json!(reports.iter().all(|r| r.conforms));
    j["assertions"] = json!(assertions);
    j["reports"] = J::Array(reports.iter().map(|x| report_view_json(o, x, r)).collect());
    j
}

/// The JSON of what an edit did. `addedAxioms` and `removedAxioms` list the axioms the
/// edit wrote and removed, at most [`MAX_LIMIT`] of each, so that a client can undo the
/// edit exactly; `axiomsTruncated` says whether a list was cut.
pub fn edit_body(e: &EditOutcome, r: &Renderer) -> J {
    let list = |l: &[Axiom]| -> Vec<J> {
        l.iter()
            .take(MAX_LIMIT)
            .map(|a| axiom_json(a, None, r))
            .collect()
    };
    json!({
        "added": e.added,
        "present": e.present,
        "removed": e.removed,
        "absent": e.absent.iter().map(|a| r.axiom(a)).collect::<Vec<_>>(),
        "declared": e.declared.iter().map(|d| r.entity(d)).collect::<Vec<_>>(),
        "addedAxioms": list(&e.added_axioms),
        "removedAxioms": list(&e.removed_axioms),
        "axiomsTruncated": e.added_axioms.len() > MAX_LIMIT || e.removed_axioms.len() > MAX_LIMIT,
        "inserted": e.inserted,
        "deleted": e.deleted,
        "kept": e.kept,
        "committed": e.receipt.committed,
    })
}

/// Whether the ontology's terminology declares or uses `iri`.
pub fn mentioned(o: &Opened, iri: &str) -> bool {
    o.ontology.axiom_set().any(|a| a.mentions(iri))
}

/// The JSON of an ontology diff, with at most `limit` axioms on each side.
pub fn diff_body(
    name: &str,
    d: &OntologyDiff,
    req: &OntologyRequest,
    assertions: bool,
    limit: usize,
    r: &Renderer,
) -> J {
    let side = |v: &[super::AxiomChange]| -> J {
        J::Array(
            v.iter()
                .take(limit)
                .map(|c| axiom_json(&c.axiom, Some(&c.graph), r))
                .collect(),
        )
    };
    json!({
        "dataset": name,
        "from": d.from,
        "to": d.to,
        "graphs": req.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "mode": req.mode.as_str(),
        "assertions": assertions,
        "added": side(&d.added),
        "removed": side(&d.removed),
        "addedCount": d.added.len(),
        "removedCount": d.removed.len(),
        "truncated": d.added.len() > limit || d.removed.len() > limit,
        "changedQuads": d.changed_quads,
        "method": d.method.as_str(),
    })
}

/// The diff as lines: `+ axiom` for each added axiom, then `- axiom` for each removed
/// one, in functional-style syntax with the dataset's prefixes.
pub fn diff_lines(d: &OntologyDiff, limit: usize, r: &Renderer) -> String {
    let mut s = String::new();
    for c in d.added.iter().take(limit) {
        s.push_str("+ ");
        s.push_str(&r.axiom(&c.axiom));
        s.push('\n');
    }
    for c in d.removed.iter().take(limit) {
        s.push_str("- ");
        s.push_str(&r.axiom(&c.axiom));
        s.push('\n');
    }
    s
}

// ------------------------------------------------------------- entities ------

/// The entities of the kinds `kinds` (every kind when `None`), one page at a time.
pub fn entities_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    kinds: Option<&BTreeSet<EntityKind>>,
    limit: usize,
    offset: usize,
) -> J {
    let mut all: Vec<Entity> = Vec::new();
    for k in EntityKind::ALL {
        if kinds.is_none_or(|ks| ks.contains(&k)) {
            all.extend(o.ontology.entities(k));
        }
    }
    all.sort_by(|a, b| (a.iri(), a.kind()).cmp(&(b.iri(), b.kind())));
    let total = all.len();
    let list: Vec<J> = all
        .iter()
        .skip(offset)
        .take(limit)
        .map(|e| json!({ "kind": e.kind().name(), "iri": e.iri().as_str() }))
        .collect();
    let next = offset + list.len();
    let mut j = opened_json(name, o, req);
    j["entities"] = J::Array(list);
    j["total"] = json!(total);
    j["offset"] = json!(offset);
    j["next"] = if next < total { json!(next) } else { J::Null };
    j
}

/// Parse a comma-separated list of entity kinds, such as `Class,ObjectProperty`.
pub fn parse_entity_kinds(s: &str) -> Result<BTreeSet<EntityKind>> {
    s.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| {
            EntityKind::parse(k).ok_or_else(|| {
                bad(format!(
                    "unknown entity kind '{k}': {}",
                    EntityKind::ALL.map(|k| k.name()).join(", ")
                ))
            })
        })
        .collect()
}

/// What an entity answer reads besides the terminology.
#[derive(Clone, Debug)]
pub struct EntityQuery {
    pub iri: String,
    /// the one kind answered (every kind the IRI has when `None`)
    pub kind: Option<EntityKind>,
    /// also stream the assertions the store holds: the instances of a class, and the
    /// types and property assertions of an individual
    pub assertions: bool,
    /// the most entries each streamed list holds
    pub limit: usize,
}

fn ce_json(c: &ClassExpression, r: &Renderer) -> J {
    json!({ "ofn": r.class_expression(c), "expression": sparkles_owl::json::class_expression(c) })
}

fn ope_json(p: &ObjectPropertyExpression, r: &Renderer) -> J {
    json!({ "ofn": r.ope(p), "expression": sparkles_owl::json::ope(p) })
}

fn dr_json(d: &DataRange, r: &Renderer) -> J {
    json!({ "ofn": r.data_range(d), "expression": sparkles_owl::json::data_range(d) })
}

fn individual_json(i: &Individual, r: &Renderer) -> J {
    json!({ "ofn": r.individual(i), "expression": sparkles_owl::json::individual(i) })
}

fn sub_ope_json(p: &SubObjectPropertyExpression, r: &Renderer) -> J {
    match p {
        SubObjectPropertyExpression::Expression(p) => ope_json(p, r),
        SubObjectPropertyExpression::Chain(ps) => json!({
            "ofn": format!(
                "ObjectPropertyChain({})",
                ps.iter().map(|p| r.ope(p)).collect::<Vec<_>>().join(" ")
            ),
            "expression": { "chain": ps.iter().map(sparkles_owl::json::ope).collect::<Vec<_>>() },
        }),
    }
}

/// The JSON of what the ontology says about one entity (spec F21 §2.1): its kinds,
/// labels and annotations, and for each kind the accessors of that kind. A kind the
/// IRI does not have, or an IRI with no kind, is `NotFound`.
pub fn entity_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &Renderer,
    q: &EntityQuery,
    opts: &QueryOptions,
) -> Result<J> {
    let onto = &o.ontology;
    let iri = q.iri.as_str();
    let kinds: Vec<EntityKind> = match q.kind {
        Some(k) => {
            o.entity(k, iri, opts)?;
            vec![k]
        }
        None => o.kinds(iri, opts)?,
    };
    if kinds.is_empty() {
        return Err(Error::NotFound(format!(
            "the ontology neither declares nor uses <{iri}> as an entity"
        )));
    }
    let ces = |v: Vec<ClassExpression>| -> J { v.iter().map(|c| ce_json(c, r)).collect() };
    let opes =
        |v: Vec<ObjectPropertyExpression>| -> J { v.iter().map(|p| ope_json(p, r)).collect() };
    let iris = |v: Vec<sparkles_owl::Iri>| -> J { v.iter().map(|i| J::from(i.as_str())).collect() };
    let mut truncated = false;
    let mut j = opened_json(name, o, req);
    j["iri"] = json!(iri);
    j["kinds"] = json!(kinds.iter().map(|k| k.name()).collect::<Vec<_>>());
    j["labels"] = J::Array(
        onto.labels(iri)
            .iter()
            .map(sparkles_owl::json::literal)
            .collect(),
    );
    j["annotations"] = J::Array(
        onto.annotations(iri)
            .iter()
            .map(|a| json!({ "ofn": r.annotation(a), "annotation": sparkles_owl::json::annotation(a) }))
            .collect(),
    );
    for k in &kinds {
        match k {
            EntityKind::Class => {
                let c = sparkles_owl::Class::new(iri);
                let mut instances: Vec<J> = Vec::new();
                let mut seen: BTreeSet<Individual> = BTreeSet::new();
                let mut each = |i: Individual| {
                    if seen.insert(i.clone()) {
                        if instances.len() >= q.limit {
                            truncated = true;
                            return false;
                        }
                        instances.push(individual_json(&i, r));
                    }
                    true
                };
                if q.assertions {
                    o.instances(&c, opts, |i, _| Ok(each(i)))?;
                } else {
                    for i in onto.instances(&c) {
                        if !each(i) {
                            break;
                        }
                    }
                }
                j["class"] = json!({
                    "superClasses": ces(onto.super_classes(&c)),
                    "subClasses": ces(onto.sub_classes(&c)),
                    "equivalentClasses": ces(onto.equivalent_classes(&c)),
                    "disjointClasses": ces(onto.disjoint_classes(&c)),
                    "disjointUnions": onto.disjoint_unions(&c).into_iter().map(|cs| ces(cs.into_iter().collect())).collect::<Vec<_>>(),
                    "keys": onto.keys(&c).into_iter().map(|(ps, ds)| json!({
                        "objectProperties": opes(ps.into_iter().collect()),
                        "dataProperties": ds.iter().map(|d| d.as_str()).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "instances": instances,
                });
            }
            EntityKind::ObjectProperty => {
                let p = sparkles_owl::ObjectProperty::new(iri);
                j["objectProperty"] = json!({
                    "domains": ces(onto.object_property_domains(&p)),
                    "ranges": ces(onto.object_property_ranges(&p)),
                    "superProperties": opes(onto.super_object_properties(&p)),
                    "subProperties": onto.sub_object_properties(&p).iter().map(|x| sub_ope_json(x, r)).collect::<Vec<_>>(),
                    "equivalentProperties": opes(onto.equivalent_object_properties(&p)),
                    "disjointProperties": opes(onto.disjoint_object_properties(&p)),
                    "inverseProperties": opes(onto.inverse_object_properties(&p)),
                    "characteristics": onto.object_property_characteristics(&p).iter().map(|t| t.name()).collect::<Vec<_>>(),
                });
            }
            EntityKind::DataProperty => {
                let p = sparkles_owl::DataProperty::new(iri);
                let dps = |v: Vec<sparkles_owl::DataProperty>| -> J {
                    v.iter().map(|d| J::from(d.as_str())).collect()
                };
                j["dataProperty"] = json!({
                    "domains": ces(onto.data_property_domains(&p)),
                    "ranges": onto.data_property_ranges(&p).iter().map(|d| dr_json(d, r)).collect::<Vec<_>>(),
                    "superProperties": dps(onto.super_data_properties(&p)),
                    "subProperties": dps(onto.sub_data_properties(&p)),
                    "equivalentProperties": dps(onto.equivalent_data_properties(&p)),
                    "disjointProperties": dps(onto.disjoint_data_properties(&p)),
                    "functional": onto.is_functional_data_property(&p),
                });
            }
            EntityKind::AnnotationProperty => {
                let p = sparkles_owl::AnnotationProperty::new(iri);
                let aps = |v: Vec<sparkles_owl::AnnotationProperty>| -> J {
                    v.iter().map(|d| J::from(d.as_str())).collect()
                };
                j["annotationProperty"] = json!({
                    "superProperties": aps(onto.super_annotation_properties(&p)),
                    "subProperties": aps(onto.sub_annotation_properties(&p)),
                    "domains": iris(onto.annotation_property_domains(&p)),
                    "ranges": iris(onto.annotation_property_ranges(&p)),
                });
            }
            EntityKind::Datatype => {
                let d = sparkles_owl::Datatype::new(iri);
                j["datatype"] = json!({
                    "definitions": onto.datatype_definitions(&d).iter().map(|x| dr_json(x, r)).collect::<Vec<_>>(),
                });
            }
            EntityKind::NamedIndividual => {
                let i = Individual::named(iri);
                let mut types: Vec<J> = Vec::new();
                let mut assertions: Vec<J> = Vec::new();
                if q.assertions {
                    let mut seen: BTreeSet<ClassExpression> = BTreeSet::new();
                    o.types_of(&i, opts, |c, _| {
                        if seen.insert(c.clone()) {
                            if types.len() >= q.limit {
                                truncated = true;
                                return Ok(false);
                            }
                            types.push(ce_json(&c, r));
                        }
                        Ok(true)
                    })?;
                    o.property_assertions(&i, opts, |ax, g| {
                        if assertions.len() >= q.limit {
                            truncated = true;
                            return Ok(false);
                        }
                        assertions.push(axiom_json(&ax, Some(o.graph_name(g)), r));
                        Ok(true)
                    })?;
                } else {
                    for c in onto.types(&i) {
                        if types.len() >= q.limit {
                            truncated = true;
                            break;
                        }
                        types.push(ce_json(&c, r));
                    }
                    for ax in onto.property_assertions(&i) {
                        if assertions.len() >= q.limit {
                            truncated = true;
                            break;
                        }
                        let g = onto
                            .axioms
                            .get(&ax)
                            .and_then(|v| v.first())
                            .map(|x| x.graph);
                        assertions.push(axiom_json(&ax, g.map(|g| o.graph_name(g)), r));
                    }
                }
                let (same, different) = if q.assertions {
                    let mut same: BTreeSet<Individual> = BTreeSet::new();
                    let mut different: BTreeSet<Individual> = BTreeSet::new();
                    // each list fills to the limit on its own, and the stream stops
                    // once both are cut
                    let (mut same_cut, mut different_cut) = (false, false);
                    o.equalities(&i, opts, |ax, _| {
                        let (into, cut) = match &ax.kind {
                            AxiomKind::SameIndividual(_) => (&mut same, &mut same_cut),
                            AxiomKind::DifferentIndividuals(_) => {
                                (&mut different, &mut different_cut)
                            }
                            _ => return Ok(true),
                        };
                        let (AxiomKind::SameIndividual(set) | AxiomKind::DifferentIndividuals(set)) =
                            &ax.kind
                        else {
                            return Ok(true);
                        };
                        for x in set.iter().filter(|x| **x != i) {
                            if into.contains(x) {
                                continue;
                            }
                            if into.len() >= q.limit {
                                *cut = true;
                                truncated = true;
                                break;
                            }
                            into.insert(x.clone());
                        }
                        Ok(!(same_cut && different_cut))
                    })?;
                    (same.into_iter().collect(), different.into_iter().collect())
                } else {
                    (onto.same_individuals(&i), onto.different_individuals(&i))
                };
                j["namedIndividual"] = json!({
                    "types": types,
                    "sameIndividuals": same.iter().map(|x| individual_json(x, r)).collect::<Vec<_>>(),
                    "differentIndividuals": different.iter().map(|x| individual_json(x, r)).collect::<Vec<_>>(),
                    "assertions": assertions,
                });
            }
        }
    }
    j["assertions"] = json!(q.assertions);
    j["truncated"] = json!(truncated);
    Ok(j)
}

// ------------------------------------------------------------- requests ------

/// The operations [`call`] answers, in the names of the bindings' surface keys.
pub const OPERATIONS: [&str; 16] = [
    "summary",
    "axioms",
    "entities",
    "entity",
    "check",
    "edit",
    "removeEntity",
    "diff",
    "classify",
    "search",
    "parse",
    "dlQuery",
    "hierarchy",
    "individuals",
    "keptClassification",
    "term",
];

/// The members each operation reads besides those that name the ontology.
fn members(op: &str) -> &'static [&'static str] {
    match op {
        "summary" => &["metrics"],
        "axioms" => &["type", "about", "assertions", "limit", "offset"],
        "entities" => &["kind", "limit", "offset"],
        "entity" => &["iri", "kind", "assertions", "limit"],
        "check" => &["profiles", "assertions", "maxViolations"],
        "edit" => &["add", "remove", "declare", "target", "message"],
        "removeEntity" => &["iri", "message"],
        "diff" => &["from", "to", "assertions", "limit"],
        "search" => &[
            "q",
            "kind",
            "lang",
            "limit",
            "offset",
            "fuzzy",
            "definitions",
        ],
        "classify" => &[
            "assertions",
            "target",
            "message",
            "about",
            "nodes",
            "limit",
            "offset",
            "threads",
            "partial",
            "expansion",
            "projection",
            "lang",
        ],
        "parse" => &["text", "as"],
        "hierarchy" => &[
            "sourceId",
            "after",
            "kind",
            "iri",
            "ancestors",
            "limit",
            "offset",
            "scheme",
            "deprecated",
            "lang",
        ],
        "individuals" => &[
            "type", "untyped", "iri", "after", "limit", "lang", "sourceId",
        ],
        "keptClassification" => &["part", "about", "offset", "limit", "resultId", "lang"],
        "term" => &["iri", "mentions", "incoming", "lang", "sourceId"],
        "dlQuery" => &[
            "expression",
            "syntax",
            "reasoning",
            "answers",
            "limit",
            "threads",
            "maxDerived",
        ],
        _ => &[],
    }
}

const ONTOLOGY_MEMBERS: [&str; 5] = ["graph", "imports", "mode", "at", "inferences"];

fn bad(m: impl Into<String>) -> Error {
    Error::invalid(m.into())
}

fn graph_ref(s: &str) -> Result<GraphRef> {
    GraphRef::parse(s.trim_start_matches('<').trim_end_matches('>')).map_err(bad)
}

fn boolean(a: &Map<String, J>, k: &str) -> Result<Option<bool>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(J::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(format!("{k}: true or false"))),
    }
}

fn string<'a>(a: &'a Map<String, J>, k: &str) -> Result<Option<&'a str>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(J::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("{k}: a string"))),
    }
}

/// The expansion strategy that `expansion` names: `cautious`, the default, or `eager`.
pub fn expansion_of(s: Option<&str>) -> Result<sparkles_owl::classify::Expansion> {
    use sparkles_owl::classify::Expansion;
    match s {
        None | Some("cautious") => Ok(Expansion::Cautious),
        Some("eager") => Ok(Expansion::Eager),
        Some(x) => Err(bad(format!("expansion: cautious or eager, not '{x}'"))),
    }
}

fn number(a: &Map<String, J>, k: &str, min: usize, max: usize) -> Result<Option<usize>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(v) => {
            // the bindings send 64-bit integers as strings or numbers
            let n = match v {
                J::Number(n) => n.as_u64(),
                J::String(s) => s.parse::<u64>().ok(),
                _ => None,
            };
            n.filter(|n| (min as u64..=max as u64).contains(n))
                .map(|n| n as usize)
                .map(Some)
                .ok_or_else(|| bad(format!("{k}: an integer from {min} to {max}")))
        }
    }
}

fn at_of(v: &J, k: &str) -> Result<At> {
    match v {
        J::String(s) => s.parse::<At>().map_err(|e| bad(format!("{k}: {e}"))),
        J::Number(n) => n
            .as_u64()
            .map(At::Commit)
            .ok_or_else(|| bad(format!("{k}: a commit number or a selector"))),
        _ => Err(bad(format!("{k}: a commit number or a selector"))),
    }
}

/// The ontology the members of `a` name: `graph`, `imports`, `mode`, `at` and
/// `inferences`.
pub fn request_of(a: &Map<String, J>) -> Result<OntologyRequest> {
    let mut req = OntologyRequest::default();
    match a.get("graph") {
        None | Some(J::Null) => {}
        Some(J::String(g)) => req.graphs = vec![graph_ref(g)?],
        Some(J::Array(v)) => {
            req.graphs = v
                .iter()
                .map(|g| {
                    g.as_str()
                        .ok_or_else(|| bad("graph: a string or a list of strings"))
                        .and_then(graph_ref)
                })
                .collect::<Result<_>>()?;
        }
        Some(_) => return Err(bad("graph: a string or a list of strings")),
    }
    if let Some(b) = boolean(a, "imports")? {
        req.imports = if b {
            ImportMode::Dataset
        } else {
            ImportMode::None
        };
    }
    if let Some(m) = string(a, "mode")? {
        req.mode = read::Mode::parse(m)
            .ok_or_else(|| bad(format!("mode must be lenient or strict, not '{m}'")))?;
    }
    if let Some(v) = a.get("at").filter(|v| !v.is_null()) {
        req.at = Some(at_of(v, "at")?);
    }
    req.inferences = boolean(a, "inferences")?.unwrap_or(false);
    Ok(req)
}

/// An axiom in its JSON form or in functional-style syntax with `prefixes`.
pub fn axiom_of(v: &J, at: &str, prefixes: &[(String, String)]) -> Result<Axiom> {
    match v {
        J::String(s) => {
            sparkles_owl::ofn_read::parse_axiom(s, prefixes).map_err(|e| bad(format!("{at}: {e}")))
        }
        J::Object(_) => sparkles_owl::json::read_axiom(v).map_err(|e| bad(format!("{at}: {e}"))),
        _ => Err(bad(format!(
            "{at}: an axiom in functional-style syntax or in its JSON form"
        ))),
    }
}

fn axioms_of(a: &Map<String, J>, k: &str, prefixes: &[(String, String)]) -> Result<Vec<Axiom>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(Vec::new()),
        Some(J::Array(v)) => v
            .iter()
            .enumerate()
            .map(|(i, x)| axiom_of(x, &format!("{k}[{i}]"), prefixes))
            .collect(),
        Some(_) => Err(bad(format!("{k}: a list of axioms"))),
    }
}

fn profiles_of(a: &Map<String, J>) -> Result<Vec<Profile>> {
    let ps = match a.get("profiles") {
        None | Some(J::Null) => Vec::new(),
        Some(J::String(s)) => parse_profiles(s)?,
        Some(J::Array(v)) => {
            let mut ps = Vec::new();
            for x in v {
                let s = x
                    .as_str()
                    .ok_or_else(|| bad("profiles: a list of el, ql, rl and dl"))?;
                ps.extend(parse_profiles(s)?);
            }
            ps
        }
        Some(_) => return Err(bad("profiles: a list of el, ql, rl and dl")),
    };
    Ok(if ps.is_empty() { vec![Profile::Dl] } else { ps })
}

/// Answer the ontology operation `op` with the arguments `args`, a JSON object, in the
/// JSON forms of the HTTP routes. The operations and their members:
///
/// - `summary`: the summary of `GET /$/ontology/{ds}`, with optional `metrics`.
/// - `axioms`: a page of `GET /$/ontology/{ds}/axioms`, with `type` (a list such as
///   `SubClassOf,ClassAssertion`), `about` (an IRI), `assertions`, `limit` and
///   `offset`. `about` an IRI the ontology neither declares nor uses is `NotFound`.
/// - `entities`: a page of `GET /$/ontology/{ds}/entities`, with `kind` (a list such as
///   `Class,ObjectProperty`), `limit` and `offset`.
/// - `entity`: what `GET /$/ontology/{ds}/entity` says about the entity `iri`, with
///   `kind`, `assertions` and `limit`.
/// - `check`: the reports of `POST /$/ontology/{ds}/check`, with `profiles`,
///   `assertions` and `maxViolations`.
/// - `edit`: what `POST /$/ontology/{ds}/axioms` did, with `add`, `remove`, `declare`,
///   `target` and `message`. `opts.write` carries the commit's other options.
/// - `removeEntity`: an edit that removes the entity `iri`.
/// - `diff`: the diff of `GET /$/ontology/{ds}/diff`, with `from`, `to` (the head by
///   default), `assertions` and `limit`.
/// - `search`: the entities of `GET /$/ontology/{ds}/search` that match `q`, with `kind`
///   (a list such as `class,property`), `lang` (the caller's languages, most preferred
///   first), `limit`, `offset`, `fuzzy` and `definitions`.
/// - `classify`: the OWL 2 EL classification of `POST /$/ontology/{ds}/classify`, with
///   `assertions`, `target` and `message` (write the taxonomy to that graph), `about` (a
///   class to describe), `nodes` (list the taxonomy's nodes), `limit`, `offset`,
///   `threads`, `partial` (answer what a run stopped part way proved) and `expansion`
///   (`cautious` or `eager`).
/// - `parse`: Manchester `text` read against the ontology, as
///   `POST /$/ontology/{ds}/parse` answers it, with `as` (`classExpression`, the
///   default, `dataRange`, `individual`, `literal` or `axiom`).
/// - `dlQuery`: the DL query of `POST /$/ontology/{ds}/dl-query`, with `expression`
///   (Manchester text, functional-style text with `syntax: "functional"`, or the JSON
///   form), `reasoning` (`auto`, `el` or `rl`), `answers` (a list of
///   [`DlAnswers::NAMES`]), `limit`, `threads` and `maxDerived`.
pub fn call(ds: &Dataset, op: &str, args: &J, opts: &QueryOptions) -> Result<J> {
    let empty = Map::new();
    let a = match args {
        J::Null => &empty,
        J::Object(o) => o,
        _ => return Err(bad("the arguments are a JSON object")),
    };
    if !OPERATIONS.contains(&op) {
        return Err(bad(format!(
            "unknown ontology operation '{op}': {}",
            OPERATIONS.join(", ")
        )));
    }
    let allowed = members(op);
    if let Some(k) = a
        .keys()
        .find(|k| !ONTOLOGY_MEMBERS.contains(&k.as_str()) && !allowed.contains(&k.as_str()))
    {
        return Err(bad(format!("unknown member '{k}' of ontology.{op}")));
    }
    if op == "keptClassification" && a.contains_key("inferences") {
        return Err(bad(
            "keptClassification does not take an inferences selector",
        ));
    }
    let req = request_of(a)?;
    let name = ds.name().unwrap_or("");
    let handle = ds.ontology();
    if op == "keptClassification" {
        let query = kept_query_of(a)?;
        let answer = handle.kept_classification(&req, &query, opts)?;
        return kept_body(name, &answer);
    }
    let r = renderer(ds);
    let prefixes: Vec<(String, String)> = ds.prefixes().into_iter().collect();
    match op {
        "summary" => {
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let mut j = summary_body(name, &o, &req, &r);
            if boolean(a, "metrics")?.unwrap_or(false) {
                j["metrics"] = super::metrics::body(name, &o, &req, &r, &opts)?;
            }
            Ok(j)
        }
        "axioms" => {
            let q = AxiomQuery {
                types: string(a, "type")?.map(parse_axiom_types).transpose()?,
                about: string(a, "about")?
                    .map(|s| s.trim_start_matches('<').trim_end_matches('>').to_string()),
                assertions: boolean(a, "assertions")?.unwrap_or(false),
                limit: number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT),
                offset: number(a, "offset", 0, usize::MAX)?.unwrap_or(0),
            };
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let page = axiom_page(&o, &q, &opts)?;
            if let Some(about) = &q.about
                && page.total == 0
                && !mentioned(&o, about)
            {
                return Err(Error::NotFound(format!(
                    "the ontology neither declares nor uses <{about}>"
                )));
            }
            Ok(page_body(name, &o, &req, &r, &page))
        }
        "entities" => {
            let kinds = string(a, "kind")?.map(parse_entity_kinds).transpose()?;
            let limit = number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT);
            let offset = number(a, "offset", 0, usize::MAX)?.unwrap_or(0);
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            Ok(entities_body(name, &o, &req, kinds.as_ref(), limit, offset))
        }
        "entity" => {
            let iri = string(a, "iri")?
                .ok_or_else(|| bad("iri names the entity"))?
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            let kind = match string(a, "kind")? {
                None => None,
                Some(k) => Some(EntityKind::parse(k).ok_or_else(|| {
                    bad(format!(
                        "unknown entity kind '{k}': {}",
                        EntityKind::ALL.map(|k| k.name()).join(", ")
                    ))
                })?),
            };
            let q = EntityQuery {
                iri,
                kind,
                assertions: boolean(a, "assertions")?.unwrap_or(false),
                limit: number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT),
            };
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            entity_body(name, &o, &req, &r, &q, &opts)
        }
        "check" => {
            let check = CheckRequest {
                profiles: profiles_of(a)?,
                assertions: boolean(a, "assertions")?.unwrap_or(true),
                max_violations: number(a, "maxViolations", 1, MAX_LIMIT)?
                    .unwrap_or(CheckRequest::default().max_violations),
            };
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let reports = check_opened(&o, &check, &opts)?;
            Ok(check_body(name, &o, &req, &r, &reports, check.assertions))
        }
        "edit" => {
            let edit = EditRequest {
                add: axioms_of(a, "add", &prefixes)?,
                remove: axioms_of(a, "remove", &prefixes)?,
                declare: boolean(a, "declare")?,
                graph: string(a, "target")?.map(graph_ref).transpose()?,
                message: string(a, "message")?.map(str::to_string),
            };
            if edit.add.is_empty() && edit.remove.is_empty() {
                return Err(bad("name axioms to add or remove"));
            }
            let e = handle.edit(&req, &edit, opts)?;
            Ok(edit_body(&e, &r))
        }
        "removeEntity" => {
            let iri = string(a, "iri")?
                .ok_or_else(|| bad("iri names the entity to remove"))?
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            let mut opts = opts.clone();
            if let Some(m) = string(a, "message")? {
                opts.write.message = Some(std::sync::Arc::from(m));
            }
            let e = handle.remove_entity(&req, &iri, &opts)?;
            Ok(edit_body(&e, &r))
        }
        "diff" => {
            if req.at.is_some() {
                return Err(bad("a diff takes from and to, not at"));
            }
            let from = at_of(
                a.get("from")
                    .filter(|v| !v.is_null())
                    .ok_or_else(|| bad("from names the earlier commit"))?,
                "from",
            )?;
            let to = match a.get("to").filter(|v| !v.is_null()) {
                None => At::Head,
                Some(v) => at_of(v, "to")?,
            };
            let assertions = boolean(a, "assertions")?.unwrap_or(false);
            let limit = number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DIFF_LIMIT);
            let d = handle.diff(&req, &from, &to, assertions, opts)?;
            Ok(diff_body(name, &d, &req, assertions, limit, &r))
        }
        "classify" => {
            let about = string(a, "about")?
                .map(|s| s.trim_start_matches('<').trim_end_matches('>').to_string());
            let c = ClassifyRequest {
                assertions: boolean(a, "assertions")?.unwrap_or(false),
                threads: number(a, "threads", 0, 1024)?.unwrap_or(0),
                target: string(a, "target")?.map(graph_ref).transpose()?,
                message: string(a, "message")?.map(str::to_string),
                about: about.clone(),
                el_only: false,
                partial: boolean(a, "partial")?.unwrap_or(false),
                expansion: expansion_of(string(a, "expansion")?)?,
            };
            let q = ClassifyQuery {
                about,
                nodes: boolean(a, "nodes")?.unwrap_or(false),
                limit: number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DEFAULT_LIMIT),
                offset: number(a, "offset", 0, usize::MAX)?.unwrap_or(0),
                projection: projection_of(string(a, "projection")?)?,
                languages: languages_of(a)?,
            };
            q.validate_projection(
                c.target.is_some(),
                false,
                a.get("lang").is_some_and(|v| !v.is_null()),
            )?;
            let out = handle.classify(&req, &c, opts)?;
            classify_body(name, &out, &req, &r, &q)
        }
        "parse" => {
            let text =
                string(a, "text")?.ok_or_else(|| bad("text: the Manchester text to read"))?;
            let what = what_of(string(a, "as")?)?;
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let parsed = handle.parse_manchester(&o, text, what, &opts)?;
            Ok(parse_body(
                name,
                &o,
                &req,
                &r,
                &handle.vocabulary(&o),
                what,
                &parsed,
            ))
        }
        "dlQuery" => {
            let (q, limit) = dl_query_of(a)?;
            let opts = handle.anchored(opts)?;
            let o = handle.open(&req, &opts)?;
            let q = dl_expression(&handle, &o, a, q, &prefixes, &opts)?;
            let out = handle.dl_query_opened(o, &req, &q, &opts)?;
            Ok(dl_query_body(
                name,
                &out,
                &req,
                &r,
                &handle.vocabulary(&out.opened),
                limit,
            ))
        }
        "search" => {
            let s = search_request_of(a)?;
            let found = handle.search(&req, &s, opts)?;
            Ok(search_body(name, &s, &found))
        }
        "hierarchy" => {
            let h = hierarchy_request_of(a)?;
            let found = handle.hierarchy(&req, &h, opts)?;
            Ok(hierarchy_body(name, &h, &found))
        }
        "individuals" => {
            let i = individuals_request_of(a)?;
            let found = handle.individuals(&req, &i, opts)?;
            Ok(individuals_body(name, &found))
        }
        "term" => {
            let t = term_request_of(a)?;
            let page = handle.term(&req, &t, opts)?;
            Ok(term_body(name, &page, &r))
        }
        _ => unreachable!("checked above"),
    }
}

/// A comma-separated string or a list of strings, each trimmed, empty ones left out.
fn list_of(a: &Map<String, J>, k: &str) -> Result<Vec<String>> {
    let items: Vec<String> = match a.get(k) {
        None | Some(J::Null) => Vec::new(),
        Some(J::String(s)) => s.split(',').map(str::to_string).collect(),
        Some(J::Array(v)) => v
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad(format!("{k}: a string or a list of strings")))
            })
            .collect::<Result<_>>()?,
        Some(_) => return Err(bad(format!("{k}: a string or a list of strings"))),
    };
    Ok(items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// The kinds a comma-separated filter such as `class,property` names.
pub fn parse_search_kinds(names: &[String]) -> Result<Vec<search::Kind>> {
    let mut out: Vec<search::Kind> = Vec::new();
    for n in names {
        let ks = search::Kind::parse_filter(n).ok_or_else(|| {
            bad(format!(
                "unknown kind '{n}': {}",
                search::Kind::ALL.map(|k| k.name()).join(", ")
            ))
        })?;
        out.extend(ks);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The search a `search` request names.
pub fn search_request_of(a: &Map<String, J>) -> Result<search::SearchRequest> {
    let q = string(a, "q")?.ok_or_else(|| bad("q is the text searched for"))?;
    let mut s = search::SearchRequest::new(q);
    s.kinds = parse_search_kinds(&list_of(a, "kind")?)?;
    s.languages = list_of(a, "lang")?;
    for l in &s.languages {
        if oxilangtag::LanguageTag::parse(l.as_str()).is_err() {
            return Err(bad(format!("lang: '{l}' is not a BCP 47 language tag")));
        }
    }
    s.limit = number(a, "limit", 1, search::MAX_LIMIT)?.unwrap_or(search::DEFAULT_LIMIT);
    s.offset = number(a, "offset", 0, search::MAX_WINDOW)?.unwrap_or(0);
    s.fuzzy = boolean(a, "fuzzy")?.unwrap_or(true);
    s.definitions = boolean(a, "definitions")?.unwrap_or(true);
    Ok(s)
}

pub fn languages_of(a: &Map<String, J>) -> Result<Vec<String>> {
    let langs = list_of(a, "lang")?;
    for l in &langs {
        if oxilangtag::LanguageTag::parse(l.as_str()).is_err() {
            return Err(bad(format!("lang: '{l}' is not a BCP 47 language tag")));
        }
    }
    Ok(langs)
}

fn iri_arg(a: &Map<String, J>, k: &str) -> Result<Option<String>> {
    Ok(string(a, k)?.map(|s| s.trim_start_matches('<').trim_end_matches('>').to_string()))
}

/// The hierarchy level a `hierarchy` request names.
pub fn hierarchy_request_of(a: &Map<String, J>) -> Result<browse::HierarchyRequest> {
    let mut h = browse::HierarchyRequest {
        source_id: string(a, "sourceId")?.map(str::to_string),
        after: iri_arg(a, "after")?,
        ..Default::default()
    };
    if let Some(k) = string(a, "kind")? {
        h.kind = browse::TreeKind::parse(k).ok_or_else(|| {
            bad(format!(
                "unknown kind '{k}': {}",
                browse::TreeKind::ALL.map(|k| k.name()).join(", ")
            ))
        })?;
    }
    h.iri = iri_arg(a, "iri")?;
    h.scheme = iri_arg(a, "scheme")?;
    h.ancestors = boolean(a, "ancestors")?.unwrap_or(false);
    h.deprecated = boolean(a, "deprecated")?.unwrap_or(true);
    h.limit = number(a, "limit", 1, browse::MAX_LIMIT)?.unwrap_or(browse::DEFAULT_LIMIT);
    h.offset = number(a, "offset", 0, usize::MAX)?.unwrap_or(0);
    h.languages = languages_of(a)?;
    Ok(h)
}

/// The entity a `term` request names.
pub fn term_request_of(a: &Map<String, J>) -> Result<browse::TermRequest> {
    let iri = iri_arg(a, "iri")?.ok_or_else(|| bad("iri names the entity"))?;
    let mut t = browse::TermRequest::new(iri);
    t.source_id = string(a, "sourceId")?.map(str::to_string);
    t.mentions =
        number(a, "mentions", 0, browse::MAX_MENTIONS)?.unwrap_or(browse::DEFAULT_MENTIONS);
    t.incoming =
        number(a, "incoming", 0, browse::MAX_INCOMING)?.unwrap_or(browse::DEFAULT_INCOMING);
    t.languages = languages_of(a)?;
    Ok(t)
}

fn name_members(n: &browse::Name) -> Map<String, J> {
    let mut m = Map::new();
    m.insert("iri".into(), json!(n.iri));
    m.insert("label".into(), json!(n.label));
    m.insert("labelLang".into(), json!(n.label_lang));
    m.insert("shortForm".into(), json!(n.short_form));
    m.insert("kinds".into(), json!(n.kinds));
    m.insert("deprecated".into(), json!(n.deprecated));
    m
}

/// The JSON of a hierarchy node: its name and the numbers of its children and parents.
pub fn hierarchy_node(n: &browse::Node) -> J {
    let mut m = name_members(&n.name);
    m.insert("children".into(), json!(n.children));
    m.insert("parents".into(), json!(n.parents));
    if n.equivalence_count > 1 {
        m.insert("equivalenceCount".into(), json!(n.equivalence_count));
    }
    J::Object(m)
}

/// The JSON of a hierarchy level, as `GET /$/ontology/{ds}/hierarchy` answers it.
pub fn hierarchy_body(name: &str, h: &browse::HierarchyRequest, r: &browse::Hierarchy) -> J {
    let next = r.offset + r.children.len();
    let mut j = json!({
        "dataset": name,
        "commit": r.commit,
        "graphs": r.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "imports": r.imports.iter().map(|i| json!({"iri": i.iri, "graph": i.graph})).collect::<Vec<_>>(),
        "warnings": r.warnings,
        "kind": r.kind.name(),
        "node": r.node.as_ref().map(hierarchy_node),
        "children": r.children.iter().map(hierarchy_node).collect::<Vec<_>>(),
        "total": r.total,
        "offset": r.offset,
        "limit": h.limit,
        "next": if next < r.total { json!(next) } else { J::Null },
        "order": if r.sorted { "label" } else { "iri" },
    });
    if let Some(source) = &r.inference_source {
        j["nextAfter"] = json!(r.next_after);
        j["hierarchyBasis"] = json!("directEquivalenceGroups");
        j["inferenceSource"] =
            serde_json::to_value(source).expect("materialization metadata serializes");
    }
    if let Some(from) = r.roots_from {
        j["roots"] = json!(from.name());
        j["deprecatedRoots"] = json!(r.deprecated_roots);
    }
    if let Some(a) = &r.ancestors {
        j["ancestors"] = json!({
            "nodes": a.nodes.iter().map(hierarchy_node).collect::<Vec<_>>(),
            "edges": a.edges.iter().map(|(c, p)| json!([c, p])).collect::<Vec<_>>(),
            "truncated": a.truncated,
        });
    }
    j
}

fn value_json(v: &browse::Value) -> J {
    match v {
        browse::Value::Iri(i) => json!(i),
        browse::Value::Literal(l) => {
            sparkles_owl::json::literal(&sparkles_owl::Literal::from(l.clone()))
        }
    }
}

fn incoming_json(i: &browse::Incoming) -> J {
    json!({
        "property": i.property,
        "count": i.count,
        "more": i.more,
        "subjects": i.subjects,
    })
}

/// The JSON of what the graphs say about one entity, as `GET /$/ontology/{ds}/term`
/// answers it.
pub fn term_body(name: &str, t: &browse::TermPage, r: &Renderer) -> J {
    let axioms = |list: &[(Axiom, u32)]| -> Vec<J> {
        list.iter()
            .map(|(a, g)| axiom_json(a, Some(t.graph_name(*g)), r))
            .collect()
    };
    let mut j = json!({
        "dataset": name,
        "commit": t.commit,
        "graphs": t.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "imports": t.imports.iter().map(|i| json!({"iri": i.iri, "graph": i.graph})).collect::<Vec<_>>(),
        "warnings": t.warnings,
    });
    if let Some(source) = &t.inference_source {
        j["inferenceSource"] =
            serde_json::to_value(source).expect("materialization metadata serializes");
    }
    for (k, v) in name_members(&t.name) {
        j[k] = v;
    }
    j["types"] = json!(t.types);
    j["ontologies"] = J::Array(
        t.ontologies
            .iter()
            .enumerate()
            .map(|(n, o)| json!({"graph": t.graph_name(n as u32), "ontology": o}))
            .collect(),
    );
    j["statements"] = J::Array(
        t.statements
            .iter()
            .map(|s| {
                json!({
                    "property": s.property,
                    "value": value_json(&s.value),
                    "graph": t.graph_name(s.graph),
                    "annotations": s.annotations.iter().map(|(p, v)| json!({"property": p, "value": value_json(v)})).collect::<Vec<_>>(),
                })
            })
            .collect(),
    );
    j["statementsTruncated"] = json!(t.statements_truncated);
    j["xlLabels"] = J::Array(
        t.xl_labels
            .iter()
            .map(|s| {
                json!({
                    "property": s.property,
                    "value": value_json(&s.value),
                    "graph": t.graph_name(s.graph),
                })
            })
            .collect(),
    );
    j["axioms"] = J::Array(axioms(&t.axioms));
    j["mentions"] = json!({
        "axioms": axioms(&t.mentions),
        "total": t.mentions_total,
        "truncated": t.mentions_truncated,
    });
    j["incoming"] = J::Array(t.incoming.iter().map(incoming_json).collect());
    j["usage"] = t.usage.as_ref().map_or(J::Null, incoming_json);
    j["names"] = J::Object(
        t.names
            .iter()
            .map(|(iri, n)| {
                let mut m = name_members(n);
                m.remove("iri");
                (iri.clone(), J::Object(m))
            })
            .collect(),
    );
    j
}

/// The UTF-16 offset of byte `b` of `s`.
fn utf16_at(s: &str, b: usize) -> usize {
    s[..b.min(s.len())].encode_utf16().count()
}

/// The JSON of one entity a search found, which every list of search results shares:
/// `type` is `entity`, so a list that mixes it with other kinds of result, such as the
/// command palette's, tells them apart. The highlight ranges are `[start, end)` pairs in
/// UTF-16 code units of `match.text`, the offsets of JavaScript and Java strings.
pub fn search_hit(h: &search::SearchHit) -> J {
    let m = &h.matched;
    let highlight: Vec<J> = m
        .highlight
        .iter()
        .map(|r| json!([utf16_at(&m.text, r.start), utf16_at(&m.text, r.end)]))
        .collect();
    json!({
        "type": "entity",
        "iri": h.iri,
        "kinds": h.kinds.iter().map(|k| k.name()).collect::<Vec<_>>(),
        "label": h.label,
        "labelLang": h.label_lang,
        "shortForm": h.short_form,
        "deprecated": h.deprecated,
        "match": {
            "field": m.field.name(),
            "property": m.property,
            "text": m.text,
            "lang": m.lang,
            "kind": m.kind.name(),
            "distance": m.distance,
            "tier": m.tier,
            "highlight": highlight,
        },
    })
}

/// The JSON of a search's answer, as `GET /$/ontology/{ds}/search` answers it.
pub fn search_body(name: &str, s: &search::SearchRequest, r: &search::SearchResults) -> J {
    let next = s.offset + r.hits.len();
    json!({
        "dataset": name,
        "commit": r.commit,
        "graphs": r.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "imports": r.imports.iter().map(|i| json!({"iri": i.iri, "graph": i.graph})).collect::<Vec<_>>(),
        "warnings": r.warnings,
        "query": s.query,
        "results": r.hits.iter().map(search_hit).collect::<Vec<_>>(),
        "offset": s.offset,
        "limit": s.limit,
        "more": r.more,
        // a page starts below the window's end
        "next": if r.more && next < search::MAX_WINDOW { json!(next) } else { J::Null },
        "index": match r.index {
            search::IndexUse::Current => "current",
            search::IndexUse::Updated => "updated",
            search::IndexUse::Built => "built",
        },
    })
}

/// What the answer of a classification lists besides its counts.
#[derive(Clone, Debug, Default)]
pub struct ClassifyQuery {
    /// Missing preserves legacy output. Frame avoids materializing legacy arrays.
    pub projection: Option<ClassifyProjection>,
    /// Frame-projection labels only; not part of semantic result compatibility.
    pub languages: Vec<String>,
    /// a class whose equivalent classes, direct superclasses and subclasses and all
    /// superclasses the answer gives
    pub about: Option<String>,
    /// list the taxonomy's nodes, `limit` from `offset`
    pub nodes: bool,
    /// the most unsatisfiable classes, incomplete classes, excluded axioms and nodes
    /// listed
    pub limit: usize,
    pub offset: usize,
}

fn node_json(t: &sparkles_owl::classify::Taxonomy, i: usize) -> J {
    let n = t.node(i);
    let first = |k: &usize| t.node(*k).classes.first().map(|c| c.as_str().to_string());
    json!({
        "node": i,
        "classes": n.classes.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "parents": n.parents,
        "parentClasses": n.parents.iter().filter_map(first).collect::<Vec<_>>(),
        "children": n.children.len(),
    })
}

/// The JSON of a classification, as `POST /$/ontology/{ds}/classify` answers it: the
/// members of the ontology read, whether the ontology is consistent, whether the
/// taxonomy is complete, whether it is the partial answer of a stopped run, the rules
/// the axioms need, the counts, the unsatisfiable classes, the classes a partial answer
/// leaves open and the axioms left out or used in part (at most `q.limit` of each), the
/// times of the run, and on request the nodes, one class and the edit that wrote the
/// taxonomy. `about` a class the taxonomy does not have is `NotFound`.
pub fn classify_body(
    name: &str,
    c: &Classified,
    req: &OntologyRequest,
    r: &Renderer,
    q: &ClassifyQuery,
) -> Result<J> {
    if q.projection == Some(ClassifyProjection::Frame) {
        return frame_body(name, c, req, q);
    }
    let read = c.opened.view();
    let view = ClassificationView {
        read: &read,
        classification: &c.classification,
        excluded: &c.excluded,
        reused: c.reused,
        written: c.written.as_ref(),
    };
    let mut answer = classification_body(name, &view, req, r, q)?;
    answer["classificationSource"] = c.publication.identity();
    answer["resultId"] = json!(c.publication.id);
    Ok(answer)
}

/// A classification and the read it classified, for [`classification_body`].
pub struct ClassificationView<'a> {
    pub read: &'a ReadView<'a>,
    pub classification: &'a sparkles_owl::classify::Classification,
    pub excluded: &'a [sparkles_owl::classify::Excluded],
    pub reused: bool,
    pub written: Option<&'a EditOutcome>,
}

/// [`classify_body`] for the classification `c` describes.
pub fn classification_body(
    name: &str,
    c: &ClassificationView<'_>,
    req: &OntologyRequest,
    r: &Renderer,
    q: &ClassifyQuery,
) -> Result<J> {
    use sparkles_owl::classify::Gap;
    let t = &c.classification.taxonomy;
    let s = &c.classification.stats;
    let mut j = read_json(name, c.read, req);
    let unsat: Vec<&str> = t.unsatisfiable().map(|c| c.as_str()).collect();
    use sparkles_owl::classify::Consistency;
    let cl = c.classification;
    j["consistent"] = json!(cl.consistent);
    j["consistency"] = json!(match cl.consistency {
        Consistency::Consistent => "consistent",
        Consistency::Inconsistent => "inconsistent",
        Consistency::Unknown => "unknown",
    });
    // a partial answer is never complete, whatever it left out
    j["complete"] = json!(c.excluded.is_empty() && cl.complete);
    j["partial"] = json!(!cl.complete);
    j["tier"] = json!(s.tier.as_str());
    j["incompleteCount"] = json!(cl.incomplete.len());
    j["incomplete"] = json!(
        cl.incomplete
            .iter()
            .take(q.limit)
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
    );
    j["reused"] = json!(c.reused);
    j["classes"] = json!(t.classes().count());
    j["nodeCount"] = json!(t.nodes().len());
    j["unsatisfiableCount"] = json!(unsat.len());
    j["unsatisfiable"] = json!(unsat.iter().take(q.limit).collect::<Vec<_>>());
    j["excludedCount"] = json!(c.excluded.len());
    j["excluded"] = J::Array(
        c.excluded
            .iter()
            .take(q.limit)
            .map(|e| {
                let ax = Axiom::new(e.axiom.clone());
                let mut x = json!({
                    "type": ax.axiom_type().name(),
                    "ofn": r.axiom(&ax),
                    "axiom": sparkles_owl::json::axiom(&ax),
                });
                match &e.gap {
                    Gap::Outside { rule, message } => {
                        x["rule"] = json!(rule);
                        x["message"] = json!(message);
                    }
                    Gap::Partial(why) => x["partial"] = json!(why),
                }
                x
            })
            .collect(),
    );
    j["stats"] = json!({
        "incremental": s.incremental,
        "added": s.added,
        "removed": s.removed,
        "cleared": s.cleared,
        "contexts": s.contexts,
        "conclusions": s.conclusions,
        "estimatedBytes": s.memory,
        "threads": s.threads,
        "indexMs": s.index.as_secs_f64() * 1000.0,
        "saturationMs": s.saturation.as_secs_f64() * 1000.0,
        "taxonomyMs": s.taxonomy.as_secs_f64() * 1000.0,
    });
    if q.nodes {
        let total = t.nodes().len();
        j["nodes"] = J::Array(
            (q.offset.min(total)..total)
                .take(q.limit)
                .map(|i| node_json(t, i))
                .collect(),
        );
        j["offset"] = json!(q.offset);
        if q.offset.saturating_add(q.limit) < total {
            j["next"] = json!(q.offset + q.limit);
        }
    }
    if let Some(about) = &q.about {
        let class = sparkles_owl::Class::new(about.as_str());
        let n = t
            .node_of(&class)
            .ok_or_else(|| Error::NotFound(format!("the taxonomy has no class <{about}>")))?;
        let firsts = |ns: Vec<usize>| -> Vec<String> {
            ns.into_iter()
                .filter_map(|k| t.node(k).classes.first().map(|c| c.as_str().to_string()))
                .collect()
        };
        let mut x = node_json(t, n);
        x["iri"] = json!(about);
        x["unsatisfiable"] = json!(t.is_unsatisfiable(&class));
        x["childClasses"] = json!(firsts(t.node(n).children.clone()));
        x["superClasses"] = json!(
            t.ancestors(n)
                .into_iter()
                .flat_map(|k| t.node(k).classes.iter().map(|c| c.as_str().to_string()))
                .collect::<Vec<_>>()
        );
        j["class"] = x;
    }
    if let Some(w) = c.written {
        j["written"] = edit_body(w, r);
    }
    Ok(j)
}

// --------------------------------------------------------- Manchester and DL query ---

/// What `as` names for a parse: a class expression when absent.
pub fn what_of(s: Option<&str>) -> Result<sparkles_owl::manchester::What> {
    use sparkles_owl::manchester::What;
    match s {
        None => Ok(What::ClassExpression),
        Some(s) => What::parse(s).ok_or_else(|| {
            bad(format!(
                "as must be one of {}, not '{s}'",
                What::ALL.map(|w| w.as_str()).join(", ")
            ))
        }),
    }
}

/// The JSON of a parse, as `POST /$/ontology/{ds}/parse` answers it: the value in its
/// JSON form, in functional-style syntax and as the Manchester renderer writes it back,
/// with the members of the ontology read.
pub fn parse_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &Renderer,
    v: &sparkles_owl::manchester::Vocabulary,
    what: sparkles_owl::manchester::What,
    p: &sparkles_owl::manchester::Parsed,
) -> J {
    use sparkles_owl::manchester::{Parsed, Renderer as Mr};
    let m = Mr::of_vocabulary(v);
    let (ofn, manchester) = match p {
        Parsed::ClassExpression(c) => (r.class_expression(c), Some(m.class_expression(c))),
        Parsed::DataRange(d) => (r.data_range(d), Some(m.data_range(d))),
        Parsed::Individual(i) => (r.individual(i), Some(m.individual(i))),
        Parsed::Literal(l) => (r.literal(l), Some(m.literal(l))),
        Parsed::Axiom(a) => (r.axiom(a), m.axiom(a)),
    };
    let mut j = opened_json(name, o, req);
    j["as"] = json!(what.as_str());
    j["value"] = p.json();
    j["ofn"] = json!(ofn);
    j["manchester"] = json!(manchester);
    j
}

/// The query and the list limit of a `dlQuery` request, with the expression left to
/// [`dl_expression`].
pub fn dl_query_of(a: &Map<String, J>) -> Result<(DlQueryRequest, usize)> {
    let mut q = DlQueryRequest::new(ClassExpression::Class(sparkles_owl::Class::thing()));
    if let Some(s) = string(a, "reasoning")? {
        q.reasoning = DlReasoning::parse(s)
            .ok_or_else(|| bad(format!("reasoning must be auto, el or rl, not '{s}'")))?;
    }
    match a.get("answers") {
        None | Some(J::Null) => {}
        Some(J::String(s)) => {
            q.answers = DlAnswers::none();
            for n in s.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                q.answers.set(n)?;
            }
        }
        Some(J::Array(v)) => {
            q.answers = DlAnswers::none();
            for n in v {
                q.answers
                    .set(n.as_str().ok_or_else(|| bad("answers: a list of names"))?)?;
            }
        }
        Some(_) => return Err(bad("answers: a list of names")),
    }
    q.threads = number(a, "threads", 0, 1024)?.unwrap_or(0);
    q.max_derived = number(a, "maxDerived", 1, usize::MAX)?;
    let limit = number(a, "limit", 1, MAX_LIMIT)?.unwrap_or(DL_QUERY_LIMIT);
    Ok((q, limit))
}

/// The entries each list of a DL query's answer holds by default.
pub const DL_QUERY_LIMIT: usize = 1000;

/// The expression of a `dlQuery` request read against the open ontology: Manchester
/// text by default, functional-style text with `syntax: "functional"`, or the JSON form.
pub fn dl_expression(
    handle: &super::Ontologies,
    o: &Opened,
    a: &Map<String, J>,
    mut q: DlQueryRequest,
    prefixes: &[(String, String)],
    opts: &QueryOptions,
) -> Result<DlQueryRequest> {
    let syntax = string(a, "syntax")?.unwrap_or("manchester");
    q.expression = match (a.get("expression"), syntax) {
        (None | Some(J::Null), _) => return Err(bad("expression: the class expression to query")),
        (Some(J::String(s)), "manchester") => {
            match handle.parse_manchester(
                o,
                s,
                sparkles_owl::manchester::What::ClassExpression,
                opts,
            )? {
                sparkles_owl::manchester::Parsed::ClassExpression(c) => c,
                _ => unreachable!("asked for a class expression"),
            }
        }
        (Some(J::String(s)), "functional") => {
            sparkles_owl::ofn_read::parse_class_expression(s, prefixes)
                .map_err(|e| bad(format!("expression: {e}")))?
        }
        (Some(J::String(_)), s) => {
            return Err(bad(format!(
                "syntax must be manchester or functional, not '{s}'"
            )));
        }
        (Some(v @ J::Object(_)), _) => sparkles_owl::json::read_class_expression(v)
            .map_err(|e| bad(format!("expression: {e}")))?,
        (Some(_), _) => {
            return Err(bad(
                "expression: Manchester or functional-style text, or the JSON form",
            ));
        }
    };
    Ok(q)
}

/// The JSON of a DL query's answer, as `POST /$/ontology/{ds}/dl-query` answers it:
/// the members of the ontology read, the expression in Manchester and functional-style
/// syntax, the reasoning used with its gaps (at most `limit`) and notes, whether the
/// expression is satisfiable and the ontology consistent, and each list asked for (at
/// most `limit` of each) with its count.
pub fn dl_query_body(
    name: &str,
    a: &DlQueryAnswer,
    req: &OntologyRequest,
    r: &Renderer,
    v: &sparkles_owl::manchester::Vocabulary,
    limit: usize,
) -> J {
    let m = sparkles_owl::manchester::Renderer::of_vocabulary(v);
    let mut j = opened_json(name, &a.opened, req);
    j["expression"] = json!({
        "manchester": m.class_expression(&a.expression),
        "ofn": r.class_expression(&a.expression),
        "json": sparkles_owl::json::class_expression(&a.expression),
    });
    let u = &a.reasoning;
    j["reasoning"] = json!({
        "engine": u.engine.as_str(),
        "title": u.engine.title(),
        "why": u.why,
        "complete": u.complete,
        "instancesComplete": u.instances_complete,
        "gapCount": u.gaps.len(),
        "gaps": u.gaps.iter().take(limit).map(|g| {
            let mut x = json!({
                "message": g.message,
                "rule": g.rule,
                "partial": g.partial,
            });
            if let Some(ax) = &g.axiom {
                x["type"] = json!(ax.axiom_type().name());
                x["ofn"] = json!(r.axiom(ax));
                x["manchester"] = json!(m.axiom(ax));
                x["axiom"] = sparkles_owl::json::axiom(ax);
            }
            x
        }).collect::<Vec<_>>(),
        "notes": u.notes,
    });
    j["satisfiable"] = json!(a.satisfiable);
    j["consistent"] = json!(a.consistent);
    j["incremental"] = json!(a.incremental);
    j["elapsedMs"] = json!(a.elapsed.as_secs_f64() * 1000.0);
    let mut counts = Map::new();
    let mut list = |k: &str, v: Option<Vec<String>>| {
        if let Some(v) = v {
            counts.insert(k.to_string(), json!(v.len()));
            j[k] = json!(v.into_iter().take(limit).collect::<Vec<_>>());
        }
    };
    let iris = |v: &Option<Vec<sparkles_owl::Class>>| {
        v.as_ref()
            .map(|v| v.iter().map(|c| c.as_str().to_string()).collect::<Vec<_>>())
    };
    list("equivalents", iris(&a.equivalents));
    list("superclasses", iris(&a.superclasses));
    list("directSuperclasses", iris(&a.direct_superclasses));
    list("subclasses", iris(&a.subclasses));
    list("directSubclasses", iris(&a.direct_subclasses));
    list(
        "instances",
        a.instances.as_ref().map(|v| {
            v.iter()
                .map(|i| match i {
                    Individual::Named(n) => n.as_str().to_string(),
                    Individual::Anonymous(b) => format!("_:{}", b.label()),
                })
                .collect()
        }),
    );
    j["counts"] = J::Object(counts);
    j
}

/// The individuals read's bounded page and membership lookup.
pub fn individuals_request_of(a: &Map<String, J>) -> Result<browse::IndividualsRequest> {
    Ok(browse::IndividualsRequest {
        source_id: string(a, "sourceId")?.map(str::to_string),
        class: iri_arg(a, "type")?,
        untyped: boolean(a, "untyped")?.unwrap_or(false),
        iri: iri_arg(a, "iri")?,
        after: string(a, "after")?.map(str::to_string),
        limit: number(a, "limit", 1, browse::MAX_LIMIT)?.unwrap_or(browse::DEFAULT_LIMIT),
        languages: languages_of(a)?,
    })
}

/// The JSON page of named individuals or their type buckets.
pub fn individuals_body(name: &str, r: &browse::Individuals) -> J {
    let mut body = json!({
        "dataset": name, "commit": r.commit, "order": "iri",
        "groups": r.groups.iter().map(|g| json!({"type": g.class.as_ref().map(name_members), "total": g.total})).collect::<Vec<_>>(),
        "individuals": r.individuals.iter().map(|n| {
            let mut m = name_members(&n.name);
            m.insert("types".into(), json!(n.types.iter().map(name_members).collect::<Vec<_>>()));
            J::Object(m)
        }).collect::<Vec<_>>(),
        "total": r.total, "next": r.next, "warnings": r.warnings,
    });
    if let Some(source) = &r.inference_source {
        body["typeBasis"] = json!("materializedNamedTypes");
        body["inferenceSource"] =
            serde_json::to_value(source).expect("materialization metadata serializes");
    }
    body
}

/// A bounded explicit frame projection, separate from legacy full taxonomy output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassifyProjection {
    Frame,
}
pub fn projection_of(value: Option<&str>) -> Result<Option<ClassifyProjection>> {
    match value {
        None => Ok(None),
        Some("frame") => Ok(Some(ClassifyProjection::Frame)),
        Some(_) => Err(bad("projection must be frame or absent")),
    }
}
impl ClassifyQuery {
    /// Harmless adapter defaults (false nodes, zero/null offset, null target) stay
    /// compatible. Effective computing/output conflicts fail before any admission.
    pub fn validate_projection(&self, writes: bool, ofn: bool, has_language: bool) -> Result<()> {
        if self.projection.is_none() {
            if has_language {
                return Err(bad("lang requires projection=frame"));
            }
            return Ok(());
        }
        if self.nodes || self.offset != 0 || writes || ofn {
            return Err(bad(
                "frame projection does not take nodes=true, a nonzero offset, target or OFN output",
            ));
        }
        if !(1..=1000).contains(&self.limit) {
            return Err(bad("frame projection limit must be from 1 to 1000"));
        }
        let iri = self
            .about
            .as_deref()
            .ok_or_else(|| bad("frame projection requires about"))?;
        oxrdf::NamedNode::new(iri).map_err(|_| bad("about must be an absolute named IRI"))?;
        Ok(())
    }
}

pub fn kept_query_of(a: &Map<String, J>) -> Result<super::kept::KeptQuery> {
    use super::kept::{KeptQuery, Part};
    if a.contains_key("inferences") {
        return Err(bad("keptClassification does not take inferences"));
    }
    let part = match string(a, "part")?.unwrap_or("status") {
        "status" => Part::Status,
        "parents" => Part::Parents,
        "children" => Part::Children,
        "roots" => Part::Roots,
        "equivalents" => Part::Equivalents,
        "sources" => Part::Sources,
        "excluded" => Part::Excluded,
        _ => {
            return Err(bad(
                "part must be status, roots, parents, children, equivalents, sources or excluded",
            ));
        }
    };
    let result_id = string(a, "resultId")?
        .map(|s| {
            let id =
                uuid::Uuid::parse_str(s).map_err(|_| bad("resultId must be a canonical UUID"))?;
            if id.to_string() != s {
                return Err(bad("resultId must be a canonical UUID"));
            }
            Ok(id)
        })
        .transpose()?;
    Ok(KeptQuery {
        part,
        about: iri_arg(a, "about")?,
        offset: number(a, "offset", 0, usize::MAX)?.unwrap_or(0),
        limit: number(a, "limit", 1, 1000)?.unwrap_or(100),
        result_id,
        languages: languages_of(a)?,
    })
}
pub fn kept_body(name: &str, answer: &super::kept::KeptRead) -> Result<J> {
    let mut value =
        serde_json::to_value(answer).map_err(|e| bad(format!("kept classification: {e}")))?;
    value["dataset"] = json!(name);
    Ok(value)
}

fn frame_body(name: &str, c: &Classified, req: &OntologyRequest, q: &ClassifyQuery) -> Result<J> {
    use super::kept::{KeptQuery, Part};
    q.validate_projection(c.written.is_some(), false, !q.languages.is_empty())?;
    let about = q.about.as_ref().expect("validated frame about");
    let h = c.opened.snapshot.clone();
    let handle = &c.publication;
    // Query names through the exact already authorized snapshot. This never widens
    // an isolated view or enters the shared cache to fetch its labels.
    let mut query = KeptQuery {
        about: Some(about.clone()),
        part: Part::Parents,
        limit: q.limit,
        languages: q.languages.clone(),
        ..Default::default()
    };
    let parents = super::Ontologies::kept_page(req, &query, &h, handle, &c.read_options)?;
    query.part = Part::Equivalents;
    let equivalents = super::Ontologies::kept_page(req, &query, &h, handle, &c.read_options)?;
    let kept = c.retained;
    query.part = Part::Excluded;
    query.about = None;
    let exclusions = super::Ontologies::kept_page(req, &query, &h, handle, &c.read_options)?;
    let project = |page: super::kept::Page| json!({"total":page.total,"limit":q.limit,"truncated":page.next_offset.is_some(),"items":page.items});
    let mut source_samples = Vec::new();
    for i in 0..q.limit.min(handle.source.source_count()) {
        super::check(&c.read_options)?;
        #[cfg(test)]
        super::kept::diagnostic::visit(super::kept::diagnostic::Visit::Source);
        source_samples.push(handle.source.source_row(i).expect("source ordinal"));
    }
    let class = sparkles_owl::Class::new(about.as_str());
    Ok(
        json!({"dataset":name,"commit":h.commit,"classificationSource":handle.identity(),"frame":{
            "iri":about,"result":handle.metadata(),"outcome":handle.outcome(),
            "unsatisfiable":handle.classification.taxonomy.is_unsatisfiable(&class),
            "parents":project(parents),"equivalents":project(equivalents),"excluded":project(exclusions),
            "sources":{"items":source_samples,"total":handle.source.source_count(),"truncated":handle.source.source_count()>q.limit},
            "retention":{"kept":kept,"resultId":if kept { Some(handle.id) } else { None::<uuid::Uuid> }}
        }}),
    )
}

/// Decode selector objects at raw JSON boundaries, before duplicate singleton keys
/// disappear into a map. Legacy requests without a projection keep their old value
/// normalization. Structured callers already have unique object keys.
pub fn decode_arguments(op: &str, bytes: &[u8]) -> Result<J> {
    if matches!(op, "classify" | "keptClassification") {
        decode_argument_object(bytes, op == "keptClassification", op == "classify")
    } else {
        // Unrelated legacy operations accept null as an empty argument object.
        serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid ontology JSON: {e}")))
    }
}
/// Duplicate-sensitive selectors when the projection is supplied outside the body.
pub fn decode_strict_arguments(bytes: &[u8]) -> Result<J> {
    decode_argument_object(bytes, true, true)
}
fn decode_argument_object(
    bytes: &[u8],
    force_strict: bool,
    projection_sensitive: bool,
) -> Result<J> {
    use serde::de::{MapAccess, Visitor};
    struct Args {
        value: Map<String, J>,
        duplicate: Option<String>,
    }
    impl<'de> serde::Deserialize<'de> for Args {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Args;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("an ontology argument object")
                }
                fn visit_map<M: MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> std::result::Result<Args, M::Error> {
                    let mut value = Map::new();
                    let mut duplicate = None;
                    while let Some(key) = map.next_key::<String>()? {
                        let v = map.next_value::<J>()?;
                        if value.insert(key.clone(), v).is_some() {
                            duplicate = Some(key);
                        }
                    }
                    Ok(Args { value, duplicate })
                }
            }
            d.deserialize_any(V)
        }
    }
    let args: Args =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid ontology JSON: {e}")))?;
    if (force_strict || (projection_sensitive && args.value.contains_key("projection")))
        && let Some(key) = args.duplicate
    {
        return Err(bad(format!("duplicate selector '{key}'")));
    }
    Ok(J::Object(args.value))
}
