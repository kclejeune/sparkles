//! The additive bounded path. Legacy read/write entry points retain their behavior.
use super::*;
use crate::access::preparation::{Buffer, Map, Text, compare_text, sort_by};
use crate::access::{Exemptions, GraphGuard, GraphRule, Preparation, PreparedView, Reservation};
use crate::store::{Delta, KeySet};
mod key;

pub(crate) fn prepare(
    original: &GraphAccess,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
) -> Result<PreparedView> {
    prepare_input(Input::Legacy { original, snap }, preparation, true)
}

pub(crate) fn prepare_read_only(
    original: &GraphAccess,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
) -> Result<PreparedView> {
    prepare_input(Input::Legacy { original, snap }, preparation, false)
}

pub(crate) fn prepare_rebuild(
    original: &GraphAccess,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
) -> Result<PreparedView> {
    prepare_input(Input::Rebuild { original, snap }, preparation, true)
}

pub(crate) fn prepare_selected(
    selected: crate::store::bound_read_source::PreparedRead,
    preparation: &Preparation,
) -> Result<PreparedView> {
    prepare_input(Input::Selected(selected), preparation, true)
}

pub(crate) fn prepare_evidence(
    evidence: crate::store::BoundEvidenceView,
    policy: Arc<GraphAccess>,
    preparation: &Preparation,
) -> Result<PreparedView> {
    let identity = Arc::downgrade(&policy);
    prepare_input(
        Input::Evidence {
            evidence,
            policy,
            identity,
        },
        preparation,
        true,
    )
}

pub(crate) fn prepare_evidence_read_only(
    evidence: crate::store::BoundEvidenceView,
    policy: Arc<GraphAccess>,
    preparation: &Preparation,
) -> Result<PreparedView> {
    let identity = Arc::downgrade(&policy);
    prepare_input(
        Input::Evidence {
            evidence,
            policy,
            identity,
        },
        preparation,
        false,
    )
}

pub(crate) fn prepare_txn(
    txn: crate::store::bound_read_source::PreparedTxn,
    original: &GraphAccess,
    preparation: &Preparation,
) -> Result<PreparedView> {
    prepare_input(Input::Transaction { txn, original }, preparation, true)
}

enum Input<'a> {
    Legacy {
        original: &'a GraphAccess,
        snap: &'a Arc<Snapshot>,
    },
    Rebuild {
        original: &'a GraphAccess,
        snap: &'a Arc<Snapshot>,
    },
    Selected(crate::store::bound_read_source::PreparedRead),
    Evidence {
        evidence: crate::store::BoundEvidenceView,
        policy: Arc<GraphAccess>,
        identity: std::sync::Weak<GraphAccess>,
    },
    Transaction {
        txn: crate::store::bound_read_source::PreparedTxn,
        original: &'a GraphAccess,
    },
}

impl Input<'_> {
    fn original(&self) -> &GraphAccess {
        match self {
            Self::Legacy { original, .. }
            | Self::Rebuild { original, .. }
            | Self::Transaction { original, .. } => original,
            Self::Selected(selected) => selected.policy(),
            Self::Evidence { policy, .. } => policy,
        }
    }

    fn snapshot(&self) -> &Arc<Snapshot> {
        match self {
            Self::Legacy { snap, .. } | Self::Rebuild { snap, .. } => snap,
            Self::Selected(selected) => selected.snapshot(),
            Self::Evidence { evidence, .. } => evidence.shared_snapshot(),
            Self::Transaction { txn, .. } => txn.snapshot(),
        }
    }

    fn into_owners(
        self,
    ) -> (
        Option<crate::store::bound_read_source::PreparedRead>,
        Option<crate::store::BoundEvidenceView>,
        Option<std::sync::Weak<GraphAccess>>,
        Option<crate::store::bound_read_source::PreparedTxn>,
    ) {
        match self {
            Self::Legacy { .. } | Self::Rebuild { .. } => (None, None, None, None),
            Self::Selected(selected) => (Some(selected.into_identity()), None, None, None),
            Self::Evidence {
                evidence, identity, ..
            } => (None, Some(evidence), Some(identity), None),
            Self::Transaction { txn, .. } => (None, None, None, Some(txn)),
        }
    }
}

fn finish(
    access: Arc<GraphAccess>,
    snapshot: Arc<Snapshot>,
    held: Reservation,
    preparation: &Preparation,
    input: Input<'_>,
) -> Result<PreparedView> {
    // Binding is complete, so strict and evidence views drop their strong reference
    // to the original policy and keep only the admitted weak identity alongside any
    // source owner and reservation.
    let (selected, evidence, evidence_policy, txn) = input.into_owners();
    PreparedView::new_owned(
        access,
        snapshot,
        held,
        preparation,
        selected,
        evidence,
        evidence_policy,
        txn,
    )
}

fn prepare_input(
    input: Input<'_>,
    preparation: &Preparation,
    bind_write: bool,
) -> Result<PreparedView> {
    // Strict Store-minted, BoundEvidenceView and WriteTxn preparations never publish
    // into any base snapshot deduplication table. Controlled raw-snapshot preparations
    // use only the dedicated non-owning controlled weak table, separate from legacy
    // GraphAccess::masked slots.
    let use_controlled_cache = matches!(&input, Input::Legacy { .. });
    let original = input.original();
    let snap = input.snapshot();
    preparation.stage("preparing permission view")?;
    original.check_bound_snapshot(snap)?;
    // For Legacy inputs, the caller owns and pays for the input snapshot. Selected,
    // Evidence and Transaction inputs already carry their own admitted reservations.
    let mut held = preparation.reserve(PreparedView::header_bytes()?)?;
    if matches!(&input, Input::Selected(_) | Input::Evidence { .. }) {
        // Strict and evidence views keep only a weak identity of the invocation's
        // original policy, so a replacement policy can be refused without extending
        // the lifetime of its contents or opaque guard providers.
        held.merge(preparation.reserve(weak_policy_bytes()?)?);
    }
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
    let normalize = original.read.undecided() || (bind_write && original.write.undecided());
    let bound = crate::access::BoundSnapshot::of(snap);
    let (read, read_held) = bind_graphs(&original.read, snap, preparation, normalize, bound)?;
    held.merge(read_held);
    let write = if bind_write {
        let (write, write_held) =
            bind_graphs(&original.write, snap, preparation, normalize, bound)?;
        held.merge(write_held);
        write
    } else {
        Graphs::none()
    };
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
        // A policy that hides no triples keeps an existing mask, which only hides more.
        preparation.check()?;
        return finish(access, snap.clone(), held, preparation, input);
    };
    let (key, key_held) = cache_key(&access, rules, preparation)?;
    if let Some(mask) = &snap.mask {
        // A mask left by another policy could show triples this policy hides, so only
        // this exact policy's own mask can be returned as its prepared view.
        if !preparation.compare(&mask.key, &key)?.is_eq() {
            return Err(already_masked());
        }
        drop(key_held);
        preparation.check()?;
        return finish(access, snap.clone(), held, preparation, input);
    }
    let existing = if use_controlled_cache {
        snap.counts.prepared_mask_slot(&key, preparation)?
    } else {
        None
    };
    let cached = if let Some(slot) = &existing {
        let guard = preparation.lock(slot)?;
        guard.upgrade()
    } else {
        None
    };
    if let Some(cached) = cached {
        // The slot lock is already released, so an error during reservation or finish
        // can drop `cached` and evict a dead slot immediately without contention.
        // The view may outlive any other live holder, so it owns the mask's own storage.
        held.merge(preparation.reserve(mask_storage(&cached))?);
        preparation.check()?;
        return finish(access, cached, held, preparation, input);
    }
    // Build outside the slot lock. User progress and controlled provider helpers
    // never execute while this cache lock is owned. A cold race may build twice.
    let hidden = hidden(&access, rules, snap, preparation)?;
    let (snapshot, derived) = apply_controlled(snap, hidden, &key, preparation)?;
    held.merge(derived);
    preparation.check()?;
    if use_controlled_cache
        && let Some(slot) = snap
            .counts
            .cache_prepared_mask(key, &snapshot, preparation)?
        && let Some(mask) = &snapshot.mask
    {
        mask.attach_cache(
            &snap.counts,
            crate::sparql::stats::MaskCacheKind::Controlled,
            slot,
        );
    }
    drop(key_held);
    preparation.check()?;
    finish(access, snapshot, held, preparation, input)
}

fn already_masked() -> Error {
    Error::Unsupported(
        "controlled permission preparation cannot apply a policy to a snapshot masked by another policy".into(),
    )
}

/// The storage a masked snapshot owns apart from the snapshot it was made from: its
/// copied delta nodes, hidden keys, key text and the snapshot and mask headers.
fn mask_storage(snap: &Snapshot) -> u64 {
    snap.mask.as_ref().map_or(0, |mask| {
        (mask.hidden.capacity() as u64)
            .saturating_mul(32)
            .saturating_add(mask.key.capacity() as u64)
            .saturating_add(mask.retained)
            .saturating_add(SNAPSHOT_HEADERS)
    })
}

/// The allocations of a masked snapshot, its mask and their shared counters.
const SNAPSHOT_HEADERS: u64 = 4096;

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

/// The Arc allocation of a policy that a weak identity keeps after its
/// contents are dropped.
fn weak_policy_bytes() -> Result<u64> {
    let counters = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>();
    let (layout, _) = counters
        .extend(std::alloc::Layout::new::<GraphAccess>())
        .map_err(|_| Error::invalid("policy layout overflows"))?;
    u64::try_from(layout.pad_to_align().size())
        .map_err(|_| Error::invalid("policy layout overflows"))
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
        // Legacy binding leaves guards in triple/protection graph definitions
        // unresolved. Retaining their opaque provider Arc would bypass admission;
        // invoking or clearing it here would change that established policy.
        if guard.decide.is_some() {
            return Err(Error::Unsupported(
                "controlled permission preparation cannot retain unresolved nested guard providers"
                    .into(),
            ));
        }
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
    Ok(a.exempt
        .len()
        .cmp(&b.exempt.len())
        .then_with(|| a.bound_to.cmp(&b.bound_to)))
}

fn bind_graphs(
    graphs: &Graphs,
    snap: &Arc<Snapshot>,
    preparation: &Preparation,
    normalize: bool,
    bound: crate::access::BoundSnapshot,
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
        let guard_snap = crate::access::GuardSnapshot::new(snap);
        let (mut exempt, charge) = match &guard.decide {
            Some(provider) => provider
                .0
                .exempt_controlled(&guard_snap, preparation)?
                .into_parts(preparation)?,
            None => copy_strings(&guard.exempt, preparation)?,
        };
        held.merge(charge);
        if normalize {
            normalize_strings(&mut exempt, preparation)?;
        }
        let bound_to = if guard.decide.is_some() {
            Some(bound)
        } else {
            guard.bound_to
        };
        guards.push(GraphGuard {
            pattern,
            exempt,
            decide: None,
            bound_to,
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

fn allows_blank_nodes(graphs: &Graphs) -> bool {
    match graphs {
        Graphs::All => true,
        Graphs::Only(rule) => rule.blank_nodes,
    }
}

fn is_empty_graphs(graphs: &Graphs) -> bool {
    match graphs {
        Graphs::All => false,
        Graphs::Only(rule) => {
            !rule.default_graph
                && !rule.blank_nodes
                && rule.iris.is_empty()
                && rule.patterns.is_empty()
        }
    }
}

fn exact_named_graph_ids(
    graphs: &Graphs,
    snap: &Snapshot,
    preparation: &Preparation,
) -> Result<Option<Map<u64, ()>>> {
    let Graphs::Only(rule) = graphs else {
        return Ok(None);
    };
    if !rule.patterns.is_empty() {
        return Ok(None);
    }
    let mut ids = Map::new(preparation)?;
    if rule.default_graph
        && let Some(id) = preparation.lookup_iri(snap, crate::sparql::ctx::DEFAULT_GRAPH_IRI)?
    {
        ids.insert(id.0, ())?;
    }
    for iri in &rule.iris {
        preparation.check()?;
        if preparation.allows_named_graph_iri(graphs, iri)?
            && let Some(id) = preparation.lookup_iri(snap, iri)?
        {
            ids.insert(id.0, ())?;
        }
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
    let exact_readable = exact_named_graph_ids(&access.read, snap, preparation)?;
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
        let exact_protected = match &protection.graphs {
            Some(graphs) => exact_named_graph_ids(graphs, snap, preparation)?,
            None => None,
        };
        let covers_all_named = protection.graphs.as_ref().is_none_or(Graphs::is_all);
        let rule_allows_none = is_empty_graphs(&rule.read);
        let mut by_graph = Map::new(preparation)?;
        let mut pass: Option<Pass> = None;
        let mut visit = |quad: [Id; 4]| -> Result<bool> {
            preparation.check()?;
            #[cfg(test)]
            tests::record_hidden_visit();
            let graph = quad[3];
            let hide = if let Some(&hide) = by_graph.get(&graph.0) {
                hide
            } else {
                let hide = if graph == Id::DEFAULT_GRAPH {
                    let readable = if let Some(&readable) = read.get(&graph.0) {
                        readable
                    } else {
                        let readable = access.read.default_graph();
                        read.insert(graph.0, readable)?;
                        readable
                    };
                    readable
                        && preparation.covers_graph(protection, None)?
                        && !preparation.allows(&rule.read, None)?
                } else if graph.tag() == crate::id::Tag::BNode {
                    let readable = if let Some(&readable) = read.get(&graph.0) {
                        readable
                    } else {
                        let readable = allows_blank_nodes(&access.read);
                        read.insert(graph.0, readable)?;
                        readable
                    };
                    readable
                        && protection.graphs.as_ref().is_none_or(allows_blank_nodes)
                        && !allows_blank_nodes(&rule.read)
                } else if exact_protected
                    .as_ref()
                    .is_some_and(|exact| !exact.contains_key(&graph.0))
                {
                    false
                } else {
                    let eval_hide_iri = |iri: Option<&str>| -> Result<bool> {
                        match iri {
                            Some(iri) => Ok(preparation.covers_named_graph_iri(protection, iri)?
                                && !preparation.allows_named_graph_iri(&rule.read, iri)?),
                            None => Ok(covers_all_named),
                        }
                    };
                    if let Some(&readable) = read.get(&graph.0) {
                        if !readable {
                            false
                        } else if covers_all_named && rule_allows_none {
                            true
                        } else {
                            preparation.with_iri(snap, graph, eval_hide_iri)?
                        }
                    } else if let Some(exact) = &exact_readable {
                        let readable = exact.contains_key(&graph.0);
                        read.insert(graph.0, readable)?;
                        if !readable {
                            false
                        } else if covers_all_named && rule_allows_none {
                            true
                        } else {
                            preparation.with_iri(snap, graph, eval_hide_iri)?
                        }
                    } else if access.read.is_all() {
                        read.insert(graph.0, true)?;
                        if covers_all_named && rule_allows_none {
                            true
                        } else {
                            preparation.with_iri(snap, graph, eval_hide_iri)?
                        }
                    } else {
                        let (readable, hide) = preparation.with_iri(snap, graph, |iri| {
                            let readable = match iri {
                                Some(iri) => {
                                    preparation.allows_named_graph_iri(&access.read, iri)?
                                }
                                None => false,
                            };
                            let hide = if !readable {
                                false
                            } else if covers_all_named && rule_allows_none {
                                true
                            } else {
                                eval_hide_iri(iri)?
                            };
                            Ok((readable, hide))
                        })?;
                        read.insert(graph.0, readable)?;
                        hide
                    }
                };
                by_graph.insert(graph.0, hide)?;
                hide
            };
            if hide {
                if let Some(pattern) = &protection.pattern {
                    let resolved = match &mut pass {
                        Some(pass) => pass,
                        None => pass.insert(passes(
                            snap,
                            protection,
                            pattern,
                            &rules.caller,
                            &rules.limits,
                            preparation,
                        )?),
                    };
                    if resolved.passes(&quad) {
                        return Ok(!matches!(resolved, Pass::All));
                    }
                }
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
                    #[cfg(test)]
                    tests::record_hidden_range();
                    let mut stop = false;
                    preparation.scan(snap, Perm::Spo, &[subject], |quad| {
                        if predicates
                            .as_ref()
                            .is_none_or(|ids| ids.contains_key(&quad[1].0))
                            && !visit(quad)?
                        {
                            stop = true;
                            return Ok(false);
                        }
                        Ok(true)
                    })?;
                    if stop {
                        break;
                    }
                }
            }
            (None, Some(predicates)) => {
                for &predicate in predicates.keys() {
                    #[cfg(test)]
                    tests::record_hidden_range();
                    let mut stop = false;
                    preparation.scan(snap, Perm::Pso, &[predicate], |quad| {
                        if !visit(quad)? {
                            stop = true;
                            return Ok(false);
                        }
                        Ok(true)
                    })?;
                    if stop {
                        break;
                    }
                }
            }
            (None, None) => {
                #[cfg(test)]
                tests::record_hidden_range();
                preparation.scan(snap, Perm::Gspo, &[], &mut visit)?;
            }
        }
    }
    preparation.stage("ordering permission mask")?;
    crate::access::preparation::sort_keys(&mut hidden, preparation)?;
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

/// The solutions of a protection's pattern, as [`super::Passes`] holds them for the
/// legacy path, in a map charged to the preparation.
enum Pass {
    All,
    None,
    Keys {
        cols: [bool; 3],
        set: Map<[u64; 3], ()>,
    },
}

impl Pass {
    fn passes(&self, quad: &[Id; 4]) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Keys { cols, set } => {
                let mut key = [0; 3];
                for (index, on) in cols.iter().enumerate() {
                    if *on {
                        key[index] = quad[index].0;
                    }
                }
                set.contains_key(&key)
            }
        }
    }
}

/// Write the query of [`pattern_query`] into charged text: the same prefixes, the
/// same projection and the caller's values as escaped literals. `None` when the caller
/// lacks an attribute the pattern uses, so the pattern matches nothing.
fn pattern_text(
    text: &mut Text,
    protection: &Protection,
    pattern: &str,
    caller: &Caller,
) -> Result<Option<[bool; 3]>> {
    for (prefix, iri) in &protection.prefixes {
        for part in ["PREFIX ", prefix, ": <", iri, ">\n"] {
            text.push_str(part)?;
        }
    }
    let subject = if uses(pattern, "this") { "this" } else { "s" };
    let cols = [
        uses(pattern, subject),
        uses(pattern, "p"),
        uses(pattern, "o"),
    ];
    text.push_str("SELECT DISTINCT")?;
    for (on, var) in cols.iter().zip([subject, "p", "o"]) {
        if *on {
            text.push_str(" ?")?;
            text.push_str(var)?;
        }
    }
    if !cols.iter().any(|on| *on) {
        text.push_str(" (1 AS ?any)")?;
    }
    text.push_str(" WHERE {\n")?;
    let user = caller.user.as_slice();
    for (var, values) in [
        ("user", user),
        ("role", caller.roles.as_slice()),
        ("group", caller.groups.as_slice()),
    ] {
        if !uses(pattern, var) {
            continue;
        }
        if values.is_empty() {
            return Ok(None);
        }
        text.push_str("  VALUES ?")?;
        text.push_str(var)?;
        text.push_str(" { ")?;
        for (at, value) in values.iter().enumerate() {
            if at != 0 {
                text.push_str(" ")?;
            }
            push_literal(text, value)?;
        }
        text.push_str(" }\n")?;
    }
    text.push_str("  { ")?;
    text.push_str(pattern)?;
    text.push_str("\n  }\n}")?;
    Ok(Some(cols))
}

/// A simple literal in SPARQL syntax, with runs of plain characters copied whole.
fn push_literal(text: &mut Text, value: &str) -> Result<()> {
    text.push_str("\"")?;
    let mut start = 0;
    for (at, c) in value.char_indices() {
        let escaped = match c {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            c if c.is_control() => "",
            _ => continue,
        };
        text.push_str(&value[start..at])?;
        if escaped.is_empty() {
            let mut code = [0u8; 6];
            let code = hex_escape(c, &mut code);
            text.push_str(code)?;
        } else {
            text.push_str(escaped)?;
        }
        start = at + c.len_utf8();
    }
    text.push_str(&value[start..])?;
    text.push_str("\"")
}

/// The `\uXXXX` escape of a control character, which is always below U+10000.
fn hex_escape(c: char, out: &mut [u8; 6]) -> &str {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let code = c as u32;
    out[0] = b'\\';
    out[1] = b'u';
    for (at, shift) in [12, 8, 4, 0].into_iter().enumerate() {
        out[2 + at] = HEX[((code >> shift) & 0xF) as usize];
    }
    std::str::from_utf8(out).expect("ASCII escape")
}

/// Evaluate a protection's pattern as [`super::passes`] does, with every graph merged
/// into the default graph by graph ID and `GRAPH` for the named ones. The query text,
/// the default graph ID set and the pattern's query are charged to `preparation`, and
/// the query runs under its controls on the unmasked snapshot without copying graph IRIs.
fn passes(
    snap: &Arc<Snapshot>,
    protection: &Protection,
    pattern: &str,
    caller: &Caller,
    limits: &Limits,
    preparation: &Preparation,
) -> Result<Pass> {
    preparation.check()?;
    let mut text = Text::new(preparation)?;
    let Some(cols) = pattern_text(&mut text, protection, pattern, caller)? else {
        return Ok(Pass::None);
    };
    let (text, _text_held) = text.finish()?;
    let opts = crate::sparql::QueryOptions {
        max_rows: Some(usize::try_from(limits.max_pattern_rows).unwrap_or(usize::MAX)),
        ..preparation.nested_query_options()
    };
    let rows_refused = |requested| {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::Rows,
            limit: limits.max_pattern_rows,
            requested,
        })
    };
    let view = GraphAccess::all().masked_controlled(snap, preparation)?;
    let result = crate::sparql::query_prepared_pattern_source(view.read_source(), &text, &opts)
        .map_err(|error| match error {
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::Rows,
                ..
            }) => rows_refused(limits.max_pattern_rows.saturating_add(1)),
            Error::SparqlSyntax(error) => Error::invalid(format!(
                "the pattern of protection '{}': {error}",
                protection.name
            )),
            error => error,
        })?;
    let table = result.table();
    if table.len() as u64 > limits.max_pattern_rows {
        return Err(rows_refused(table.len() as u64));
    }
    if !cols.iter().any(|on| *on) {
        return Ok(if table.is_empty() {
            Pass::None
        } else {
            Pass::All
        });
    }
    let mut set = Map::new(preparation)?;
    for row in 0..table.len() {
        preparation.check()?;
        let mut key = [0; 3];
        let mut column = 0;
        for (index, on) in cols.iter().enumerate() {
            if *on {
                key[index] = table.get(row, column)?.0;
                column += 1;
            }
        }
        set.insert(key, ())?;
    }
    Ok(Pass::Keys { cols, set })
}

/// Lower the predicate statistics for the hidden quads the delta had inserted, by
/// predicate with their distinct subjects and objects, as the legacy path does.
fn forget_removed(
    delta: &mut Delta,
    hidden: &[Key],
    inserted: &[bool],
    preparation: &Preparation,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let mut by_subject = Buffer::new(preparation)?;
    let mut by_object = Buffer::new(preparation)?;
    for (at, (key, &was_inserted)) in hidden.iter().zip(inserted).enumerate() {
        if at.is_multiple_of(1024) {
            check()?;
        }
        if was_inserted {
            let [s, p, o, _] = *key;
            by_subject.push([p, s, o, 0])?;
            by_object.push([p, o, s, 0])?;
        }
    }
    crate::access::preparation::sort_keys(&mut by_subject, preparation)?;
    crate::access::preparation::sort_keys(&mut by_object, preparation)?;
    let mut subjects = Buffer::new(preparation)?;
    let mut objects = Buffer::new(preparation)?;
    let (mut a, mut b) = (0, 0);
    while a < by_subject.len() {
        check()?;
        let p = by_subject[a][0];
        let start = a;
        subjects.truncate(0);
        while a < by_subject.len() && by_subject[a][0] == p {
            if a.is_multiple_of(1024) {
                check()?;
            }
            if subjects.last() != Some(&by_subject[a][1]) {
                subjects.push(by_subject[a][1])?;
            }
            a += 1;
        }
        objects.truncate(0);
        while b < by_object.len() && by_object[b][0] == p {
            if b.is_multiple_of(1024) {
                check()?;
            }
            if objects.last() != Some(&by_object[b][1]) {
                objects.push(by_object[b][1])?;
            }
            b += 1;
        }
        delta.forget_removed(p, (a - start) as u64, &subjects, &objects, check)?;
    }
    check()
}

/// Hide `hidden` in one permutation: remove the keys the delta inserted and add the
/// others to its deletions. Returns the admission of the nodes the writes allocated.
fn write_permutation(
    perm: Perm,
    ins: &mut KeySet,
    del: &mut KeySet,
    hidden: &[Key],
    inserted: &[bool],
    preparation: &Preparation,
    owned: &mut Reservation,
) -> Result<()> {
    // `owned` belongs to the caller, which keeps it until the sets these writes change
    // are dropped. A refused or interrupted batch keeps its whole admission there too,
    // so no other writer can reuse the allowance of nodes that are still alive.
    let mut at = 0;
    while at < hidden.len() {
        // One more level than either set has now covers a root split in the batch.
        let bound = ins
            .mutation_bound()
            .max(del.mutation_bound())
            .saturating_add(KeySet::level_bound());
        // A batch admits at most a quarter of what the writes already own, or an eighth
        // of the room the cap leaves each of the seven writers, so headroom stays small
        // next to the charge and the cap while admissions stay few.
        let share = (owned.bytes() / 4).max(preparation.room() / 8 / Perm::ALL.len() as u64);
        let batch = usize::try_from(share / bound.max(1))
            .unwrap_or(usize::MAX)
            .clamp(1, 4096)
            .min(hidden.len() - at);
        let mut headroom = preparation.reserve(bound.saturating_mul(batch as u64))?;
        let mut allocated = 0u64;
        let written = (at..at + batch).try_for_each(|index| {
            if index.is_multiple_of(64) {
                preparation.check()?;
            }
            let key = perm.to_key(&Perm::Spo.to_quad(&hidden[index]));
            if inserted[index] {
                ins.remove_metered(&key, &mut allocated);
            } else {
                del.insert_metered(key, &mut allocated);
            }
            Ok(())
        });
        let grown = written.and_then(|()| headroom.grow_to(allocated));
        if grown.is_ok() {
            headroom.release_to(allocated);
        }
        owned.merge(headroom);
        grown?;
        at += batch;
    }
    preparation.check()
}

fn apply_controlled(
    snap: &Snapshot,
    hidden: Buffer<Key>,
    key: &str,
    preparation: &Preparation,
) -> Result<(Arc<Snapshot>, Reservation)> {
    preparation.stage("applying permission mask")?;
    let mut held = preparation.reserve(SNAPSHOT_HEADERS)?;
    // The clone shares every node of the input delta, and hiding quads copies the
    // nodes on their paths. The seven permutations are written in parallel, as the
    // legacy path writes them. Each batch of key-set writes is admitted at an upper
    // bound before it runs and then keeps only the bytes it allocated, so the view is
    // charged for the nodes it owns and the cap holds at the peak of every batch.
    let mut inserted = Buffer::new(preparation)?;
    for key in hidden.iter() {
        inserted.push(snap.delta.ins[Perm::Spo.index()].contains(key))?;
    }
    let mut predicates = preparation.reserve(0)?;
    if inserted.iter().any(|&i| i) {
        // the statistics map is persistent too, and cannot copy more than its nodes
        predicates.merge(preparation.reserve(snap.delta.predicate_bytes_bound())?);
    }
    let mut copies = Perm::ALL
        .iter()
        .map(|_| preparation.reserve(0))
        .collect::<Result<Vec<_>>>()?;
    // Declared after the reservations, so an early return drops the copied nodes
    // before it releases their admission.
    let mut delta = snap.delta.clone();
    let sets: Vec<(Perm, &mut KeySet, &mut KeySet)> = Perm::ALL
        .into_iter()
        .zip(delta.ins.iter_mut().zip(delta.del.iter_mut()))
        .map(|(perm, (ins, del))| (perm, ins, del))
        .collect();
    {
        use rayon::prelude::*;
        sets.into_par_iter()
            .zip(copies.par_iter_mut())
            .map(|((perm, ins, del), owned)| {
                write_permutation(perm, ins, del, &hidden, &inserted, preparation, owned)
            })
            .collect::<Result<()>>()?
    };
    forget_removed(&mut delta, &hidden, &inserted, preparation, &mut || {
        preparation.check()
    })?;
    drop(inserted);
    for copy in copies {
        held.merge(copy);
    }
    held.merge(predicates);
    let retained = held.bytes().saturating_sub(SNAPSHOT_HEADERS);
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
            retained,
            cache_link: std::sync::OnceLock::new(),
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
            let _live_holder = view.masked_controlled(&snap, &prep).unwrap();
            let slot = snap
                .counts
                .prepared_mask_slot(&key, &prep)
                .unwrap()
                .unwrap();
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
    fn the_view_reserves_at_least_every_delta_node_its_mask_owns() {
        let store = crate::store::Store::in_memory(Default::default());
        for chunk in 0..3 {
            let rows: String = (chunk * 4000..(chunk + 1) * 4000)
                .map(|i| {
                    let p = if i % 7 == 0 { "urn:p" } else { "urn:q" };
                    format!("<urn:s{i}> <{p}> <urn:o{i}> . ")
                })
                .collect();
            crate::sparql::update::update(
                &store,
                &format!("INSERT DATA {{ {rows} }}"),
                &Default::default(),
            )
            .unwrap();
        }
        // half of the hidden quads come from the delta and half from the base
        store.compact().unwrap();
        let rows: String = (12_000..16_000)
            .map(|i| {
                let p = if i % 7 == 0 { "urn:p" } else { "urn:q" };
                format!("<urn:s{i}> <{p}> <urn:o{i}> . ")
            })
            .collect();
        crate::sparql::update::update(
            &store,
            &format!("INSERT DATA {{ {rows} }}"),
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        let legacy = view().masked(&snap).unwrap();
        let prep = Preparation::new(Control::none(), 64 << 20).unwrap();
        let before = prep.inner_used();
        let prepared = view().masked_controlled(&snap, &prep).unwrap();
        let owned = prepared
            .snapshot()
            .delta
            .unshared_bytes(|| Ok(()))
            .unwrap()
            .0;
        let mask = prepared.snapshot().mask.as_ref().unwrap();
        assert!(owned > 0);
        assert!(mask.retained >= owned);
        assert!(prep.inner_used() - before >= mask_storage(prepared.snapshot()));
        assert_eq!(prepared.snapshot().len(), legacy.len());
        let legacy_mask = legacy.mask.as_ref().unwrap();
        assert!(legacy_mask.retained >= legacy.delta.unshared_bytes(|| Ok(())).unwrap().0);
        drop(prepared);
        assert_eq!(prep.inner_used(), before);
    }

    #[test]
    fn a_writer_refused_mid_mask_keeps_the_charge_for_the_nodes_it_wrote() {
        // A refused batch must not release the admission of nodes that stay alive in
        // the caller's delta, or other writers could reuse it while they exist.
        let key = |i: u64| [i, 1, i, 0];
        let base: KeySet = (0..20_000).map(|i| key(i * 2)).collect();
        let hidden: Vec<Key> = (0..20_000).map(|i| key(i * 2 + 1)).collect();
        let inserted = vec![false; hidden.len()];
        let prep = Preparation::new(Control::none(), 512 << 10).unwrap();
        let mut ins = KeySet::new();
        let mut del = base.clone();
        let mut copies = prep.reserve(0).unwrap();
        let result = write_permutation(
            Perm::Spo,
            &mut ins,
            &mut del,
            &hidden,
            &inserted,
            &prep,
            &mut copies,
        );
        assert!(
            matches!(result, Err(Error::BudgetExceeded(_))),
            "{result:?}"
        );
        let (owned, _) = del.unshared_bytes(|| Ok(())).unwrap();
        assert!(owned > 0);
        assert!(
            prep.inner_used() >= owned,
            "charged {} for {owned} live bytes",
            prep.inner_used()
        );
        drop((ins, del));
        drop(copies);
    }

    #[test]
    fn statistics_of_one_large_predicate_poll_the_controls() {
        // One predicate with many distinct subjects and objects is one statistics
        // update, which must still poll its controls as it goes.
        let store = crate::store::Store::in_memory(Default::default());
        let rows: String = (0..20_000)
            .map(|i| format!("<urn:s{i}> <urn:p> <urn:o{i}> . "))
            .collect();
        crate::sparql::update::update(
            &store,
            &format!("INSERT DATA {{ {rows} }}"),
            &Default::default(),
        )
        .unwrap();
        let mut delta = store.snapshot().delta.clone();
        let hidden: Vec<Key> = delta.ins[Perm::Spo.index()].iter().copied().collect();
        assert_eq!(hidden.len(), 20_000);
        let inserted = vec![true; hidden.len()];
        let prep = Preparation::new(Control::none(), 64 << 20).unwrap();
        let mut checks = 0usize;
        forget_removed(&mut delta, &hidden, &inserted, &prep, &mut || {
            checks += 1;
            Ok(())
        })
        .unwrap();
        // two lists of 20,000 values, polled at least every 1024 steps
        assert!(checks >= 2 * 20_000 / 1024, "{checks} checks");
        let mut refused = 0usize;
        let cancelled = forget_removed(&mut delta.clone(), &hidden, &inserted, &prep, &mut || {
            refused += 1;
            if refused > 20 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(cancelled, Err(Error::Cancelled)));
    }

    #[test]
    fn a_statistics_update_refused_while_probing_changes_nothing() {
        // The delta's own update polls while it probes subjects and then objects, and
        // a refusal there leaves the predicate's statistics as they were.
        let store = crate::store::Store::in_memory(Default::default());
        let rows: String = (0..20_000)
            .map(|i| format!("<urn:s{i}> <urn:p> <urn:o{i}> . "))
            .collect();
        crate::sparql::update::update(
            &store,
            &format!("INSERT DATA {{ {rows} }}"),
            &Default::default(),
        )
        .unwrap();
        let delta = store.snapshot().delta.clone();
        let keys: Vec<Key> = delta.ins[Perm::Pso.index()].iter().copied().collect();
        let p = keys[0][0];
        let mut subjects: Vec<u64> = keys.iter().map(|k| k[1]).collect();
        let mut objects: Vec<u64> = keys.iter().map(|k| k[2]).collect();
        subjects.dedup();
        objects.sort_unstable();
        objects.dedup();
        assert_eq!((subjects.len(), objects.len()), (20_000, 20_000));
        let n = keys.len() as u64;
        let before = *delta.predicate(p).unwrap();
        // unchecked, the update lowers the count
        let mut changed = delta.clone();
        changed
            .forget_removed(p, n / 2, &subjects, &objects, &mut || Ok(()))
            .unwrap();
        assert_ne!(*changed.predicate(p).unwrap(), before);
        // 20 polls cover the subjects and 20 more the objects
        for refuse_at in [3usize, 23] {
            let mut target = delta.clone();
            let mut calls = 0usize;
            let result = target.forget_removed(p, n / 2, &subjects, &objects, &mut || {
                calls += 1;
                if calls == refuse_at {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(Error::Cancelled)),
                "refusal at poll {refuse_at}: {result:?} after {calls} polls"
            );
            assert_eq!(*target.predicate(p).unwrap(), before);
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

    fn owned_by(user: &str) -> GraphAccess {
        GraphAccess::with_triples(
            Graphs::All,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "owned".into(),
                        predicates: Some(vec!["urn:p".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: Some("?s <urn:owner> ?user".into()),
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller {
                    user: Some(user.into()),
                    ..Caller::default()
                },
                limits: Limits::default(),
            },
        )
    }

    fn quads(snap: &Snapshot) -> Vec<[Id; 4]> {
        let mut out = Vec::new();
        snap.for_each_quad(|q| {
            out.push(*q);
            Ok(())
        })
        .unwrap();
        out.sort_unstable();
        out
    }

    #[test]
    fn protection_patterns_hide_what_the_legacy_mask_hides_and_key_their_caller() {
        let store = crate::store::Store::in_memory(Default::default());
        crate::sparql::update::update(
            &store,
            r#"INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 .
                <urn:a> <urn:owner> "alice" . <urn:b> <urn:owner> "bob" .
                <urn:d> <urn:p> 4 . <urn:d> <urn:owner> "q\"\\\n\u0000\u007Fx" .
                GRAPH <urn:g> { <urn:c> <urn:p> 3 . <urn:c> <urn:owner> "alice" } }"#,
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        let all = quads(&snap);
        // The last caller's value needs escaping in the pattern's query and still reads
        // as itself, so it sees its one triple.
        for (user, expected) in [
            ("alice", 2),
            ("bob", 3),
            ("carol", 4),
            ("q\"\\\n\u{0}\u{7f}x", 3),
        ] {
            let access = owned_by(user);
            let legacy = quads(&access.masked(&snap).unwrap());
            let prep = Preparation::new(Control::none(), 8 << 20).unwrap();
            let view = access.masked_controlled(&snap, &prep).unwrap();
            assert_eq!(quads(view.snapshot()), legacy, "{user:?}");
            assert_eq!(all.len() - legacy.len(), expected, "{user:?}");
        }
        let prep = Preparation::new(Control::none(), 8 << 20).unwrap();
        let alice = owned_by("alice");
        let bob = owned_by("bob");
        let (a, _a_held) = cache_key(&alice, alice.triples.as_ref().unwrap(), &prep).unwrap();
        let (b, _b_held) = cache_key(&bob, bob.triples.as_ref().unwrap(), &prep).unwrap();
        assert_ne!(a, b, "a pattern's mask is keyed by its caller");
    }

    #[test]
    fn pattern_preparation_skips_unreadable_rules_and_uses_graph_ids_for_cross_graph_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let g_readable = "urn:g:readable";
        let g_empty_ok = "urn:g:empty-ok";
        let g_meta = "urn:g:meta";
        {
            let store = crate::store::Store::open(dir.path(), Default::default()).unwrap();
            let mut trig = format!(
                "<urn:d1> <urn:secret:p> \"1\" ; <urn:secret:p2> \"1\" .\n\
                 <urn:d2> <urn:secret:p> \"2\" .\n\
                 <urn:r1> <urn:approved> \"alice\" .\n\
                 GRAPH <{g_readable}> {{\n\
                     <urn:r1> <urn:secret:p> \"1\" ; <urn:secret:p2> \"1\" .\n\
                     <urn:r2> <urn:secret:p> \"2\" .\n\
                 }}\n\
                 GRAPH <{g_empty_ok}> {{\n\
                     <urn:e1> <urn:plain:p> \"ok\" .\n\
                 }}\n\
                 GRAPH <{g_meta}> {{\n\
                     <urn:d1> <urn:approved> \"alice\" .\n\
                     <{g_meta}> <urn:enabled> \"yes\" .\n\
                 }}\n"
            );
            // 16 denied graphs * 64 KiB = ~1 MiB of denied graph IRIs, well above the
            // 512 KiB preparation budget used below.
            let pad = "x".repeat(64 * 1024);
            for i in 0..16 {
                trig.push_str(&format!(
                    "GRAPH <urn:g:denied-{i:02}-{pad}> {{\n\
                         <urn:hidden:{i}> <urn:only-denied:p> \"x\" ; <urn:secret:p> \"x\" .\n\
                     }}\n"
                ));
            }
            crate::sparql::update::update(
                &store,
                &format!("INSERT DATA {{ {trig} }}"),
                &Default::default(),
            )
            .unwrap();
            store.compact().unwrap();
        }

        let store = crate::store::Store::open(dir.path(), Default::default()).unwrap();
        let snap = store.snapshot();
        assert!(snap.delta.is_empty(), "expected compacted base-only store");

        // 1. No readable covered candidate: pattern evaluation is skipped completely.
        //    Even with `max_pattern_rows: 0` and ~1 MiB of denied graph IRIs, a narrow
        //    exact allowlist succeeds under a 512 KiB cap.
        let skip_access = GraphAccess::with_triples(
            Graphs::Only(GraphRule::new([g_empty_ok], &[])),
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "skip-unreadable".into(),
                        predicates: Some(vec!["urn:only-denied:p".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: Some("FILTER(false)".into()),
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits {
                    max_pattern_rows: 0,
                    ..Limits::default()
                },
            },
        );
        let skip_prep = Preparation::new(Control::none(), 512 << 10).unwrap();
        let skip_view = skip_access.masked_controlled(&snap, &skip_prep).unwrap();
        assert!(
            skip_view
                .snapshot()
                .mask
                .as_ref()
                .unwrap()
                .hidden
                .is_empty()
        );

        // 2. Active cross-graph pattern: default-graph fact unlocks named-graph triple,
        //    named-graph fact unlocks default-graph triple, and explicit `GRAPH <g_meta>`
        //    works via graph-ID dataset setup without copying the ~1 MiB of denied IRIs.
        let mut readable_rule = GraphRule::new([g_readable], &[]);
        readable_rule.default_graph = true;
        let active_access = GraphAccess::with_triples(
            Graphs::Only(readable_rule.clone()),
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "cross-graph-pattern".into(),
                        predicates: Some(vec!["urn:secret:p".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: Some(format!(
                            "?s <urn:approved> ?user . GRAPH <{g_meta}> {{ <{g_meta}> <urn:enabled> \"yes\" }}"
                        )),
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller {
                    user: Some("alice".into()),
                    ..Caller::default()
                },
                limits: Limits::default(),
            },
        );
        let active_prep = Preparation::new(Control::none(), 512 << 10).unwrap();
        let active_view = active_access
            .masked_controlled(&snap, &active_prep)
            .unwrap();
        let legacy = active_access.masked(&snap).unwrap();
        assert_eq!(quads(active_view.snapshot()), quads(&legacy));
        // Exactly `<urn:d2>` (default graph) and `<urn:r2>` (`g_readable`) are hidden.
        assert_eq!(
            active_view.snapshot().mask.as_ref().unwrap().hidden.len(),
            2
        );

        // 3. Zero-variable pattern returning `Pass::All` stops early across multiple
        //    predicate ranges without scanning remaining candidates and hides nothing.
        let pass_all_access = GraphAccess::with_triples(
            Graphs::Only(readable_rule),
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "pass-all-pattern".into(),
                        predicates: Some(vec!["urn:secret:p".into(), "urn:secret:p2".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: Some(format!(
                            "GRAPH <{g_meta}> {{ <{g_meta}> <urn:enabled> \"yes\" }}"
                        )),
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        );
        let pass_all_prep = Preparation::new(Control::none(), 512 << 10).unwrap();
        let (pass_all_view, ranges, visits) =
            observe_hidden_scan(|| pass_all_access.masked_controlled(&snap, &pass_all_prep));
        let pass_all_view = pass_all_view.unwrap();
        assert!(
            pass_all_view
                .snapshot()
                .mask
                .as_ref()
                .unwrap()
                .hidden
                .is_empty()
        );
        assert!(
            ranges == 1 && visits == 1,
            "Pass::All must stop after the first readable candidate range (ranges={ranges}, visits={visits})"
        );
    }

    thread_local! {
        static HIDDEN_SCAN: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
    }

    pub(super) fn record_hidden_range() {
        HIDDEN_SCAN.with(|cell| {
            if let Some((ranges, visits)) = cell.get() {
                cell.set(Some((ranges + 1, visits)));
            }
        });
    }

    pub(super) fn record_hidden_visit() {
        HIDDEN_SCAN.with(|cell| {
            if let Some((ranges, visits)) = cell.get() {
                cell.set(Some((ranges, visits + 1)));
            }
        });
    }

    fn observe_hidden_scan<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
        struct Reset(Option<(usize, usize)>);
        impl Drop for Reset {
            fn drop(&mut self) {
                HIDDEN_SCAN.with(|cell| cell.set(self.0));
            }
        }
        let prev = HIDDEN_SCAN.with(|cell| cell.replace(Some((0, 0))));
        let _reset = Reset(prev);
        let out = f();
        let (ranges, visits) = HIDDEN_SCAN.with(|cell| cell.get()).unwrap();
        (out, ranges, visits)
    }

    #[test]
    fn blank_node_graph_witness_agrees_across_controlled_legacy_and_write_checks() {
        use crate::io::{RdfFormat, Source};
        use oxrdf::NamedNode;

        let g_reports = "urn:g:reports";
        let trig = format!(
            "<{g_reports}> {{ <urn:report> <urn:secret:p> \"v1\" . }}\n\
             _:witness {{ <urn:report> <urn:approved> \"alice\" . }}\n"
        );
        for union_default_graph in [false, true] {
            let store = crate::store::Store::in_memory(crate::store::StoreOptions {
                union_default_graph,
                ..Default::default()
            });
            store
                .load(&[Source::from_bytes(
                    trig.as_bytes().to_vec(),
                    RdfFormat::TriG,
                    None,
                )])
                .unwrap();

            for compacted in [false, true] {
                if compacted {
                    store.compact().unwrap();
                }
                let snap = store.snapshot();
                let pred_id = snap.lookup_iri("urn:secret:p").unwrap();
                let graph_term = Term::NamedNode(NamedNode::new_unchecked(g_reports));
                let mut protected_quad = None;
                snap.for_each_quad(|q| {
                    if q[1] == pred_id {
                        protected_quad = Some(*q);
                    }
                    Ok(())
                })
                .unwrap();
                let protected_quad = protected_quad.expect("protected quad present");

                let make_access = |user: &str| {
                    let read_write = Graphs::Only(GraphRule::new([g_reports], &[]));
                    GraphAccess::with_triples(
                        read_write.clone(),
                        read_write,
                        TripleRules {
                            rules: vec![Rule {
                                protection: Arc::new(Protection {
                                    name: "approved-report".into(),
                                    predicates: Some(vec!["urn:secret:p".into()]),
                                    classes: None,
                                    subclasses: false,
                                    graphs: None,
                                    pattern: Some("?s <urn:approved> ?user".into()),
                                    prefixes: Default::default(),
                                    hide_inferences: false,
                                }),
                                read: Graphs::none(),
                                write: Graphs::none(),
                            }],
                            caller: Caller {
                                user: Some(user.into()),
                                ..Caller::default()
                            },
                            limits: Limits::default(),
                        },
                    )
                };

                for (user, allowed) in [("alice", true), ("bob", false)] {
                    let access = make_access(user);
                    let prep = Preparation::new(Control::none(), 8 << 20).unwrap();
                    let controlled = access.masked_controlled(&snap, &prep).unwrap();
                    let legacy = access.masked(&snap).unwrap();
                    assert_eq!(
                        quads(controlled.snapshot()),
                        quads(&legacy),
                        "controlled vs legacy parity failed for {user} (union={union_default_graph}, compacted={compacted})"
                    );
                    let expected_hidden = if allowed { 0 } else { 1 };
                    assert_eq!(
                        controlled.snapshot().mask.as_ref().unwrap().hidden.len(),
                        expected_hidden,
                        "unexpected controlled mask for {user} (union={union_default_graph}, compacted={compacted})"
                    );
                    assert_eq!(
                        legacy.mask.as_ref().unwrap().hidden.len(),
                        expected_hidden,
                        "unexpected legacy mask for {user} (union={union_default_graph}, compacted={compacted})"
                    );

                    // Direct WriteCheck verdict on the same snapshot and protection.
                    let mut write_check =
                        WriteCheck::new(access.triples.as_ref().unwrap(), snap.clone())
                            .unwrap()
                            .expect("protection limits writes");
                    let check_res =
                        write_check.check(protected_quad, "urn:secret:p", Some(&graph_term));
                    if allowed {
                        check_res.expect("authorized write check should pass");
                    } else {
                        assert!(
                            matches!(check_res, Err(Error::NotPermitted(_))),
                            "unauthorized write check should be refused, got {check_res:?}"
                        );
                    }

                    // End-to-end SPARQL update under the same protection and graph grants.
                    let update_opts = crate::sparql::QueryOptions {
                        graphs: Some(Arc::new(access)),
                        ..Default::default()
                    };
                    let update_res = crate::sparql::update::update(
                        &store,
                        &format!(
                            "INSERT DATA {{ GRAPH <{g_reports}> {{ <urn:report> <urn:secret:p> \"v2\" }} }}"
                        ),
                        &update_opts,
                    );
                    if allowed {
                        update_res.expect("authorized update should pass");
                        crate::sparql::update::update(
                            &store,
                            &format!(
                                "DELETE DATA {{ GRAPH <{g_reports}> {{ <urn:report> <urn:secret:p> \"v2\" }} }}"
                            ),
                            &Default::default(),
                        )
                        .unwrap();
                    } else {
                        assert!(
                            matches!(update_res, Err(Error::NotPermitted(_))),
                            "unauthorized update should be refused, got {update_res:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pass_all_pattern_stops_inner_scan_and_outer_loops_early() {
        let g_readable = "urn:g:readable";
        let g_meta = "urn:g:meta";
        let store = crate::store::Store::in_memory(Default::default());
        let mut trig = format!(
            "GRAPH <{g_meta}> {{ <{g_meta}> <urn:enabled> \"yes\" . }}\n\
             GRAPH <{g_readable}> {{\n"
        );
        for m in 0..3 {
            trig.push_str(&format!(
                "  <urn:m{m}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:SecretClass> .\n"
            ));
            for p in 0..3 {
                for v in 0..4 {
                    trig.push_str(&format!("  <urn:m{m}> <urn:secret:p{p}> \"v{v}\" .\n"));
                }
            }
        }
        trig.push_str("}\n");
        crate::sparql::update::update(
            &store,
            &format!("INSERT DATA {{ {trig} }}"),
            &Default::default(),
        )
        .unwrap();
        store.compact().unwrap();
        let snap = store.snapshot();

        let pattern = format!("GRAPH <{g_meta}> {{ <{g_meta}> <urn:enabled> \"yes\" }}");
        let readable_only = Graphs::Only(GraphRule::new([g_readable], &[]));

        let cases = [
            (
                "pass-all-predicates",
                readable_only.clone(),
                Some(vec![
                    "urn:secret:p0".into(),
                    "urn:secret:p1".into(),
                    "urn:secret:p2".into(),
                ]),
                None,
            ),
            (
                "pass-all-class-and-predicates",
                readable_only.clone(),
                Some(vec![
                    "urn:secret:p0".into(),
                    "urn:secret:p1".into(),
                    "urn:secret:p2".into(),
                ]),
                Some(vec!["urn:SecretClass".into()]),
            ),
            (
                "pass-all-class-only",
                readable_only,
                None,
                Some(vec!["urn:SecretClass".into()]),
            ),
            ("pass-all-unscoped", Graphs::All, None, None),
        ];

        for (name, read_graphs, predicates, classes) in cases {
            let access = GraphAccess::with_triples(
                read_graphs,
                Graphs::none(),
                TripleRules {
                    rules: vec![Rule {
                        protection: Arc::new(Protection {
                            name: name.into(),
                            predicates,
                            classes,
                            subclasses: false,
                            graphs: None,
                            pattern: Some(pattern.clone()),
                            prefixes: Default::default(),
                            hide_inferences: false,
                        }),
                        read: Graphs::none(),
                        write: Graphs::none(),
                    }],
                    caller: Caller::default(),
                    limits: Limits::default(),
                },
            );
            let prep = Preparation::new(Control::none(), 8 << 20).unwrap();
            let (view, ranges, visits) =
                observe_hidden_scan(|| access.masked_controlled(&snap, &prep));
            let view = view.unwrap();
            assert!(
                view.snapshot().mask.as_ref().unwrap().hidden.is_empty(),
                "{name}: expected empty mask"
            );
            assert_eq!(
                ranges, 1,
                "{name}: Pass::All must break out of outer range loop after first range"
            );
            assert_eq!(
                visits, 1,
                "{name}: Pass::All must stop inner scan after first matching candidate"
            );
        }
    }

    #[test]
    fn count_cache_masks_do_not_outlive_prepared_view_or_legacy_caller() {
        let store = crate::store::Store::in_memory(Default::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> <urn:o> ; <urn:q> <urn:v> }",
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        let policy = Arc::new(view());

        // Controlled legacy path deduplicates live holders and evicts on final drop.
        let prep1 = Preparation::new(Control::none(), 8 << 20).unwrap();
        let before1 = prep1.inner_used();
        let prep2 = Preparation::new(Control::none(), 8 << 20).unwrap();
        let before2 = prep2.inner_used();
        let first = policy.masked_controlled(&snap, &prep1).unwrap();
        assert_eq!(snap.counts.live_mask_count(), 1);
        assert_eq!(snap.counts.mask_entry_count(), 1);
        let weak_mask = Arc::downgrade(first.snapshot().mask.as_ref().unwrap());
        let second = policy.masked_controlled(&snap, &prep2).unwrap();
        assert!(first.same_snapshot_as(&second));
        assert_eq!(snap.counts.live_mask_count(), 1);
        drop(first);
        assert_eq!(prep1.inner_used(), before1);
        assert!(weak_mask.upgrade().is_some(), "second view still owns mask");
        drop(second);
        assert_eq!(prep2.inner_used(), before2);
        assert!(
            weak_mask.upgrade().is_none(),
            "mask must not survive on base snapshot after last PreparedView drops"
        );
        assert_eq!(snap.counts.live_mask_count(), 0);
        assert_eq!(snap.counts.mask_entry_count(), 0);

        // Strict Store::prepare_read never installs into CountCache::masks at all.
        let strict_prep = Preparation::new(Control::none(), 8 << 20).unwrap();
        let strict_before = strict_prep.inner_used();
        let strict_view = store.prepare_read(policy.clone(), &strict_prep).unwrap();
        assert_eq!(snap.counts.mask_entry_count(), 0);
        let strict_mask = Arc::downgrade(strict_view.snapshot().mask.as_ref().unwrap());
        drop(strict_view);
        assert_eq!(strict_prep.inner_used(), strict_before);
        assert!(strict_mask.upgrade().is_none());

        // Legacy GraphAccess::masked deduplicates live callers and evicts on drop.
        let m1 = policy.masked(&snap).unwrap();
        let m2 = policy.masked(&snap).unwrap();
        assert!(Arc::ptr_eq(&m1, &m2));
        let legacy_mask = Arc::downgrade(m1.mask.as_ref().unwrap());
        assert_eq!(snap.counts.live_mask_count(), 1);
        drop(m1);
        assert!(legacy_mask.upgrade().is_some());
        drop(m2);
        assert!(legacy_mask.upgrade().is_none());
        assert_eq!(snap.counts.live_mask_count(), 0);
        assert_eq!(snap.counts.mask_entry_count(), 0);

        // Mixed-ownership: legacy GraphAccess::masked cannot acquire a mask paid for by
        // masked_controlled, and dropping the controlled view drops its physical mask
        // immediately even while a legacy caller still holds its own mask.
        let mix_prep = Preparation::new(Control::none(), 8 << 20).unwrap();
        let mix_before = mix_prep.inner_used();
        let ctrl_view = policy.masked_controlled(&snap, &mix_prep).unwrap();
        let ctrl_weak = Arc::downgrade(ctrl_view.snapshot().mask.as_ref().unwrap());
        let leg_snap = policy.masked(&snap).unwrap();
        assert!(!ctrl_view.same_snapshot(&leg_snap));
        assert_ne!(ctrl_view.mask_id(), leg_snap.mask_id());
        assert_eq!(snap.counts.controlled_mask_entry_count(), 1);
        assert_eq!(snap.counts.legacy_mask_entry_count(), 1);
        drop(ctrl_view);
        assert_eq!(mix_prep.inner_used(), mix_before);
        assert!(
            ctrl_weak.upgrade().is_none(),
            "controlled mask must drop when PreparedView drops even if legacy mask is live"
        );
        assert_eq!(snap.counts.controlled_mask_entry_count(), 0);
        assert_eq!(snap.counts.legacy_mask_entry_count(), 1);
        drop(leg_snap);
        assert_eq!(snap.counts.legacy_mask_entry_count(), 0);

        // Warm hit failure (tight cap) releases the slot lock before failing so the
        // dead slot evicts cleanly when the last holder drops, and a subsequent cold
        // refill succeeds cleanly.
        let holder_prep = Preparation::new(Control::none(), 8 << 20).unwrap();
        let holder = policy.masked_controlled(&snap, &holder_prep).unwrap();
        let holder_weak = Arc::downgrade(holder.snapshot().mask.as_ref().unwrap());
        let tiny_prep = Preparation::new(Control::none(), 4096).unwrap();
        assert!(policy.masked_controlled(&snap, &tiny_prep).is_err());
        drop(holder);
        assert!(holder_weak.upgrade().is_none());
        assert_eq!(snap.counts.controlled_mask_entry_count(), 0);
        let refilled = policy.masked_controlled(&snap, &holder_prep).unwrap();
        assert_eq!(snap.counts.controlled_mask_entry_count(), 1);
        drop(refilled);
        assert_eq!(snap.counts.controlled_mask_entry_count(), 0);
    }

    #[test]
    fn count_cache_visible_graphs_do_not_outlive_their_holders() {
        let store = crate::store::Store::in_memory(Default::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { GRAPH <urn:g1> { <urn:s> <urn:p> 1 } GRAPH <urn:g2> { <urn:s> <urn:p> 2 } }",
            &Default::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        let access = GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["urn:g1"], &[])),
            Graphs::none(),
        );
        let first = access.visible_named(&snap).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(snap.counts.live_view_count(), 1);
        let second = access.visible_named(&snap).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let weak = Arc::downgrade(&first);
        drop(first);
        assert!(weak.upgrade().is_some());
        drop(second);
        assert!(
            weak.upgrade().is_none(),
            "VisibleGraphs must not stay strongly retained on base snapshot after holders drop"
        );
        assert_eq!(snap.counts.live_view_count(), 0);
        // Subsequent lookup prunes the dead entry before inserting the fresh one.
        let fresh = access.visible_named(&snap).unwrap();
        assert_eq!(snap.counts.view_entry_count(), 1);
        assert_eq!(snap.counts.live_view_count(), 1);
        drop(fresh);
        assert_eq!(snap.counts.live_view_count(), 0);
        assert!(snap.counts.view(&access.read_key()).is_none());
        assert_eq!(snap.counts.view_entry_count(), 0);
    }
}
