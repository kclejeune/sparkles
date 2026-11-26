//! OBO Graphs JSON (`.json`) writer and reader.
//!
//! Implements the OBO Graphs JSON schema (`{"graphs": [...]}`) used by ROBOT and OBO
//! release pipelines, representing ontology metadata, nodes (`CLASS`, `PROPERTY`,
//! `INDIVIDUAL`), edges (`is_a`, `subPropertyOf`, `inverseOf`, `type`, existential
//! restrictions and object property assertions), `equivalentNodesSets`,
//! `logicalDefinitionAxioms`, `domainRangeAxioms` and `propertyChainAxioms`.
//!
//! [`write_document`] streams deterministic, pretty-printed JSON directly to any
//! [`std::io::Write`] sink without allocating a full-document `serde_json::Value` DOM.
//! Axioms outside the OBO Graphs JSON model are either refused
//! ([`WriteOptions::strict`]) or skipped and recorded in [`WriteReport`].
//!
//! [`parse`] reads an OBO Graphs JSON document back into an
//! [`crate::ofn_read::Document`].

use crate::model::*;
use crate::obo::{
    IAO_DEFINITION, OIO_HAS_BROAD_SYNONYM, OIO_HAS_DBXREF, OIO_HAS_EXACT_SYNONYM,
    OIO_HAS_NARROW_SYNONYM, OIO_HAS_RELATED_SYNONYM, OIO_HAS_SYNONYM_TYPE, OIO_IN_SUBSET,
};
use crate::ofn::Renderer;
use crate::ofn_read::{Document, ParseError};
use crate::validate;
use crate::vocab;
use crate::write_common::{
    WriteError, WriteOptions, WriteReport, check_axiom_depth, check_header_depth,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

// ------------------------------------------------------------------ model --------

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct BasicPv {
    pred: String,
    val: String,
    xrefs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct DefVal {
    val: String,
    xrefs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SynVal {
    pred: String,
    val: String,
    synonym_type: Option<String>,
    xrefs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Meta {
    version: Option<String>,
    definition: Option<DefVal>,
    comments: Vec<String>,
    subsets: Vec<String>,
    synonyms: Vec<SynVal>,
    xrefs: Vec<String>,
    basic_property_values: Vec<BasicPv>,
    deprecated: bool,
}

impl Meta {
    fn is_empty(&self) -> bool {
        self.version.is_none()
            && self.definition.is_none()
            && self.comments.is_empty()
            && self.subsets.is_empty()
            && self.synonyms.is_empty()
            && self.xrefs.is_empty()
            && self.basic_property_values.is_empty()
            && !self.deprecated
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NodeType {
    Class,
    Property,
    Individual,
}

impl NodeType {
    fn as_str(self) -> &'static str {
        match self {
            NodeType::Class => "CLASS",
            NodeType::Property => "PROPERTY",
            NodeType::Individual => "INDIVIDUAL",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PropertyType {
    Object,
    Data,
    Annotation,
}

impl PropertyType {
    fn as_str(self) -> &'static str {
        match self {
            PropertyType::Object => "OBJECT",
            PropertyType::Data => "DATA",
            PropertyType::Annotation => "ANNOTATION",
        }
    }
}

#[derive(Default)]
struct NodeEntry {
    id: String,
    lbl: Option<String>,
    node_type: Option<NodeType>,
    property_type: Option<PropertyType>,
    meta: Meta,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeEntry {
    sub: String,
    pred: String,
    obj: String,
    meta: Meta,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EquivNodesSet {
    representative: String,
    node_ids: Vec<String>,
    meta: Meta,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LogicalDef {
    defined_class_id: String,
    genus_ids: Vec<String>,
    restrictions: Vec<(String, String)>,
    meta: Meta,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DomainRangeEntry {
    predicate_id: String,
    domain_class_ids: Vec<String>,
    range_class_ids: Vec<String>,
    meta: Meta,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PropertyChainEntry {
    predicate_id: String,
    chain_predicate_ids: Vec<String>,
    meta: Meta,
}

// ------------------------------------------------------------------ writing ------

fn annotations_to_meta(anns: &BTreeSet<Annotation>) -> Option<Meta> {
    let mut meta = Meta::default();
    for a in anns {
        if !a.annotations.is_empty() {
            return None;
        }
        // OBO Graphs JSON basicPropertyValues stores only a string `val` with no datatype
        // or IRI flag, so only plain xsd:string literals round-trip without losing type.
        let val = plain_str(&a.value)?.to_string();
        meta.basic_property_values.push(BasicPv {
            pred: a.property.as_str().to_string(),
            val,
            xrefs: Vec::new(),
        });
    }
    meta.basic_property_values.sort();
    Some(meta)
}

fn plain_str(v: &AnnotationValue) -> Option<&str> {
    match v {
        AnnotationValue::Literal(l)
            if l.language().is_none() && l.datatype() == vocab::XSD_STRING =>
        {
            Some(l.value())
        }
        _ => None,
    }
}

fn extract_xref_strings(anns: &BTreeSet<Annotation>) -> Option<Vec<String>> {
    let mut xrefs = Vec::new();
    for a in anns {
        if a.property.as_str() == OIO_HAS_DBXREF
            && a.annotations.is_empty()
            && let Some(s) = plain_str(&a.value)
        {
            xrefs.push(s.to_string());
        } else {
            return None;
        }
    }
    xrefs.sort();
    Some(xrefs)
}

fn apply_node_annotation(
    node: &mut NodeEntry,
    p: &AnnotationProperty,
    v: &AnnotationValue,
    anns: &BTreeSet<Annotation>,
) -> bool {
    let pstr = p.as_str();
    if pstr == vocab::RDFS_LABEL && anns.is_empty() && plain_str(v).is_some() {
        let s = plain_str(v).unwrap().to_string();
        if node.lbl.is_none() {
            node.lbl = Some(s);
        } else {
            node.meta.basic_property_values.push(BasicPv {
                pred: pstr.to_string(),
                val: s,
                xrefs: Vec::new(),
            });
        }
        return true;
    }
    if pstr == IAO_DEFINITION
        && let Some(val) = plain_str(v)
        && let Some(xrefs) = extract_xref_strings(anns)
        && node.meta.definition.is_none()
    {
        node.meta.definition = Some(DefVal {
            val: val.to_string(),
            xrefs,
        });
        return true;
    }
    if pstr == vocab::RDFS_COMMENT
        && anns.is_empty()
        && let Some(s) = plain_str(v)
    {
        node.meta.comments.push(s.to_string());
        return true;
    }
    if pstr == OIO_IN_SUBSET
        && anns.is_empty()
        && let AnnotationValue::Iri(i) = v
    {
        node.meta.subsets.push(i.as_str().to_string());
        return true;
    }
    if pstr == OIO_HAS_DBXREF
        && anns.is_empty()
        && let Some(s) = plain_str(v)
    {
        node.meta.xrefs.push(s.to_string());
        return true;
    }
    if pstr == vocab::OWL_DEPRECATED
        && anns.is_empty()
        && let AnnotationValue::Literal(l) = v
        && l.datatype() == vocab::XSD_BOOLEAN
        && l.value() == "true"
    {
        node.meta.deprecated = true;
        return true;
    }
    if let Some(syn_pred) = match pstr {
        OIO_HAS_EXACT_SYNONYM => Some("hasExactSynonym"),
        OIO_HAS_BROAD_SYNONYM => Some("hasBroadSynonym"),
        OIO_HAS_NARROW_SYNONYM => Some("hasNarrowSynonym"),
        OIO_HAS_RELATED_SYNONYM => Some("hasRelatedSynonym"),
        _ => None,
    } && let Some(val) = plain_str(v)
    {
        let mut xrefs = Vec::new();
        let mut syn_ty = None;
        let mut ok = true;
        for a in anns {
            if !a.annotations.is_empty() {
                ok = false;
                break;
            }
            if a.property.as_str() == OIO_HAS_DBXREF
                && let Some(x) = plain_str(&a.value)
            {
                xrefs.push(x.to_string());
            } else if a.property.as_str() == OIO_HAS_SYNONYM_TYPE
                && syn_ty.is_none()
                && let AnnotationValue::Iri(i) = &a.value
            {
                syn_ty = Some(i.as_str().to_string());
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            xrefs.sort();
            node.meta.synonyms.push(SynVal {
                pred: syn_pred.to_string(),
                val: val.to_string(),
                synonym_type: syn_ty,
                xrefs,
            });
            return true;
        }
    }
    if let Some(val) = plain_str(v)
        && let Some(xrefs) = extract_xref_strings(anns)
    {
        node.meta.basic_property_values.push(BasicPv {
            pred: pstr.to_string(),
            val: val.to_string(),
            xrefs,
        });
        return true;
    }
    false
}

fn node_compatible(
    nodes: &BTreeMap<String, NodeEntry>,
    id: &str,
    ty: NodeType,
    prop_ty: Option<PropertyType>,
) -> bool {
    let Some(existing) = nodes.get(id) else {
        return true;
    };
    if let Some(et) = existing.node_type
        && et != ty
    {
        return false;
    }
    if let (Some(ep), Some(np)) = (existing.property_type, prop_ty)
        && ep != np
    {
        return false;
    }
    true
}

fn ensure_node_typed(
    nodes: &mut BTreeMap<String, NodeEntry>,
    id: &str,
    ty: NodeType,
    prop_ty: Option<PropertyType>,
) {
    let e = nodes.entry(id.to_string()).or_insert_with(|| NodeEntry {
        id: id.to_string(),
        ..Default::default()
    });
    if e.node_type.is_none() {
        e.node_type = Some(ty);
    }
    if e.property_type.is_none() {
        e.property_type = prop_ty;
    }
}

/// Write an ontology header and axioms as an OBO Graphs JSON (`.json`) document.
pub fn write_document<'a>(
    w: &mut dyn Write,
    renderer: &Renderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
    options: WriteOptions,
) -> Result<WriteReport, WriteError> {
    check_header_depth(header, "OBO Graphs JSON")?;
    let mut axioms: Vec<&'a Axiom> = axioms.into_iter().collect();
    for &ax in &axioms {
        check_axiom_depth(ax, "OBO Graphs JSON")?;
    }
    axioms.sort();
    let mut report = WriteReport::default();

    let mut nodes: BTreeMap<String, NodeEntry> = BTreeMap::new();
    let mut edges: Vec<EdgeEntry> = Vec::new();
    let mut equivs: Vec<EquivNodesSet> = Vec::new();
    let mut logical_defs: Vec<LogicalDef> = Vec::new();
    let mut domain_ranges: Vec<DomainRangeEntry> = Vec::new();
    let mut chains: Vec<PropertyChainEntry> = Vec::new();
    let mut pending_anns: Vec<&'a Axiom> = Vec::new();

    // First pass: declarations.
    for &ax in &axioms {
        if let AxiomKind::Declaration(e) = &ax.kind {
            if !ax.annotations.is_empty() {
                report.skip_or_err(
                    options,
                    renderer,
                    ax,
                    "OBO Graphs JSON cannot attach annotations to Declaration axioms",
                )?;
                continue;
            }
            let target = match e {
                Entity::Class(c) => Some((c.as_str(), NodeType::Class, None)),
                Entity::ObjectProperty(p) => {
                    Some((p.as_str(), NodeType::Property, Some(PropertyType::Object)))
                }
                Entity::DataProperty(p) => {
                    Some((p.as_str(), NodeType::Property, Some(PropertyType::Data)))
                }
                Entity::AnnotationProperty(p) => Some((
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Annotation),
                )),
                Entity::NamedIndividual(i) => Some((i.as_str(), NodeType::Individual, None)),
                Entity::Datatype(_) => None,
            };
            let Some((id, ty, pt)) = target else {
                report.skip_or_err(
                    options,
                    renderer,
                    ax,
                    "OBO Graphs JSON has no node type for Datatype declarations",
                )?;
                continue;
            };
            if !node_compatible(&nodes, id, ty, pt) {
                report.skip_or_err(
                    options,
                    renderer,
                    ax,
                    "OBO Graphs JSON cannot represent punned entity kinds on the same node IRI",
                )?;
                continue;
            }
            ensure_node_typed(&mut nodes, id, ty, pt);
            report.written_axioms += 1;
        }
    }

    // Second pass: axioms.
    for &ax in &axioms {
        use AxiomKind as K;
        match &ax.kind {
            K::Declaration(_) => continue,
            K::SubClassOf(ClassExpression::Class(sub), ClassExpression::Class(sup)) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(&nodes, sub.as_str(), NodeType::Class, None)
                    || !node_compatible(&nodes, sup.as_str(), NodeType::Class, None)
                {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent SubClassOf on a node punned with a non-class kind",
                    )?;
                    continue;
                }
                ensure_node_typed(&mut nodes, sub.as_str(), NodeType::Class, None);
                ensure_node_typed(&mut nodes, sup.as_str(), NodeType::Class, None);
                edges.push(EdgeEntry {
                    sub: sub.as_str().to_string(),
                    pred: "is_a".to_string(),
                    obj: sup.as_str().to_string(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::SubClassOf(
                ClassExpression::Class(sub),
                ClassExpression::ObjectSomeValuesFrom(
                    ObjectPropertyExpression::Property(p),
                    filler,
                ),
            ) if matches!(filler.as_ref(), ClassExpression::Class(_)) => {
                let ClassExpression::Class(target) = filler.as_ref() else {
                    unreachable!()
                };
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(&nodes, sub.as_str(), NodeType::Class, None)
                    || !node_compatible(
                        &nodes,
                        p.as_str(),
                        NodeType::Property,
                        Some(PropertyType::Object),
                    )
                    || !node_compatible(&nodes, target.as_str(), NodeType::Class, None)
                {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent existential restriction on a punned node IRI",
                    )?;
                    continue;
                }
                ensure_node_typed(&mut nodes, sub.as_str(), NodeType::Class, None);
                ensure_node_typed(
                    &mut nodes,
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                ensure_node_typed(&mut nodes, target.as_str(), NodeType::Class, None);
                edges.push(EdgeEntry {
                    sub: sub.as_str().to_string(),
                    pred: p.as_str().to_string(),
                    obj: target.as_str().to_string(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::EquivalentClasses(cs) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if cs.len() >= 2 && cs.iter().all(|c| matches!(c, ClassExpression::Class(_))) {
                    let ids: Vec<String> = cs
                        .iter()
                        .map(|c| c.as_class().expect("checked").as_str().to_string())
                        .collect();
                    if !ids
                        .iter()
                        .all(|id| node_compatible(&nodes, id, NodeType::Class, None))
                    {
                        report.skip_or_err(
                            options,
                            renderer,
                            ax,
                            "OBO Graphs JSON cannot represent EquivalentClasses on a punned non-class node",
                        )?;
                        continue;
                    }
                    for id in &ids {
                        ensure_node_typed(&mut nodes, id, NodeType::Class, None);
                    }
                    let representative = ids[0].clone();
                    equivs.push(EquivNodesSet {
                        representative,
                        node_ids: ids,
                        meta,
                    });
                    report.written_axioms += 1;
                } else if cs.len() == 2 {
                    let items: Vec<&ClassExpression> = cs.iter().collect();
                    let pair = match (items[0], items[1]) {
                        (ClassExpression::Class(c), other) | (other, ClassExpression::Class(c)) => {
                            Some((c, other))
                        }
                        _ => None,
                    };
                    if let Some((c, other)) = pair {
                        let mut genus_ids = Vec::new();
                        let mut restrictions = Vec::new();
                        let mut ok = true;
                        match other {
                            ClassExpression::ObjectIntersectionOf(members) => {
                                for m in members {
                                    match m {
                                        ClassExpression::Class(mc) => {
                                            genus_ids.push(mc.as_str().to_string())
                                        }
                                        ClassExpression::ObjectSomeValuesFrom(
                                            ObjectPropertyExpression::Property(p),
                                            f,
                                        ) => {
                                            if let ClassExpression::Class(fc) = f.as_ref() {
                                                restrictions.push((
                                                    p.as_str().to_string(),
                                                    fc.as_str().to_string(),
                                                ));
                                            } else {
                                                ok = false;
                                                break;
                                            }
                                        }
                                        _ => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                            }
                            ClassExpression::ObjectSomeValuesFrom(
                                ObjectPropertyExpression::Property(p),
                                f,
                            ) => {
                                if let ClassExpression::Class(fc) = f.as_ref() {
                                    restrictions
                                        .push((p.as_str().to_string(), fc.as_str().to_string()));
                                } else {
                                    ok = false;
                                }
                            }
                            _ => ok = false,
                        }
                        if ok && (!genus_ids.is_empty() || !restrictions.is_empty()) {
                            let kinds_ok =
                                node_compatible(&nodes, c.as_str(), NodeType::Class, None)
                                    && genus_ids
                                        .iter()
                                        .all(|g| node_compatible(&nodes, g, NodeType::Class, None))
                                    && restrictions.iter().all(|(p, f)| {
                                        node_compatible(
                                            &nodes,
                                            p,
                                            NodeType::Property,
                                            Some(PropertyType::Object),
                                        ) && node_compatible(&nodes, f, NodeType::Class, None)
                                    });
                            if !kinds_ok {
                                report.skip_or_err(
                                    options,
                                    renderer,
                                    ax,
                                    "OBO Graphs JSON cannot represent logicalDefinitionAxioms on a punned node",
                                )?;
                                continue;
                            }
                            genus_ids.sort();
                            restrictions.sort();
                            ensure_node_typed(&mut nodes, c.as_str(), NodeType::Class, None);
                            for g in &genus_ids {
                                ensure_node_typed(&mut nodes, g, NodeType::Class, None);
                            }
                            for (p, f) in &restrictions {
                                ensure_node_typed(
                                    &mut nodes,
                                    p,
                                    NodeType::Property,
                                    Some(PropertyType::Object),
                                );
                                ensure_node_typed(&mut nodes, f, NodeType::Class, None);
                            }
                            logical_defs.push(LogicalDef {
                                defined_class_id: c.as_str().to_string(),
                                genus_ids,
                                restrictions,
                                meta,
                            });
                            report.written_axioms += 1;
                        } else {
                            report.skip_or_err(options, renderer,
                                ax,
                                "OBO Graphs JSON logicalDefinitionAxioms supports only named genera and simple existentials",
                            )?;
                        }
                    } else {
                        report.skip_or_err(
                            options,
                            renderer,
                            ax,
                            "OBO Graphs JSON requires a named class in EquivalentClasses",
                        )?;
                    }
                } else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent this EquivalentClasses axiom",
                    )?;
                }
            }
            K::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ObjectPropertyExpression::Property(a)),
                ObjectPropertyExpression::Property(b),
            ) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(
                    &nodes,
                    a.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                ) || !node_compatible(
                    &nodes,
                    b.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                ) {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent SubObjectPropertyOf on a punned node",
                    )?;
                    continue;
                }
                ensure_node_typed(
                    &mut nodes,
                    a.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                ensure_node_typed(
                    &mut nodes,
                    b.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                edges.push(EdgeEntry {
                    sub: a.as_str().to_string(),
                    pred: "subPropertyOf".to_string(),
                    obj: b.as_str().to_string(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::SubObjectPropertyOf(
                SubObjectPropertyExpression::Chain(ps),
                ObjectPropertyExpression::Property(sup),
            ) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                let mut ids = Vec::with_capacity(ps.len());
                let mut ok = true;
                for p in ps {
                    if let ObjectPropertyExpression::Property(np) = p {
                        ids.push(np.as_str().to_string());
                    } else {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    let kinds_ok = node_compatible(
                        &nodes,
                        sup.as_str(),
                        NodeType::Property,
                        Some(PropertyType::Object),
                    ) && ids.iter().all(|id| {
                        node_compatible(&nodes, id, NodeType::Property, Some(PropertyType::Object))
                    });
                    if !kinds_ok {
                        report.skip_or_err(
                            options,
                            renderer,
                            ax,
                            "OBO Graphs JSON cannot represent propertyChainAxioms on a punned node",
                        )?;
                        continue;
                    }
                    ensure_node_typed(
                        &mut nodes,
                        sup.as_str(),
                        NodeType::Property,
                        Some(PropertyType::Object),
                    );
                    for id in &ids {
                        ensure_node_typed(
                            &mut nodes,
                            id,
                            NodeType::Property,
                            Some(PropertyType::Object),
                        );
                    }
                    chains.push(PropertyChainEntry {
                        predicate_id: sup.as_str().to_string(),
                        chain_predicate_ids: ids,
                        meta,
                    });
                    report.written_axioms += 1;
                } else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON propertyChainAxioms supports only named object properties",
                    )?;
                }
            }
            K::InverseObjectProperties(
                ObjectPropertyExpression::Property(a),
                ObjectPropertyExpression::Property(b),
            ) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(
                    &nodes,
                    a.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                ) || !node_compatible(
                    &nodes,
                    b.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                ) {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent InverseObjectProperties on a punned node",
                    )?;
                    continue;
                }
                ensure_node_typed(
                    &mut nodes,
                    a.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                ensure_node_typed(
                    &mut nodes,
                    b.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                edges.push(EdgeEntry {
                    sub: a.as_str().to_string(),
                    pred: "inverseOf".to_string(),
                    obj: b.as_str().to_string(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::ObjectPropertyDomain(
                ObjectPropertyExpression::Property(p),
                ClassExpression::Class(c),
            ) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(
                    &nodes,
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                ) || !node_compatible(&nodes, c.as_str(), NodeType::Class, None)
                {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent ObjectPropertyDomain on a punned node",
                    )?;
                    continue;
                }
                ensure_node_typed(
                    &mut nodes,
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                ensure_node_typed(&mut nodes, c.as_str(), NodeType::Class, None);
                domain_ranges.push(DomainRangeEntry {
                    predicate_id: p.as_str().to_string(),
                    domain_class_ids: vec![c.as_str().to_string()],
                    range_class_ids: Vec::new(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::ObjectPropertyRange(
                ObjectPropertyExpression::Property(p),
                ClassExpression::Class(c),
            ) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(
                    &nodes,
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                ) || !node_compatible(&nodes, c.as_str(), NodeType::Class, None)
                {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent ObjectPropertyRange on a punned node",
                    )?;
                    continue;
                }
                ensure_node_typed(
                    &mut nodes,
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                ensure_node_typed(&mut nodes, c.as_str(), NodeType::Class, None);
                domain_ranges.push(DomainRangeEntry {
                    predicate_id: p.as_str().to_string(),
                    domain_class_ids: Vec::new(),
                    range_class_ids: vec![c.as_str().to_string()],
                    meta,
                });
                report.written_axioms += 1;
            }
            K::ClassAssertion(ClassExpression::Class(c), Individual::Named(i)) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(&nodes, i.as_str(), NodeType::Individual, None)
                    || !node_compatible(&nodes, c.as_str(), NodeType::Class, None)
                {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent ClassAssertion on a punned node",
                    )?;
                    continue;
                }
                ensure_node_typed(&mut nodes, i.as_str(), NodeType::Individual, None);
                ensure_node_typed(&mut nodes, c.as_str(), NodeType::Class, None);
                edges.push(EdgeEntry {
                    sub: i.as_str().to_string(),
                    pred: "type".to_string(),
                    obj: c.as_str().to_string(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::ObjectPropertyAssertion(
                ObjectPropertyExpression::Property(p),
                Individual::Named(a),
                Individual::Named(b),
            ) => {
                let Some(meta) = annotations_to_meta(&ax.annotations) else {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent these axiom annotations",
                    )?;
                    continue;
                };
                if !node_compatible(&nodes, a.as_str(), NodeType::Individual, None)
                    || !node_compatible(
                        &nodes,
                        p.as_str(),
                        NodeType::Property,
                        Some(PropertyType::Object),
                    )
                    || !node_compatible(&nodes, b.as_str(), NodeType::Individual, None)
                {
                    report.skip_or_err(
                        options,
                        renderer,
                        ax,
                        "OBO Graphs JSON cannot represent ObjectPropertyAssertion on a punned node IRI",
                    )?;
                    continue;
                }
                ensure_node_typed(&mut nodes, a.as_str(), NodeType::Individual, None);
                ensure_node_typed(
                    &mut nodes,
                    p.as_str(),
                    NodeType::Property,
                    Some(PropertyType::Object),
                );
                ensure_node_typed(&mut nodes, b.as_str(), NodeType::Individual, None);
                edges.push(EdgeEntry {
                    sub: a.as_str().to_string(),
                    pred: p.as_str().to_string(),
                    obj: b.as_str().to_string(),
                    meta,
                });
                report.written_axioms += 1;
            }
            K::AnnotationAssertion(..) => pending_anns.push(ax),
            _ => {
                report.skip_or_err(
                    options,
                    renderer,
                    ax,
                    &format!(
                        "OBO Graphs JSON cannot cleanly represent {} axiom",
                        ax.axiom_type().name()
                    ),
                )?;
            }
        }
    }

    for ax in pending_anns {
        let AxiomKind::AnnotationAssertion(p, subj, val) = &ax.kind else {
            unreachable!()
        };
        let AnnotationSubject::Iri(siri) = subj else {
            report.skip_or_err(
                options,
                renderer,
                ax,
                "OBO Graphs JSON cannot represent annotations on anonymous individuals",
            )?;
            continue;
        };
        let node = nodes
            .entry(siri.as_str().to_string())
            .or_insert_with(|| NodeEntry {
                id: siri.as_str().to_string(),
                ..Default::default()
            });
        if apply_node_annotation(node, p, val, &ax.annotations) {
            report.written_axioms += 1;
        } else {
            report.skip_or_err(
                options,
                renderer,
                ax,
                "OBO Graphs JSON cannot represent this AnnotationAssertion",
            )?;
        }
    }

    // Build header meta.
    let mut graph_meta = Meta {
        version: header.version_iri.as_ref().map(|v| v.as_str().to_string()),
        ..Default::default()
    };
    for imp in &header.imports {
        graph_meta.basic_property_values.push(BasicPv {
            pred: vocab::OWL_IMPORTS.to_string(),
            val: imp.as_str().to_string(),
            xrefs: Vec::new(),
        });
    }
    for ann in &header.annotations {
        if ann.property.as_str() == vocab::RDFS_COMMENT
            && ann.annotations.is_empty()
            && let Some(s) = plain_str(&ann.value)
        {
            graph_meta.comments.push(s.to_string());
        } else if let Some(val) = plain_str(&ann.value)
            && let Some(xrefs) = extract_xref_strings(&ann.annotations)
        {
            graph_meta.basic_property_values.push(BasicPv {
                pred: ann.property.as_str().to_string(),
                val: val.to_string(),
                xrefs,
            });
        } else {
            report.skip_header_or_err(
                options,
                renderer,
                ann,
                "OBO Graphs JSON cannot represent header annotation",
            )?;
        }
    }
    graph_meta.comments.sort();
    graph_meta.basic_property_values.sort();

    edges.sort();
    equivs.sort();
    logical_defs.sort();
    domain_ranges.sort();
    chains.sort();

    // --- Stream deterministic pretty-printed JSON ---
    w.write_all(b"{\n  \"graphs\" : [ {\n")?;
    let mut first_field = true;
    if let Some(iri) = &header.iri {
        w.write_all(b"    \"id\" : ")?;
        write_json_str(w, iri.as_str())?;
        first_field = false;
    }
    if !graph_meta.is_empty() {
        if !first_field {
            w.write_all(b",\n")?;
        }
        w.write_all(b"    \"meta\" : ")?;
        write_meta(w, &graph_meta, 4)?;
        first_field = false;
    }

    // nodes
    if !first_field {
        w.write_all(b",\n")?;
    }
    w.write_all(b"    \"nodes\" : [")?;
    for (i, mut node) in nodes.into_values().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        node.meta.comments.sort();
        node.meta.subsets.sort();
        node.meta.synonyms.sort();
        node.meta.xrefs.sort();
        node.meta.basic_property_values.sort();
        w.write_all(b" {\n      \"id\" : ")?;
        write_json_str(w, &node.id)?;
        if let Some(lbl) = &node.lbl {
            w.write_all(b",\n      \"lbl\" : ")?;
            write_json_str(w, lbl)?;
        }
        if let Some(ty) = node.node_type {
            w.write_all(b",\n      \"type\" : ")?;
            write_json_str(w, ty.as_str())?;
        }
        if let Some(pt) = node.property_type {
            w.write_all(b",\n      \"propertyType\" : ")?;
            write_json_str(w, pt.as_str())?;
        }
        if !node.meta.is_empty() {
            w.write_all(b",\n      \"meta\" : ")?;
            write_meta(w, &node.meta, 6)?;
        }
        w.write_all(b"\n    }")?;
    }
    w.write_all(b" ],\n    \"edges\" : [")?;
    for (i, edge) in edges.iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        w.write_all(b" {\n      \"sub\" : ")?;
        write_json_str(w, &edge.sub)?;
        w.write_all(b",\n      \"pred\" : ")?;
        write_json_str(w, &edge.pred)?;
        w.write_all(b",\n      \"obj\" : ")?;
        write_json_str(w, &edge.obj)?;
        if !edge.meta.is_empty() {
            w.write_all(b",\n      \"meta\" : ")?;
            write_meta(w, &edge.meta, 6)?;
        }
        w.write_all(b"\n    }")?;
    }
    w.write_all(b" ]")?;

    if !equivs.is_empty() {
        w.write_all(b",\n    \"equivalentNodesSets\" : [")?;
        for (i, eq) in equivs.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            w.write_all(b" {\n      \"representativeNodeId\" : ")?;
            write_json_str(w, &eq.representative)?;
            w.write_all(b",\n      \"nodeIds\" : ")?;
            write_str_array(w, &eq.node_ids)?;
            if !eq.meta.is_empty() {
                w.write_all(b",\n      \"meta\" : ")?;
                write_meta(w, &eq.meta, 6)?;
            }
            w.write_all(b"\n    }")?;
        }
        w.write_all(b" ]")?;
    }

    if !logical_defs.is_empty() {
        w.write_all(b",\n    \"logicalDefinitionAxioms\" : [")?;
        for (i, ld) in logical_defs.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            w.write_all(b" {\n      \"definedClassId\" : ")?;
            write_json_str(w, &ld.defined_class_id)?;
            if !ld.genus_ids.is_empty() {
                w.write_all(b",\n      \"genusIds\" : ")?;
                write_str_array(w, &ld.genus_ids)?;
            }
            if !ld.restrictions.is_empty() {
                w.write_all(b",\n      \"restrictions\" : [")?;
                for (j, (prop, filler)) in ld.restrictions.iter().enumerate() {
                    if j > 0 {
                        w.write_all(b",")?;
                    }
                    w.write_all(b" {\n        \"propertyId\" : ")?;
                    write_json_str(w, prop)?;
                    w.write_all(b",\n        \"fillerId\" : ")?;
                    write_json_str(w, filler)?;
                    w.write_all(b"\n      }")?;
                }
                w.write_all(b" ]")?;
            }
            if !ld.meta.is_empty() {
                w.write_all(b",\n      \"meta\" : ")?;
                write_meta(w, &ld.meta, 6)?;
            }
            w.write_all(b"\n    }")?;
        }
        w.write_all(b" ]")?;
    }

    if !domain_ranges.is_empty() {
        w.write_all(b",\n    \"domainRangeAxioms\" : [")?;
        for (i, dr) in domain_ranges.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            w.write_all(b" {\n      \"predicateId\" : ")?;
            write_json_str(w, &dr.predicate_id)?;
            if !dr.domain_class_ids.is_empty() {
                w.write_all(b",\n      \"domainClassIds\" : ")?;
                write_str_array(w, &dr.domain_class_ids)?;
            }
            if !dr.range_class_ids.is_empty() {
                w.write_all(b",\n      \"rangeClassIds\" : ")?;
                write_str_array(w, &dr.range_class_ids)?;
            }
            if !dr.meta.is_empty() {
                w.write_all(b",\n      \"meta\" : ")?;
                write_meta(w, &dr.meta, 6)?;
            }
            w.write_all(b"\n    }")?;
        }
        w.write_all(b" ]")?;
    }

    if !chains.is_empty() {
        w.write_all(b",\n    \"propertyChainAxioms\" : [")?;
        for (i, ch) in chains.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            w.write_all(b" {\n      \"predicateId\" : ")?;
            write_json_str(w, &ch.predicate_id)?;
            w.write_all(b",\n      \"chainPredicateIds\" : ")?;
            write_str_array(w, &ch.chain_predicate_ids)?;
            if !ch.meta.is_empty() {
                w.write_all(b",\n      \"meta\" : ")?;
                write_meta(w, &ch.meta, 6)?;
            }
            w.write_all(b"\n    }")?;
        }
        w.write_all(b" ]")?;
    }

    w.write_all(b"\n  } ]\n}\n")?;
    Ok(report)
}

/// Format an ontology header and axioms as an OBO Graphs JSON string.
pub fn document<'a>(
    renderer: &Renderer,
    header: &Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
    options: WriteOptions,
) -> Result<(String, WriteReport), WriteError> {
    let mut buf = Vec::new();
    let report = write_document(&mut buf, renderer, header, axioms, options)?;
    let s = String::from_utf8(buf).map_err(|e| WriteError::Unsupported(e.to_string()))?;
    Ok((s, report))
}

fn write_json_str(w: &mut dyn Write, s: &str) -> Result<(), WriteError> {
    serde_json::to_writer(w, s).map_err(|e| {
        if e.is_io() {
            WriteError::Io(e.into())
        } else {
            WriteError::Unsupported(e.to_string())
        }
    })
}

fn write_str_array(w: &mut dyn Write, items: &[String]) -> Result<(), WriteError> {
    w.write_all(b"[ ")?;
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            w.write_all(b", ")?;
        }
        write_json_str(w, s)?;
    }
    w.write_all(b" ]")?;
    Ok(())
}

fn write_meta(w: &mut dyn Write, meta: &Meta, indent: usize) -> Result<(), WriteError> {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 2);
    w.write_all(b"{\n")?;
    let mut first = true;
    if let Some(ver) = &meta.version {
        write!(w, "{inner}\"version\" : ")?;
        write_json_str(w, ver)?;
        first = false;
    }
    if let Some(def) = &meta.definition {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"definition\" : {{\n{inner}  \"val\" : ")?;
        write_json_str(w, &def.val)?;
        if !def.xrefs.is_empty() {
            write!(w, ",\n{inner}  \"xrefs\" : ")?;
            write_str_array(w, &def.xrefs)?;
        }
        write!(w, "\n{inner}}}")?;
        first = false;
    }
    if !meta.comments.is_empty() {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"comments\" : ")?;
        write_str_array(w, &meta.comments)?;
        first = false;
    }
    if !meta.subsets.is_empty() {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"subsets\" : ")?;
        write_str_array(w, &meta.subsets)?;
        first = false;
    }
    if !meta.synonyms.is_empty() {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"synonyms\" : [")?;
        for (i, syn) in meta.synonyms.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            write!(w, " {{\n{inner}  \"pred\" : ")?;
            write_json_str(w, &syn.pred)?;
            write!(w, ",\n{inner}  \"val\" : ")?;
            write_json_str(w, &syn.val)?;
            if let Some(ty) = &syn.synonym_type {
                write!(w, ",\n{inner}  \"synonymType\" : ")?;
                write_json_str(w, ty)?;
            }
            if !syn.xrefs.is_empty() {
                write!(w, ",\n{inner}  \"xrefs\" : ")?;
                write_str_array(w, &syn.xrefs)?;
            }
            write!(w, "\n{inner}}}")?;
        }
        w.write_all(b" ]")?;
        first = false;
    }
    if !meta.xrefs.is_empty() {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"xrefs\" : [")?;
        for (i, x) in meta.xrefs.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            write!(w, " {{ \"val\" : ")?;
            write_json_str(w, x)?;
            w.write_all(b" }")?;
        }
        w.write_all(b" ]")?;
        first = false;
    }
    if meta.deprecated {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"deprecated\" : true")?;
        first = false;
    }
    if !meta.basic_property_values.is_empty() {
        if !first {
            w.write_all(b",\n")?;
        }
        write!(w, "{inner}\"basicPropertyValues\" : [")?;
        for (i, pv) in meta.basic_property_values.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            write!(w, " {{\n{inner}  \"pred\" : ")?;
            write_json_str(w, &pv.pred)?;
            write!(w, ",\n{inner}  \"val\" : ")?;
            write_json_str(w, &pv.val)?;
            if !pv.xrefs.is_empty() {
                write!(w, ",\n{inner}  \"xrefs\" : ")?;
                write_str_array(w, &pv.xrefs)?;
            }
            write!(w, "\n{inner}}}")?;
        }
        w.write_all(b" ]")?;
    }
    write!(w, "\n{pad}}}")?;
    Ok(())
}

// ------------------------------------------------------------------ reading ------

/// Read an OBO Graphs JSON (`.json`) document into an [`crate::ofn_read::Document`].
pub fn parse(text: &str) -> Result<Document, ParseError> {
    let root: Value = serde_json::from_str(text).map_err(|e| ParseError {
        line: e.line(),
        column: e.column(),
        message: format!("invalid OBO Graphs JSON: {e}"),
    })?;
    let graphs = root
        .get("graphs")
        .and_then(Value::as_array)
        .ok_or_else(|| jerr("OBO Graphs JSON missing top-level \"graphs\" array"))?;
    let graph = graphs
        .first()
        .and_then(Value::as_object)
        .ok_or_else(|| jerr("OBO Graphs JSON \"graphs\" array must hold a graph object"))?;

    let mut header = Header::default();
    if let Some(id) = graph.get("id").and_then(Value::as_str)
        && !id.is_empty()
    {
        header.iri = Some(parse_iri(id).map_err(|e| jerr(e.0))?);
    }
    if let Some(meta) = graph.get("meta").and_then(Value::as_object) {
        if let Some(ver) = meta.get("version").and_then(Value::as_str) {
            header.version_iri = Some(parse_iri(ver).map_err(|e| jerr(e.0))?);
        }
        for c in meta
            .get("comments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            header.annotations.insert(Annotation::comment(c));
        }
        for pv in meta
            .get("basicPropertyValues")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
        {
            let Some(pred) = pv.get("pred").and_then(Value::as_str) else {
                continue;
            };
            let Some(val) = pv.get("val").and_then(Value::as_str) else {
                continue;
            };
            if pred == vocab::OWL_IMPORTS {
                header
                    .imports
                    .insert(parse_iri(val).map_err(|e| jerr(e.0))?);
            } else {
                let mut anns = BTreeSet::new();
                for x in pv
                    .get("xrefs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    anns.insert(Annotation::new(
                        AnnotationProperty::new(OIO_HAS_DBXREF),
                        AnnotationValue::Literal(Literal::simple(x)),
                    ));
                }
                header.annotations.insert(Annotation {
                    property: AnnotationProperty(parse_iri(pred).map_err(|e| jerr(e.0))?),
                    value: AnnotationValue::Literal(Literal::simple(val)),
                    annotations: anns,
                });
            }
        }
    }

    let mut axioms = Vec::new();
    let mut node_types: BTreeMap<String, String> = BTreeMap::new();

    for node in graph
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("node missing \"id\""))?;
        let iri = parse_iri(id).map_err(|e| jerr(e.0))?;
        if let Some(ty) = node.get("type").and_then(Value::as_str) {
            node_types.insert(id.to_string(), ty.to_string());
            match ty {
                "CLASS" => axioms.push(Axiom::declaration(Entity::Class(Class(iri.clone())))),
                "PROPERTY" => {
                    let entity = match node.get("propertyType").and_then(Value::as_str) {
                        Some("DATA") => Entity::DataProperty(DataProperty(iri.clone())),
                        Some("ANNOTATION") => {
                            Entity::AnnotationProperty(AnnotationProperty(iri.clone()))
                        }
                        _ => Entity::ObjectProperty(ObjectProperty(iri.clone())),
                    };
                    axioms.push(Axiom::declaration(entity));
                }
                "INDIVIDUAL" => axioms.push(Axiom::declaration(Entity::NamedIndividual(
                    NamedIndividual(iri.clone()),
                ))),
                _ => {}
            }
        }
        if let Some(lbl) = node.get("lbl").and_then(Value::as_str) {
            axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(vocab::RDFS_LABEL),
                AnnotationSubject::Iri(iri.clone()),
                AnnotationValue::Literal(Literal::simple(lbl)),
            )));
        }
        if let Some(meta) = node.get("meta").and_then(Value::as_object) {
            if let Some(def) = meta.get("definition").and_then(Value::as_object)
                && let Some(val) = def.get("val").and_then(Value::as_str)
            {
                let mut anns = BTreeSet::new();
                for x in def
                    .get("xrefs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    anns.insert(Annotation::new(
                        AnnotationProperty::new(OIO_HAS_DBXREF),
                        AnnotationValue::Literal(Literal::simple(x)),
                    ));
                }
                axioms.push(Axiom {
                    kind: AxiomKind::AnnotationAssertion(
                        AnnotationProperty::new(IAO_DEFINITION),
                        AnnotationSubject::Iri(iri.clone()),
                        AnnotationValue::Literal(Literal::simple(val)),
                    ),
                    annotations: anns,
                });
            }
            for c in meta
                .get("comments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                    AnnotationProperty::new(vocab::RDFS_COMMENT),
                    AnnotationSubject::Iri(iri.clone()),
                    AnnotationValue::Literal(Literal::simple(c)),
                )));
            }
            for sub in meta
                .get("subsets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let sub_iri = parse_iri(sub).map_err(|e| jerr(e.0))?;
                axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                    AnnotationProperty::new(OIO_IN_SUBSET),
                    AnnotationSubject::Iri(iri.clone()),
                    AnnotationValue::Iri(sub_iri),
                )));
            }
            for syn in meta
                .get("synonyms")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
            {
                let Some(pred) = syn.get("pred").and_then(Value::as_str) else {
                    continue;
                };
                let Some(val) = syn.get("val").and_then(Value::as_str) else {
                    continue;
                };
                let prop = match pred {
                    "hasExactSynonym" => OIO_HAS_EXACT_SYNONYM,
                    "hasBroadSynonym" => OIO_HAS_BROAD_SYNONYM,
                    "hasNarrowSynonym" => OIO_HAS_NARROW_SYNONYM,
                    "hasRelatedSynonym" => OIO_HAS_RELATED_SYNONYM,
                    other => other,
                };
                let mut anns = BTreeSet::new();
                for x in syn
                    .get("xrefs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    anns.insert(Annotation::new(
                        AnnotationProperty::new(OIO_HAS_DBXREF),
                        AnnotationValue::Literal(Literal::simple(x)),
                    ));
                }
                if let Some(ty) = syn.get("synonymType").and_then(Value::as_str) {
                    anns.insert(Annotation::new(
                        AnnotationProperty::new(OIO_HAS_SYNONYM_TYPE),
                        AnnotationValue::Iri(parse_iri(ty).map_err(|e| jerr(e.0))?),
                    ));
                }
                axioms.push(Axiom {
                    kind: AxiomKind::AnnotationAssertion(
                        AnnotationProperty(parse_iri(prop).map_err(|e| jerr(e.0))?),
                        AnnotationSubject::Iri(iri.clone()),
                        AnnotationValue::Literal(Literal::simple(val)),
                    ),
                    annotations: anns,
                });
            }
            for x in meta
                .get("xrefs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.get("val").and_then(Value::as_str))
            {
                axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationSubject::Iri(iri.clone()),
                    AnnotationValue::Literal(Literal::simple(x)),
                )));
            }
            if meta.get("deprecated").and_then(Value::as_bool) == Some(true) {
                axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                    AnnotationProperty::new(vocab::OWL_DEPRECATED),
                    AnnotationSubject::Iri(iri.clone()),
                    AnnotationValue::Literal(Literal::typed("true", vocab::XSD_BOOLEAN)),
                )));
            }
            for pv in meta
                .get("basicPropertyValues")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
            {
                let Some(pred) = pv.get("pred").and_then(Value::as_str) else {
                    continue;
                };
                let Some(val) = pv.get("val").and_then(Value::as_str) else {
                    continue;
                };
                let mut anns = BTreeSet::new();
                for x in pv
                    .get("xrefs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    anns.insert(Annotation::new(
                        AnnotationProperty::new(OIO_HAS_DBXREF),
                        AnnotationValue::Literal(Literal::simple(x)),
                    ));
                }
                axioms.push(Axiom {
                    kind: AxiomKind::AnnotationAssertion(
                        AnnotationProperty(parse_iri(pred).map_err(|e| jerr(e.0))?),
                        AnnotationSubject::Iri(iri.clone()),
                        AnnotationValue::Literal(Literal::simple(val)),
                    ),
                    annotations: anns,
                });
            }
        }
    }

    for edge in graph
        .get("edges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let sub = edge
            .get("sub")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("edge missing \"sub\""))?;
        let pred = edge
            .get("pred")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("edge missing \"pred\""))?;
        let obj = edge
            .get("obj")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("edge missing \"obj\""))?;
        let sub_iri = parse_iri(sub).map_err(|e| jerr(e.0))?;
        let obj_iri = parse_iri(obj).map_err(|e| jerr(e.0))?;
        let anns = parse_meta_annotations(edge.get("meta"))?;
        let kind = match pred {
            "is_a" => AxiomKind::SubClassOf(
                ClassExpression::Class(Class(sub_iri)),
                ClassExpression::Class(Class(obj_iri)),
            ),
            "subPropertyOf" => AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ObjectProperty(sub_iri).into()),
                ObjectProperty(obj_iri).into(),
            ),
            "inverseOf" => AxiomKind::InverseObjectProperties(
                ObjectProperty(sub_iri).into(),
                ObjectProperty(obj_iri).into(),
            ),
            "type" => AxiomKind::ClassAssertion(
                ClassExpression::Class(Class(obj_iri)),
                Individual::Named(NamedIndividual(sub_iri)),
            ),
            _ => {
                let pred_iri = parse_iri(pred).map_err(|e| jerr(e.0))?;
                if node_types.get(sub).map(String::as_str) == Some("INDIVIDUAL") {
                    AxiomKind::ObjectPropertyAssertion(
                        ObjectProperty(pred_iri).into(),
                        Individual::Named(NamedIndividual(sub_iri)),
                        Individual::Named(NamedIndividual(obj_iri)),
                    )
                } else {
                    AxiomKind::SubClassOf(
                        ClassExpression::Class(Class(sub_iri)),
                        ClassExpression::some(ObjectProperty(pred_iri), Class(obj_iri)),
                    )
                }
            }
        };
        axioms.push(Axiom {
            kind,
            annotations: anns,
        });
    }

    for eq in graph
        .get("equivalentNodesSets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let anns = parse_meta_annotations(eq.get("meta"))?;
        let mut set = BTreeSet::new();
        for id in eq
            .get("nodeIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            set.insert(ClassExpression::Class(Class(
                parse_iri(id).map_err(|e| jerr(e.0))?,
            )));
        }
        if !set.is_empty() {
            axioms.push(Axiom {
                kind: AxiomKind::EquivalentClasses(set),
                annotations: anns,
            });
        }
    }

    for ld in graph
        .get("logicalDefinitionAxioms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let defined = ld
            .get("definedClassId")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("logicalDefinitionAxiom missing \"definedClassId\""))?;
        let defined_iri = parse_iri(defined).map_err(|e| jerr(e.0))?;
        let anns = parse_meta_annotations(ld.get("meta"))?;
        let mut conjuncts = BTreeSet::new();
        for g in ld
            .get("genusIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            conjuncts.insert(ClassExpression::Class(Class(
                parse_iri(g).map_err(|e| jerr(e.0))?,
            )));
        }
        for r in ld
            .get("restrictions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
        {
            let p = r
                .get("propertyId")
                .and_then(Value::as_str)
                .ok_or_else(|| jerr("restriction missing \"propertyId\""))?;
            let f = r
                .get("fillerId")
                .and_then(Value::as_str)
                .ok_or_else(|| jerr("restriction missing \"fillerId\""))?;
            conjuncts.insert(ClassExpression::some(
                ObjectProperty(parse_iri(p).map_err(|e| jerr(e.0))?),
                Class(parse_iri(f).map_err(|e| jerr(e.0))?),
            ));
        }
        let rhs = if conjuncts.len() == 1 {
            conjuncts.into_iter().next().expect("len 1")
        } else {
            ClassExpression::ObjectIntersectionOf(conjuncts)
        };
        let mut set = BTreeSet::new();
        set.insert(ClassExpression::Class(Class(defined_iri)));
        set.insert(rhs);
        axioms.push(Axiom {
            kind: AxiomKind::EquivalentClasses(set),
            annotations: anns,
        });
    }

    for dr in graph
        .get("domainRangeAxioms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let p = dr
            .get("predicateId")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("domainRangeAxiom missing \"predicateId\""))?;
        let p_iri = parse_iri(p).map_err(|e| jerr(e.0))?;
        let anns = parse_meta_annotations(dr.get("meta"))?;
        for d in dr
            .get("domainClassIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            axioms.push(Axiom {
                kind: AxiomKind::ObjectPropertyDomain(
                    ObjectProperty(p_iri.clone()).into(),
                    ClassExpression::Class(Class(parse_iri(d).map_err(|e| jerr(e.0))?)),
                ),
                annotations: anns.clone(),
            });
        }
        for r in dr
            .get("rangeClassIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            axioms.push(Axiom {
                kind: AxiomKind::ObjectPropertyRange(
                    ObjectProperty(p_iri.clone()).into(),
                    ClassExpression::Class(Class(parse_iri(r).map_err(|e| jerr(e.0))?)),
                ),
                annotations: anns.clone(),
            });
        }
    }

    for ch in graph
        .get("propertyChainAxioms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let sup = ch
            .get("predicateId")
            .and_then(Value::as_str)
            .ok_or_else(|| jerr("propertyChainAxiom missing \"predicateId\""))?;
        let sup_iri = parse_iri(sup).map_err(|e| jerr(e.0))?;
        let anns = parse_meta_annotations(ch.get("meta"))?;
        let mut chain = Vec::new();
        for p in ch
            .get("chainPredicateIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            chain.push(ObjectProperty(parse_iri(p).map_err(|e| jerr(e.0))?).into());
        }
        axioms.push(Axiom {
            kind: AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Chain(chain),
                ObjectProperty(sup_iri).into(),
            ),
            annotations: anns,
        });
    }

    for ax in &axioms {
        validate::axiom(ax).map_err(|e| jerr(e.0))?;
    }

    Ok(Document {
        prefixes: Vec::new(),
        header,
        axioms,
        rules: 0,
    })
}

fn parse_meta_annotations(meta: Option<&Value>) -> Result<BTreeSet<Annotation>, ParseError> {
    let mut anns = BTreeSet::new();
    let Some(obj) = meta.and_then(Value::as_object) else {
        return Ok(anns);
    };
    for pv in obj
        .get("basicPropertyValues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let Some(pred) = pv.get("pred").and_then(Value::as_str) else {
            continue;
        };
        let Some(val) = pv.get("val").and_then(Value::as_str) else {
            continue;
        };
        anns.insert(Annotation::new(
            AnnotationProperty(parse_iri(pred).map_err(|e| jerr(e.0))?),
            AnnotationValue::Literal(Literal::simple(val)),
        ));
    }
    Ok(anns)
}

fn jerr(message: impl Into<String>) -> ParseError {
    ParseError {
        line: 1,
        column: 1,
        message: message.into(),
    }
}
