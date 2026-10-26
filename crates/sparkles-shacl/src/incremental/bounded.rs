//! Early-bounded reverse-path traversal for affected-target discovery.

use super::CPath;
use crate::ValidateOptions;
use crate::data::DataGraph;
use rustc_hash::FxHashSet;
use sparkles_core::error::{Budget, BudgetKind, Error, Result};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;
use sparkles_core::store::Chunk;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// One allowance is shared by all paths, shapes and both snapshots. Every scanned
/// row and retained frontier entry is charged before it can grow a collection.
pub(crate) struct Work<'a> {
    opts: &'a ValidateOptions,
    deadline: Option<Instant>,
    limit: u64,
    used: u64,
}

impl<'a> Work<'a> {
    pub(crate) fn new(opts: &'a ValidateOptions, limit: u64) -> Self {
        Self {
            opts,
            deadline: opts
                .deadline
                .into_iter()
                .chain(opts.timeout.map(|t| Instant::now() + t))
                .min(),
            limit,
            used: 0,
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self
            .opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if let Some(c) = &self.opts.cancellation {
            c.check()?;
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    pub(crate) fn charge(&mut self) -> Result<()> {
        self.check()?;
        let requested = self.used.saturating_add(1);
        if requested > self.limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::ValidationWork,
                limit: self.limit,
                requested,
            }));
        }
        self.used = requested;
        Ok(())
    }
}

fn push(out: &mut Vec<Id>, seen: &mut FxHashSet<Id>, n: Id, work: &mut Work<'_>) -> Result<()> {
    if !seen.contains(&n) {
        work.charge()?;
        seen.insert(n);
        out.push(n);
    }
    Ok(())
}

/// Unlike `CPath::eval`, this never materializes an entire high-degree predicate
/// step before the caller can enforce its allowance.
pub(crate) fn eval(
    path: &CPath,
    data: &DataGraph,
    node: Id,
    inverse: bool,
    work: &mut Work<'_>,
) -> Result<Vec<Id>> {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    visit(path, data, node, inverse, work, &mut |n, work| {
        push(&mut out, &mut seen, n, work)
    })?;
    Ok(out)
}

pub(crate) fn visit<'w>(
    path: &CPath,
    data: &DataGraph,
    node: Id,
    inverse: bool,
    work: &mut Work<'w>,
    emit: &mut dyn FnMut(Id, &mut Work<'w>) -> Result<()>,
) -> Result<()> {
    work.charge()?;
    match path {
        CPath::Pred(p) => {
            if matches!(node.tag(), Tag::Local | Tag::Undef)
                || matches!(p.tag(), Tag::Local | Tag::Undef)
            {
                return Ok(());
            }
            let (perm, prefix) = if inverse {
                (Perm::Pos, [p.0, node.0])
            } else {
                (Perm::Spo, [node.0, p.0])
            };
            project_visit(data, perm, &prefix, &[2], work, emit)?;
        }
        CPath::Inverse(p) => visit(p, data, node, !inverse, work, emit)?,
        CPath::Alt(paths) => {
            for p in paths {
                visit(p, data, node, inverse, work, emit)?;
            }
        }
        CPath::Seq(paths) => {
            if paths.is_empty() {
                return emit(node, work);
            }
            work.charge()?;
            let mut current = vec![node];
            for step in 0..paths.len() {
                let i = if inverse {
                    paths.len() - step - 1
                } else {
                    step
                };
                if step + 1 == paths.len() {
                    for n in current {
                        visit(&paths[i], data, n, inverse, work, emit)?;
                    }
                    return Ok(());
                }
                let mut next = Vec::new();
                let mut next_seen = FxHashSet::default();
                for n in current {
                    visit(&paths[i], data, n, inverse, work, &mut |v, work| {
                        push(&mut next, &mut next_seen, v, work)
                    })?;
                }
                current = next;
                if current.is_empty() {
                    break;
                }
            }
        }
        CPath::ZeroOrOne(p) => {
            emit(node, work)?;
            visit(p, data, node, inverse, work, emit)?;
        }
        CPath::ZeroOrMore(p) | CPath::OneOrMore(p) => {
            let mut seen = FxHashSet::default();
            if matches!(path, CPath::ZeroOrMore(_)) {
                emit(node, work)?;
                work.charge()?;
                seen.insert(node);
            }
            work.charge()?;
            let mut frontier = vec![node];
            while let Some(n) = frontier.pop() {
                visit(p, data, n, inverse, work, &mut |v, work| {
                    if !seen.contains(&v) {
                        emit(v, work)?;
                        work.charge()?;
                        seen.insert(v);
                        frontier.push(v);
                    }
                    Ok(())
                })?;
            }
        }
    }
    Ok(())
}

pub(crate) fn project_visit<'w>(
    data: &DataGraph,
    perm: Perm,
    prefix: &[u64],
    columns: &[usize],
    work: &mut Work<'w>,
    emit: &mut dyn FnMut(Id, &mut Work<'w>) -> Result<()>,
) -> Result<()> {
    if prefix
        .iter()
        .any(|&n| matches!(Id(n).tag(), Tag::Local | Tag::Undef))
    {
        return Ok(());
    }
    data.snap.scan(perm, prefix, |chunk| {
        work.check()?;
        let mut row = |k: &[u64; 4]| -> Result<()> {
            work.charge()?;
            if data.sel.accepts(k[3]) {
                for &column in columns {
                    emit(Id(k[column]), work)?;
                }
            }
            Ok(())
        };
        match chunk {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    row(&b.key(i))?;
                }
            }
            Chunk::Row(k) => row(&k)?,
        }
        Ok(true)
    })
}

pub(crate) fn exists(
    data: &DataGraph,
    perm: Perm,
    prefix: &[u64],
    work: &mut Work<'_>,
) -> Result<bool> {
    if prefix
        .iter()
        .any(|&n| matches!(Id(n).tag(), Tag::Local | Tag::Undef))
    {
        return Ok(false);
    }
    let mut found = false;
    data.snap.scan(perm, prefix, |chunk| {
        work.check()?;
        let mut row = |k: &[u64; 4]| -> Result<bool> {
            work.charge()?;
            found = data.sel.accepts(k[3]);
            Ok(!found)
        };
        match chunk {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    if !row(&b.key(i))? {
                        return Ok(false);
                    }
                }
            }
            Chunk::Row(k) => {
                if !row(&k)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    })?;
    Ok(found)
}

/// Stream one index projection, admitting retained values before growth and checking
/// controls even while rows from excluded graphs are skipped.
pub(crate) fn project(
    data: &DataGraph,
    perm: Perm,
    prefix: &[u64],
    columns: &[usize],
    work: &mut Work<'_>,
) -> Result<Vec<Id>> {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    project_visit(data, perm, prefix, columns, work, &mut |n, work| {
        push(&mut out, &mut seen, n, work)
    })?;
    Ok(out)
}

pub(crate) fn instances_visit<'a>(
    data: &DataGraph,
    class: Id,
    work: &mut Work<'a>,
    emit: &mut dyn FnMut(Id, &mut Work<'a>) -> Result<()>,
) -> Result<()> {
    let path = CPath::Seq(vec![
        CPath::Inverse(Box::new(CPath::ZeroOrMore(Box::new(CPath::Pred(
            data.sub_class_of,
        ))))),
        CPath::Inverse(Box::new(CPath::Pred(data.rdf_type))),
    ]);
    visit(&path, data, class, false, work, emit)
}

pub(crate) fn instances(data: &DataGraph, class: Id, work: &mut Work<'_>) -> Result<Vec<Id>> {
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    instances_visit(data, class, work, &mut |n, work| {
        push(&mut out, &mut seen, n, work)
    })?;
    Ok(out)
}

fn compile(path: &super::PropertyPath, data: &DataGraph, work: &mut Work<'_>) -> Result<CPath> {
    use super::PropertyPath as P;
    work.charge()?;
    Ok(match path {
        P::Predicate(p) => CPath::Pred(data.snap.lookup_iri(p.as_str()).unwrap_or(Id::UNDEF)),
        P::Inverse(p) => CPath::Inverse(Box::new(compile(p, data, work)?)),
        P::ZeroOrMore(p) => CPath::ZeroOrMore(Box::new(compile(p, data, work)?)),
        P::OneOrMore(p) => CPath::OneOrMore(Box::new(compile(p, data, work)?)),
        P::ZeroOrOne(p) => CPath::ZeroOrOne(Box::new(compile(p, data, work)?)),
        P::Sequence(paths) | P::Alternative(paths) => {
            let mut out = Vec::new();
            for path in paths {
                work.charge()?;
                out.push(compile(path, data, work)?);
            }
            if matches!(path, P::Sequence(_)) {
                CPath::Seq(out)
            } else {
                CPath::Alt(out)
            }
        }
    })
}

/// Conservative candidate roots per localized shape. Global shapes are marked
/// separately and require complete bounded actual-target admission by the caller.
/// Both data states use the vocabulary of one writer's before/after snapshots.
pub(super) fn candidates(
    model: &super::Model,
    states: [&DataGraph; 2],
    changes: &[[Id; 3]],
    touched: &[Id],
    work: &mut Work<'_>,
) -> Result<Vec<Option<Vec<Id>>>> {
    use super::{FxHashMap, Plan};
    let post = states[0];
    let mut extra = Vec::new();
    if model.uses_classes() {
        let sub = post.snap.lookup_iri(super::rdfs::SUB_CLASS_OF.as_str());
        if let Some(sub) = sub {
            let ty = post
                .snap
                .lookup_iri(super::rdf::TYPE.as_str())
                .unwrap_or(Id::UNDEF);
            let class_instances = CPath::Seq(vec![
                CPath::Inverse(Box::new(CPath::ZeroOrMore(Box::new(CPath::Pred(sub))))),
                CPath::Inverse(Box::new(CPath::Pred(ty))),
            ]);
            let mut seen = FxHashSet::default();
            for &[class, p, _] in changes {
                work.charge()?;
                if p != sub {
                    continue;
                }
                for data in states {
                    for instance in eval(&class_instances, data, class, false, work)? {
                        if !seen.contains(&instance) {
                            work.charge()?;
                            seen.insert(instance);
                            extra.push([instance, ty, class]);
                        }
                    }
                }
            }
        }
    }
    let mut cache = FxHashMap::default();
    let mut selections = Vec::new();
    for plan in &model.plans {
        work.charge()?;
        let Some(Plan::Local(groups)) = plan else {
            selections.push(None);
            continue;
        };
        let mut nodes = Vec::new();
        let mut seen = FxHashSet::default();
        for group in groups {
            work.charge()?;
            let matches = |preds: &super::Preds, p: Id| {
                preds.any
                    || preds
                        .set
                        .iter()
                        .any(|name| post.snap.lookup_iri(name.as_str()) == Some(p))
            };
            let mut starts = Vec::new();
            let mut starts_seen = FxHashSet::default();
            // Forms validate every constraint on resources whose shape reads a
            // directly touched node, including pre-existing failures on another
            // property of that node. Exact predicate deltas alone are insufficient.
            for &n in touched {
                push(&mut starts, &mut starts_seen, n, work)?;
            }
            for &[s, p, o] in changes.iter().chain(extra.iter()) {
                work.charge()?;
                if matches(&group.out, p) {
                    push(&mut starts, &mut starts_seen, s, work)?;
                }
                if matches(&group.inn, p) {
                    push(&mut starts, &mut starts_seen, o, work)?;
                }
            }
            if starts.is_empty() {
                continue;
            }
            if group.prefix.is_empty() {
                for n in starts {
                    push(&mut nodes, &mut seen, n, work)?;
                }
                continue;
            }
            if !cache.contains_key(group.prefix.as_slice()) {
                work.charge()?;
                let mut prefix = Vec::new();
                for p in &group.prefix {
                    work.charge()?;
                    prefix.push(compile(p, post, work)?);
                }
                cache.insert(
                    group.prefix.as_slice(),
                    CPath::Inverse(Box::new(CPath::Seq(prefix))),
                );
            }
            let path = &cache[group.prefix.as_slice()];
            for start in starts {
                for data in states {
                    for n in eval(path, data, start, false, work)? {
                        push(&mut nodes, &mut seen, n, work)?;
                    }
                }
            }
        }
        nodes.sort_unstable();
        selections.push(Some(nodes));
    }
    Ok(selections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RdfFormat, Shapes};
    use oxrdf::NamedNode;
    use sparkles_core::io::Source;
    use sparkles_core::store::{Store, StoreOptions};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    fn data(text: &str, opts: &ValidateOptions) -> DataGraph {
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                text.as_bytes().to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let shapes = Shapes::parse("", RdfFormat::Turtle, None).unwrap();
        DataGraph::new(
            store.snapshot(),
            opts.data_graph.as_deref(),
            &opts.extra_graphs,
            &opts.exclude_graphs,
            &shapes,
        )
        .unwrap()
        .0
    }

    fn id(data: &DataGraph, name: &str) -> Id {
        data.snap
            .lookup_iri(&format!("http://ex.org/{name}"))
            .unwrap()
    }

    #[test]
    fn localized_candidates_find_far_targets_and_deleted_connecting_paths() {
        use crate::incremental::Model;
        use oxrdf::{Literal, Term};
        use sparkles_core::sparql::{QueryOptions, update};
        let store = Store::in_memory(StoreOptions::default());
        store.load(&[Source::from_bytes(b"@prefix ex: <http://ex.org/> . ex:r ex:a ex:b . ex:b ex:b ex:c . ex:c ex:c ex:d . ex:d ex:value \"old\" .".to_vec(), RdfFormat::Turtle, None)]).unwrap();
        let before = store.snapshot();
        update::update(&store, "PREFIX ex: <http://ex.org/> DELETE DATA { ex:c ex:c ex:d } ; INSERT DATA { ex:d ex:value \"new\" }", &QueryOptions::default()).unwrap();
        let after = store.snapshot();
        let shapes = Shapes::parse("@prefix ex: <http://ex.org/> . @prefix sh: <http://www.w3.org/ns/shacl#> . ex:S a sh:NodeShape ; sh:targetNode ex:r ; sh:property [ sh:path (ex:a ex:b ex:c ex:value) ; sh:maxCount 1 ] .", RdfFormat::Turtle, None).unwrap();
        let model = Model::new(&shapes);
        let post = DataGraph::new(after.clone(), None, &[], &[], &shapes)
            .unwrap()
            .0;
        let pre = DataGraph::new(before, None, &[], &[], &shapes).unwrap().0;
        let changes = [[
            id(&post, "d"),
            id(&post, "value"),
            after
                .lookup_term(&Term::Literal(Literal::new_simple_literal("new")))
                .unwrap(),
        ]];
        let opts = ValidateOptions::default();
        let selected = candidates(
            &model,
            [&post, &pre],
            &changes,
            &[],
            &mut Work::new(&opts, 10_000),
        )
        .unwrap();
        let si = shapes
            .shapes
            .iter()
            .position(|s| s.node == Term::NamedNode(NamedNode::new("http://ex.org/S").unwrap()))
            .unwrap();
        assert!(
            selected[si].as_ref().unwrap().contains(&id(&post, "r")),
            "pre-state reverse paths must retain the far target"
        );
    }

    #[test]
    fn bounded_paths_match_existing_evaluator_for_every_operator_and_inverse() {
        let opts = ValidateOptions::default();
        let data = data(
            "@prefix ex: <http://ex.org/> . ex:a ex:p ex:b, ex:c . ex:b ex:p ex:c . ex:c ex:p ex:a . ex:b ex:q ex:d .",
            &opts,
        );
        let p = CPath::Pred(id(&data, "p"));
        let q = CPath::Pred(id(&data, "q"));
        let paths = [
            p.clone(),
            CPath::Inverse(Box::new(p.clone())),
            CPath::Alt(vec![p.clone(), q.clone()]),
            CPath::Seq(vec![p.clone(), q]),
            CPath::ZeroOrOne(Box::new(p.clone())),
            CPath::ZeroOrMore(Box::new(p.clone())),
            CPath::OneOrMore(Box::new(p)),
        ];
        for path in paths {
            for inverse in [false, true] {
                let ordinary = if inverse {
                    CPath::Inverse(Box::new(path.clone()))
                } else {
                    path.clone()
                };
                let mut expected = ordinary.eval(&data, id(&data, "a")).unwrap();
                let mut actual = eval(
                    &path,
                    &data,
                    id(&data, "a"),
                    inverse,
                    &mut Work::new(&opts, 10_000),
                )
                .unwrap();
                expected.sort_unstable();
                actual.sort_unstable();
                assert_eq!(actual, expected, "{path:?}, inverse={inverse}");
            }
        }
    }

    #[test]
    fn dense_predicate_refuses_during_scan_and_shares_work_across_calls() {
        let opts = ValidateOptions::default();
        let mut text = String::from("@prefix ex: <http://ex.org/> .\n");
        for n in 0..10_000 {
            text.push_str(&format!("ex:a ex:p ex:n{n} .\n"));
        }
        let data = data(&text, &opts);
        let path = CPath::Pred(id(&data, "p"));
        let mut work = Work::new(&opts, 32);
        let error = eval(&path, &data, id(&data, "a"), false, &mut work).unwrap_err();
        assert!(matches!(
            error,
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::ValidationWork,
                limit: 32,
                requested: 33
            })
        ));
        assert_eq!(work.used, 32);
        assert!(matches!(
            eval(&path, &data, id(&data, "a"), false, &mut work),
            Err(Error::BudgetExceeded(_))
        ));
    }

    #[test]
    fn selected_graphs_filter_before_path_deduplication() {
        let opts = ValidateOptions {
            data_graph: Some("urn:x-arq:UnionGraph".into()),
            exclude_graphs: vec!["urn:hidden".into()],
            ..Default::default()
        };
        let data = data(
            "@prefix ex: <http://ex.org/> . GRAPH <urn:hidden> { ex:a ex:p ex:x . } GRAPH <urn:visible> { ex:a ex:p ex:y . }",
            &opts,
        );
        let result = eval(
            &CPath::Pred(id(&data, "p")),
            &data,
            id(&data, "a"),
            false,
            &mut Work::new(&opts, 100),
        )
        .unwrap();
        assert_eq!(result, [id(&data, "y")]);
    }

    #[test]
    fn shared_discovery_observes_cancel_and_absolute_deadline() {
        let cancel = Arc::new(AtomicBool::new(false));
        let opts = ValidateOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        let data = data("@prefix ex: <http://ex.org/> . ex:a ex:p ex:b .", &opts);
        let path = CPath::Pred(id(&data, "p"));
        let mut work = Work::new(&opts, 100);
        assert_eq!(
            eval(&path, &data, id(&data, "a"), false, &mut work)
                .unwrap()
                .len(),
            1
        );
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            eval(&path, &data, id(&data, "a"), false, &mut work),
            Err(Error::Cancelled)
        ));
        let opts = ValidateOptions {
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        assert!(matches!(
            eval(
                &path,
                &data,
                id(&data, "a"),
                false,
                &mut Work::new(&opts, 100)
            ),
            Err(Error::Timeout)
        ));
    }

    #[test]
    fn inverse_missing_predicate_does_not_reinterpret_local_ids() {
        let opts = ValidateOptions::default();
        let data = data(
            "<http://ex.org/a> <http://ex.org/p> <http://ex.org/b> .",
            &opts,
        );
        let missing = CPath::compile(
            &super::super::PropertyPath::Predicate(NamedNode::new("urn:missing").unwrap()),
            &mut |_| Id::local(99),
        );
        assert!(
            eval(
                &missing,
                &data,
                id(&data, "a"),
                true,
                &mut Work::new(&opts, 100)
            )
            .unwrap()
            .is_empty()
        );
    }
}
