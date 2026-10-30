//! The additive bounded path. Legacy read/write entry points retain their behavior.
use super::*;
use crate::access::preparation::{Buffer, Map, Text, compare_text, sort_by};
use crate::access::{Exemptions, GraphGuard, GraphRule, Preparation, PreparedView, Reservation};
mod key;

pub(crate) fn prepare(
    original: &GraphAccess,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
) -> Result<PreparedView> {
    preparation.stage("preparing permission view")?;
    let mut held = preparation.reserve(
        snap.retained_preparation_bytes()
            .saturating_add(mask_storage(snap))
            .saturating_add(1024),
    )?;
    // Shared policy definitions are retained too. Inspect their sizes with controls
    // before taking the returned owner's Arc references.
    if let Some(rules) = &original.triples {
        let mut bytes = 1024u64
            .saturating_add(
                (rules.rules.capacity() as u64).saturating_mul(std::mem::size_of::<Rule>() as u64),
            )
            .saturating_add(strings_bytes(&rules.caller.roles, preparation)?)
            .saturating_add(strings_bytes(&rules.caller.groups, preparation)?);
        if let Some(user) = &rules.caller.user {
            bytes = bytes.saturating_add(user.capacity() as u64);
        }
        for rule in &rules.rules {
            preparation.check()?;
            bytes = bytes.saturating_add(protection_bytes(&rule.protection, preparation)?);
            bytes = bytes.saturating_add(graphs_bytes(&rule.read, preparation)?);
            bytes = bytes.saturating_add(graphs_bytes(&rule.write, preparation)?);
        }
        held.merge(preparation.reserve(bytes)?);
    }
    let normalize = original.read.undecided() || original.write.undecided();
    let (read, read_held) = bind_graphs(&original.read, snap, preparation, normalize)?;
    held.merge(read_held);
    let (write, write_held) = bind_graphs(&original.write, snap, preparation, normalize)?;
    held.merge(write_held);
    let access = Arc::new(GraphAccess {
        read,
        write,
        triples: original.triples.clone(),
    });
    let mut hides = false;
    if let Some(rules) = &access.triples {
        for rule in &rules.rules {
            preparation.check()?;
            if !rule.read.is_all() {
                hides = true;
                break;
            }
        }
    }
    let Some(rules) = access.triples.as_ref().filter(|_| hides) else {
        preparation.check()?;
        return Ok(PreparedView::new(access, snap.clone(), held));
    };
    if snap.mask.is_some() {
        preparation.check()?;
        return Ok(PreparedView::new(access, snap.clone(), held));
    }
    for rule in &rules.rules {
        preparation.check()?;
        if rule.read.is_all() {
            continue;
        }
        if rule.protection.pattern.is_some() {
            return Err(Error::Unsupported(
                "controlled permission preparation does not yet support protection pattern planning".into(),
            ));
        }
    }
    let (key, key_held) = cache_key(&access, rules, preparation)?;
    let existing = snap.counts.prepared_mask_slot(&key, preparation)?;
    if let Some(slot) = &existing {
        let slot = preparation.lock(slot)?;
        if let Some(cached) = &*slot {
            held.merge(
                preparation.reserve(
                    cached
                        .retained_preparation_bytes()
                        .saturating_add(mask_storage(cached)),
                )?,
            );
            preparation.check()?;
            return Ok(PreparedView::new(access, cached.clone(), held));
        }
    }
    // Build outside the slot lock. User progress and controlled provider helpers
    // never execute while this cache lock is owned. A cold race may build twice.
    let hidden = hidden(&access, rules, snap, preparation)?;
    let (snapshot, derived) = apply_controlled(snap, hidden, &key, preparation)?;
    held.merge(derived);
    preparation.check()?;
    if let Some(slot) = existing {
        let mut slot = preparation.lock(&slot)?;
        if slot.is_none() {
            *slot = Some(snapshot.clone());
        }
    } else {
        snap.counts
            .cache_prepared_mask(key, snapshot.clone(), preparation)?;
    }
    drop(key_held);
    preparation.check()?;
    Ok(PreparedView::new(access, snapshot, held))
}

fn mask_storage(snap: &Snapshot) -> u64 {
    snap.mask.as_ref().map_or(0, |mask| {
        (mask.hidden.capacity() as u64)
            .saturating_mul(32)
            .saturating_add(mask.key.capacity() as u64)
            .saturating_add(2048)
    })
}

fn strings_bytes(values: &Vec<String>, preparation: &Preparation) -> Result<u64> {
    let mut bytes = 64u64.saturating_add(
        (values.capacity() as u64).saturating_mul(std::mem::size_of::<String>() as u64),
    );
    for value in values {
        preparation.check()?;
        bytes = bytes
            .saturating_add(value.capacity() as u64)
            .saturating_add(64);
    }
    Ok(bytes)
}

fn graphs_bytes(graphs: &Graphs, preparation: &Preparation) -> Result<u64> {
    let Graphs::Only(rule) = graphs else {
        return Ok(0);
    };
    let mut bytes = 512u64.saturating_add(
        (rule.guards.capacity() as u64).saturating_mul(std::mem::size_of::<GraphGuard>() as u64),
    );
    for values in [&rule.iris, &rule.patterns, &rule.protected, &rule.excluded] {
        bytes = bytes.saturating_add(strings_bytes(values, preparation)?);
    }
    for guard in &rule.guards {
        preparation.check()?;
        bytes = bytes
            .saturating_add(guard.pattern.capacity() as u64)
            .saturating_add(128)
            .saturating_add(strings_bytes(&guard.exempt, preparation)?);
    }
    Ok(bytes)
}

fn protection_bytes(protection: &Protection, preparation: &Preparation) -> Result<u64> {
    let mut bytes = 1024u64.saturating_add(protection.name.capacity() as u64);
    for values in [&protection.predicates, &protection.classes]
        .into_iter()
        .flatten()
    {
        bytes = bytes.saturating_add(strings_bytes(values, preparation)?);
    }
    if let Some(graphs) = &protection.graphs {
        bytes = bytes.saturating_add(graphs_bytes(graphs, preparation)?);
    }
    if let Some(pattern) = &protection.pattern {
        bytes = bytes.saturating_add(pattern.capacity() as u64);
    }
    for (key, value) in &protection.prefixes {
        preparation.check()?;
        bytes = bytes
            .saturating_add(key.capacity() as u64)
            .saturating_add(value.capacity() as u64)
            .saturating_add(256);
    }
    Ok(bytes)
}

fn copy_strings(
    values: &[String],
    preparation: &Preparation,
) -> Result<(Vec<String>, Reservation)> {
    let mut output = Exemptions::new(preparation)?;
    for value in values {
        output.push(value)?;
    }
    output.into_parts(preparation)
}

fn normalize_strings(values: &mut Vec<String>, preparation: &Preparation) -> Result<()> {
    sort_by(values, preparation, |a, b| compare_text(a, b, preparation))?;
    let mut kept = 0;
    for index in 0..values.len() {
        preparation.check()?;
        if kept == 0 || !compare_text(&values[kept - 1], &values[index], preparation)?.is_eq() {
            values.swap(kept, index);
            kept += 1;
        }
    }
    values.truncate(kept);
    Ok(())
}

fn compare_guards(
    a: &GraphGuard,
    b: &GraphGuard,
    preparation: &Preparation,
) -> Result<std::cmp::Ordering> {
    let order = compare_text(&a.pattern, &b.pattern, preparation)?;
    if !order.is_eq() {
        return Ok(order);
    }
    for (a, b) in a.exempt.iter().zip(&b.exempt) {
        let order = compare_text(a, b, preparation)?;
        if !order.is_eq() {
            return Ok(order);
        }
    }
    Ok(a.exempt.len().cmp(&b.exempt.len()))
}

fn bind_graphs(
    graphs: &Graphs,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
    normalize: bool,
) -> Result<(Graphs, Reservation)> {
    let mut held = preparation.reserve(512)?;
    let Graphs::Only(rule) = graphs else {
        return Ok((Graphs::All, held));
    };
    // Legacy binding normalizes both sides when either side is undecided.
    // With no provider on either side, preserve deliberately static ordering.
    let (iris, charge) = copy_strings(&rule.iris, preparation)?;
    held.merge(charge);
    let (patterns, charge) = copy_strings(&rule.patterns, preparation)?;
    held.merge(charge);
    let (protected, charge) = copy_strings(&rule.protected, preparation)?;
    held.merge(charge);
    let (excluded, charge) = copy_strings(&rule.excluded, preparation)?;
    held.merge(charge);
    let mut guards = Buffer::new(preparation)?;
    for guard in &rule.guards {
        preparation.check()?;
        let mut pattern = Text::new(preparation)?;
        pattern.push_str(&guard.pattern)?;
        let (pattern, charge) = pattern.finish()?;
        held.merge(charge);
        let (mut exempt, charge) = match &guard.decide {
            Some(provider) => provider
                .0
                .exempt_controlled(snap, preparation)?
                .into_parts(preparation)?,
            None => copy_strings(&guard.exempt, preparation)?,
        };
        held.merge(charge);
        if normalize {
            normalize_strings(&mut exempt, preparation)?;
        }
        guards.push(GraphGuard {
            pattern,
            exempt,
            decide: None,
        })?;
    }
    if normalize {
        sort_by(&mut guards, preparation, |a, b| {
            compare_guards(a, b, preparation)
        })?;
        let mut kept = 0;
        for index in 0..guards.len() {
            preparation.check()?;
            if kept == 0 || !compare_guards(&guards[kept - 1], &guards[index], preparation)?.is_eq()
            {
                guards.swap(kept, index);
                kept += 1;
            }
        }
        guards.truncate(kept);
    }
    let (guards, charge) = guards.into_parts();
    held.merge(charge);
    let mut rule = GraphRule {
        default_graph: rule.default_graph,
        blank_nodes: rule.blank_nodes,
        iris,
        patterns,
        protected,
        excluded,
        guards,
    };
    if normalize {
        for values in [
            &mut rule.iris,
            &mut rule.patterns,
            &mut rule.protected,
            &mut rule.excluded,
        ] {
            normalize_strings(values, preparation)?;
        }
    }
    preparation.check()?;
    Ok((Graphs::Only(rule), held))
}

fn cache_key(
    access: &GraphAccess,
    rules: &TripleRules,
    preparation: &Preparation,
) -> Result<(String, Reservation)> {
    key::serialize(access, rules, preparation)
}

fn closure(
    snap: &Snapshot,
    names: &[String],
    subclasses: bool,
    preparation: &Preparation,
) -> Result<Map<u64, ()>> {
    let mut classes = Map::new(preparation)?;
    let mut frontier = Buffer::new(preparation)?;
    for name in names {
        if let Some(id) = preparation.lookup_iri(snap, name)?
            && classes.insert(id.0, ())?.is_none()
        {
            frontier.push(id.0)?;
        }
    }
    if subclasses && let Some(predicate) = preparation.lookup_iri(snap, SUB_CLASS_OF)? {
        preparation.stage("preparing permission class closure")?;
        while let Some(class) = frontier.pop() {
            preparation.scan(snap, Perm::Pos, &[predicate.0, class], |quad| {
                if classes.insert(quad[0].0, ())?.is_none() {
                    frontier.push(quad[0].0)?;
                }
                Ok(true)
            })?;
        }
    }
    preparation.check()?;
    Ok(classes)
}

fn members(
    snap: &Snapshot,
    classes: &Map<u64, ()>,
    preparation: &Preparation,
) -> Result<Map<u64, ()>> {
    let mut members = Map::new(preparation)?;
    if let Some(predicate) = preparation.lookup_iri(snap, RDF_TYPE)? {
        for &class in classes.keys() {
            preparation.check()?;
            preparation.scan(snap, Perm::Pos, &[predicate.0, class], |quad| {
                members.insert(quad[0].0, ())?;
                Ok(true)
            })?;
        }
    }
    Ok(members)
}

fn predicates(
    snap: &Snapshot,
    protection: &Protection,
    preparation: &Preparation,
) -> Result<Option<Map<u64, ()>>> {
    let Some(patterns) = &protection.predicates else {
        return Ok(None);
    };
    let mut ids = Map::new(preparation)?;
    let mut wildcard = false;
    for pattern in patterns {
        preparation.check()?;
        if preparation.has_star(pattern)? {
            wildcard = true;
        } else if let Some(id) = preparation.lookup_iri(snap, pattern)? {
            ids.insert(id.0, ())?;
        }
    }
    if wildcard {
        let mut previous = None;
        preparation.scan(snap, Perm::Pso, &[], |quad| {
            if previous != Some(quad[1]) {
                previous = Some(quad[1]);
                preparation.with_term(snap, quad[1], |term| {
                    if let Some(Term::NamedNode(node)) = term
                        && preparation.covers_predicate(protection, node.as_str())?
                    {
                        ids.insert(quad[1].0, ())?;
                    }
                    Ok(())
                })?;
            }
            Ok(true)
        })?;
    }
    Ok(Some(ids))
}

fn hidden(
    access: &GraphAccess,
    rules: &TripleRules,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
) -> Result<Buffer<Key>> {
    let mut hidden = Buffer::new(preparation)?;
    let mut read = Map::new(preparation)?;
    for rule in &rules.rules {
        preparation.check()?;
        if rule.read.is_all() {
            continue;
        }
        preparation.stage("preparing permission rule")?;
        let protection = &rule.protection;
        let predicates = predicates(snap, protection, preparation)?;
        let members = protection
            .classes
            .as_ref()
            .map(|names| {
                members(
                    snap,
                    &closure(snap, names, protection.subclasses, preparation)?,
                    preparation,
                )
            })
            .transpose()?;
        let mut by_graph = Map::new(preparation)?;
        let mut visit = |quad: [Id; 4]| -> Result<bool> {
            preparation.check()?;
            let graph = quad[3];
            let hide = if let Some(&hide) = by_graph.get(&graph.0) {
                hide
            } else {
                let readable = if let Some(&readable) = read.get(&graph.0) {
                    readable
                } else {
                    let readable = if graph == Id::DEFAULT_GRAPH {
                        access.read.default_graph()
                    } else {
                        preparation.with_term(snap, graph, |term| match term {
                            Some(term) => preparation.allows(&access.read, Some(term)),
                            None => Ok(false),
                        })?
                    };
                    read.insert(graph.0, readable)?;
                    readable
                };
                let hide = readable
                    && if graph == Id::DEFAULT_GRAPH {
                        preparation.covers_graph(protection, None)?
                            && !preparation.allows(&rule.read, None)?
                    } else {
                        preparation.with_term(snap, graph, |term| match term {
                            Some(term) => Ok(preparation.covers_graph(protection, Some(term))?
                                && !preparation.allows(&rule.read, Some(term))?),
                            None => Ok(false),
                        })?
                    };
                by_graph.insert(graph.0, hide)?;
                hide
            };
            if hide {
                if hidden.len() as u64 >= rules.limits.max_hidden {
                    return Err(too_many(
                        &rules.limits,
                        rules.limits.max_hidden.saturating_add(1),
                    ));
                }
                hidden.push([quad[0].0, quad[1].0, quad[2].0, quad[3].0])?;
            }
            Ok(true)
        };
        match (&members, &predicates) {
            (Some(members), predicates) => {
                for &subject in members.keys() {
                    preparation.scan(snap, Perm::Spo, &[subject], |quad| {
                        if predicates
                            .as_ref()
                            .is_none_or(|ids| ids.contains_key(&quad[1].0))
                        {
                            visit(quad)?;
                        }
                        Ok(true)
                    })?;
                }
            }
            (None, Some(predicates)) => {
                for &predicate in predicates.keys() {
                    preparation.scan(snap, Perm::Pso, &[predicate], &mut visit)?;
                }
            }
            (None, None) => preparation.scan(snap, Perm::Gspo, &[], &mut visit)?,
        }
    }
    preparation.stage("ordering permission mask")?;
    sort_by(&mut hidden, preparation, |a, b| Ok(a.cmp(b)))?;
    let mut kept = 0;
    for index in 0..hidden.len() {
        preparation.check()?;
        if kept == 0 || hidden[kept - 1] != hidden[index] {
            hidden.swap(kept, index);
            kept += 1;
        }
    }
    hidden.truncate(kept);
    Ok(hidden)
}

fn apply_controlled(
    snap: &Snapshot,
    hidden: Buffer<Key>,
    key: &str,
    preparation: &Preparation,
) -> Result<(Arc<Snapshot>, Reservation)> {
    preparation.stage("applying permission mask")?;
    let quads = (snap.delta.inserts() as u64)
        .saturating_add(snap.delta.deletes() as u64)
        .saturating_add(hidden.len() as u64);
    let mut held = preparation.reserve(quads.saturating_mul(2048).saturating_add(128 << 10))?;
    let paths = (u64::from(quads.saturating_add(1).ilog2()) / 3 + 2)
        .saturating_mul(64 << 10)
        .saturating_add(2048);
    let _temporary = preparation.reserve(paths)?;
    let mut delta = snap.delta.clone();
    for key in hidden.iter() {
        preparation.check()?;
        let quad = Perm::Spo.to_quad(key);
        if delta.ins[Perm::Spo.index()].contains(key) {
            delta.remove_inserted(&quad);
        } else {
            for perm in Perm::ALL {
                preparation.check()?;
                delta.del[perm.index()].insert(perm.to_key(&quad));
            }
        }
    }
    let mut text = Text::new(preparation)?;
    text.push_str(key)?;
    let (key, charge) = text.finish()?;
    held.merge(charge);
    let (hidden, charge) = hidden.into_parts();
    held.merge(charge);
    preparation.check()?;
    let snapshot = Arc::new(Snapshot {
        delta,
        counts: Default::default(),
        mask: Some(Arc::new(Mask {
            id: MASK_IDS.fetch_add(1, Ordering::Relaxed),
            key,
            hidden,
        })),
        ..snap.clone()
    });
    Ok((snapshot, held))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Control, Progress};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn view() -> GraphAccess {
        GraphAccess::with_triples(
            Graphs::All,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "secret".into(),
                        predicates: Some(vec!["urn:p".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        )
    }

    #[test]
    fn actual_held_mask_slot_observes_cancel_deadline_and_no_wait_before_release() {
        for mode in 0..3 {
            let store = crate::store::Store::in_memory(Default::default());
            crate::sparql::update::update(
                &store,
                "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
                &Default::default(),
            )
            .unwrap();
            let snap = store.snapshot();
            let view = view();
            let prep = Preparation::new(Control::none(), 8 << 20).unwrap();
            let (key, _key_held) = cache_key(&view, view.triples.as_ref().unwrap(), &prep).unwrap();
            let slot = snap.counts.mask_slot(&key);
            let held = slot.lock();
            let (entered_tx, entered_rx) = mpsc::sync_channel(1);
            let control = Control {
                progress: Progress::new(move |_, message| {
                    if message == "waiting for view preparation" {
                        let _ = entered_tx.try_send(());
                    }
                }),
                deadline: (mode == 1).then(|| Instant::now() + Duration::from_millis(300)),
                ..Control::none()
            };
            let deadline = control.deadline;
            let cancel = control.cancel.clone();
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let prep = Preparation::new(control, 8 << 20)
                    .unwrap()
                    .with_no_wait(mode == 2);
                done_tx
                    .send(view.masked_controlled(&snap, &prep).map(drop))
                    .unwrap();
            });
            let entered = if mode != 2 {
                entered_rx.recv_timeout(Duration::from_secs(2)).is_ok()
            } else {
                true
            };
            if mode == 0 {
                cancel.cancel();
            }
            if let Some(deadline) = deadline {
                while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                    std::thread::park_timeout(remaining);
                }
            }
            // The physical mutex is still held here. Cleanup releases it only
            // after the bounded call has been required to settle.
            let result = done_rx.recv_timeout(Duration::from_secs(2));
            drop(held);
            worker.join().unwrap();
            assert!(entered, "the actual failed try_lock was not observed");
            let result =
                result.expect("view preparation did not settle while the real mask slot was held");
            match mode {
                0 => assert!(matches!(result, Err(Error::Cancelled))),
                1 => assert!(matches!(result, Err(Error::Timeout))),
                _ => assert!(
                    matches!(result, Err(Error::Invalid(ref message)) if message.contains("no_wait"))
                ),
            }
        }
    }

    #[test]
    fn returned_view_owns_its_allowance_and_releases_it_on_drop() {
        let store = crate::store::Store::in_memory(Default::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            &Default::default(),
        )
        .unwrap();
        let prep = Preparation::new(Control::none(), 8 << 20).unwrap();
        let baseline = prep.reserve(0).unwrap();
        let view = view().masked_controlled(&store.snapshot(), &prep).unwrap();
        // A live returned owner leaves less than the complete cap available.
        assert!(prep.reserve((8 << 20) - 256).is_err());
        drop(view);
        let all = prep.reserve((8 << 20) - 256).unwrap();
        drop(all);
        drop(baseline);
    }

    #[test]
    fn cancellation_during_ordering_installs_no_partial_cache_entry() {
        let store = crate::store::Store::in_memory(Default::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        let view = view();
        let control = Control::none();
        let cancel = control.cancel.clone();
        let control = Control {
            progress: Progress::new(move |_, stage| {
                if stage == "ordering permission mask" {
                    cancel.cancel();
                }
            }),
            ..control
        };
        let preparation = Preparation::new(control, 8 << 20).unwrap();
        assert!(matches!(
            view.masked_controlled(&snap, &preparation),
            Err(Error::Cancelled)
        ));
        let clean = Preparation::new(Control::none(), 8 << 20).unwrap();
        let (key, _held) = cache_key(&view, view.triples.as_ref().unwrap(), &clean).unwrap();
        assert!(
            snap.counts
                .prepared_mask_slot(&key, &clean)
                .unwrap()
                .is_none()
        );
        let completed = view.masked_controlled(&snap, &clean).unwrap();
        assert_eq!(completed.snapshot().len(), 0);
    }
    #[test]
    fn long_policy_serialization_observes_cancel_and_deadline_before_publication() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for deadline_mode in [false, true] {
            let store = crate::store::Store::in_memory(Default::default());
            crate::sparql::update::update(
                &store,
                "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
                &Default::default(),
            )
            .unwrap();
            let snap = store.snapshot();
            let mut access = view();
            Arc::make_mut(
                &mut Arc::make_mut(access.triples.as_mut().unwrap()).rules[0].protection,
            )
            .name = "printable-policy".repeat(16_384);
            let seen = Arc::new(AtomicBool::new(false));
            let observed = seen.clone();
            let control = Control::none();
            let cancel = control.cancel.clone();
            let deadline = deadline_mode.then(|| Instant::now() + Duration::from_millis(200));
            let control = Control {
                deadline,
                progress: Progress::new(move |_, message| {
                    if message == "serializing permission policy"
                        && !observed.swap(true, Ordering::SeqCst)
                    {
                        if let Some(deadline) = deadline {
                            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                                std::thread::park_timeout(left);
                            }
                        } else {
                            cancel.cancel();
                        }
                    }
                }),
                ..control
            };
            let preparation = Preparation::new(control, 16 << 20).unwrap();
            let result = access.masked_controlled(&snap, &preparation);
            assert!(
                seen.load(Ordering::SeqCst),
                "no checkpoint after actual bounded policy serialization"
            );
            assert!(if deadline_mode {
                matches!(result, Err(Error::Timeout))
            } else {
                matches!(result, Err(Error::Cancelled))
            });
            let clean = Preparation::new(Control::none(), 16 << 20).unwrap();
            let (key, _held) =
                cache_key(&access, access.triples.as_ref().unwrap(), &clean).unwrap();
            assert!(
                snap.counts
                    .prepared_mask_slot(&key, &clean)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(store.snapshot().len(), 1);
        }
    }
}
