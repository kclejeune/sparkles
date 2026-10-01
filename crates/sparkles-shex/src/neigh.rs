//! Neighbourhoods from snapshots: for a (node, shape) pair, only the arcs the shape can
//! observe, through prefix scans of the data graph: outgoing arcs per predicate of the
//! shape, incoming arcs per inverse predicate, and, for a CLOSED shape, the other
//! outgoing arcs up to the first one that is not allowed. With a single-graph data graph,
//! index counts above the shape's maximum occurrences fail the pair before any value is
//! read.

use crate::ir::{Dir, Ir, ShapeId};
use sparkles::id::Id;
use sparkles::store::Snapshot;
use sparkles::validation::DataGraph;

/// A compiled schema resolved against one snapshot.
#[derive(Clone, Debug, Default)]
pub struct SnapPlan {
    /// per shape, the store id of the predicate of each [`crate::ir::ShapeIr::preds`]
    /// entry (`None`: not in the store, so it matches no arcs)
    pub preds: Vec<Vec<Option<Id>>>,
    shapes: Vec<ShapePlan>,
}

/// What a fetch needs of one shape.
#[derive(Clone, Debug, Default)]
struct ShapePlan {
    closed: bool,
    /// per `preds` entry: the direction, and the most arcs it can hold: the sum of the
    /// maximum occurrences of its triple constraints, `None` when unbounded or when the
    /// predicate is EXTRA (unmatched arcs are then allowed)
    entries: Vec<(Dir, Option<u64>)>,
    /// the outgoing entries by predicate id, sorted (the CLOSED scan)
    out: Vec<(Id, usize)>,
}

impl SnapPlan {
    pub fn new(snap: &Snapshot, ir: &Ir) -> SnapPlan {
        let preds: Vec<Vec<Option<Id>>> = ir
            .shapes
            .iter()
            .map(|s| s.preds.iter().map(|(p, _, _)| snap.lookup_iri(p)).collect())
            .collect();
        let shapes = ir
            .shapes
            .iter()
            .zip(&preds)
            .map(|(s, ids)| {
                let entries = s
                    .preds
                    .iter()
                    .map(|(p, dir, tcs)| {
                        let cap = if s.extra.iter().any(|e| e == p) {
                            None
                        } else {
                            tcs.iter().try_fold(0u64, |sum, tc| {
                                s.max_occ[tc.index()].map(|m| sum.saturating_add(m))
                            })
                        };
                        (*dir, cap)
                    })
                    .collect();
                let mut out: Vec<(Id, usize)> = s
                    .preds
                    .iter()
                    .zip(ids)
                    .enumerate()
                    .filter_map(|(i, ((_, dir, _), id))| match (dir, id) {
                        (Dir::Out, Some(id)) => Some((*id, i)),
                        _ => None,
                    })
                    .collect();
                out.sort_unstable();
                ShapePlan {
                    closed: s.closed,
                    entries,
                    out,
                }
            })
            .collect();
        SnapPlan { preds, shapes }
    }
}

/// The arcs of one node that a shape observes.
#[derive(Clone, Debug, Default)]
pub struct Neigh {
    /// the node
    pub node: Id,
    /// the store id of the predicate of each `preds` entry ([`Id::UNDEF`] when the
    /// predicate is not in the store)
    pub preds: Vec<Id>,
    /// the values of the arcs for each entry of the shape's `preds`, in order (objects
    /// of outgoing arcs, subjects of incoming ones)
    pub values: Vec<Vec<Id>>,
    /// for a CLOSED shape: an outgoing arc whose predicate is in no `preds` entry
    pub closed_violation: Option<(Id, Id)>,
}

impl Neigh {
    /// The triple of an arc of entry `entry` with value `value`: `(node, p, value)`, or
    /// `(value, p, node)` for an incoming arc.
    pub fn triple(&self, dir: Dir, entry: usize, value: Id) -> [Id; 3] {
        match dir {
            Dir::Out => [self.node, self.preds[entry], value],
            Dir::In => [value, self.preds[entry], self.node],
        }
    }
}

/// How a fetch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// `out` holds the neighbourhood
    Ok,
    /// the pair fails without matching: more arcs than the shape's maximum occurrences
    /// on a predicate that is not EXTRA, or an arc a CLOSED shape forbids
    /// ([`Neigh::closed_violation`])
    FailFast,
}

/// Fetch the neighbourhood of `node` for `shape` into `out` (cleared first), stopping
/// as soon as the pair cannot match.
pub fn fetch(
    data: &DataGraph,
    plan: &SnapPlan,
    node: Id,
    shape: ShapeId,
    out: &mut Neigh,
) -> anyhow::Result<FetchOutcome> {
    fetch_with(data, plan, node, shape, out, true)
}

/// Fetch the whole neighbourhood of `node` for `shape` (explanations): no early stop;
/// a CLOSED violation is the first forbidden arc.
pub fn fetch_all(
    data: &DataGraph,
    plan: &SnapPlan,
    node: Id,
    shape: ShapeId,
    out: &mut Neigh,
) -> anyhow::Result<()> {
    fetch_with(data, plan, node, shape, out, false).map(|_| ())
}

fn fetch_with(
    data: &DataGraph,
    plan: &SnapPlan,
    node: Id,
    shape: ShapeId,
    out: &mut Neigh,
    fail_fast: bool,
) -> anyhow::Result<FetchOutcome> {
    let sp = &plan.shapes[shape.index()];
    let ids = &plan.preds[shape.index()];
    let n = sp.entries.len();
    out.node = node;
    out.preds.clear();
    out.preds
        .extend(ids.iter().map(|id| id.unwrap_or(Id::UNDEF)));
    out.values.truncate(n);
    for v in &mut out.values {
        v.clear();
    }
    out.values.resize_with(n, Vec::new);
    out.closed_violation = None;

    let over = |len: usize, cap: Option<u64>| fail_fast && cap.is_some_and(|c| len as u64 > c);
    let mut failed = false;
    if sp.closed {
        // one scan of the outgoing arcs fills the outgoing entries and finds the first
        // forbidden arc
        let values = &mut out.values;
        let violation = &mut out.closed_violation;
        data.scan_out_edges(node, |p, o| {
            match sp.out.binary_search_by_key(&p, |&(id, _)| id) {
                Ok(i) => {
                    let e = sp.out[i].1;
                    values[e].push(o);
                    if over(values[e].len(), sp.entries[e].1) {
                        failed = true;
                        return false;
                    }
                }
                Err(_) => {
                    if violation.is_none() {
                        *violation = Some((p, o));
                    }
                    if fail_fast {
                        failed = true;
                        return false;
                    }
                }
            }
            true
        })?;
        if failed {
            return Ok(FetchOutcome::FailFast);
        }
    }
    for (e, &(dir, cap)) in sp.entries.iter().enumerate() {
        let Some(p) = ids[e] else { continue };
        if sp.closed && dir == Dir::Out {
            continue;
        }
        if fail_fast && let Some(cap) = cap {
            let count = match dir {
                Dir::Out => data.count_objects(node, p)?,
                Dir::In => data.count_subjects(p, node)?,
            };
            if count.is_some_and(|c| c > cap) {
                return Ok(FetchOutcome::FailFast);
            }
        }
        let values = &mut out.values[e];
        let push = |v: Id| {
            values.push(v);
            !over(values.len(), cap)
        };
        match dir {
            Dir::Out => data.scan_objects(node, p, push)?,
            Dir::In => data.scan_subjects(p, node, push)?,
        }
        if over(values.len(), cap) {
            return Ok(FetchOutcome::FailFast);
        }
    }
    Ok(FetchOutcome::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SemAct;
    use crate::ir::{SeId, ShapeIr, Tri};
    use crate::matcher::build::Builder;
    use crate::matcher::{ActMark, Acts, Budget, matches_with};
    use sparkles::io::{RdfFormat, Source};
    use sparkles::sparql::ctx::UNION_GRAPH_IRI;
    use sparkles::store::{Store, StoreOptions};
    use std::sync::Arc;

    const EX: &str = "http://ex.org/";

    fn store(trig: &str) -> Arc<Snapshot> {
        let s = Store::in_memory(StoreOptions::default());
        let text = format!("@prefix ex: <{EX}> .\n{trig}");
        s.load(&[Source::from_bytes(text.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        s.snapshot()
    }

    fn iri(snap: &Snapshot, local: &str) -> Id {
        snap.lookup_iri(&ex(local)).unwrap()
    }

    fn ex(local: &str) -> String {
        format!("{EX}{local}")
    }

    fn plan(snap: &Snapshot, shape: ShapeIr) -> (Ir, SnapPlan) {
        let ir = Ir {
            shapes: vec![shape],
            ..Default::default()
        };
        let plan = SnapPlan::new(snap, &ir);
        (ir, plan)
    }

    fn get(data: &DataGraph, plan: &SnapPlan, node: Id) -> (FetchOutcome, Neigh) {
        let mut n = Neigh::default();
        let r = fetch(data, plan, node, ShapeId(0), &mut n).unwrap();
        (r, n)
    }

    struct NoActs;
    impl Acts for NoActs {
        fn can_fail(&self, _: &[SemAct]) -> bool {
            false
        }
        fn on_tc(&mut self, _: &[SemAct], _: [Id; 3]) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn on_group(&mut self, _: &[SemAct], _: &[[Id; 3]]) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn on_shape(&mut self, _: &[SemAct], _: Id) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn mark(&self) -> ActMark {
            unreachable!()
        }
        fn rollback(&mut self, _: ActMark) {}
    }

    fn matches(ir: &Ir, n: &Neigh) -> Tri {
        let mut b = Budget::new(None);
        matches_with(&ir.shapes[0], n, &|_, _| Tri::True, &mut b, &mut NoActs).unwrap()
    }

    #[test]
    fn outgoing_and_incoming_arcs() {
        let snap = store("ex:a ex:p ex:b, ex:c ; ex:q 1 . ex:d ex:r ex:a . ex:e ex:r ex:a .");
        let mut b = Builder::default();
        let kids = vec![
            b.p(&ex("p"), 0, None),
            b.tc(&ex("r"), Dir::In, Some(SeId(0)), 0, None),
            b.p(&ex("absent"), 0, None),
        ];
        let root = b.each(kids, 1, Some(1));
        let (_, plan) = plan(&snap, b.shape(Some(root), &[], false));
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let (r, n) = get(&data, &plan, iri(&snap, "a"));
        assert_eq!(r, FetchOutcome::Ok);
        assert_eq!(n.values[0], vec![iri(&snap, "b"), iri(&snap, "c")]);
        assert_eq!(n.values[1], vec![iri(&snap, "d"), iri(&snap, "e")]);
        assert!(n.values[2].is_empty());
        assert_eq!(n.preds[2], Id::UNDEF);
        let (a, r, d) = (iri(&snap, "a"), iri(&snap, "r"), iri(&snap, "d"));
        assert_eq!(n.triple(Dir::In, 1, d), [d, r, a]);
        // a node not in the store has no arcs
        let (r, n) = get(&data, &plan, Id::local(3));
        assert_eq!(r, FetchOutcome::Ok);
        assert!(n.values.iter().all(|v| v.is_empty()));
    }

    #[test]
    fn closed_shapes() {
        let snap = store("ex:acme ex:name \"ACME\" ; ex:mayor ex:bob . ex:x ex:name \"X\" .");
        let mut b = Builder::default();
        let root = b.tc(&ex("name"), Dir::Out, None, 1, Some(1));
        let (ir, p) = plan(&snap, b.shape(Some(root), &[], true));
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let (r, n) = get(&data, &p, iri(&snap, "acme"));
        assert_eq!(r, FetchOutcome::FailFast);
        let violation = Some((iri(&snap, "mayor"), iri(&snap, "bob")));
        assert_eq!(n.closed_violation, violation);
        assert_eq!(matches(&ir, &n), Tri::False);
        // the whole neighbourhood, for explanations
        let mut n = Neigh::default();
        fetch_all(&data, &p, iri(&snap, "acme"), ShapeId(0), &mut n).unwrap();
        assert_eq!(n.closed_violation, violation);
        assert_eq!(n.values[0].len(), 1);
        let (r, n) = get(&data, &p, iri(&snap, "x"));
        assert_eq!((r, matches(&ir, &n)), (FetchOutcome::Ok, Tri::True));
        // CLOSED {}: every outgoing arc is forbidden
        let (_, p) = plan(&snap, Builder::default().shape(None, &[], true));
        assert_eq!(get(&data, &p, iri(&snap, "x")).0, FetchOutcome::FailFast);
    }

    #[test]
    fn counts_fail_fast_on_a_single_graph() {
        let snap = store(
            "ex:a ex:p 1, 2, 3 . \
             ex:x ex:parentOf ex:c . ex:y ex:parentOf ex:c . ex:z ex:parentOf ex:c .",
        );
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let shape = |pred: &str, dir: Dir, extra: &[&str]| {
            let mut b = Builder::default();
            let root = b.tc(&ex(pred), dir, Some(SeId(0)), 1, Some(2));
            b.shape(Some(root), extra, false)
        };
        // { ex:p . {1,2} } on three arcs
        let (_, p) = plan(&snap, shape("p", Dir::Out, &[]));
        assert_eq!(get(&data, &p, iri(&snap, "a")).0, FetchOutcome::FailFast);
        // EXTRA: unmatched arcs may be left over, so no bound
        let (_, p) = plan(&snap, shape("p", Dir::Out, &[&ex("p")]));
        let (r, n) = get(&data, &p, iri(&snap, "a"));
        assert_eq!((r, n.values[0].len()), (FetchOutcome::Ok, 3));
        // { ^ex:parentOf . {1,2} } on three parents
        let (ir, p) = plan(&snap, shape("parentOf", Dir::In, &[]));
        assert_eq!(get(&data, &p, iri(&snap, "c")).0, FetchOutcome::FailFast);
        let mut n = Neigh::default();
        fetch_all(&data, &p, iri(&snap, "c"), ShapeId(0), &mut n).unwrap();
        assert_eq!(n.values[0].len(), 3);
        assert_eq!(matches(&ir, &n), Tri::False);
    }

    #[test]
    fn no_fast_fail_under_the_union_graph() {
        // the same triple in two graphs is one arc of the merge
        let snap = store(
            "ex:g1 { ex:a ex:p ex:b . ex:c ex:r ex:a . } \
             ex:g2 { ex:a ex:p ex:b . ex:c ex:r ex:a . }",
        );
        let data = DataGraph::new(snap.clone(), Some(UNION_GRAPH_IRI), &[], &[]).unwrap();
        let mut b = Builder::default();
        let kids = vec![
            b.p(&ex("p"), 1, Some(1)),
            b.tc(&ex("r"), Dir::In, Some(SeId(0)), 1, Some(1)),
        ];
        let root = b.each(kids, 1, Some(1));
        let (ir, plan) = plan(&snap, b.shape(Some(root), &[], false));
        let (r, n) = get(&data, &plan, iri(&snap, "a"));
        assert_eq!(r, FetchOutcome::Ok);
        assert_eq!((n.values[0].len(), n.values[1].len()), (1, 1));
        assert_eq!(matches(&ir, &n), Tri::True);
    }

    /// The value expression of a constraint at a node, with references unknown.
    fn eval(ir: &Ir, nc: &crate::nc::NcPlan, se: crate::ir::SeId, v: Id) -> Tri {
        use crate::ir::Se;
        match &ir.ses[se.index()] {
            Se::Nc(n) => Tri::from(nc.check(*n, v).unwrap()),
            Se::And(xs) => xs.iter().fold(Tri::True, |a, &x| a.and(eval(ir, nc, x, v))),
            Se::Or(xs) => xs.iter().fold(Tri::False, |a, &x| a.or(eval(ir, nc, x, v))),
            Se::Not(x) => !eval(ir, nc, *x, v),
            Se::Ref(_) | Se::Shape(_) | Se::External => Tri::Unknown,
        }
    }

    /// The acceptance example's schema, compiled, on its data, before any typing.
    #[test]
    fn compiled_schema_before_typing() {
        let snap = store(
            "@prefix foaf: <http://xmlns.com/foaf/0.1/> .
             ex:alice a ex:Person ; foaf:name \"Alice\" ; foaf:age 30 ; foaf:knows ex:bob .
             ex:bob a ex:Person ; foaf:name \"Bob\" ; foaf:knows ex:alice .
             ex:carol a ex:Person ; foaf:age 200 .
             ex:dave a ex:Person ; foaf:name \"Dave\" .
             ex:acme a ex:Org ; foaf:name \"ACME\" ; ex:city \"Paris\" ; ex:mayor ex:bob .
             ex:kyoto a ex:Org ; foaf:name \"K\" ; ex:city \"Kyoto\" .",
        );
        let schema = crate::Schema::parse_shexc(
            "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
             PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
             start = @ex:Person
             ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ;
               foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
             ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city [\"Paris\" \"Kyoto\"] }",
            None,
        )
        .unwrap();
        let schema = crate::compile(&schema, &crate::NoImports).unwrap();
        let ir = schema.ir();
        let plan = SnapPlan::new(&snap, ir);
        let nc = crate::nc::NcPlan::new(&snap, &ir.ncs);
        let data = DataGraph::new(snap.clone(), None, &[], &[]).unwrap();
        let registry = crate::semact::Registry::new(false);
        let check = |node: &str, label: &str| {
            let kind = schema.label(&ex(label)).unwrap();
            let crate::ir::Se::Shape(sid) = ir.ses[ir.pairs[kind.index()].se.index()] else {
                panic!("{label} is not a shape")
            };
            let shape = &ir.shapes[sid.index()];
            let mut n = Neigh::default();
            if fetch(&data, &plan, iri(&snap, node), sid, &mut n).unwrap() == FetchOutcome::FailFast
            {
                return Tri::False;
            }
            let read =
                |v: Id, tc: crate::ir::TcId| eval(ir, &nc, shape.tcs[tc.index()].value.unwrap(), v);
            let mut cx = crate::semact::ActCtx::new(&registry, &snap);
            let mut budget = Budget::new(Some(1000));
            crate::matcher::matches(shape, &n, &read, &mut budget, &mut cx).unwrap()
        };
        // alice and bob know a person: undecided until the typing
        assert_eq!(check("alice", "Person"), Tri::Unknown);
        assert_eq!(check("bob", "Person"), Tri::Unknown);
        assert_eq!(check("dave", "Person"), Tri::True);
        // no name, age above 150
        assert_eq!(check("carol", "Person"), Tri::False);
        // CLOSED: ex:mayor is not allowed
        assert_eq!(check("acme", "Org"), Tri::False);
        assert_eq!(check("kyoto", "Org"), Tri::True);
        assert_eq!(check("alice", "Org"), Tri::False);
    }
}
