//! SPIN rules as SHACL-AF SPARQL rules.
//!
//! A SPIN rule is a SPARQL CONSTRUCT attached to a class with `spin:rule`, or with a
//! subproperty of it, and SPIN runs it with `?this` bound to each instance of the class.
//! [`translate`] writes each one as a `sh:SPARQLRule` of a new node shape whose target is
//! that class, which is the same evaluation. A rule that does not mention `?this` runs
//! once in SPIN, so its shape targets the class node alone and the query runs once.
//!
//! Queries are read from SPIN RDF syntax (`sp:Construct`, `sp:where`, `sp:templates`, and
//! all SPARQL 1.1 graph pattern, property path, expression, built-in and aggregate nodes),
//! from SPIN template calls (`spin:Template`, `spin:ConstructTemplate`, `spin:body`,
//! `spl:Argument` with defaults, optional arguments, value types and `rdfs:subClassOf`
//! inheritance), or from `sp:text`. Malformed, cyclic or over-budget SPIN graphs are
//! refused cleanly. SPIN's rule properties all run in one group, which iterates to a fixed
//! point as TopBraid's SPIN inferencing does.

mod rdf;
mod template;
mod vocab;

use crate::shapes::G;
use crate::vocab::{rdf as rdf_vocab, rdfs as rdfs_vocab, sh};
use anyhow::{Context as _, Result, bail};
use oxrdf::{BlankNode, Graph, Literal, NamedNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::Query;
use sparkles_core::id::Id;
use sparkles_core::index::{Key, Perm};
use sparkles_core::store::{Chunk, Snapshot};

pub use vocab::{SP_NS, SPIN_NS, SPL_NS};

/// Optional read-only store lookup for `rdf:type` and `rdfs:subClassOf` when shapes and
/// data live in separate graphs (e.g. `shacl-af?shapes=...`).
pub(crate) struct ExtraTypes<'a> {
    snap: &'a Snapshot,
    graph_ids: Vec<Id>,
    rdf_type: Option<Id>,
    sub_class_of: Option<Id>,
}

impl<'a> ExtraTypes<'a> {
    pub(crate) fn new(snap: &'a Snapshot, graphs: &[String]) -> Self {
        let mut graph_ids = Vec::new();
        let mut seen = FxHashSet::default();
        for iri in graphs {
            if let Some(id) = sparkles_core::validation::graph_id(snap, iri)
                && seen.insert(id)
            {
                graph_ids.push(id);
            }
        }
        let rdf_type = snap.lookup_iri(rdf_vocab::TYPE.as_str());
        let sub_class_of = snap.lookup_iri(rdfs_vocab::SUB_CLASS_OF.as_str());
        Self {
            snap,
            graph_ids,
            rdf_type,
            sub_class_of,
        }
    }

    pub(crate) fn rdf_types(
        &self,
        subject: &Term,
        budget: &mut rdf::WorkBudget,
    ) -> Result<Vec<Term>> {
        self.objects_for(subject, self.rdf_type, budget)
    }

    pub(crate) fn superclasses(
        &self,
        class: &Term,
        budget: &mut rdf::WorkBudget,
    ) -> Result<Vec<Term>> {
        self.objects_for(class, self.sub_class_of, budget)
    }

    fn objects_for(
        &self,
        subject: &Term,
        pred: Option<Id>,
        budget: &mut rdf::WorkBudget,
    ) -> Result<Vec<Term>> {
        budget.step()?;
        let Some(pred_id) = pred else {
            return Ok(Vec::new());
        };
        let Some(sub_id) = self.snap.lookup_term(subject) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut budget_err: Option<anyhow::Error> = None;
        for &gid in &self.graph_ids {
            budget.step()?;
            let res = self.snap.scan_checked(
                Perm::Gspo,
                &[gid.0, sub_id.0, pred_id.0],
                || Ok(()),
                |chunk| {
                    let mut visit = |k: Key| -> sparkles_core::Result<()> {
                        if let Err(e) = budget.step() {
                            budget_err = Some(e);
                            return Err(sparkles_core::Error::Cancelled);
                        }
                        let q = Perm::Gspo.to_quad(&k);
                        if let Some(term) = self.snap.term(q[2])
                            && matches!(term, Term::NamedNode(_) | Term::BlankNode(_))
                        {
                            match rdf::term_ref_size_and_depth(term.as_ref(), 0, budget)
                                .and_then(|bytes| budget.charge_bytes(bytes))
                            {
                                Ok(()) => out.push(term),
                                Err(e) => {
                                    budget_err = Some(e);
                                    return Err(sparkles_core::Error::Cancelled);
                                }
                            }
                        }
                        Ok(())
                    };
                    match chunk {
                        Chunk::Block(b, s, e) => {
                            for idx in s..e {
                                visit(b.key(idx))?;
                            }
                        }
                        Chunk::Row(k) => visit(k)?,
                    }
                    Ok(true)
                },
            );
            if let Some(err) = budget_err {
                return Err(err);
            }
            res.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(out)
    }
}

/// Optional validation gate applied to a dual-form RDF candidate before accepting it over
/// `sp:text` fallback (used by rule translation to verify parseability and `CONSTRUCT` form).
pub(crate) struct CandidateCheck<'a> {
    pub(crate) prefix_head: &'a str,
    pub(crate) require_construct: bool,
}

impl CandidateCheck<'_> {
    pub(crate) fn validate(&self, sparql: &str) -> Result<Query> {
        let full = if self.prefix_head.is_empty() {
            std::borrow::Cow::Borrowed(sparql)
        } else {
            std::borrow::Cow::Owned(format!("{}{sparql}", self.prefix_head))
        };
        let parsed = sparkles_core::sparql::parse_query(&full, None, &[])
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if self.require_construct && !matches!(parsed, Query::Construct { .. }) {
            bail!("SPIN rule query must be a CONSTRUCT query");
        }
        Ok(parsed)
    }
}

/// The prefixes a SPIN query may use without declaring them: those of RDF, RDFS, OWL,
/// XSD, SHACL and SPIN's own vocabularies.
pub const STANDARD_PREFIXES: &[(&str, &str)] = &[
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("sh", "http://www.w3.org/ns/shacl#"),
    ("sp", SP_NS),
    ("spin", SPIN_NS),
    ("spl", SPL_NS),
];

/// Whether a graph holds SPIN rules.
pub fn has_rules(graph: &Graph) -> bool {
    let mut budget = rdf::WorkBudget::new(rdf::MAX_NODES);
    // On budget exhaustion return true so `translate` surfaces the limit error cleanly.
    rule_properties(graph, &mut budget).map_or(true, |props| !props.is_empty())
}

/// `spin:rule` and its subproperties (`rdfs:subPropertyOf*`).
fn rule_properties(graph: &Graph, budget: &mut rdf::WorkBudget) -> Result<Vec<NamedNode>> {
    let mut out = vec![vocab::spin::RULE.into_owned()];
    let mut seen: FxHashSet<NamedNode> = FxHashSet::default();
    seen.insert(vocab::spin::RULE.into_owned());
    let mut i = 0;
    while i < out.len() {
        budget.step()?;
        let p = Term::NamedNode(out[i].clone());
        for t in graph.subjects_for_predicate_object(oxrdf::vocab::rdfs::SUB_PROPERTY_OF, &p) {
            budget.step()?;
            if let oxrdf::NamedOrBlankNodeRef::NamedNode(n) = t {
                let n = rdf::check_and_own_iri(n, budget)?;
                if seen.insert(n.clone()) {
                    out.push(n);
                }
            }
        }
        i += 1;
    }
    out.retain(|p| graph.triples_for_predicate(p).next().is_some());
    Ok(out)
}

/// Serialize a SPIN query node (in SPIN RDF syntax, as a SPIN template call, or via
/// `sp:text`) into SPARQL 1.1 query text.
pub fn serialize_query(graph: &Graph, query: &Term) -> Result<String> {
    let g = G { g: graph };
    let mut budget = rdf::WorkBudget::new(rdf::MAX_NODES);
    rdf::term_ref_size_and_depth(query.as_ref(), 0, &mut budget)?;
    let mut cache = template::HierarchyCache::default();
    query_to_sparql(&g, None, query, None, &mut budget, &mut cache)
}

fn read_direct_sp_text(
    g: &G<'_>,
    node: &Term,
    budget: &mut rdf::WorkBudget,
) -> Result<Option<String>> {
    match rdf::single_object_ref(g, node, vocab::sp::TEXT, budget)? {
        None => Ok(None),
        Some(oxrdf::TermRef::Literal(l)) => {
            let val = l.value();
            if val.len() > rdf::MAX_OUTPUT_BYTES {
                return Err(rdf::limit_err(format!(
                    "SPIN sp:text exceeds maximum output size ({} bytes)",
                    rdf::MAX_OUTPUT_BYTES
                )));
            }
            budget.charge_bytes(val.len())?;
            Ok(Some(val.to_string()))
        }
        Some(other) => bail!("the sp:text of the SPIN rule {node} is {other}, not a string"),
    }
}

fn query_to_sparql(
    g: &G<'_>,
    extra: Option<&ExtraTypes<'_>>,
    node: &Term,
    check: Option<&CandidateCheck<'_>>,
    budget: &mut rdf::WorkBudget,
    cache: &mut template::HierarchyCache,
) -> Result<String> {
    // 1. Template call (an instance of a spin:Template / spin:ConstructTemplate or subclass)
    if let Some(template) = template::template_of_call(g, node, budget, cache)? {
        return template::expand_template_call(g, extra, node, &template, check, budget, cache);
    }
    // 2. SPIN RDF syntax (prefer structural RDF over sp:text so TopBraid files with full
    //    IRIs run even when Turtle prefix declarations are absent; fall back to sp:text on
    //    unsupported or non-runnable structural constructs to preserve compatibility for
    //    dual-form files)
    if rdf::has_rdf_query_structure(g, node) {
        let empty = FxHashMap::default();
        let rdf_res = rdf::serialize_query_with_bindings(g, node, &empty, budget).and_then(|q| {
            if let Some(c) = check {
                c.validate(&q)?;
            }
            Ok(q)
        });
        match rdf_res {
            Ok(sparql) => return Ok(sparql),
            Err(rdf_err) => {
                if !rdf::is_limit_or_cycle_error(&rdf_err)
                    && let Some(text) = read_direct_sp_text(g, node, budget)?
                {
                    return Ok(text);
                }
                return Err(rdf_err);
            }
        }
    }
    // 3. Direct sp:text query
    if let Some(text) = read_direct_sp_text(g, node, budget)? {
        return Ok(text);
    }
    let kind = crate::shapes::as_subject(node)
        .and_then(|sub| g.g.object_for_subject_predicate(sub, rdf_vocab::TYPE))
        .map_or_else(
            || "no type".to_string(),
            |t| match t {
                oxrdf::TermRef::NamedNode(n) if n.as_str().len() <= 256 => n.to_string(),
                oxrdf::TermRef::BlankNode(b) if b.as_str().len() <= 256 => b.to_string(),
                _ => "<non-IRI or oversized type>".to_string(),
            },
        );
    bail!(
        "the SPIN rule {node} ({kind}) has neither SPIN RDF syntax, a SPIN template \
         definition, nor sp:text"
    );
}

/// The SHACL-AF triples of the SPIN rules of `graph`: for each class with rules, a node
/// shape with the class as its target and one `sh:SPARQLRule` per rule. The queries get
/// the declarations of `prefixes` and of [`STANDARD_PREFIXES`] before their own text.
pub fn translate(graph: &Graph, prefixes: &[(String, String)]) -> Result<Graph> {
    translate_with_extra_types(graph, None, prefixes)
}

pub(crate) fn translate_with_extra_types(
    graph: &Graph,
    extra: Option<&ExtraTypes<'_>>,
    prefixes: &[(String, String)],
) -> Result<Graph> {
    let g = G { g: graph };
    let mut head = String::new();
    let mut declared = FxHashSet::default();
    for (p, ns) in prefixes
        .iter()
        .map(|(p, n)| (p.as_str(), n.as_str()))
        .chain(STANDARD_PREFIXES.iter().copied())
    {
        if declared.insert(p) {
            head.push_str(&format!("PREFIX {p}: <{ns}>\n"));
        }
    }
    let mut discovery_budget = rdf::WorkBudget::new(rdf::MAX_NODES);
    let mut pairs: Vec<(Term, Term)> = Vec::new();
    for p in rule_properties(graph, &mut discovery_budget)? {
        for t in graph.triples_for_predicate(&p) {
            discovery_budget.step()?;
            let s = rdf::check_and_own_subject(t.subject, &mut discovery_budget)?;
            let o = rdf::check_and_own_term(t.object, &mut discovery_budget)?;
            pairs.push((Term::from(s), o));
        }
    }
    pairs.sort_by_cached_key(|(c, r)| (c.to_string(), r.to_string()));
    pairs.dedup();
    let mut cache = template::HierarchyCache::default();
    let mut out = Graph::new();
    let mut n = 0usize;
    let mut fresh = || {
        n += 1;
        BlankNode::new_unchecked(format!("spinr{n}"))
    };
    let check = CandidateCheck {
        prefix_head: &head,
        require_construct: true,
    };
    for (class, rule) in pairs {
        let Term::NamedNode(class) = class else {
            bail!("the SPIN rule {rule} belongs to {class}, which is not an IRI");
        };
        let mut rule_budget = rdf::WorkBudget::new(rdf::MAX_NODES);
        let text = query_to_sparql(&g, extra, &rule, Some(&check), &mut rule_budget, &mut cache)
            .with_context(|| format!("translating SPIN rule {rule} of {class}"))?;
        let shape = fresh();
        let r = fresh();
        // a variable named this in the parsed query, not a word in a comment or a
        // longer variable name
        let full = format!("{head}{text}");
        let parsed = sparkles_core::sparql::parse_query(&full, None, &[])
            .map_err(|e| anyhow::anyhow!("{e}"))
            .map_err(|e| e.context(format!("the SPARQL query of the SPIN rule {rule}")))?;
        if !matches!(parsed, Query::Construct { .. }) {
            bail!("the SPIN rule {rule} of {class} must be a CONSTRUCT query");
        }
        let mentions_this = mentions_this(&parsed.to_string());
        let target = if mentions_this {
            sh::TARGET_CLASS
        } else {
            sh::TARGET_NODE
        };
        out.insert(&Triple::new(shape.clone(), rdf_vocab::TYPE, sh::NODE_SHAPE));
        out.insert(&Triple::new(shape.clone(), target, class.clone()));
        out.insert(&Triple::new(shape.clone(), sh::RULE, r.clone()));
        out.insert(&Triple::new(r.clone(), rdf_vocab::TYPE, sh::SPARQL_RULE));
        out.insert(&Triple::new(
            r,
            sh::CONSTRUCT,
            Literal::new_simple_literal(full),
        ));
    }
    Ok(out)
}

/// Whether the text of a parsed query, in which every variable is written `?name`, uses
/// the variable `this` outside its string literals and IRIs.
fn mentions_this(q: &str) -> bool {
    let b: Vec<char> = q.chars().collect();
    let word = |c: Option<&char>| c.is_some_and(|c| crate::sparql::varname_char(*c));
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < b.len() {
        match (quote, b[i]) {
            (Some(_), '\\') => i += 1,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, c @ ('"' | '\'')) => quote = Some(c),
            // an IRI, which a comparison's `<` never starts, since it is followed by a
            // space or `=`
            (None, '<')
                if b.get(i + 1)
                    .is_some_and(|n| !n.is_whitespace() && *n != '=') =>
            {
                quote = Some('>');
            }
            (None, '?') if b[i + 1..].starts_with(&['t', 'h', 'i', 's']) && !word(b.get(i + 5)) => {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{mentions_this, serialize_query, translate};
    use crate::shapes::Shapes;
    use crate::syntax::ShapesSyntax;
    use oxrdf::{BlankNode, Graph, Literal, NamedNode, Term, Triple};
    use sparkles_core::sparql::parse_query;

    const TTL_HEAD: &str = "
@prefix ex:   <http://example.org/> .
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .
@prefix sp:   <http://spinrdf.org/sp#> .
@prefix spin: <http://spinrdf.org/spin#> .
@prefix spl:  <http://spinrdf.org/spl#> .
";

    fn parse_ttl(body: &str) -> Graph {
        Shapes::read_graph(
            &format!("{TTL_HEAD}\n{body}"),
            ShapesSyntax::default(),
            None,
        )
        .unwrap()
    }

    fn ex(local: &str) -> Term {
        Term::NamedNode(NamedNode::new_unchecked(format!(
            "http://example.org/{local}"
        )))
    }

    #[test]
    fn only_the_variable_this_counts() {
        assert!(mentions_this(
            "CONSTRUCT { ?this ?p ?o } WHERE { ?this ?p ?o }"
        ));
        assert!(mentions_this("SELECT * WHERE { ?s ?p ?this . }"));
        assert!(!mentions_this(
            "CONSTRUCT { ?thisOne ?p ?o } WHERE { ?thisOne ?p ?o }"
        ));
        assert!(!mentions_this("SELECT * WHERE { ?this\u{B7} ?p ?o }"));
        assert!(!mentions_this("SELECT * WHERE { ?this\u{301} ?p ?o }"));
        assert!(!mentions_this("SELECT * WHERE { ?s ?p \"?this\" }"));
        assert!(!mentions_this("SELECT * WHERE { ?s <http://x/?this> ?o }"));
    }

    #[test]
    fn serialize_construct_with_expressions_filter_bind_optional_and_paths() {
        let g = parse_ttl(
            r#"
            ex:q a sp:Construct ;
              sp:templates (
                [ sp:subject spin:_this ; sp:predicate ex:area ; sp:object [ sp:varName "area" ] ]
                [ sp:subject spin:_this ; sp:predicate ex:tag ; sp:object _:tpl ]
                [ sp:subject _:tpl ; sp:predicate ex:label ; sp:object [ sp:varName "lbl" ] ]
              ) ;
              sp:where (
                [ sp:subject spin:_this ; sp:predicate ex:width ; sp:object [ sp:varName "w" ] ]
                [ a sp:TriplePath ;
                  sp:subject spin:_this ;
                  sp:path [ a sp:SeqPath ;
                            sp:path1 [ a sp:ReversePath ; sp:subPath ex:parent ] ;
                            sp:path2 [ a sp:ModPath ;
                                       sp:subPath [ a sp:AltPath ; sp:path1 ex:height ; sp:path2 ex:h ] ;
                                       sp:modMin 1 ; sp:modMax -2 ] ] ;
                  sp:object [ sp:varName "h" ] ]
                [ a sp:Bind ;
                  sp:expression [ a sp:mul ; sp:arg1 [ sp:varName "w" ] ; sp:arg2 [ sp:varName "h" ] ] ;
                  sp:variable [ sp:varName "area" ] ]
                [ a sp:Filter ;
                  sp:expression [ a sp:and ;
                                  sp:arg1 [ a sp:gt ; sp:arg1 [ sp:varName "area" ] ; sp:arg2 0 ] ;
                                  sp:arg2 [ a sp:not ; sp:arg1 [ a sp:eq ; sp:arg1 [ sp:varName "w" ] ; sp:arg2 0 ] ] ] ]
                [ a sp:Optional ;
                  sp:elements (
                    [ sp:subject spin:_this ; sp:predicate ex:name ; sp:object [ sp:varName "n" ] ]
                    [ a sp:Let ;
                      sp:expression [ a sp:ucase ; sp:arg1 [ a sp:concat ; sp:arg1 "R:" ; sp:arg2 [ a sp:str ; sp:arg1 [ sp:varName "n" ] ] ] ] ;
                      sp:variable [ sp:varName "lbl" ] ]
                  ) ]
              ) .
            "#,
        );
        let sparql = serialize_query(&g, &ex("q")).unwrap();
        let parsed = parse_query(&sparql, None, &[]).expect(&sparql);
        assert!(
            matches!(parsed, spargebra::Query::Construct { .. }),
            "{sparql}"
        );
        assert!(sparql.contains("?this"), "{sparql}");
        assert!(sparql.contains("BIND ("), "{sparql}");
        assert!(sparql.contains("OPTIONAL {"), "{sparql}");
        assert!(sparql.contains("UCASE(CONCAT("), "{sparql}");
    }

    #[test]
    fn serialize_select_modifiers_aggregates_union_subquery_values_minus_graph_service_exists() {
        let g = parse_ttl(
            r#"
            ex:q a sp:Select ;
              sp:distinct true ;
              sp:from ex:g1 ;
              sp:fromNamed ex:g2 ;
              sp:resultVariables (
                [ sp:varName "s" ]
                [ sp:varName "cnt" ; sp:expression [ a sp:Count ; sp:distinct true ; sp:expression [ sp:varName "o" ] ] ]
                [ sp:varName "all" ; sp:expression [ a sp:Count ] ]
                [ sp:varName "joined" ; sp:expression [ a sp:GroupConcat ; sp:expression [ sp:varName "o" ] ; sp:separator "," ] ]
              ) ;
              sp:where (
                [ a sp:Union ;
                  sp:elements (
                    ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:p ; sp:object [ sp:varName "o" ] ] )
                    ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:q ; sp:object [ sp:varName "o" ] ] )
                  ) ]
                [ a sp:SubQuery ;
                  sp:query [ a sp:Select ;
                             sp:reduced true ;
                             sp:where ( [ sp:subject [ sp:varName "s" ] ; sp:predicate rdf:type ; sp:object ex:Item ] ) ] ]
                [ a sp:Values ;
                  sp:varNames ( "k" "v" ) ;
                  sp:bindings ( ( 1 "a" ) ( 2 sp:undef ) ) ]
                [ a sp:Minus ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:skip ; sp:object true ] ) ]
                [ a sp:NamedGraph ;
                  sp:graphNameNode ex:g2 ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:seen ; sp:object [ sp:varName "k" ] ] ) ]
                [ a sp:Service ;
                  sp:serviceURI ex:remote ;
                  sp:silent true ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:ext ; sp:object [ sp:varName "e" ] ] ) ]
                [ a sp:NotExists ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:banned ; sp:object true ] ) ]
                [ a sp:Exists ;
                  sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:active ; sp:object true ] ) ]
                [ a sp:Filter ;
                  sp:expression [ a sp:in ; sp:arg1 [ sp:varName "k" ] ; sp:arg2 1 ; sp:arg3 2 ] ]
              ) ;
              sp:groupBy ( [ sp:varName "s" ] ) ;
              sp:having ( [ a sp:ge ; sp:arg1 [ a sp:Count ] ; sp:arg2 1 ] ) ;
              sp:orderBy ( [ a sp:Desc ; sp:expression [ sp:varName "cnt" ] ] [ a sp:Asc ; sp:expression [ sp:varName "s" ] ] ) ;
              sp:limit 10 ;
              sp:offset 5 .
            "#,
        );
        let sparql = serialize_query(&g, &ex("q")).unwrap();
        let parsed = parse_query(&sparql, None, &[]).expect(&sparql);
        assert!(
            matches!(parsed, spargebra::Query::Select { .. }),
            "{sparql}"
        );
        assert!(sparql.contains("SELECT DISTINCT"), "{sparql}");
        assert!(sparql.contains("COUNT(DISTINCT ?o)"), "{sparql}");
        assert!(sparql.contains("COUNT(*)"), "{sparql}");
        assert!(
            sparql.contains("GROUP_CONCAT(?o ; SEPARATOR = \",\")"),
            "{sparql}"
        );
        assert!(sparql.contains("UNION"), "{sparql}");
        assert!(sparql.contains("VALUES (?k ?v)"), "{sparql}");
        assert!(sparql.contains("UNDEF"), "{sparql}");
        assert!(sparql.contains("MINUS"), "{sparql}");
        assert!(sparql.contains("GRAPH <http://example.org/g2>"), "{sparql}");
        assert!(
            sparql.contains("SERVICE SILENT <http://example.org/remote>"),
            "{sparql}"
        );
        assert!(sparql.contains("FILTER NOT EXISTS"), "{sparql}");
        assert!(sparql.contains("FILTER EXISTS"), "{sparql}");
        assert!(sparql.contains("GROUP BY ?s"), "{sparql}");
        assert!(sparql.contains("HAVING"), "{sparql}");
        assert!(sparql.contains("ORDER BY DESC(?cnt) ASC(?s)"), "{sparql}");
        assert!(sparql.contains("LIMIT 10 OFFSET 5"), "{sparql}");
    }

    #[test]
    fn serialize_ask_describe_and_builtins() {
        let g = parse_ttl(
            r#"
            ex:ask a sp:Ask ;
              sp:where (
                [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:v ; sp:object [ sp:varName "v" ] ]
                [ a sp:Filter ;
                  sp:expression [ a sp:and ;
                                  sp:arg1 [ a sp:regex ; sp:arg1 [ a sp:str ; sp:arg1 [ sp:varName "v" ] ] ; sp:arg2 "^a" ; sp:arg3 "i" ] ;
                                  sp:arg2 [ a sp:notIn ; sp:arg1 [ sp:varName "v" ] ; sp:arg2 "bad" ] ] ]
                [ a sp:Filter ;
                  sp:expression [ a sp:Exists ;
                                  sp:elements ( [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:ok ; sp:object true ] ) ] ]
                [ a sp:Bind ;
                  sp:expression [ a sp:if ;
                                  sp:arg1 [ a sp:isNumeric ; sp:arg1 [ sp:varName "v" ] ] ;
                                  sp:arg2 [ a sp:abs ; sp:arg1 [ a sp:unaryMinus ; sp:arg1 [ sp:varName "v" ] ] ] ;
                                  sp:arg3 [ a sp:coalesce ; sp:arg1 [ a sp:now ] ; sp:arg2 [ a sp:rand ] ] ] ;
                  sp:variable [ sp:varName "z" ] ]
              ) .
            ex:desc1 a sp:Describe ;
              sp:resultNodes ( ex:Alice [ sp:varName "b" ] ) ;
              sp:where ( [ sp:subject ex:Alice ; sp:predicate ex:knows ; sp:object [ sp:varName "b" ] ] ) .
            ex:desc2 a sp:Describe ;
              sp:resultNodes ( ex:Alice ) .
            "#,
        );
        let ask = serialize_query(&g, &ex("ask")).unwrap();
        assert!(matches!(
            parse_query(&ask, None, &[]).expect(&ask),
            spargebra::Query::Ask { .. }
        ));
        let d1 = serialize_query(&g, &ex("desc1")).unwrap();
        assert!(matches!(
            parse_query(&d1, None, &[]).expect(&d1),
            spargebra::Query::Describe { .. }
        ));
        let d2 = serialize_query(&g, &ex("desc2")).unwrap();
        assert!(matches!(
            parse_query(&d2, None, &[]).expect(&d2),
            spargebra::Query::Describe { .. }
        ));
    }

    #[test]
    fn template_expansion_handles_text_rdf_defaults_optional_and_inheritance() {
        let g = parse_ttl(
            r#"
            ex:BaseTmpl a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:targetClass ; spl:valueType rdfs:Class ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:minVal ; spl:valueType xsd:integer ; spl:defaultValue 10 ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:optTag ; spl:optional true ; spl:valueType xsd:string ] ;
              spin:body [
                a sp:Construct ;
                sp:text """
                  # ?targetClass in comment stays untouched
                  CONSTRUCT { ?this a ?targetClass ; <http://example.org/tag> ?label }
                  WHERE {
                    ?this <http://example.org/score> ?s .
                    FILTER (?s >= ?minVal && "?minVal" != "")
                    BIND (IF(BOUND(?optTag), ?optTag, "none") AS ?label)
                  }
                """
              ] .
            ex:SubTmpl a spin:ConstructTemplate ;
              rdfs:subClassOf ex:BaseTmpl ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:minVal ; spl:defaultValue 50 ] .

            ex:HighScorer a rdfs:Class .
            ex:call1 a ex:SubTmpl ; ex:targetClass ex:HighScorer ; ex:optTag "gold" .
            ex:call2 a ex:SubTmpl ; ex:targetClass ex:HighScorer .
            "#,
        );
        let q1 = serialize_query(&g, &ex("call1")).unwrap();
        assert!(
            matches!(
                parse_query(&q1, None, &[]).expect(&q1),
                spargebra::Query::Construct { .. }
            ),
            "{q1}"
        );
        assert!(q1.contains("<http://example.org/HighScorer>"), "{q1}");
        assert!(
            q1.contains("\"50\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            "{q1}"
        );
        assert!(q1.contains("IF((true), \"gold\", \"none\")"), "{q1}");
        assert!(q1.contains("\"?minVal\""), "string literal untouched: {q1}");

        let q2 = serialize_query(&g, &ex("call2")).unwrap();
        assert!(
            matches!(
                parse_query(&q2, None, &[]).expect(&q2),
                spargebra::Query::Construct { .. }
            ),
            "{q2}"
        );
        assert!(q2.contains("IF(BOUND(?optTag), ?optTag, \"none\")"), "{q2}");
    }

    #[test]
    fn template_rdf_body_and_diamond_inheritance_succeed() {
        let g = parse_ttl(
            r#"
            ex:Root a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:pred ] ;
              spin:body [
                a sp:Construct ;
                sp:templates ( [ sp:subject spin:_this ; sp:predicate [ sp:varName "pred" ] ; sp:object [ sp:varName "val" ] ] ) ;
                sp:where (
                  [ sp:subject spin:_this ; sp:predicate ex:src ; sp:object [ sp:varName "x" ] ]
                  [ a sp:Filter ; sp:expression [ a sp:bound ; sp:arg1 [ sp:varName "val" ] ] ]
                )
              ] .
            ex:Left rdfs:subClassOf ex:Root ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:val ; spl:defaultValue 1 ] .
            ex:Right rdfs:subClassOf ex:Root .
            ex:Leaf rdfs:subClassOf ex:Left, ex:Right .
            ex:call a ex:Leaf ; ex:pred ex:out .
            "#,
        );
        let q = serialize_query(&g, &ex("call")).unwrap();
        assert!(
            matches!(
                parse_query(&q, None, &[]).expect(&q),
                spargebra::Query::Construct { .. }
            ),
            "{q}"
        );
        assert!(q.contains("<http://example.org/out>"), "{q}");
        assert!(q.contains("FILTER (true)"), "{q}");
    }

    #[test]
    fn refuses_cycles_over_depth_missing_args_and_malformed_inputs() {
        // 1. Cyclic RDF list
        let mut g = Graph::new();
        let q = NamedNode::new_unchecked("http://example.org/q");
        let c1 = BlankNode::new_unchecked("c1");
        g.insert(&Triple::new(
            q.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::ASK,
        ));
        g.insert(&Triple::new(q.clone(), super::vocab::sp::WHERE, c1.clone()));
        g.insert(&Triple::new(
            c1.clone(),
            crate::vocab::rdf::FIRST,
            Literal::from(1),
        ));
        g.insert(&Triple::new(c1.clone(), crate::vocab::rdf::REST, c1));
        let err = serialize_query(&g, &Term::NamedNode(q)).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic RDF list"), "{err:#}");

        // 2. Cyclic expression
        let g = parse_ttl(
            r#"
            ex:q a sp:Ask ; sp:where ( [ a sp:Filter ; sp:expression ex:e1 ] ) .
            ex:e1 a sp:not ; sp:arg1 ex:e2 .
            ex:e2 a sp:not ; sp:arg1 ex:e1 .
            "#,
        );
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");

        // 3. Cyclic property path
        let g = parse_ttl(
            r#"
            ex:q a sp:Ask ;
              sp:where ( [ a sp:TriplePath ; sp:subject ex:a ; sp:path ex:p1 ; sp:object ex:b ] ) .
            ex:p1 a sp:ReversePath ; sp:subPath ex:p1 .
            "#,
        );
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");

        // 4. Over-depth expression (> 64)
        let mut ttl =
            String::from("ex:q a sp:Ask ; sp:where ( [ a sp:Filter ; sp:expression ex:e0 ] ) .\n");
        for i in 0..70 {
            ttl.push_str(&format!("ex:e{i} a sp:not ; sp:arg1 ex:e{} .\n", i + 1));
        }
        ttl.push_str("ex:e70 sp:varName \"x\" .\n");
        let g = parse_ttl(&ttl);
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum nesting depth"),
            "{err:#}"
        );

        // 5. Exponential DAG node-budget explosion (binary tree of depth 20 -> 2^20 nodes)
        let mut ttl =
            String::from("ex:q a sp:Ask ; sp:where ( [ a sp:Filter ; sp:expression ex:d0 ] ) .\n");
        for i in 0..20 {
            ttl.push_str(&format!(
                "ex:d{i} a sp:and ; sp:arg1 ex:d{} ; sp:arg2 ex:d{} .\n",
                i + 1,
                i + 1
            ));
        }
        ttl.push_str("ex:d20 sp:varName \"x\" .\n");
        let g = parse_ttl(&ttl);
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("node budget"), "{err:#}");

        // 6. Cyclic template rdfs:subClassOf
        let g = parse_ttl(
            r#"
            ex:T1 a spin:ConstructTemplate ; rdfs:subClassOf ex:T2 ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this a <http://example.org/A> } WHERE { }" ] .
            ex:T2 rdfs:subClassOf ex:T1 .
            ex:c a ex:T1 .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("cyclic rdfs:subClassOf"),
            "{err:#}"
        );

        // 7. Cyclic nested template body
        let g = parse_ttl(
            r#"
            ex:T1 a spin:ConstructTemplate ; spin:body [ a ex:T2 ] .
            ex:T2 a spin:ConstructTemplate ; spin:body [ a ex:T1 ] .
            ex:c a ex:T1 .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("cyclic SPIN template"),
            "{err:#}"
        );

        // 8. Missing required argument
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:req ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this a ?req } WHERE { }" ] .
            ex:c a ex:T .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing required argument"),
            "{err:#}"
        );

        // 9. Wrong spl:valueType
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:n ; spl:valueType xsd:integer ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/n> ?n } WHERE { }" ] .
            ex:c a ex:T ; ex:n "not-an-int" .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(format!("{err:#}").contains("datatype"), "{err:#}");

        // 10. Invalid sp:varName (SPARQL injection attempt)
        let g = parse_ttl(
            r#"
            ex:q a sp:Ask ;
              sp:where ( [ sp:subject [ sp:varName "x } UNION { ?a ?b ?c" ] ; sp:predicate ex:p ; sp:object 1 ] ) .
            "#,
        );
        let err = serialize_query(&g, &ex("q")).unwrap_err();
        assert!(format!("{err:#}").contains("invalid"), "{err:#}");

        // 11. Non-CONSTRUCT spin:rule refused by translate
        let g = parse_ttl(
            r#"
            ex:Person spin:rule [ a sp:Ask ; sp:where ( [ sp:subject spin:_this ; sp:predicate ex:p ; sp:object 1 ] ) ] .
            "#,
        );
        let err = translate(&g, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("CONSTRUCT"), "{err:#}");

        // 12. Assigning to bound template argument with AS refused
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:x ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/p> ?x } WHERE { BIND(1 AS ?x) }" ] .
            ex:c a ex:T ; ex:x 2 .
            "#,
        );
        let err = serialize_query(&g, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("assigns to bound argument"),
            "{err:#}"
        );
    }

    #[test]
    fn bounds_pre_ast_work_unrelated_vars_oversized_literals_and_nested_triples() {
        // 1. Unrelated graph-wide sp:varName triples do not exhaust a small query's budget
        let mut g = parse_ttl(
            r#"
            ex:q a sp:Ask ;
              sp:where ( [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:p ; sp:object 1 ] ) .
            "#,
        );
        for i in 0..20_000 {
            let b = BlankNode::new_unchecked(format!("unrel{i}"));
            g.insert(&Triple::new(
                b,
                super::vocab::sp::VAR_NAME,
                Literal::new_simple_literal(format!("v{i}")),
            ));
        }
        let sparql = serialize_query(&g, &ex("q")).unwrap();
        assert!(sparql.contains("?x"), "{sparql}");

        // 2. Wide template hierarchy exceeds shared work budget cleanly
        let mut g = Graph::new();
        let root = NamedNode::new_unchecked("http://example.org/WideRoot");
        let call = NamedNode::new_unchecked("http://example.org/wideCall");
        g.insert(&Triple::new(
            call.clone(),
            crate::vocab::rdf::TYPE,
            root.clone(),
        ));
        for i in 0..17_000 {
            let sup = NamedNode::new_unchecked(format!("http://example.org/Sup{i}"));
            g.insert(&Triple::new(
                root.clone(),
                crate::vocab::rdfs::SUB_CLASS_OF,
                sup,
            ));
        }
        let err = serialize_query(&g, &Term::NamedNode(call)).unwrap_err();
        assert!(format!("{err:#}").contains("node budget"), "{err:#}");

        // 3. Many unrelated template types on one call refuse cleanly without O(k^2) blowup
        let mut g = Graph::new();
        let call = NamedNode::new_unchecked("http://example.org/multiCall");
        for i in 0..120 {
            let t = NamedNode::new_unchecked(format!("http://example.org/T{i}"));
            g.insert(&Triple::new(
                t.clone(),
                crate::vocab::rdf::TYPE,
                super::vocab::spin::CONSTRUCT_TEMPLATE,
            ));
            g.insert(&Triple::new(call.clone(), crate::vocab::rdf::TYPE, t));
        }
        let err = serialize_query(&g, &Term::NamedNode(call)).unwrap_err();
        assert!(
            format!("{err:#}").contains("multiple unrelated template types"),
            "{err:#}"
        );

        // 4. Oversized literal rejected before allocation
        let mut g = Graph::new();
        let q = NamedNode::new_unchecked("http://example.org/bigLitQ");
        let cell = BlankNode::new_unchecked("c1");
        let tp = BlankNode::new_unchecked("tp1");
        let huge = "a".repeat(super::rdf::MAX_OUTPUT_BYTES + 16);
        g.insert(&Triple::new(
            q.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::ASK,
        ));
        g.insert(&Triple::new(
            q.clone(),
            super::vocab::sp::WHERE,
            cell.clone(),
        ));
        g.insert(&Triple::new(
            cell.clone(),
            crate::vocab::rdf::FIRST,
            tp.clone(),
        ));
        g.insert(&Triple::new(
            cell,
            crate::vocab::rdf::REST,
            crate::vocab::rdf::NIL,
        ));
        g.insert(&Triple::new(
            tp.clone(),
            super::vocab::sp::SUBJECT,
            NamedNode::new_unchecked("http://example.org/s"),
        ));
        g.insert(&Triple::new(
            tp.clone(),
            super::vocab::sp::PREDICATE,
            NamedNode::new_unchecked("http://example.org/p"),
        ));
        g.insert(&Triple::new(
            tp,
            super::vocab::sp::OBJECT,
            Literal::new_simple_literal(huge),
        ));
        let err = serialize_query(&g, &Term::NamedNode(q)).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum output size"),
            "{err:#}"
        );

        // 5. Deeply nested Term::Triple rejected under MAX_DEPTH
        let mut nested = Term::NamedNode(NamedNode::new_unchecked("http://example.org/leaf"));
        let s = NamedNode::new_unchecked("http://example.org/s");
        let p = NamedNode::new_unchecked("http://example.org/p");
        for _ in 0..70 {
            nested = Term::Triple(Box::new(Triple::new(s.clone(), p.clone(), nested)));
        }
        let mut out = String::new();
        let err = super::rdf::write_term_value(&mut out, &nested, 0).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum nesting depth"),
            "{err:#}"
        );

        // 6. Over-long RDF list (> MAX_LIST_LEN) rejected
        let mut g = Graph::new();
        let q = NamedNode::new_unchecked("http://example.org/longListQ");
        g.insert(&Triple::new(
            q.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::ASK,
        ));
        let mut head = Term::NamedNode(crate::vocab::rdf::NIL.into_owned());
        for i in 0..(super::rdf::MAX_LIST_LEN + 2) {
            let c = BlankNode::new_unchecked(format!("ll{i}"));
            g.insert(&Triple::new(
                c.clone(),
                crate::vocab::rdf::FIRST,
                Literal::from(1),
            ));
            g.insert(&Triple::new(c.clone(), crate::vocab::rdf::REST, head));
            head = Term::BlankNode(c);
        }
        g.insert(&Triple::new(q.clone(), super::vocab::sp::WHERE, head));
        let err = serialize_query(&g, &Term::NamedNode(q)).unwrap_err();
        assert!(format!("{err:#}").contains("maximum length"), "{err:#}");
    }

    #[test]
    fn substitute_sparql_text_handles_prefixed_names_lt_spacing_long_strings_and_clauses() {
        let g = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:target ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:limit ] ;
              spin:body [
                a sp:Construct ;
                sp:text """
                  PREFIX ex: <http://example.org/>
                  CONSTRUCT { ?this ex:same-as ?target ; ex:v1.as $target ; ex:quote ?q }
                  WHERE {
                    { SELECT ?target WHERE { ?target ex:known-as ?o } GROUP BY ?target ORDER BY ?target }
                    ?this ex:val ?x .
                    FILTER (?x<?limit)
                    BIND (ex:upper-bound(?limit) AS ?u)
                    BIND (IF(BOUND($limit), \"\"\"say "hi" and ""ok""!\"\"\" , "") AS ?q)
                    FILTER (?target != <http://example.org/none>)
                  }
                """
              ] .
            ex:call a ex:T ; ex:target ex:Alice ; ex:limit ex:MaxVal .
            "#,
        );
        let q = serialize_query(&g, &ex("call")).unwrap();
        let parsed = parse_query(&q, None, &[]).expect(&q);
        assert!(matches!(parsed, spargebra::Query::Construct { .. }), "{q}");
        assert!(q.contains("ex:same-as <http://example.org/Alice>"), "{q}");
        assert!(q.contains("ex:v1.as <http://example.org/Alice>"), "{q}");
        assert!(
            q.contains("SELECT (<http://example.org/Alice> AS ?target)"),
            "{q}"
        );
        assert!(q.contains("GROUP BY (<http://example.org/Alice>)"), "{q}");
        assert!(q.contains("ORDER BY (<http://example.org/Alice>)"), "{q}");
        assert!(q.contains("?x< <http://example.org/MaxVal>"), "{q}");
        assert!(
            q.contains("ex:upper-bound(<http://example.org/MaxVal>)"),
            "{q}"
        );
        assert!(q.contains("IF((true),"), "{q}");
        assert!(
            q.contains("<http://example.org/Alice> != <http://example.org/none>"),
            "{q}"
        );

        // Parser-valid long string literals (escaped trailing content quote, and adjacent
        // quoted tokens) parse cleanly and never swallow or mis-substitute `?arg`.
        let mut bindings = super::FxHashMap::default();
        bindings.insert("arg".to_string(), Some(ex("Bob")));
        let mut b1 = super::rdf::WorkBudget::new(super::rdf::MAX_NODES);
        let out_esc = super::template::substitute_sparql_text(
            r#"SELECT * WHERE { ?s <http://example.org/p> """say "hi\"""" . ?s <http://example.org/q> ?arg }"#,
            &bindings,
            &mut b1,
        )
        .unwrap();
        assert!(parse_query(&out_esc, None, &[]).is_ok(), "{out_esc}");
        assert!(
            out_esc.contains("<http://example.org/q> <http://example.org/Bob>"),
            "{out_esc}"
        );
        let mut b2 = super::rdf::WorkBudget::new(super::rdf::MAX_NODES);
        let out_adj = super::template::substitute_sparql_text(
            r#"SELECT * WHERE { ?s <http://example.org/p> ("""hello""""?arg") . ?s <http://example.org/q> ?arg }"#,
            &bindings,
            &mut b2,
        )
        .unwrap();
        assert!(parse_query(&out_adj, None, &[]).is_ok(), "{out_adj}");
        assert!(
            out_adj.contains(r#""""hello""""?arg""#),
            "adjacent quoted string must stay untouched: {out_adj}"
        );
        assert!(
            out_adj.contains("<http://example.org/q> <http://example.org/Bob>"),
            "{out_adj}"
        );

        // VALUES with bound argument is refused
        let bad_values = parse_ttl(
            r#"
            ex:Tv a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:x ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/p> ?x } WHERE { VALUES (?x ?y) { (1 2) } }" ] .
            ex:cv a ex:Tv ; ex:x 1 .
            "#,
        );
        let err = serialize_query(&bad_values, &ex("cv")).unwrap_err();
        assert!(format!("{err:#}").contains("VALUES"), "{err:#}");
    }

    #[test]
    fn nested_template_forwarding_and_unbound_spin_variables() {
        // 1. Omitted optional outer argument falls back to inner spl:defaultValue
        let g = parse_ttl(
            r#"
            ex:Inner a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:x ; spl:defaultValue 42 ] ;
              spin:body [
                a sp:Construct ;
                sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:val ; sp:object [ sp:varName "x" ] ] ) ;
                sp:where ( [ sp:subject spin:_this ; sp:predicate ex:active ; sp:object true ] )
              ] .
            ex:Outer a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:opt ; spl:optional true ] ;
              spin:body [ a ex:Inner ; ex:x [ sp:varName "opt" ] ] .
            ex:callDefault a ex:Outer .
            ex:callPassed a ex:Outer ; ex:opt 7 .
            "#,
        );
        let q_def = serialize_query(&g, &ex("callDefault")).unwrap();
        assert!(q_def.contains("\"42\""), "{q_def}");
        let q_pass = serialize_query(&g, &ex("callPassed")).unwrap();
        assert!(q_pass.contains("\"7\""), "{q_pass}");

        // 2. Omitted optional outer argument forwarded to required inner argument fails
        let g_req = parse_ttl(
            r#"
            ex:InnerReq a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:req ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/v> ?req } WHERE {}" ] .
            ex:OuterOpt a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:opt ; spl:optional true ] ;
              spin:body [ a ex:InnerReq ; ex:req [ sp:varName "opt" ] ] .
            ex:c a ex:OuterOpt .
            "#,
        );
        let err = serialize_query(&g_req, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing required argument"),
            "{err:#}"
        );

        // 3. Unbound spin:_this, spin:_arg1, and named sp:Variable are rejected instead of
        //    being bound as constant IRIs
        for arg_expr in ["spin:_this", "spin:_arg1", "ex:namedVar"] {
            let g_var = parse_ttl(&format!(
                r#"
                ex:namedVar a sp:Variable ; sp:varName "foo" .
                ex:T a spin:ConstructTemplate ;
                  spin:constraint [ a spl:Argument ; spl:predicate ex:arg ] ;
                  spin:body [ a sp:Construct ; sp:text "CONSTRUCT {{ ?this <http://example.org/p> ?arg }} WHERE {{}}" ] .
                ex:c a ex:T ; ex:arg {arg_expr} .
                "#
            ));
            let err = serialize_query(&g_var, &ex("c")).unwrap_err();
            assert!(
                format!("{err:#}").contains("unbound SPIN variable"),
                "{arg_expr}: {err:#}"
            );
        }
    }

    #[test]
    fn template_reserves_this_enforces_value_types_and_rejects_arg_conflicts() {
        // 1. Reserving ?this
        let g_this = parse_ttl(
            r#"
            ex:BadThis a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:this ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/flag> true } WHERE {}" ] .
            ex:c a ex:BadThis ; ex:this ex:other .
            "#,
        );
        let err = serialize_query(&g_this, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("reserved variable name ?this"),
            "{err:#}"
        );

        // 2. Class-valued spl:valueType (subclass passes, unrelated class and untyped fail)
        let g_cls = parse_ttl(
            r#"
            ex:Student rdfs:subClassOf ex:Person .
            ex:alice a ex:Student .
            ex:car a ex:Vehicle .
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:who ; spl:valueType ex:Person ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/owner> ?who } WHERE {}" ] .
            ex:ok a ex:T ; ex:who ex:alice .
            ex:bad1 a ex:T ; ex:who ex:car .
            ex:bad2 a ex:T ; ex:who ex:untyped .
            "#,
        );
        assert!(serialize_query(&g_cls, &ex("ok")).is_ok());
        assert!(serialize_query(&g_cls, &ex("bad1")).is_err());
        assert!(serialize_query(&g_cls, &ex("bad2")).is_err());

        // 3. Custom rdfs:Datatype
        let g_dt = parse_ttl(
            r#"
            ex:Code a rdfs:Datatype .
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:code ; spl:valueType ex:Code ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/c> ?code } WHERE {}" ] .
            ex:ok a ex:T ; ex:code "A1"^^ex:Code .
            ex:badLit a ex:T ; ex:code "A1" .
            ex:badIri a ex:T ; ex:code ex:notLit .
            "#,
        );
        assert!(serialize_query(&g_dt, &ex("ok")).is_ok());
        assert!(serialize_query(&g_dt, &ex("badLit")).is_err());
        assert!(serialize_query(&g_dt, &ex("badIri")).is_err());

        // 4. Conflicting spl:Argument in diamond siblings refused; subclass override resolves
        let g_diamond = parse_ttl(
            r#"
            ex:Root a spin:ConstructTemplate ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/v> ?v } WHERE {}" ] .
            ex:Left rdfs:subClassOf ex:Root ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:v ; spl:defaultValue 1 ] .
            ex:Right rdfs:subClassOf ex:Root ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:v ; spl:defaultValue 2 ] .
            ex:ConflictLeaf rdfs:subClassOf ex:Left, ex:Right .
            ex:ResolvedLeaf rdfs:subClassOf ex:Left, ex:Right ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:v ; spl:defaultValue 3 ] .
            ex:cBad a ex:ConflictLeaf .
            ex:cOk a ex:ResolvedLeaf .
            "#,
        );
        let err = serialize_query(&g_diamond, &ex("cBad")).unwrap_err();
        assert!(
            format!("{err:#}").contains("conflicting spl:defaultValue"),
            "{err:#}"
        );
        let q_ok = serialize_query(&g_diamond, &ex("cOk")).unwrap();
        assert!(q_ok.contains("\"3\""), "{q_ok}");

        // 5. Duplicate conflicting spl:Argument on one class refused
        let g_dup = parse_ttl(
            r#"
            ex:Dup a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:v ; spl:optional true ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:v ; spl:optional false ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/v> ?v } WHERE {}" ] .
            ex:c a ex:Dup ; ex:v 1 .
            "#,
        );
        let err = serialize_query(&g_dup, &ex("c")).unwrap_err();
        assert!(
            format!("{err:#}").contains("conflicting spl:optional"),
            "{err:#}"
        );
    }

    #[test]
    fn upstream_encodings_rdf_edge_cases_strict_booleans_and_anon_var_collisions() {
        // 1. Upstream ARQ2SPIN encodings: ModPath (0, -1), lowercase sp:exists / sp:notExists,
        //    direct sp:Count with sp:as, fixed ModPath unrolling, ReverseLinkPath, sp:Values
        //    with sp:variables, spin:_arg1, custom XSD cast, anonymous sp:Variable predicate,
        //    and sp:Ask with trailing sp:values.
        let g = parse_ttl(
            r#"
            ex:q a sp:Select ;
              sp:distinct "1"^^xsd:boolean ;
              sp:resultVariables (
                [ sp:varName "s" ]
                [ a sp:Count ; sp:as [ sp:varName "cnt" ] ; sp:expression [ sp:varName "o" ] ]
                [ a sp:Count ; sp:as [ sp:varName "all" ] ]
                [ sp:varName "joined" ; sp:expression [ a sp:GroupConcat ; sp:expression [ sp:varName "o" ] ; sp:separator "," ] ]
              ) ;
              sp:where (
                [ a sp:TriplePath ;
                  sp:subject [ sp:varName "s" ] ;
                  sp:path [ a sp:ModPath ; sp:subPath ex:opt ; sp:modMin 0 ; sp:modMax -1 ] ;
                  sp:object [ sp:varName "o" ] ]
                [ a sp:TriplePath ;
                  sp:subject [ sp:varName "s" ] ;
                  sp:path [ a sp:ModPath ; sp:subPath ex:step ; sp:modMin 2 ; sp:modMax 3 ] ;
                  sp:object [ sp:varName "o2" ] ]
                [ a sp:TriplePath ;
                  sp:subject [ sp:varName "s" ] ;
                  sp:path [ a sp:ModPath ; sp:subPath ex:hop ; sp:modMin 2 ; sp:modMax -2 ] ;
                  sp:object [ sp:varName "o3" ] ]
                [ a sp:TriplePath ;
                  sp:subject [ sp:varName "s" ] ;
                  sp:path [ a sp:ReverseLinkPath ; sp:node ex:parent ] ;
                  sp:object [ sp:varName "p1" ] ]
                [ a sp:TriplePath ;
                  sp:subject [ sp:varName "s" ] ;
                  sp:path [ a sp:ReverseLinkPath ; sp:node [ a sp:ReversePath ; sp:subPath ex:parent ] ] ;
                  sp:object [ sp:varName "p2" ] ]
                [ sp:subject [ sp:varName "s" ] ; sp:predicate [ a sp:Variable ] ; sp:object spin:_arg1 ]
                [ a sp:Filter ;
                  sp:expression [ a sp:and ;
                                  sp:arg1 [ a sp:exists ; sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:ok ; sp:object true ] ) ] ;
                                  sp:arg2 [ a sp:notExists ; sp:elements ( [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:no ; sp:object true ] ) ] ] ]
                [ a sp:Bind ;
                  sp:expression [ a xsd:integer ; sp:arg1 [ sp:varName "o" ] ] ;
                  sp:variable [ sp:varName "num" ] ]
                [ a sp:Values ;
                  sp:variables ( [ sp:varName "k" ] ) ;
                  sp:bindings ( ( 1 ) ( 2 ) ) ]
              ) ;
              sp:groupBy ( [ sp:varName "s" ] ) .

            ex:askVal a sp:Ask ;
              sp:where ( [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:p ; sp:object [ sp:varName "v" ] ] ) ;
              sp:values [ a sp:Values ; sp:varNames ( "v" ) ; sp:bindings ( ( 1 ) ) ] .
            "#,
        );
        let q = serialize_query(&g, &ex("q")).unwrap();
        assert!(
            matches!(
                parse_query(&q, None, &[]).expect(&q),
                spargebra::Query::Select { .. }
            ),
            "{q}"
        );
        assert!(q.contains("SELECT DISTINCT"), "{q}");
        assert!(q.contains("(COUNT(?o) AS ?cnt)"), "{q}");
        assert!(q.contains("(COUNT(*) AS ?all)"), "{q}");
        assert!(q.contains("(<http://example.org/opt>)?"), "{q}");
        assert!(q.contains("^<http://example.org/parent>"), "{q}");
        assert!(q.contains("^(^(<http://example.org/parent>))"), "{q}");
        assert!(q.contains("?arg1"), "{q}");
        assert!(
            q.contains("<http://www.w3.org/2001/XMLSchema#integer>(?o)"),
            "{q}"
        );

        let ask = serialize_query(&g, &ex("askVal")).unwrap();
        assert!(
            matches!(
                parse_query(&ask, None, &[]).expect(&ask),
                spargebra::Query::Ask { .. }
            ),
            "{ask}"
        );
        assert!(ask.contains("VALUES (?v)"), "{ask}");

        // 2. Invalid xsd:boolean refused; "1"^^xsd:boolean accepted for spl:optional
        let g_bool = parse_ttl(
            r#"
            ex:badFlag a sp:Select ; sp:distinct "notabool"^^xsd:boolean ; sp:where () .
            ex:TOpt a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:opt ; spl:optional "1"^^xsd:boolean ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/p> ?opt } WHERE {}" ] .
            ex:cOpt a ex:TOpt .
            ex:TBadOpt a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:opt ; spl:optional "yes"^^xsd:boolean ] ;
              spin:body [ a sp:Construct ; sp:text "CONSTRUCT { ?this <http://example.org/p> ?opt } WHERE {}" ] .
            ex:cBadOpt a ex:TBadOpt .
            "#,
        );
        assert!(serialize_query(&g_bool, &ex("badFlag")).is_err());
        assert!(serialize_query(&g_bool, &ex("cOpt")).is_ok());
        assert!(serialize_query(&g_bool, &ex("cBadOpt")).is_err());

        // 3. Invalid GroupConcat separator, invalid SubQuery (Ask / FROM), and >16 ModPath
        let g_bad = parse_ttl(
            r#"
            ex:badSep1 a sp:Select ;
              sp:resultVariables ( [ sp:varName "j" ; sp:expression [ a sp:GroupConcat ; sp:expression [ sp:varName "o" ] ; sp:separator 123 ] ] ) ;
              sp:where () .
            ex:badSep2 a sp:Select ;
              sp:resultVariables ( [ sp:varName "j" ; sp:expression [ a sp:GroupConcat ; sp:expression [ sp:varName "o" ] ; sp:separator ","@en ] ] ) ;
              sp:where () .
            ex:badSubAsk a sp:Select ;
              sp:where ( [ a sp:SubQuery ; sp:query [ a sp:Ask ; sp:where () ] ] ) .
            ex:badSubFrom a sp:Select ;
              sp:where ( [ a sp:SubQuery ; sp:query [ a sp:Select ; sp:from ex:g1 ; sp:where () ] ] ) .
            ex:badMod a sp:Ask ;
              sp:where ( [ a sp:TriplePath ; sp:subject ex:a ; sp:path [ a sp:ModPath ; sp:subPath ex:p ; sp:modMin 1 ; sp:modMax 17 ] ; sp:object ex:b ] ) .
            "#,
        );
        for name in ["badSep1", "badSep2", "badSubAsk", "badSubFrom", "badMod"] {
            assert!(
                serialize_query(&g_bad, &ex(name)).is_err(),
                "expected {name} to fail"
            );
        }

        // 4. Anonymous WHERE variable avoids collisions with sp:SubQuery sp:text and sp:Values
        let g_col = parse_ttl(
            r#"
            ex:qCol a sp:Select ;
              sp:where (
                [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:p ; sp:object [] ]
                [ a sp:SubQuery ; sp:query [ a sp:Select ; sp:text "SELECT ?_spin_v1 WHERE { ?_spin_v1 <http://example.org/q> 1 }" ] ]
                [ a sp:Values ; sp:varNames ( "_spin_v2" ) ; sp:bindings ( ( 2 ) ) ]
              ) .
            "#,
        );
        let q_col = serialize_query(&g_col, &ex("qCol")).unwrap();
        assert!(parse_query(&q_col, None, &[]).is_ok(), "{q_col}");
        assert!(
            q_col.contains("?s <http://example.org/p> ?_spin_v3 ."),
            "anonymous node must avoid both ?_spin_v1 (subquery text) and ?_spin_v2 (VALUES): {q_col}"
        );
    }

    #[test]
    fn dual_form_prefers_rdf_and_falls_back_to_sp_text_on_unsupported_rdf() {
        // 1. Both valid RDF (full IRIs) and sp:text (undeclared prefix): RDF wins
        let g_both = parse_ttl(
            r#"
            ex:Person spin:rule [
              a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object 1 ] ) ;
              sp:where ( [ sp:subject spin:_this ; sp:predicate ex:in ; sp:object [ sp:varName "x" ] ] ) ;
              sp:text "CONSTRUCT { ?this undeclared:out 1 } WHERE { ?this undeclared:in ?x }"
            ] .
            "#,
        );
        let translated = translate(&g_both, &[]).unwrap();
        assert!(!translated.is_empty());

        // 2. Dual-form rule whose RDF uses an unsupported construct falls back to valid sp:text
        let g_fb = parse_ttl(
            r#"
            ex:Person spin:rule [
              a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object 1 ] ) ;
              sp:where ( [ a sp:UnsupportedExtension ; sp:subject spin:_this ] ) ;
              sp:text "CONSTRUCT { ?this <http://example.org/out> 1 } WHERE { ?this <http://example.org/in> ?x }"
            ] .
            "#,
        );
        let translated = translate(&g_fb, &[]).unwrap();
        assert!(!translated.is_empty());

        // 3. RDF-only rule with unsupported construct still fails
        let g_only = parse_ttl(
            r#"
            ex:Person spin:rule [
              a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object 1 ] ) ;
              sp:where ( [ a sp:UnsupportedExtension ; sp:subject spin:_this ] )
            ] .
            "#,
        );
        assert!(translate(&g_only, &[]).is_err());

        // 4. Dual-form rule with cyclic RDF still fails (limit/cycle error is never masked)
        let g_cyc = parse_ttl(
            r#"
            ex:Person spin:rule [
              a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object 1 ] ) ;
              sp:where ( [ a sp:Filter ; sp:expression ex:e1 ] ) ;
              sp:text "CONSTRUCT { ?this <http://example.org/out> 1 } WHERE {}"
            ] .
            ex:e1 a sp:not ; sp:arg1 ex:e1 .
            "#,
        );
        let err = translate(&g_cyc, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("cyclic"), "{err:#}");

        // 5. Template call with both base and subclass template types selects most specific
        //    and supports bound sp:separator on sp:GroupConcat and bound SELECT ?arg in RDF body
        let g_spec = parse_ttl(
            r#"
            ex:Base a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:sep ; spl:valueType xsd:string ] ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:who ] ;
              spin:body [
                a sp:Construct ;
                sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:joined ; sp:object [ sp:varName "j" ] ] ) ;
                sp:where (
                  [ a sp:SubQuery ;
                    sp:query [
                      a sp:Select ;
                      sp:resultVariables (
                        [ sp:varName "who" ]
                        [ sp:varName "j" ; sp:expression [ a sp:GroupConcat ; sp:expression [ sp:varName "v" ] ; sp:separator [ sp:varName "sep" ] ] ]
                      ) ;
                      sp:where ( [ sp:subject [ sp:varName "who" ] ; sp:predicate ex:v ; sp:object [ sp:varName "v" ] ] ) ;
                      sp:groupBy ( [ sp:varName "who" ] )
                    ] ]
                )
              ] .
            ex:Child rdfs:subClassOf ex:Base ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:sep ; spl:defaultValue "|" ] .
            ex:call a ex:Base, ex:Child ; ex:who ex:a .
            "#,
        );
        let q_spec = serialize_query(&g_spec, &ex("call")).unwrap();
        assert!(parse_query(&q_spec, None, &[]).is_ok(), "{q_spec}");
        assert!(q_spec.contains("SEPARATOR = \"|\""), "{q_spec}");
        assert!(
            q_spec.contains("(<http://example.org/a> AS ?who)"),
            "{q_spec}"
        );
    }

    #[test]
    fn round2_pre_clone_bounds_linear_text_and_extra_types_budget() {
        // 1. Oversized sp:text rejected through serialize_query and translate before cloning
        let mut g = Graph::new();
        let cls = NamedNode::new_unchecked("http://example.org/A");
        let rule = NamedNode::new_unchecked("http://example.org/hugeTextRule");
        let huge = "x".repeat(super::rdf::MAX_OUTPUT_BYTES + 32);
        g.insert(&Triple::new(
            cls.clone(),
            super::vocab::spin::RULE,
            rule.clone(),
        ));
        g.insert(&Triple::new(
            rule.clone(),
            super::vocab::sp::TEXT,
            Literal::new_simple_literal(huge),
        ));
        let err = serialize_query(&g, &Term::NamedNode(rule)).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum output size"),
            "{err:#}"
        );
        let err = translate(&g, &[]).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum output size"),
            "{err:#}"
        );

        // 2. Deeply nested Term::Triple in graph rejected through serialize_query (not just write_term_value)
        let mut nested = Term::NamedNode(NamedNode::new_unchecked("http://example.org/leaf"));
        let s = NamedNode::new_unchecked("http://example.org/s");
        let p = NamedNode::new_unchecked("http://example.org/p");
        for _ in 0..70 {
            nested = Term::Triple(Box::new(Triple::new(s.clone(), p.clone(), nested)));
        }
        let mut g_deep = Graph::new();
        let q = NamedNode::new_unchecked("http://example.org/deepQ");
        let cell = BlankNode::new_unchecked("c1");
        let tp = BlankNode::new_unchecked("tp1");
        g_deep.insert(&Triple::new(
            q.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::ASK,
        ));
        g_deep.insert(&Triple::new(
            q.clone(),
            super::vocab::sp::WHERE,
            cell.clone(),
        ));
        g_deep.insert(&Triple::new(
            cell.clone(),
            crate::vocab::rdf::FIRST,
            tp.clone(),
        ));
        g_deep.insert(&Triple::new(
            cell,
            crate::vocab::rdf::REST,
            crate::vocab::rdf::NIL,
        ));
        g_deep.insert(&Triple::new(
            tp.clone(),
            super::vocab::sp::SUBJECT,
            s.clone(),
        ));
        g_deep.insert(&Triple::new(
            tp.clone(),
            super::vocab::sp::PREDICATE,
            p.clone(),
        ));
        g_deep.insert(&Triple::new(tp, super::vocab::sp::OBJECT, nested));
        let err = serialize_query(&g_deep, &Term::NamedNode(q)).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum nesting depth"),
            "{err:#}"
        );

        // 3. Bounded-length list of moderately large literals exceeds cumulative retained byte cap
        let mut g_list = Graph::new();
        let q_list = NamedNode::new_unchecked("http://example.org/bigListQ");
        let row_head = BlankNode::new_unchecked("row_head");
        let vals = BlankNode::new_unchecked("vals");
        let var_cell = BlankNode::new_unchecked("vcell");
        g_list.insert(&Triple::new(
            q_list.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::ASK,
        ));
        g_list.insert(&Triple::new(
            q_list.clone(),
            super::vocab::sp::WHERE,
            crate::vocab::rdf::NIL,
        ));
        g_list.insert(&Triple::new(
            q_list.clone(),
            super::vocab::sp::VALUES,
            vals.clone(),
        ));
        g_list.insert(&Triple::new(
            vals.clone(),
            super::vocab::sp::VAR_NAMES,
            var_cell.clone(),
        ));
        g_list.insert(&Triple::new(
            var_cell.clone(),
            crate::vocab::rdf::FIRST,
            Literal::new_simple_literal("v"),
        ));
        g_list.insert(&Triple::new(
            var_cell,
            crate::vocab::rdf::REST,
            crate::vocab::rdf::NIL,
        ));
        // 40 literals x 64 KiB = 2.5 MiB > MAX_RETAINED_BYTES (2 MiB)
        let chunk = "z".repeat(65_536);
        let mut list_head = Term::NamedNode(crate::vocab::rdf::NIL.into_owned());
        for i in 0..40 {
            let c = BlankNode::new_unchecked(format!("big_item_{i}"));
            g_list.insert(&Triple::new(
                c.clone(),
                crate::vocab::rdf::FIRST,
                Literal::new_simple_literal(format!("{chunk}{i}")),
            ));
            g_list.insert(&Triple::new(c.clone(), crate::vocab::rdf::REST, list_head));
            list_head = Term::BlankNode(c);
        }
        g_list.insert(&Triple::new(
            row_head.clone(),
            crate::vocab::rdf::FIRST,
            list_head,
        ));
        g_list.insert(&Triple::new(
            row_head.clone(),
            crate::vocab::rdf::REST,
            crate::vocab::rdf::NIL,
        ));
        g_list.insert(&Triple::new(vals, super::vocab::sp::BINDINGS, row_head));
        let err = serialize_query(&g_list, &Term::NamedNode(q_list)).unwrap_err();
        assert!(
            format!("{err:#}").contains("maximum retained term size"),
            "{err:#}"
        );

        // 4. Quadratic-trap dotted words and repeated ORDER BY stay bounded in both template and subquery paths
        let dotted = "a.".repeat(10_000);
        let order_spam = "ORDER BY ".repeat(10_000);
        for malformed in [&dotted, &order_spam] {
            // Template expansion entry point
            let mut g_t = parse_ttl(
                r#"
                ex:T a spin:ConstructTemplate ;
                  spin:constraint [ a spl:Argument ; spl:predicate ex:x ] ;
                  spin:body ex:body .
                ex:c a ex:T ; ex:x 1 .
                "#,
            );
            g_t.insert(&Triple::new(
                NamedNode::new_unchecked("http://example.org/body"),
                super::vocab::sp::TEXT,
                Literal::new_simple_literal(malformed.clone()),
            ));
            let err = serialize_query(&g_t, &ex("c")).unwrap_err();
            assert!(format!("{err:#}").contains("node budget"), "{err:#}");

            // Mixed RDF + textual subquery entry point
            let mut g_sq = parse_ttl(
                r#"
                ex:q a sp:Select ;
                  sp:where ( [ a sp:SubQuery ; sp:query ex:inner ] ) .
                "#,
            );
            g_sq.insert(&Triple::new(
                NamedNode::new_unchecked("http://example.org/inner"),
                super::vocab::sp::TEXT,
                Literal::new_simple_literal(format!("SELECT ?x WHERE {{ {malformed} }}")),
            ));
            let err = serialize_query(&g_sq, &ex("q")).unwrap_err();
            assert!(format!("{err:#}").contains("node budget"), "{err:#}");
        }

        // 5. ExtraTypes charges empty per-graph probes and deduplicates graph IDs
        let store = sparkles_core::store::Store::in_memory(Default::default());
        let mut sources = Vec::new();
        let mut graph_iris = Vec::new();
        for i in 0..30 {
            let iri = format!("urn:x:g{i}");
            graph_iris.push(iri.clone());
            graph_iris.push(iri.clone()); // duplicate to exercise set deduplication
            sources.push(sparkles_core::io::Source::from_bytes(
                format!(
                    "<http://example.org/other{i}> <http://example.org/p> <http://example.org/v> .\n"
                )
                .into_bytes(),
                sparkles_core::io::RdfFormat::NTriples,
                Some(NamedNode::new_unchecked(iri)),
            ));
        }
        // Seed rdf:type and subject IRI in one graph so lookup_term & pred_id resolve, while
        // the subject has no rdf:type in any of the 30 graphs (all probes miss).
        sources.push(sparkles_core::io::Source::from_bytes(
            b"<http://example.org/missingSub> <http://example.org/unrelated> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> .\n"
                .to_vec(),
            sparkles_core::io::RdfFormat::NTriples,
            Some(NamedNode::new_unchecked("urn:x:seed")),
        ));
        store.load(&sources).unwrap();
        let snap = store.snapshot();
        let extra = super::ExtraTypes::new(&snap, &graph_iris);
        assert_eq!(extra.graph_ids.len(), 30);
        let mut tight = super::rdf::WorkBudget::new(10);
        let err = extra.rdf_types(&ex("missingSub"), &mut tight).unwrap_err();
        assert!(format!("{err:#}").contains("node budget"), "{err:#}");
    }

    #[test]
    fn round2_bound_clauses_nested_select_scope_comments_iris_and_ask_modifiers() {
        // 1. BOUND(?arg) in FILTER, HAVING, GROUP BY, ORDER BY emits valid (true)
        let g_bound = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:arg ] ;
              spin:body [
                a sp:Construct ;
                sp:text """
                  CONSTRUCT { ?this <http://example.org/ok> ?arg }
                  WHERE {
                    {
                      SELECT ?s (COUNT(?v) AS ?c) WHERE {
                        ?s <http://example.org/v> ?v .
                        FILTER BOUND(?arg)
                      }
                      GROUP BY ?s BOUND(?arg)
                      HAVING BOUND(?arg)
                      ORDER BY BOUND(?arg)
                    }
                    ?this <http://example.org/ref> ?s .
                  }
                """
              ] .
            ex:call a ex:T ; ex:arg 42 .
            "#,
        );
        let q_bound = serialize_query(&g_bound, &ex("call")).unwrap();
        assert!(parse_query(&q_bound, None, &[]).is_ok(), "{q_bound}");
        assert!(q_bound.contains("FILTER (true)"), "{q_bound}");
        assert!(q_bound.contains("GROUP BY ?s (true)"), "{q_bound}");
        assert!(q_bound.contains("HAVING (true)"), "{q_bound}");
        assert!(q_bound.contains("ORDER BY (true)"), "{q_bound}");

        // 2. Nested SELECT projections of the same bound argument in text and RDF bodies
        let g_nested_text = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:arg ] ;
              spin:body [
                a sp:Construct ;
                sp:text """
                  CONSTRUCT { ?this <http://example.org/out> ?arg }
                  WHERE {
                    {
                      SELECT ?arg WHERE {
                        {
                          SELECT * WHERE {
                            { SELECT ?arg WHERE { ?this <http://example.org/p> ?arg } }
                          }
                        }
                      }
                    }
                  }
                """
              ] .
            ex:call a ex:T ; ex:arg ex:Val .
            "#,
        );
        let q_nt = serialize_query(&g_nested_text, &ex("call")).unwrap();
        assert!(parse_query(&q_nt, None, &[]).is_ok(), "{q_nt}");
        assert!(
            q_nt.contains("SELECT ?arg WHERE"),
            "outer SELECT must keep bare ?arg: {q_nt}"
        );
        assert!(
            q_nt.contains("SELECT (<http://example.org/Val> AS ?arg) WHERE"),
            "innermost SELECT must bind ?arg: {q_nt}"
        );

        let g_nested_rdf = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:arg ] ;
              spin:body [
                a sp:Construct ;
                sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object [ sp:varName "arg" ] ] ) ;
                sp:where (
                  [ a sp:SubQuery ;
                    sp:query [
                      a sp:Select ;
                      sp:resultVariables ( [ sp:varName "arg" ] ) ;
                      sp:where (
                        [ a sp:SubQuery ;
                          sp:query [
                            a sp:Select ;
                            sp:resultVariables ( [ sp:varName "arg" ] ) ;
                            sp:where ( [ sp:subject spin:_this ; sp:predicate ex:p ; sp:object [ sp:varName "arg" ] ] )
                          ] ]
                      ) ;
                      sp:groupBy ( [ sp:varName "arg" ] )
                    ] ]
                )
              ] .
            ex:call a ex:T ; ex:arg ex:Val .
            "#,
        );
        let q_nr = serialize_query(&g_nested_rdf, &ex("call")).unwrap();
        assert!(parse_query(&q_nr, None, &[]).is_ok(), "{q_nr}");
        assert!(
            q_nr.contains("SELECT ?arg WHERE { { SELECT (<http://example.org/Val> AS ?arg) WHERE"),
            "{q_nr}"
        );
        assert!(q_nr.contains("GROUP BY ?arg"), "{q_nr}");

        // 3. CR-terminated comment and IRI Unicode escapes (\uXXXX / \UXXXXXXXX)
        let mut g_cr = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:arg ] ;
              spin:body ex:body .
            ex:call a ex:T ; ex:arg ex:Bob .
            "#,
        );
        g_cr.insert(&Triple::new(
            NamedNode::new_unchecked("http://example.org/body"),
            super::vocab::sp::TEXT,
            Literal::new_simple_literal(
                "CONSTRUCT { ?this <https://example.org/\\u0061?arg> ?arg ; <https://example.org/\\U00000062?arg> ?v } WHERE { # cr comment\r ?this <http://example.org/p> ?arg . ?this <http://example.org/q> ?v }",
            ),
        ));
        let q_cr = serialize_query(&g_cr, &ex("call")).unwrap();
        assert!(parse_query(&q_cr, None, &[]).is_ok(), "{q_cr}");
        assert!(
            q_cr.contains("<https://example.org/\\u0061?arg> <http://example.org/Bob>"),
            "{q_cr}"
        );
        assert!(
            q_cr.contains("<https://example.org/\\U00000062?arg> ?v"),
            "{q_cr}"
        );
        assert!(
            q_cr.contains("# cr comment\r ?this <http://example.org/p> <http://example.org/Bob>"),
            "{q_cr}"
        );

        // CR-terminated comment inside sp:SubQuery sp:text still inventories ?_spin_v1
        let mut g_sq_cr = parse_ttl(
            r#"
            ex:q a sp:Select ;
              sp:where (
                [ sp:subject [ sp:varName "s" ] ; sp:predicate ex:p ; sp:object [] ]
                [ a sp:SubQuery ; sp:query ex:inner ]
              ) .
            "#,
        );
        g_sq_cr.insert(&Triple::new(
            NamedNode::new_unchecked("http://example.org/inner"),
            super::vocab::sp::TEXT,
            Literal::new_simple_literal(
                "SELECT # comment\r ?_spin_v1 WHERE { ?_spin_v1 <http://example.org/q> 1 }",
            ),
        ));
        let q_sq_cr = serialize_query(&g_sq_cr, &ex("q")).unwrap();
        assert!(parse_query(&q_sq_cr, None, &[]).is_ok(), "{q_sq_cr}");
        assert!(
            q_sq_cr.contains("?s <http://example.org/p> ?_spin_v2 ."),
            "{q_sq_cr}"
        );

        // 4. ASK preserves solution modifiers (ORDER BY, LIMIT, OFFSET, VALUES, and GROUP BY)
        let g_ask = parse_ttl(
            r#"
            ex:askMod a sp:Ask ;
              sp:where ( [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:p ; sp:object [ sp:varName "v" ] ] ) ;
              sp:orderBy ( [ a sp:Asc ; sp:expression [ sp:varName "x" ] ] ) ;
              sp:limit 0 ;
              sp:offset 1 ;
              sp:values [ a sp:Values ; sp:varNames ( "v" ) ; sp:bindings ( ( 1 ) ) ] .
            ex:askGroup a sp:Ask ;
              sp:where ( [ sp:subject [ sp:varName "x" ] ; sp:predicate ex:p ; sp:object [ sp:varName "v" ] ] ) ;
              sp:groupBy ( [ sp:varName "x" ] ) ;
              sp:having ( [ a sp:ne ; sp:arg1 [ sp:varName "x" ] ; sp:arg2 ex:bad ] ) .
            "#,
        );
        let q_ask = serialize_query(&g_ask, &ex("askMod")).unwrap();
        assert!(
            matches!(
                parse_query(&q_ask, None, &[]).expect(&q_ask),
                spargebra::Query::Ask { .. }
            ),
            "{q_ask}"
        );
        assert!(q_ask.contains("ORDER BY ASC(?x)"), "{q_ask}");
        assert!(q_ask.contains("LIMIT 0 OFFSET 1"), "{q_ask}");
        assert!(q_ask.contains("VALUES (?v)"), "{q_ask}");
        let q_ask_grp = serialize_query(&g_ask, &ex("askGroup")).unwrap();
        assert!(q_ask_grp.contains("GROUP BY ?x HAVING"), "{q_ask_grp}");
    }

    #[test]
    fn round2_dual_form_fallback_subquery_validation_this_immutability_and_triple_terms() {
        // 1. Dual-form rule & template body with non-CONSTRUCT RDF (sp:Select) fall back to CONSTRUCT sp:text
        let g_fb_rule = parse_ttl(
            r#"
            ex:Person spin:rule [
              a sp:Select ;
              sp:where () ;
              sp:text "CONSTRUCT { ?this <http://example.org/out> 1 } WHERE { ?this <http://example.org/in> ?x }"
            ] .
            "#,
        );
        let tr = translate(&g_fb_rule, &[]).unwrap();
        assert!(!tr.is_empty());

        let g_fb_tmpl = parse_ttl(
            r#"
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:v ] ;
              spin:body [
                a sp:Select ;
                sp:where () ;
                sp:text "CONSTRUCT { ?this <http://example.org/out> ?v } WHERE { ?this <http://example.org/in> ?x }"
              ] .
            ex:Person spin:rule [ a ex:T ; ex:v 7 ] .
            "#,
        );
        let tr_t = translate(&g_fb_tmpl, &[]).unwrap();
        assert!(!tr_t.is_empty());

        // 2. Textual subquery ending in `# note` (text-only and fallback) parses cleanly
        let g_sq_comment = parse_ttl(
            r#"
            ex:qText a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object [ sp:varName "x" ] ] ) ;
              sp:where (
                [ a sp:SubQuery ;
                  sp:query [ sp:text "SELECT ?x WHERE { ?s <http://example.org/p> ?x } # trailing note" ] ]
              ) .
            ex:qFb a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object [ sp:varName "x" ] ] ) ;
              sp:where (
                [ a sp:SubQuery ;
                  sp:query [
                    a sp:Ask ;
                    sp:where () ;
                    sp:text "SELECT ?x WHERE { ?s <http://example.org/p> ?x } # fallback note"
                  ] ]
              ) .
            "#,
        );
        for name in ["qText", "qFb"] {
            let s = serialize_query(&g_sq_comment, &ex(name)).unwrap();
            assert!(
                matches!(
                    parse_query(&s, None, &[]).expect(&s),
                    spargebra::Query::Construct { .. }
                ),
                "{name}: {s}"
            );
        }

        // 3. Invalid textual subqueries (ASK, FROM, brace escape) refused in both text-only and fallback
        for bad_text in [
            "ASK {}",
            "SELECT ?x FROM <http://example.org/g> WHERE { ?s <http://example.org/p> ?x }",
            "SELECT ?x WHERE { ?s <http://example.org/p> ?x } } UNION { ?a <http://example.org/q> ?b",
        ] {
            let mut g_only = parse_ttl(
                r#"
                ex:q a sp:Select ; sp:where ( [ a sp:SubQuery ; sp:query ex:inner ] ) .
                "#,
            );
            g_only.insert(&Triple::new(
                NamedNode::new_unchecked("http://example.org/inner"),
                super::vocab::sp::TEXT,
                Literal::new_simple_literal(bad_text),
            ));
            assert!(
                serialize_query(&g_only, &ex("q")).is_err(),
                "text-only subquery should fail: {bad_text}"
            );

            let mut g_fb = parse_ttl(
                r#"
                ex:q a sp:Select ; sp:where ( [ a sp:SubQuery ; sp:query ex:inner ] ) .
                ex:inner a sp:Ask ; sp:where () .
                "#,
            );
            g_fb.insert(&Triple::new(
                NamedNode::new_unchecked("http://example.org/inner"),
                super::vocab::sp::TEXT,
                Literal::new_simple_literal(bad_text),
            ));
            assert!(
                serialize_query(&g_fb, &ex("q")).is_err(),
                "fallback subquery should fail: {bad_text}"
            );
        }

        // 4. Intrinsic spin:_this cannot be renamed via sp:varName; matching "this" is allowed
        let g_rename = parse_ttl(
            r#"
            spin:_this sp:varName "other" .
            ex:T a spin:ConstructTemplate ;
              spin:constraint [ a spl:Argument ; spl:predicate ex:other ] ;
              spin:body [
                a sp:Construct ;
                sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object 1 ] ) ;
                sp:where ( [ sp:subject spin:_this ; sp:predicate ex:in ; sp:object [ sp:varName "v" ] ] )
              ] .
            ex:Person spin:rule [ a ex:T ; ex:other ex:Const ] .
            "#,
        );
        let err = translate(&g_rename, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("cannot be renamed"), "{err:#}");

        let g_same = parse_ttl(
            r#"
            spin:_this a sp:Variable ; sp:varName "this" .
            ex:Person spin:rule [
              a sp:Construct ;
              sp:templates ( [ sp:subject spin:_this ; sp:predicate ex:out ; sp:object 1 ] ) ;
              sp:where ( [ sp:subject spin:_this ; sp:predicate ex:in ; sp:object [ sp:varName "v" ] ] )
            ] .
            "#,
        );
        assert!(translate(&g_same, &[]).is_ok());

        // 5. Shallow embedded triple term serializes as ground `<<( s p o )>>` (not reifier `<< s p o >>`)
        let embedded = Term::Triple(Box::new(Triple::new(
            NamedNode::new_unchecked("http://example.org/s"),
            NamedNode::new_unchecked("http://example.org/p"),
            NamedNode::new_unchecked("http://example.org/o"),
        )));
        let mut g_tt = Graph::new();
        let q_tt = NamedNode::new_unchecked("http://example.org/ttQ");
        let t_cell = BlankNode::new_unchecked("tc1");
        let t_pat = BlankNode::new_unchecked("tp1");
        let w_cell = BlankNode::new_unchecked("wc1");
        let w_pat = BlankNode::new_unchecked("wp1");
        g_tt.insert(&Triple::new(
            q_tt.clone(),
            crate::vocab::rdf::TYPE,
            super::vocab::sp::CONSTRUCT,
        ));
        g_tt.insert(&Triple::new(
            q_tt.clone(),
            super::vocab::sp::TEMPLATES,
            t_cell.clone(),
        ));
        g_tt.insert(&Triple::new(
            t_cell.clone(),
            crate::vocab::rdf::FIRST,
            t_pat.clone(),
        ));
        g_tt.insert(&Triple::new(
            t_cell,
            crate::vocab::rdf::REST,
            crate::vocab::rdf::NIL,
        ));
        g_tt.insert(&Triple::new(
            t_pat.clone(),
            super::vocab::sp::SUBJECT,
            super::vocab::spin::THIS,
        ));
        g_tt.insert(&Triple::new(
            t_pat.clone(),
            super::vocab::sp::PREDICATE,
            NamedNode::new_unchecked("http://example.org/claim"),
        ));
        g_tt.insert(&Triple::new(t_pat, super::vocab::sp::OBJECT, embedded));
        g_tt.insert(&Triple::new(
            q_tt.clone(),
            super::vocab::sp::WHERE,
            w_cell.clone(),
        ));
        g_tt.insert(&Triple::new(
            w_cell.clone(),
            crate::vocab::rdf::FIRST,
            w_pat.clone(),
        ));
        g_tt.insert(&Triple::new(
            w_cell,
            crate::vocab::rdf::REST,
            crate::vocab::rdf::NIL,
        ));
        g_tt.insert(&Triple::new(
            w_pat.clone(),
            super::vocab::sp::SUBJECT,
            super::vocab::spin::THIS,
        ));
        g_tt.insert(&Triple::new(
            w_pat.clone(),
            super::vocab::sp::PREDICATE,
            NamedNode::new_unchecked("http://example.org/ready"),
        ));
        g_tt.insert(&Triple::new(
            w_pat,
            super::vocab::sp::OBJECT,
            Literal::from(true),
        ));
        let sparql_tt = serialize_query(&g_tt, &Term::NamedNode(q_tt)).unwrap();
        assert!(
            sparql_tt.contains(
                "<<( <http://example.org/s> <http://example.org/p> <http://example.org/o> )>>"
            ),
            "{sparql_tt}"
        );
        let parsed_tt = parse_query(&sparql_tt, None, &[]).expect(&sparql_tt);
        let spargebra::Query::Construct { template, .. } = parsed_tt else {
            panic!("expected CONSTRUCT: {sparql_tt}");
        };
        assert_eq!(
            template.len(),
            1,
            "ground triple term must not expand into a reifier + rdf:reifies triple: {template:?}"
        );
        assert!(
            matches!(template[0].object, spargebra::term::TermPattern::Triple(_)),
            "{template:?}"
        );
    }
}
