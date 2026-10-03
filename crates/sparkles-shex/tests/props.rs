//! Property tests: node constraints decided on store ids (inline numbers, vocabulary and
//! delta ids, IRI stems as id ranges, the per-constraint cache) agree with the same
//! constraints decided on the terms, and compact shape maps read back what they say.

use oxrdf::{BlankNode, Literal, NamedNode, Term};
use proptest::prelude::*;
use sparkles_core::id::Id;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shex::ast::{
    Exclusion, NodeConstraint, NodeKind, NumericLiteral, ObjectLiteral, ObjectValue, Stem,
    ValueSetValue,
};
use sparkles_shex::ir::{NcId, NcIr};
use sparkles_shex::nc::NcPlan;
use sparkles_shex::{NodeSelector, ShapeLabel, ShapeMap};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const EX: &str = "http://ex.org/";

fn lexical() -> impl Strategy<Value = String> {
    prop_oneof![
        "[+-]?[0-9]{1,6}",
        "[+-]?[0-9]{0,4}\\.[0-9]{0,4}",
        "[+-]?[0-9]\\.[0-9]{1,3}[eE][+-]?[0-9]{1,2}",
        Just("NaN".to_string()),
        Just("INF".to_string()),
        "[a-z𝒳é]{0,6}",
        Just("true".to_string()),
        Just("2020-01-02".to_string()),
    ]
}

fn datatype() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("integer"),
        Just("decimal"),
        Just("double"),
        Just("float"),
        Just("byte"),
        Just("nonNegativeInteger"),
        Just("string"),
        Just("boolean"),
        Just("date"),
    ]
    .prop_map(|d| format!("{XSD}{d}"))
}

fn term() -> impl Strategy<Value = Term> {
    prop_oneof![
        (lexical(), datatype()).prop_map(|(l, d)| Literal::new_typed_literal(
            l,
            NamedNode::new_unchecked(d)
        )
        .into()),
        (
            lexical(),
            prop_oneof![Just("en"), Just("en-us"), Just("fr"), Just("en-gb")]
        )
            .prop_map(|(l, t)| Literal::new_language_tagged_literal_unchecked(l, t).into()),
        "[a-c]{0,3}(/[a-c]{0,2}){0,2}"
            .prop_map(|p| NamedNode::new_unchecked(format!("{EX}{p}")).into()),
        "[a-z]{1,4}".prop_map(|l| BlankNode::new_unchecked(l).into()),
    ]
}

fn numeric() -> impl Strategy<Value = NumericLiteral> {
    prop_oneof![
        "-?[0-9]{1,4}".prop_map(NumericLiteral::Integer),
        "-?[0-9]{1,3}\\.[0-9]{1,2}".prop_map(NumericLiteral::Decimal),
        "-?[0-9]\\.[0-9]e[0-3]".prop_map(NumericLiteral::Double),
    ]
}

fn value() -> impl Strategy<Value = ValueSetValue> {
    let stem = prop_oneof![
        Just(Stem::Wildcard),
        "[a-c]{0,2}".prop_map(|s| Stem::Value(format!("{EX}{s}"))),
    ];
    let excl = prop_oneof![
        "[a-c]{0,3}".prop_map(|s| Exclusion::Value(format!("{EX}{s}"))),
        "[a-c]{0,2}".prop_map(|s| Exclusion::Stem(format!("{EX}{s}"))),
    ];
    let lang_excl = prop_oneof![
        prop_oneof![Just("en"), Just("en-US")].prop_map(|s| Exclusion::Value(s.into())),
        prop_oneof![Just("en"), Just("fr")].prop_map(|s| Exclusion::Stem(s.into())),
    ];
    prop_oneof![
        (lexical(), proptest::option::of(datatype())).prop_map(|(v, datatype)| {
            ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
                value: v,
                language: None,
                datatype,
            }))
        }),
        "[a-c]{0,3}".prop_map(|s| ValueSetValue::Object(ObjectValue::Iri(format!("{EX}{s}")))),
        "[a-c]{0,3}".prop_map(|s| ValueSetValue::IriStem(format!("{EX}{s}"))),
        (stem, proptest::collection::vec(excl, 0..3))
            .prop_map(|(stem, exclusions)| ValueSetValue::IriStemRange { stem, exclusions }),
        "[0-9a-z]{0,2}".prop_map(ValueSetValue::LiteralStem),
        prop_oneof![Just("en"), Just("EN-us"), Just("fr")]
            .prop_map(|l| ValueSetValue::Language(l.into())),
        prop_oneof![Just(""), Just("en"), Just("en-us")]
            .prop_map(|l| ValueSetValue::LanguageStem(l.into())),
        (
            prop_oneof![Just(Stem::Wildcard), Just(Stem::Value(String::new()))],
            proptest::collection::vec(lang_excl, 0..3)
        )
            .prop_map(|(stem, exclusions)| ValueSetValue::LanguageStemRange { stem, exclusions }),
    ]
}

fn constraint() -> impl Strategy<Value = NodeConstraint> {
    let kind = proptest::option::of(prop_oneof![
        Just(NodeKind::Iri),
        Just(NodeKind::BNode),
        Just(NodeKind::NonLiteral),
        Just(NodeKind::Literal),
    ]);
    let len = proptest::option::of(0u64..6);
    (
        (
            kind,
            proptest::option::of(datatype()),
            len.clone(),
            len.clone(),
            len,
        ),
        (
            proptest::option::of(prop_oneof![Just("^[0-9]"), Just("a.$"), Just("𝒳")]),
            proptest::option::of(numeric()),
            proptest::option::of(numeric()),
            proptest::option::of(1u64..6),
            proptest::option::of(0u64..4),
        ),
        proptest::option::of(proptest::collection::vec(value(), 0..4)),
    )
        .prop_map(
            |(
                (node_kind, datatype, length, min_length, max_length),
                (pat, min, max, td, fd),
                values,
            )| {
                NodeConstraint {
                    node_kind,
                    datatype,
                    length,
                    min_length,
                    max_length,
                    pattern: pat.map(str::to_string),
                    min_inclusive: min,
                    max_exclusive: max,
                    total_digits: td,
                    fraction_digits: fd,
                    values,
                    ..Default::default()
                }
            },
        )
}

/// A store holding `terms` as objects: the first half in the base vocabulary, the rest
/// added by a later write (delta ids). Returns the ids of the terms.
fn store_with(terms: &[Term]) -> (Store, Vec<Id>) {
    let store = Store::in_memory(StoreOptions::default());
    let (base, delta) = terms.split_at(terms.len() / 2);
    let s = NamedNode::new_unchecked(format!("{EX}s"));
    let p = NamedNode::new_unchecked(format!("{EX}p"));
    let quads: Vec<oxrdf::Quad> = base
        .iter()
        .filter(|t| !matches!(t, Term::BlankNode(_)))
        .map(|o| {
            oxrdf::Quad::new(
                s.clone(),
                p.clone(),
                o.clone(),
                oxrdf::GraphName::DefaultGraph,
            )
        })
        .collect();
    if !quads.is_empty() {
        let mut nt = String::new();
        for q in &quads {
            nt.push_str(&format!("{} {} {} .\n", q.subject, q.predicate, q.object));
        }
        store
            .load(&[sparkles_core::io::Source::from_bytes(
                nt.into_bytes(),
                sparkles_core::io::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let mut txn = store.write();
    let si = txn.intern(&s.clone().into()).unwrap();
    let pi = txn.intern(&p.clone().into()).unwrap();
    let mut labels = std::collections::HashMap::new();
    for o in delta
        .iter()
        .chain(base.iter().filter(|t| matches!(t, Term::BlankNode(_))))
    {
        let oi = txn.intern_scoped(o, &mut labels).unwrap();
        txn.insert([si, pi, oi, Id::DEFAULT_GRAPH]).unwrap();
    }
    txn.commit().unwrap();
    let snap = store.snapshot();
    let ids = terms
        .iter()
        .map(|t| match t {
            Term::BlankNode(b) => labels[b.as_str()],
            t => snap.lookup_term(t).unwrap(),
        })
        .collect();
    (store, ids)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Deciding on ids (with the cache) and on terms gives the same answers.
    #[test]
    fn ids_and_terms_agree(
        nc in constraint(),
        terms in proptest::collection::vec(term(), 1..12),
    ) {
        let (store, ids) = store_with(&terms);
        let snap = store.snapshot();
        let ncs = [NcIr { nc, regex: None }];
        let stored = NcPlan::new(&snap, &ncs);
        let empty = Store::in_memory(StoreOptions::default());
        let unstored = NcPlan::new(&empty.snapshot(), &ncs);
        for (t, id) in terms.iter().zip(&ids) {
            // blank nodes are relabelled by the store: compare with the stored label
            let t = match t {
                Term::BlankNode(_) => snap.term(*id).unwrap(),
                t => t.clone(),
            };
            let by_term = unstored.check_term(NcId(0), &t).unwrap();
            for _ in 0..2 {
                prop_assert_eq!(stored.check(NcId(0), *id).unwrap(), by_term, "{}", t);
            }
            prop_assert_eq!(
                stored.explain(NcId(0), *id).unwrap().is_none(),
                by_term,
                "{}", t
            );
            prop_assert_eq!(stored.check_term(NcId(0), &t).unwrap(), by_term, "{}", t);
        }
    }

    /// A compact shape map of IRI and literal nodes reads back as written.
    #[test]
    fn compact_maps_read_back(
        nodes in proptest::collection::vec(
            prop_oneof![
                "[a-z][a-z0-9]{0,4}".prop_map(|l| (format!("ex:{l}"), Term::from(NamedNode::new_unchecked(format!("{EX}{l}"))))),
                "[a-z ]{0,5}".prop_map(|l| (format!("{l:?}"), Term::from(Literal::new_simple_literal(l)))),
                "-?[0-9]{1,4}".prop_map(|l| (l.clone(), Term::from(Literal::new_typed_literal(l, oxrdf::vocab::xsd::INTEGER)))),
            ],
            1..6,
        ),
        start in proptest::collection::vec(any::<bool>(), 6),
        commas in any::<bool>(),
    ) {
        let text = nodes
            .iter()
            .zip(&start)
            .map(|((n, _), s)| format!("{n}@{}", if *s { "START" } else { "ex:S" }))
            .collect::<Vec<_>>()
            .join(if commas { ", " } else { "\n" });
        let map = ShapeMap::parse(&text, &vec![("ex".into(), EX.into())], None).unwrap();
        prop_assert_eq!(map.0.len(), nodes.len());
        for (a, ((_, t), s)) in map.0.iter().zip(nodes.iter().zip(&start)) {
            prop_assert_eq!(&a.node, &NodeSelector::Term(t.clone()));
            let label = if *s { ShapeLabel::Start } else { ShapeLabel::Iri(format!("{EX}S")) };
            prop_assert_eq!(&a.shape, &label);
        }
    }
}

// ------------------------------------------------------------ end to end ------
//
// Random small schemas (shapes of triple constraints with references, negated
// references, node constraints, cardinalities, inverse constraints, EXTRA and CLOSED) on
// random graphs, validated by the engine and by a naive reading of the specification:
// strata of the label dependency graph in order, the greatest fixed point of each stratum
// over every node, and matchesShape by enumerating every assignment of arcs to triple
// constraints (or the remainder).

/// A value expression of the generated schemas.
#[derive(Clone, Debug)]
enum Val {
    Any,
    Ref(usize),
    NotRef(usize),
    Literal,
    AtLeastOne,
}

#[derive(Clone, Debug)]
struct Tc {
    pred: usize,
    inverse: bool,
    val: Val,
    min: u32,
    /// `None`: unbounded
    max: Option<u32>,
}

#[derive(Clone, Debug)]
struct Shape {
    closed: bool,
    extra: Vec<usize>,
    tcs: Vec<Tc>,
}

const PREDS: usize = 3;
const NODES: usize = 5;
const LITS: i64 = 3;

/// The values of the generated graphs: IRIs `ex:n0…` and the integers `0…`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum V {
    Node(usize),
    Int(i64),
}

impl V {
    fn term(self) -> Term {
        match self {
            V::Node(i) => NamedNode::new_unchecked(format!("{EX}n{i}")).into(),
            V::Int(i) => Literal::from(i).into(),
        }
    }
}

fn pred_iri(p: usize) -> String {
    format!("{EX}p{p}")
}

fn shape_iri(s: usize) -> String {
    format!("{EX}S{s}")
}

fn schema_strategy() -> impl Strategy<Value = Vec<Shape>> {
    (1usize..=3).prop_flat_map(|n| {
        let val = prop_oneof![
            2 => Just(Val::Any),
            3 => (0..n).prop_map(Val::Ref),
            1 => (0..n).prop_map(Val::NotRef),
            1 => Just(Val::Literal),
            1 => Just(Val::AtLeastOne),
        ];
        let tc = (
            0..PREDS,
            proptest::bool::weighted(0.2),
            val,
            0u32..=1,
            prop_oneof![Just(Some(1u32)), Just(Some(2)), Just(None)],
        )
            .prop_map(|(pred, inverse, val, min, max)| Tc {
                pred,
                inverse,
                val,
                min,
                max,
            });
        let shape = (
            proptest::bool::weighted(0.3),
            proptest::collection::vec(0..PREDS, 0..2),
            proptest::collection::vec(tc, 1..=3),
        )
            .prop_map(|(closed, extra, tcs)| Shape { closed, extra, tcs });
        proptest::collection::vec(shape, n)
    })
}

fn graph_strategy() -> impl Strategy<Value = Vec<(usize, usize, V)>> {
    let value = prop_oneof![
        3 => (0..NODES).prop_map(V::Node),
        1 => (0..LITS).prop_map(V::Int),
    ];
    proptest::collection::vec((0..NODES, 0..PREDS, value), 0..14)
}

/// The AST of the generated schema.
fn to_schema(shapes: &[Shape]) -> sparkles_shex::Schema {
    use sparkles_shex::ast::Group;
    use sparkles_shex::ast::{
        Label, Shape as AstShape, ShapeDecl, ShapeExpr, TripleConstraint, TripleExpr,
    };
    let tc = |t: &Tc| {
        let value_expr = match &t.val {
            Val::Any => None,
            Val::Ref(s) => Some(ShapeExpr::Ref(Label::Iri(shape_iri(*s)))),
            Val::NotRef(s) => Some(ShapeExpr::Not(Box::new(ShapeExpr::Ref(Label::Iri(
                shape_iri(*s),
            ))))),
            Val::Literal => Some(ShapeExpr::Nc(Box::new(NodeConstraint {
                node_kind: Some(NodeKind::Literal),
                ..Default::default()
            }))),
            Val::AtLeastOne => Some(ShapeExpr::Nc(Box::new(NodeConstraint {
                min_inclusive: Some(NumericLiteral::Integer("1".into())),
                ..Default::default()
            }))),
        };
        TripleExpr::Tc(TripleConstraint {
            id: None,
            inverse: t.inverse.then_some(true),
            predicate: pred_iri(t.pred),
            value_expr: value_expr.map(Box::new),
            min: Some(t.min),
            max: Some(t.max.map_or(-1, i64::from)),
            sem_acts: vec![],
            annotations: vec![],
        })
    };
    sparkles_shex::Schema {
        shapes: shapes
            .iter()
            .enumerate()
            .map(|(i, s)| ShapeDecl {
                label: Label::Iri(shape_iri(i)),
                expr: ShapeExpr::Shape(Box::new(AstShape {
                    closed: s.closed.then_some(true),
                    extra: s.extra.iter().map(|p| pred_iri(*p)).collect(),
                    expression: Some(if s.tcs.len() == 1 {
                        tc(&s.tcs[0])
                    } else {
                        TripleExpr::EachOf(Group {
                            id: None,
                            exprs: s.tcs.iter().map(tc).collect(),
                            min: None,
                            max: None,
                            sem_acts: vec![],
                            annotations: vec![],
                        })
                    }),
                    sem_acts: vec![],
                    annotations: vec![],
                })),
            })
            .collect(),
        ..Default::default()
    }
}

/// The label dependency edges `(from, to, negative)`.
fn edges(shapes: &[Shape]) -> Vec<(usize, usize, bool)> {
    let mut e = Vec::new();
    for (i, s) in shapes.iter().enumerate() {
        for t in &s.tcs {
            let extra = s.extra.contains(&t.pred);
            match t.val {
                Val::Ref(j) => e.push((i, j, extra)),
                Val::NotRef(j) => e.push((i, j, true)),
                _ => {}
            }
        }
    }
    e
}

/// The strongly connected components in dependency order (dependencies first), or
/// `None` when a negative edge lies on a cycle.
fn strata(shapes: &[Shape]) -> Option<Vec<Vec<usize>>> {
    let n = shapes.len();
    let e = edges(shapes);
    let mut reach = vec![vec![false; n]; n];
    for &(a, b, _) in &e {
        reach[a][b] = true;
    }
    for k in 0..n {
        for i in 0..n {
            for j in 0..n {
                if reach[i][k] && reach[k][j] {
                    reach[i][j] = true;
                }
            }
        }
    }
    let same = |a: usize, b: usize| a == b || (reach[a][b] && reach[b][a]);
    if e.iter().any(|&(a, b, neg)| neg && reach[b][a]) {
        return None;
    }
    let mut done = vec![false; n];
    let mut out = Vec::new();
    while out.iter().map(Vec::len).sum::<usize>() < n {
        // an SCC whose dependencies outside it are all done
        let next = (0..n)
            .filter(|&i| !done[i])
            .find(|&i| (0..n).all(|j| done[j] || same(i, j) || !reach[i][j]))
            .expect("a DAG of SCCs has a sink");
        let scc: Vec<usize> = (0..n).filter(|&j| !done[j] && same(next, j)).collect();
        for &j in &scc {
            done[j] = true;
        }
        out.push(scc);
    }
    Some(out)
}

struct Naive<'a> {
    shapes: &'a [Shape],
    triples: Vec<(V, usize, V)>,
    /// the typing: (value, shape) → conforms
    typing: std::collections::BTreeMap<(V, usize), bool>,
}

impl<'a> Naive<'a> {
    fn sat(&self, v: V, val: &Val) -> bool {
        match val {
            Val::Any => true,
            Val::Ref(s) => self.typing[&(v, *s)],
            Val::NotRef(s) => !self.typing[&(v, *s)],
            Val::Literal => matches!(v, V::Int(_)),
            Val::AtLeastOne => matches!(v, V::Int(i) if i >= 1),
        }
    }

    /// matchesShape: some assignment of the arcs to triple constraints or the remainder
    /// satisfies the cardinalities, EXTRA and CLOSED.
    fn matches(&self, n: V, s: usize) -> bool {
        let shape = &self.shapes[s];
        // (pred, inverse, value) of each arc of n
        let arcs: Vec<(usize, bool, V)> = self
            .triples
            .iter()
            .filter(|t| t.0 == n)
            .map(|t| (t.1, false, t.2))
            .chain(
                self.triples
                    .iter()
                    .filter(|t| t.2 == n)
                    .map(|t| (t.1, true, t.0)),
            )
            .collect();
        let mentioned =
            |p: usize, inv: bool| shape.tcs.iter().any(|t| t.pred == p && t.inverse == inv);
        // arcs nobody mentions: CLOSED forbids the outgoing ones
        if shape.closed && arcs.iter().any(|&(p, inv, _)| !inv && !mentioned(p, inv)) {
            return false;
        }
        let relevant: Vec<(usize, bool, V)> = arcs
            .into_iter()
            .filter(|&(p, inv, _)| mentioned(p, inv))
            .collect();
        // per arc: the constraints it may be assigned to
        let cands: Vec<Vec<usize>> = relevant
            .iter()
            .map(|&(p, inv, v)| {
                (0..shape.tcs.len())
                    .filter(|&i| {
                        let t = &shape.tcs[i];
                        t.pred == p && t.inverse == inv && self.sat(v, &t.val)
                    })
                    .collect()
            })
            .collect();
        // a remainder arc that is a matchable needs EXTRA and must match no constraint
        let may_remain: Vec<bool> = relevant
            .iter()
            .zip(&cands)
            .map(|(&(p, _, _), c)| c.is_empty() && shape.extra.contains(&p))
            .collect();
        let mut counts = vec![0u32; shape.tcs.len()];
        fn go(
            i: usize,
            cands: &[Vec<usize>],
            may_remain: &[bool],
            counts: &mut Vec<u32>,
            tcs: &[Tc],
        ) -> bool {
            if i == cands.len() {
                return tcs
                    .iter()
                    .zip(counts.iter())
                    .all(|(t, &c)| c >= t.min && t.max.is_none_or(|m| c <= m));
            }
            if may_remain[i] && go(i + 1, cands, may_remain, counts, tcs) {
                return true;
            }
            for &k in &cands[i] {
                counts[k] += 1;
                let ok = go(i + 1, cands, may_remain, counts, tcs);
                counts[k] -= 1;
                if ok {
                    return true;
                }
            }
            false
        }
        go(0, &cands, &may_remain, &mut counts, &shape.tcs)
    }

    fn run(shapes: &'a [Shape], triples: Vec<(V, usize, V)>, strata: &[Vec<usize>]) -> Self {
        let mut values: Vec<V> = (0..NODES)
            .map(V::Node)
            .chain((0..LITS).map(V::Int))
            .collect();
        values.dedup();
        let mut me = Naive {
            shapes,
            triples,
            typing: Default::default(),
        };
        for v in &values {
            for s in 0..shapes.len() {
                me.typing.insert((*v, s), true);
            }
        }
        for scc in strata {
            loop {
                let mut changed = false;
                for &s in scc {
                    for &v in &values {
                        if me.typing[&(v, s)] && !me.matches(v, s) {
                            me.typing.insert((v, s), false);
                            changed = true;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
        }
        me
    }
}

fn not_implemented(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("not implemented")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The engine's result map is the naive greatest fixed point's.
    #[test]
    fn validation_agrees_with_the_naive_semantics(
        shapes in schema_strategy(),
        graph in graph_strategy(),
        focus in proptest::collection::btree_set((0..NODES + 1, 0usize..3), 1..6),
    ) {
        let Some(strata) = strata(&shapes) else {
            // a negated reference cycle: the schema is rejected
            return Ok(());
        };
        let schema = match sparkles_shex::compile(&to_schema(&shapes), &sparkles_shex::NoImports) {
            Ok(s) => s,
            Err(e) if e.message.contains("not implemented") => return Ok(()),
            Err(e) => return Err(TestCaseError::fail(format!("compile: {e}"))),
        };
        let triples: Vec<(V, usize, V)> = {
            let mut t: Vec<_> = graph.iter().map(|&(s, p, o)| (V::Node(s), p, o)).collect();
            t.sort();
            t.dedup();
            t
        };
        let store = Store::in_memory(StoreOptions::default());
        let mut txn = store.write();
        for (s, p, o) in &triples {
            let s = txn.intern(&s.term()).unwrap();
            let p = txn.intern(&NamedNode::new_unchecked(pred_iri(*p)).into()).unwrap();
            let o = txn.intern(&o.term()).unwrap();
            txn.insert([s, p, o, Id::DEFAULT_GRAPH]).unwrap();
        }
        txn.commit().unwrap();
        // focus nodes: ex:n0…, and a literal that is not in the graph; shapes beyond the schema's
        // are dropped
        let assocs: Vec<(V, usize)> = focus
            .into_iter()
            .filter(|&(_, s)| s < shapes.len())
            .map(|(n, s)| (if n < NODES { V::Node(n) } else { V::Int(LITS + 7) }, s))
            .collect();
        prop_assume!(!assocs.is_empty());
        let map = ShapeMap(
            assocs
                .iter()
                .map(|(v, s)| sparkles_shex::Association {
                    node: NodeSelector::Term(v.term()),
                    shape: ShapeLabel::Iri(shape_iri(*s)),
                })
                .collect(),
        );
        for parallel in [false, true] {
            let opts = sparkles_shex::ValidateOptions { parallel, ..Default::default() };
            let results = match sparkles_shex::validate(&store.snapshot(), &schema, &map, &opts) {
                Ok(r) => r,
                Err(e) if not_implemented(&e) => return Ok(()),
                Err(e) => return Err(TestCaseError::fail(format!("validate: {e:#}"))),
            };
            let naive = Naive::run(&shapes, triples.clone(), &strata);
            prop_assert_eq!(results.results.len(), assocs.len());
            for (r, (v, s)) in results.results.iter().zip(&assocs) {
                prop_assert_eq!(&r.node, &v.term());
                // a node outside the graph (and a literal not in it) has no arcs
                let expected = match naive.typing.get(&(*v, *s)) {
                    Some(b) => *b,
                    None => Naive { shapes: &shapes, triples: vec![], typing: Default::default() }
                        .matches_isolated(*s),
                };
                prop_assert_eq!(
                    r.status == sparkles_shex::Status::Conformant,
                    expected,
                    "{:?} @ S{}: {:?}\nschema {:?}\ntriples {:?}",
                    v, s, r.reason, shapes, triples
                );
            }
        }
    }
}

impl<'a> Naive<'a> {
    /// Does a node without arcs match shape `s`? Every minimum must be 0.
    fn matches_isolated(&self, s: usize) -> bool {
        self.shapes[s].tcs.iter().all(|t| t.min == 0)
    }
}

/// The naive oracle itself, on the acceptance examples: a greatest fixed point over a
/// cycle, EXTRA, and inverse cardinalities with direction-symmetric matchables.
#[test]
fn naive_oracle_examples() {
    let tc = |pred, inverse, val, min, max| Tc {
        pred,
        inverse,
        val,
        min,
        max,
    };
    // S0 { p0 @S0 * ; p1 LITERAL }
    // S1 EXTRA p0 { p0 @S0 }, S2 { p0 @S0 } (no EXTRA), S3 { ^p0 . {1,2} }
    let shapes = vec![
        Shape {
            closed: false,
            extra: vec![],
            tcs: vec![
                tc(0, false, Val::Ref(0), 0, None),
                tc(1, false, Val::Literal, 1, Some(1)),
            ],
        },
        Shape {
            closed: false,
            extra: vec![0],
            tcs: vec![tc(0, false, Val::Ref(0), 1, Some(1))],
        },
        Shape {
            closed: false,
            extra: vec![],
            tcs: vec![tc(0, false, Val::Ref(0), 1, Some(1))],
        },
        Shape {
            closed: false,
            extra: vec![],
            tcs: vec![tc(0, true, Val::Any, 1, Some(2))],
        },
    ];
    let order = strata(&shapes).unwrap();
    assert_eq!(order[0], [0]);
    let n = V::Node;
    // n0 and n1 know each other and have a p1 literal; n3 has none
    let base = vec![
        (n(0), 0, n(1)),
        (n(1), 0, n(0)),
        (n(0), 1, V::Int(1)),
        (n(1), 1, V::Int(2)),
        (n(3), 0, n(0)),
    ];
    let t = Naive::run(&shapes, base.clone(), &order);
    assert!(t.typing[&(n(0), 0)] && t.typing[&(n(1), 0)]);
    assert!(!t.typing[&(n(3), 0)]);
    // n0 has three incoming p0 arcs (n1, n3, n4), more than {1,2}
    let mut more = base.clone();
    more.push((n(4), 0, n(0)));
    assert!(!Naive::run(&shapes, more, &order).typing[&(n(0), 3)]);
    assert!(t.typing[&(n(0), 3)]);
    // n2 knows a conforming n0 and a nonconforming n3: allowed by EXTRA only
    let mut knows = base.clone();
    knows.extend([(n(2), 0, n(0)), (n(2), 0, n(3))]);
    let t = Naive::run(&shapes, knows.clone(), &order);
    assert!(t.typing[&(n(2), 1)]);
    assert!(!t.typing[&(n(2), 2)]);
    // two conforming arcs: the matching one may not stay in the remainder
    knows.push((n(2), 0, n(1)));
    let t = Naive::run(&shapes, knows, &order);
    assert!(!t.typing[&(n(2), 1)] && !t.typing[&(n(2), 2)]);
    // once n1 loses its literal, the cycle fails as a whole
    let broken: Vec<_> = base
        .into_iter()
        .filter(|x| x.0 != n(1) || x.1 != 1)
        .collect();
    let t = Naive::run(&shapes, broken, &order);
    assert!(!t.typing[&(n(0), 0)] && !t.typing[&(n(1), 0)]);
    // a negated reference on a cycle has no strata
    let mut neg = shapes.clone();
    neg[0].tcs[0].val = Val::NotRef(0);
    assert!(strata(&neg).is_none());
}
