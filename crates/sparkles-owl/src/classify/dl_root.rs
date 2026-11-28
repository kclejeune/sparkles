//! The root context of a rooted run of roadmap item 603's Tier D (F24, "Nominals in the
//! disjunctive rules"): the context with the empty core, whose clauses hold no x, where the
//! facts about named individuals meet. It follows the general rooted calculus of Tena
//! Cucala, Cuenca Grau and Horrocks (*Pay-As-You-Go Consequence-Based Reasoning for the
//! Description Logic SROIQ*, AIJ 2021, Tables 2 and 3).
//!
//! The root starts with `⊤ → N_u(u)` for the singleton class `N_u` of every constant u, so
//! that every named individual exists, and with what the ontology's clauses with the body
//! `owl:Thing` give each constant. Its Hyper rule binds the ontology clause's x to a
//! constant, so that a premise is a root clause whose maximal literal is a ground atom
//! `B(u)`. The ontology clauses are those of the EL index, read as the rules of
//! [`super::dl_rules`] read them over x (told subsumers, conjunctions, existential
//! restrictions, disjointness and universal restrictions), Tier D's clauses with two or
//! more disjuncts, and `N_v(x) → x ≈ v` of each singleton class. An existential restriction
//! e gives `r(u, f_e(u))` and the atom of its successor's core over `f_e(u)`, and a
//! universal restriction meets the role atoms over `f_e(u)` in the root. The empty clause
//! makes the root unsatisfiable, which makes the ontology inconsistent.
//!
//! The equality rules are those of [`super::dl_eq`], which rewrite a constant everywhere in
//! a root literal. Join resolves a maximal ground atom or equality of constants with the
//! same literal in another clause's body, in the root and in every other context.
//!
//! Every context reasons about its own ground literals (F24, item 804's first departure).
//! Hyper binds the ontology clause's x to a constant in every context, so that a premise is
//! any clause whose maximal literal is a ground atom `B(u)`, and a role atom with a constant
//! at an end meets the universal restrictions of the constant's holders in the context that
//! holds it. Each context keeps its processed clauses by the constants of their maximal
//! literals for these rules, as the root does. The root holds the unconditional facts, and
//! its processed clauses with an empty body go to the contexts registered for one of
//! their constants, whether the registration or the clause comes first. A context
//! registers with the root for the constants of every maximal literal of its processed
//! clauses, inside `f_e(u)` included, and for those of the ground literals in its bodies.
//! The run uses no markers in the root, no r-Succ and no r-Pred of a root clause with a
//! body. A conditional ground clause of a context, such as `⊤ → B(u) ∨ C(x)`, has its
//! consequences at u derived in that context from the actual disjunction, where the
//! journal's root derives them under every combination of markers.
//!
//! A maximal literal over `f_e(u)` in any context takes Succ: the context registers with e's
//! hypothetical successor, which has the empty core, records the constant u for the edge and
//! sends the successor the markers of the atoms over `f_e(u)`. The edge is identified by the
//! context, the restriction and the constant, and is distinct from the context's edge for
//! `f_e(x)`. The successor's clauses over its predecessor come back with y read as u and x as
//! `f_e(u)` (Pred), for every constant the context reached it by, and a role atom between x
//! and a constant comes back too, as Table 3 admits for the root. Every context sends each
//! of its successors the markers of its maximal ground atoms and equalities of constants,
//! so that one edge covers every successor trigger of Definition 4 (the journal's K2).
//!
//! The at-most restrictions and the qualified roles of counting are not read in the root.

use super::dl_clause::{
    Clause, Lit, Made, RTerm, Rooted, XL, XLit, admit_lit, hash_table_bytes, hash_table_grow_bound,
    map_vec_bytes, map_vec_push_bound, map_vec_push_dedup, next_vec_cap, set_insert_bound,
    set_insert_delta, terms_of, vec_grow_bound, vec_push_delta,
};
use super::dl_rules::{
    Bill, DlIx, DlSt, Found, MREPAIR_CF, MREPAIR_HOLDER, MREPAIR_NO_REPLAY, MREPAIR_NO_RETURN,
    MREPAIR_ON, MREPAIR_UNITS, MREPAIR_WIDE, Meter, PRUNE_SCRATCH_ELEM, RECORD_SLACK, RootDerive,
    Side, WORK_READS, dix, dl, drain, drain_admitted, drain_with, drain_with_admitted, ds,
    ds_admit, for_each_all, ground_restrict, mark_interrupted, merge_parts, no_xy, pred_id,
    restricted, side, side_exact_bytes, side_parts_bytes, unit,
};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, CreditGuard, DlGuard, Guard, Sink, State};
use rustc_hash::{FxHashMap, FxHashSet};

/// What a context of a rooted run keeps for the rules at the constants and over `f_e(u)`.
/// Every context keeps its processed clauses by constant and its edges over `f_e(u)`. The
/// root keeps the contexts registered with it and the facts it sends them, and another
/// context keeps the root's facts it received and the constants it registered for.
#[derive(Default, Clone)]
pub struct RootSt {
    /// the processed clauses with a maximal ground atom `B(u)`, by u
    by_const: FxHashMap<u32, Vec<u32>>,
    /// the processed clauses with a maximal role atom that has the constant u at an end,
    /// by u
    roles_of: FxHashMap<u32, Vec<u32>>,
    /// in the root, the contexts registered for each constant
    edges: FxHashMap<u32, Vec<Ex>>,
    /// in the root, the same registrations as pairs of context and constant
    edge_set: FxHashSet<(Ex, u32)>,
    /// in the root, the processed clauses with an empty body, by each constant of their
    /// heads
    ready: FxHashMap<u32, Vec<u32>>,
    /// in another context, the root's clauses it received, by table id
    seen: FxHashSet<u32>,
    /// in another context, the constants it registered with the root for
    edged: FxHashSet<u32>,
    /// the context's maximal ground atoms and equalities of constants, in the order found,
    /// which its successors take as markers
    ground: Vec<Lit>,
    ground_set: FxHashSet<Lit>,
    /// the ground markers sent, by successor
    gsent: FxHashSet<(Ex, Lit)>,
    /// in another context, the ground markers it holds
    gmarks: FxHashSet<Lit>,
    /// the context's edges over `f_e(u)`, by existential restriction and constant
    succ_set: FxHashSet<(Ex, u32)>,
    /// the existential restrictions and constants of the context's edges over `f_e(u)` to
    /// each successor
    by_succ: FxHashMap<Ex, Vec<(Ex, u32)>>,
    /// cached constants-only successor clauses that arrived before any eligible edge and
    /// have not yet been delivered, by successor
    pub(super) pred_pending: FxHashMap<Ex, Vec<u32>>,
    /// row 1231's marker repair state, made with its first use and only with the repair
    m: Option<Box<MRep>>,
}

/// What a context keeps for row 1231's marker repair (PLAN v3.1's rule M), apart from
/// [`RootSt`] so that a run without the repair holds none of it.
#[derive(Default, Clone)]
struct MRep {
    /// with row 1231's marker repair, in another context, the ground literals it told the
    /// root it holds (M-send)
    mheld: FxHashSet<Lit>,
    /// with the repair, in another context, the clauses the root returned to it, by table
    /// id, under each literal of their bodies (M-return)
    rets: FxHashMap<Lit, Vec<u32>>,
    /// with the repair, in the root, the contexts that hold each ground literal
    holders: FxHashMap<Lit, Vec<Ex>>,
    /// with the repair, in the root, the same holders as pairs of context and literal
    hold_set: FxHashSet<(Ex, Lit)>,
    /// with the repair, in the root, the held literals by id, under each of their constants
    held_at: FxHashMap<u32, Vec<u32>>,
    /// with the repair, in the root, its processed clauses whose bodies are ground
    /// triggers, under each body literal
    gbody: FxHashMap<Lit, Vec<u32>>,
    /// with the repair's pair set, whether the root reaches the context by successors,
    /// which the root is from its start
    reached: bool,
    /// with the repair's pair set, in the root, the contexts it reaches by successors that
    /// told it so, and the constants each context registered for
    reach_set: FxHashSet<Ex>,
    regs: FxHashMap<Ex, Vec<u32>>,
    /// with the repair's pair set, in a context the root reaches, the successors it told
    told: FxHashSet<Ex>,
    /// with the repair, in the root, the ids of its processed clauses with an empty body
    /// and one ground literal in the head, under that literal
    units: FxHashMap<Lit, Vec<u32>>,
}

impl MRep {
    fn bytes(&self) -> usize {
        std::mem::size_of::<MRep>()
            + hash_table_bytes(self.mheld.capacity(), std::mem::size_of::<Lit>())
            + map_vec_bytes(&self.rets)
            + map_vec_bytes(&self.holders)
            + hash_table_bytes(self.hold_set.capacity(), std::mem::size_of::<(Ex, Lit)>())
            + map_vec_bytes(&self.held_at)
            + map_vec_bytes(&self.gbody)
            + hash_table_bytes(self.reach_set.capacity(), std::mem::size_of::<Ex>())
            + map_vec_bytes(&self.regs)
            + hash_table_bytes(self.told.capacity(), std::mem::size_of::<Ex>())
            + map_vec_bytes(&self.units)
    }
}

impl RootSt {
    /// The existential restrictions and constants of the context's edges over `f_e(u)` to
    /// the successor w.
    pub(super) fn edges_to(&self, w: Ex) -> &[(Ex, u32)] {
        self.by_succ.get(&w).map_or(&[][..], |v| v)
    }

    pub fn bytes(&self) -> usize {
        std::mem::size_of::<RootSt>()
            + map_vec_bytes(&self.by_const)
            + map_vec_bytes(&self.roles_of)
            + map_vec_bytes(&self.edges)
            + hash_table_bytes(self.edge_set.capacity(), std::mem::size_of::<(Ex, u32)>())
            + map_vec_bytes(&self.ready)
            + hash_table_bytes(self.seen.capacity(), std::mem::size_of::<u32>())
            + hash_table_bytes(self.edged.capacity(), std::mem::size_of::<u32>())
            + self.ground.capacity() * std::mem::size_of::<Lit>()
            + hash_table_bytes(self.ground_set.capacity(), std::mem::size_of::<Lit>())
            + hash_table_bytes(self.gsent.capacity(), std::mem::size_of::<(Ex, Lit)>())
            + hash_table_bytes(self.gmarks.capacity(), std::mem::size_of::<Lit>())
            + hash_table_bytes(self.succ_set.capacity(), std::mem::size_of::<(Ex, u32)>())
            + map_vec_bytes(&self.by_succ)
            + map_vec_bytes(&self.pred_pending)
            + self.m.as_deref().map_or(0, MRep::bytes)
    }

    /// The repair's state, which [`mrep_new`] made.
    fn mr(&self) -> &MRep {
        self.m.as_deref().expect("made by mrep_new")
    }
    fn mw(&mut self) -> &mut MRep {
        self.m.as_deref_mut().expect("made by mrep_new")
    }

    /// Pre-admits and inserts `v` into `set`. Returns `Some(true)` when inserted,
    /// `Some(false)` when already present, and `None` when refused.
    fn ins_admit<T: std::hash::Hash + Eq, S: Sink>(
        set: &mut FxHashSet<T>,
        v: T,
        out: &mut S,
    ) -> Option<bool> {
        if set.contains(&v) {
            return Some(false);
        }
        let Some(bound) = set_insert_bound(set, &v) else {
            out.speculative_overflow();
            return None;
        };
        if bound > 0 && !out.admit_dl(bound) {
            return None;
        }
        let delta = set_insert_delta(set, v).unwrap_or(0);
        if bound > 0 {
            out.commit_dl(bound, delta);
        }
        Some(true)
    }

    /// Pre-admits and pushes `id` to the list of `k` in `map` unless already last.
    fn push_admit<K: std::hash::Hash + Eq, S: Sink>(
        map: &mut FxHashMap<K, Vec<u32>>,
        k: K,
        id: u32,
        out: &mut S,
    ) -> bool {
        if map.get(&k).is_some_and(|v| v.last() == Some(&id)) {
            return true;
        }
        let Some(bound) = map_vec_push_bound(map, &k) else {
            out.speculative_overflow();
            return false;
        };
        if bound > 0 && !out.admit_dl(bound) {
            return false;
        }
        let delta = map_vec_push_dedup(map, k, id);
        if bound > 0 {
            out.commit_dl(bound, delta);
        }
        true
    }
}

/// A role's inverse, the qualified roles' included, for the literals of the root.
#[inline]
fn inv_of<'a>(ix: &'a Index, dx: &'a DlIx) -> impl Fn(Role) -> Option<Role> + Copy + Sync + 'a {
    move |r| dx.inv(ix, r)
}

/// The literal of `l`, which in the root is never true or false but for an equality or
/// inequality of a term with itself.
#[cfg(test)]
#[inline]
fn lit<S: Sink>(ix: &Index, out: &S, l: XLit) -> Made {
    out.dl().lits.lit(l, inv_of(ix, dix(ix)))
}

/// The literal of a ground atom `a(u)`.
#[cfg(test)]
#[inline]
fn atom<S: Sink>(ix: &Index, out: &S, a: Ex, u: u32) -> Lit {
    match lit(ix, out, XLit::Atom(a, RTerm::C(u))) {
        Made::Lit(l) => l,
        _ => unreachable!("an atom is a literal"),
    }
}

/// Upper bound on peak growth when pushing one `u32` under each distinct key in `keys`.
fn map_vec_push_keys_bound<K: Eq + std::hash::Hash>(
    m: &FxHashMap<K, Vec<u32>>,
    keys: &[K],
) -> Option<usize> {
    map_vec_push_keys_bound_with(m, keys, || true).expect("a read that never stops")
}

/// [`map_vec_push_keys_bound`], telling `read` of each key it looks up, and `None` once
/// read answers false.
fn map_vec_push_keys_bound_with<K: Eq + std::hash::Hash>(
    m: &FxHashMap<K, Vec<u32>>,
    keys: &[K],
    mut read: impl FnMut() -> bool,
) -> Option<Option<usize>> {
    let mut bound = || -> Result<Option<usize>, ()> {
        let Some(new_vec) = next_vec_cap(0, std::mem::size_of::<u32>())
            .and_then(|c| c.checked_mul(std::mem::size_of::<u32>()))
        else {
            return Ok(None);
        };
        let mut bound = 0usize;
        let mut new_keys = 0usize;
        for k in keys {
            if !read() {
                return Err(());
            }
            let grow = match m.get(k) {
                Some(v) => vec_grow_bound(v),
                None => {
                    new_keys += 1;
                    Some(new_vec)
                }
            };
            let Some(b) = grow.and_then(|g| bound.checked_add(g)) else {
                return Ok(None);
            };
            bound = b;
        }
        Ok(hash_table_grow_bound(
            m.len(),
            m.capacity(),
            new_keys,
            std::mem::size_of::<(K, Vec<u32>)>(),
        )
        .and_then(|t| bound.checked_add(t)))
    };
    bound().ok()
}

/// The literal of `l`, pre-admitting and revalidating any `Lits` growth against `out`.
#[inline]
fn lit_admitted<S: Sink>(ix: &Index, out: &mut S, l: XLit) -> Option<Made> {
    admit_lit(l, inv_of(ix, dix(ix)), out)
}

/// The literal of a ground atom `a(u)`, pre-admitting any `Lits` growth against `out`.
#[inline]
fn atom_admitted<S: Sink>(ix: &Index, out: &mut S, a: Ex, u: u32) -> Option<Lit> {
    match lit_admitted(ix, out, XLit::Atom(a, RTerm::C(u)))? {
        Made::Lit(l) => Some(l),
        _ => unreachable!("an atom is a literal"),
    }
}

/// Starts the root: `⊤ → N_u(u)` for every constant u, and what the ontology's clauses with
/// the body `owl:Thing` give u, streamed directly through [`derive`] with pre-admission of
/// `Lits` and `Store` growth so large constants-times-TOP inputs never accumulate an
/// unbounded `Found` collection before checking bounds.
pub fn init<S: Sink>(ix: &Index, st: &mut State, out: &mut S) {
    const INIT_POLL: usize = 256;
    st.init = true;
    let Some(x) = ds_admit(st, out) else {
        return;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    if out.dl().mrepair & MREPAIR_ON != 0 {
        if !mrep_admit(r, out) {
            x.store.interrupted = true;
            return;
        }
        r.mw().reached = true;
    }
    let dx = dix(ix);
    let top_h = ix.head(TOP);
    let trace = super::dl_rules::tracing();
    let poll_init = |st: &mut State, out: &mut S| -> bool {
        st.dl_choices += 1;
        if st.dl_choices as usize >= INIT_POLL {
            st.dl_choices = 0;
            if !out.go_on() {
                ds(st).store.interrupted = true;
                return false;
            }
        }
        true
    };
    let emit = |st: &mut State, head: Vec<Lit>, rule: &'static str, out: &mut S| -> bool {
        if st.bottom {
            return false;
        }
        if out.stopped() {
            ds(st).store.interrupted = true;
            return false;
        }
        if trace {
            let lits = &out.dl().lits;
            eprintln!("TR R {rule} [] -> {}", super::dl_rules::tr(ix, lits, &head));
        }
        derive(ix, st, (Vec::new(), head), out);
        if st.bottom {
            return false;
        }
        if out.stopped() {
            ds(st).store.interrupted = true;
            return false;
        }
        poll_init(st, out)
    };
    for &(u, n) in &dx.consts {
        if st.bottom {
            break;
        }
        if out.stopped() {
            ds(st).store.interrupted = true;
            return;
        }
        let Some(nu) = atom_admitted(ix, out, n, u) else {
            ds(st).store.interrupted = true;
            return;
        };
        if !emit(st, vec![nu], "init", out) {
            return;
        }
        if let Some(es) = dx.ne_of.get(&TOP) {
            for (i, &a) in es.iter().enumerate() {
                for &b in &es[i + 1..] {
                    let Some(m) = lit_admitted(ix, out, XLit::Ne(RTerm::Fc(a, u), RTerm::Fc(b, u)))
                    else {
                        ds(st).store.interrupted = true;
                        return;
                    };
                    if let Made::Lit(l) = m
                        && !emit(st, vec![l], "hyperc", out)
                    {
                        return;
                    }
                }
            }
        }
        for &t in ix.told_of(top_h) {
            let Some(l) = atom_admitted(ix, out, t, u) else {
                ds(st).store.interrupted = true;
                return;
            };
            if !emit(st, vec![l], "hyperc", out) {
                return;
            }
        }
        for &(p, k) in ix.conj_of(top_h) {
            if p == TOP {
                let Some(l) = atom_admitted(ix, out, k, u) else {
                    ds(st).store.interrupted = true;
                    return;
                };
                if !emit(st, vec![l], "hyperc", out) {
                    return;
                }
            } else if !poll_init(st, out) {
                return;
            }
        }
        if let Some(ids) = dx.by_body.get(&TOP) {
            for &cl in ids {
                let (body, head) = &dx.clauses[cl as usize];
                // During init no clause has been processed yet, so only `TOP` has a partner.
                if !body.iter().all(|&a| a == TOP) {
                    if !poll_init(st, out) {
                        return;
                    }
                    continue;
                }
                let Some(head_bytes) = head.len().checked_mul(std::mem::size_of::<Lit>()) else {
                    out.speculative_overflow();
                    ds(st).store.interrupted = true;
                    return;
                };
                let mut head_guard = DlGuard::new(out);
                if head_bytes > 0 && !head_guard.admit_add(head_bytes) {
                    ds(st).store.interrupted = true;
                    return;
                }
                let mut lits = Vec::with_capacity(head.len());
                for &a in head.iter() {
                    let Some(l) = atom_admitted(ix, &mut head_guard, a, u) else {
                        ds(st).store.interrupted = true;
                        return;
                    };
                    lits.push(l);
                    st.dl_choices += 1;
                    if st.dl_choices as usize >= INIT_POLL {
                        st.dl_choices = 0;
                        if !head_guard.go_on() {
                            ds(st).store.interrupted = true;
                            return;
                        }
                    }
                }
                if st.bottom {
                    return;
                }
                if head_guard.stopped() {
                    ds(st).store.interrupted = true;
                    return;
                }
                if trace {
                    let dl_lits = &head_guard.dl().lits;
                    eprintln!(
                        "TR R hyperc [] -> {}",
                        super::dl_rules::tr(ix, dl_lits, &lits)
                    );
                }
                let consumed = derive_covered(
                    ix,
                    st,
                    (Vec::new(), lits),
                    head_guard.bytes(),
                    &mut head_guard,
                );
                if consumed {
                    let _ = head_guard.take();
                }
                drop(head_guard);
                if st.bottom {
                    return;
                }
                if out.stopped() {
                    ds(st).store.interrupted = true;
                    return;
                }
                if !poll_init(st, out) {
                    return;
                }
            }
        }
    }
}

/// Runs the root's rules on up to `budget` clauses of its store, as
/// [`super::dl_rules::flush`] does for the other contexts.
pub fn flush<S: Sink>(ix: &Index, st: &mut State, out: &mut S, budget: usize) -> usize {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut n = 0;
    let mut max_guard = Guard::new(out);
    let mut max: Vec<Lit> = Vec::new();
    while n < budget {
        if st.bottom {
            break;
        }
        let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
            break;
        };
        let Some(id) = x.store.peek_live(max_guard.dl()) else {
            break;
        };
        let Some((views_bytes, max_realloc)) =
            x.store.next_scratch_bound(id, true, &max, max.capacity())
        else {
            max_guard.speculative_overflow();
            x.store.interrupted = true;
            break;
        };
        let max_live = max.capacity() * std::mem::size_of::<Lit>();
        let Some(scratch_peak) = max_live
            .checked_add(max_realloc)
            .and_then(|b| b.checked_add(views_bytes))
        else {
            max_guard.speculative_overflow();
            x.store.interrupted = true;
            break;
        };
        if !max_guard.ensure_admitted(scratch_peak) {
            x.store.interrupted = true;
            break;
        }
        let ground_total = super::dl_clause::ground_total();
        let bound = {
            let o = Rooted {
                ix,
                lits: &max_guard.dl().lits,
                inv: &inv,
                ground_total,
            };
            x.store.next_prepare_id(ix, Some(&o), id, &mut max)
        };
        max_guard.release_to(max.capacity() * std::mem::size_of::<Lit>());
        let Some(bound) = bound else {
            max_guard.speculative_overflow();
            x.store.interrupted = true;
            break;
        };
        if !max_guard.admit_dl(bound) {
            x.store.interrupted = true;
            break;
        }
        let delta = {
            let o = Rooted {
                ix,
                lits: &max_guard.dl().lits,
                inv: &inv,
                ground_total,
            };
            x.store.next_commit(ix, Some(&o), id, &max, max_guard.dl())
        };
        max_guard.commit_dl(bound, delta);
        index_consts(ix, x, &max, id, &mut max_guard);
        if max_guard.stopped() {
            x.store.interrupted = true;
            break;
        }
        if max_guard.dl().merge_enabled()
            && const_eq(&max_guard, &x.store.clauses[id as usize].c, inv).is_some()
        {
            if !harvest_and_halt_merge(x, inv, &mut max_guard) {
                x.store.interrupted = true;
            }
            break;
        }
        let split_mode = max_guard.dl().split_mode();
        if split_mode != super::dl_rules::SplitMode::Disabled {
            let mut scan = Scan(0);
            let cand = disj_const_eq_sink(
                &x.store.clauses[id as usize].c,
                id as usize,
                &mut max_guard,
                inv,
                &mut scan,
            );
            scan.charge_pending(&mut max_guard);
            if max_guard.stopped() {
                x.store.interrupted = true;
                break;
            }
            if let Some(cand) = cand {
                // ALWAYS POPULATE `best` + atomic hints for both pure and mixed clauses,
                // regardless of current work count.
                max_guard.dl().record_split_cand(cand);
                if cand.prio == 2
                    && (split_mode == super::dl_rules::SplitMode::EagerPureThenProbe
                        || max_guard.dl().split_probe() == 0)
                {
                    let won = {
                        let mut slot = max_guard.dl().split_slot.lock();
                        if slot.winner.is_none() {
                            slot.winner = Some(cand);
                            let won = max_guard.publish_split_stop();
                            if !won {
                                slot.winner = None;
                            }
                            won
                        } else {
                            false
                        }
                    };
                    if won {
                        max_guard.wake_split_stop();
                        break;
                    }
                    if max_guard.stopped() {
                        x.store.interrupted = true;
                        break;
                    }
                }
            }
        }
        let c_bytes = {
            let c = &x.store.clauses[id as usize].c;
            (c.body().len() + c.head().len()) * std::mem::size_of::<Lit>()
        };
        let Some(live_need) = (max.capacity() * std::mem::size_of::<Lit>()).checked_add(c_bytes)
        else {
            max_guard.speculative_overflow();
            x.store.interrupted = true;
            break;
        };
        if !max_guard.ensure_admitted(live_need) {
            x.store.interrupted = true;
            break;
        }
        n += 1;
        let c = x.store.clauses[id as usize].c.clone();
        process(ix, st, &c, &max, &mut max_guard);
        offer(ix, st, id, &c, &mut max_guard);
        m_offer(ix, st, id, &c, &mut max_guard);
        drop(c);
        max_guard.release_to(max.capacity() * std::mem::size_of::<Lit>());
        if max_guard.stopped() {
            break;
        }
    }
    n
}

/// Returns `(min(u, v), max(u, v))` when `c` is an unconditional root unit constant
/// equality `⊤ → u ≈ v` with `u != v` and neither constant skipped by `DlRun::merge_skip`.
/// Strictly rejects non-empty bodies, disjunctions, inequalities, and function/variable
/// terms.
fn const_eq<S: Sink>(
    out: &S,
    c: &Clause,
    inv: impl Fn(Role) -> Option<Role> + Copy,
) -> Option<(u32, u32)> {
    if !c.body().is_empty() || c.head().len() != 1 {
        return None;
    }
    let XLit::Eq(RTerm::C(u), RTerm::C(v)) = out.dl().lits.view(c.head()[0], inv) else {
        return None;
    };
    if u == v {
        return None;
    }
    if let Some(skip) = out.dl().merge_skip
        && (u == skip || v == skip)
    {
        return None;
    }
    Some((u.min(v), u.max(v)))
}

/// Harvests every live empty-body unit constant equality in the root's store (both
/// processed and waiting, including the popped clause still live in `x.store.clauses`)
/// using two metered passes with pre-admitted exact capacity, publishes `STOP_MERGE` via
/// CAS, and transfers ownership and its single memory debit to `DlRun::merged`.
///
/// Returns `true` only when this call won `STOP_MERGE` and published the payload.
/// Returns `false` if any bound, memory refusal, cancellation, timeout, or racing stop
/// intervened, leaving no leaked debit or stale payload.
fn harvest_and_halt_merge<S: Sink>(
    x: &mut DlSt,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    out: &mut S,
) -> bool {
    let mut count = 0usize;
    let mut scan = Scan(0);
    for s in &x.store.clauses {
        if !scan.read(1, out) {
            return false;
        }
        if !s.dead && const_eq(out, &s.c, inv).is_some() {
            count += 1;
        }
    }
    scan.end(out);
    if !out.go_on() || count == 0 {
        return false;
    }
    let Some(bytes) = count.checked_mul(std::mem::size_of::<(u32, u32)>()) else {
        out.speculative_overflow();
        return false;
    };
    let mut pair_guard = DlGuard::new(out);
    if !pair_guard.admit_add(bytes) {
        return false;
    }
    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(count);
    let mut scan2 = Scan(0);
    for s in &x.store.clauses {
        if !scan2.read(1, &mut pair_guard) {
            return false;
        }
        if !s.dead
            && let Some(p) = const_eq(&pair_guard, &s.c, inv)
        {
            debug_assert!(pairs.len() < count);
            pairs.push(p);
        }
    }
    scan2.end(&mut pair_guard);
    if !pair_guard.go_on() {
        return false;
    }
    let Some(kept) = pairs
        .capacity()
        .checked_mul(std::mem::size_of::<(u32, u32)>())
    else {
        pair_guard.speculative_overflow();
        return false;
    };
    if kept > pair_guard.bytes() && !pair_guard.admit_add(kept - pair_guard.bytes()) {
        return false;
    }
    if !pair_guard.halt_merge() {
        return false;
    }
    pair_guard.dl().set_merged(pairs, kept);
    pair_guard.commit(kept);
    true
}

/// Literals scanned between cooperative polls in [`disj_const_eq`].
const HEAD_POLL_EVERY: usize = 1024;

fn inspect_split_lit(
    lit: Lit,
    dl: &super::dl_rules::DlRun,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    all_pos_const_eq: &mut bool,
    best_pair: &mut Option<(u32, u32, u32, u32)>,
) {
    match dl.lits.view(lit, inv) {
        XLit::Eq(RTerm::C(a), RTerm::C(b)) => {
            if a == b {
                return;
            }
            let ra = dl.const_rank.get(a as usize).copied().unwrap_or(u32::MAX);
            let rb = dl.const_rank.get(b as usize).copied().unwrap_or(u32::MAX);
            if ra == u32::MAX || rb == u32::MAX || ra == rb {
                return;
            }
            let pair_id = (a.min(b), a.max(b));
            if dl.branch_neq.binary_search(&pair_id).is_ok() {
                return;
            }
            let cand_pair = if ra < rb {
                (ra, rb, a, b)
            } else {
                (rb, ra, b, a)
            };
            if best_pair.is_none_or(|cur| (cand_pair.0, cand_pair.1) < (cur.0, cur.1)) {
                *best_pair = Some(cand_pair);
            }
        }
        _ => {
            *all_pos_const_eq = false;
        }
    }
}

/// Inspects a live empty-body root clause with `head().len() >= 2` during `dl_root::flush`,
/// charging every completed block of reads to `out` before polling `out.go_on()`.
fn disj_const_eq_sink<S: Sink>(
    c: &Clause,
    slot_id: usize,
    out: &mut S,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    scan: &mut Scan,
) -> Option<super::dl_rules::SplitCand> {
    if !c.body().is_empty() || c.head().len() < 2 {
        return None;
    }
    let clause_id = u32::try_from(slot_id).ok()?;
    let head = c.head();
    let mut all_pos_const_eq = true;
    let mut best_pair: Option<(u32, u32, u32, u32)> = None;
    for (idx, &lit) in head.iter().enumerate() {
        scan.0 = scan.0.saturating_add(1);
        if idx > 0 && idx % HEAD_POLL_EVERY == 0 {
            scan.charge_pending(out);
            if !out.go_on() {
                return None;
            }
        }
        inspect_split_lit(lit, out.dl(), inv, &mut all_pos_const_eq, &mut best_pair);
    }
    let (rank_lo, rank_hi, u_id, v_id) = best_pair?;
    Some(super::dl_rules::SplitCand {
        epoch: out.dl().split_epoch(),
        prio: if all_pos_const_eq { 2 } else { 1 },
        head_len: u32::try_from(head.len()).unwrap_or(u32::MAX),
        rank_lo,
        rank_hi,
        u_id,
        v_id,
        clause_id,
    })
}

/// Inspects a live empty-body root clause with `head().len() >= 2` for an eligible
/// undecided disjunctive constant equality (`XLit::Eq(RTerm::C(a), RTerm::C(b))`).
///
/// Scans every literal in `c.head()`, metering reads under `scan` and polling `poll()`
/// every [`HEAD_POLL_EVERY`] literals. Returns `None` when stopped, ineligible, or when
/// every constant-equality disjunct is already decided in `dl.branch_neq`.
pub(crate) fn disj_const_eq(
    c: &Clause,
    slot_id: usize,
    dl: &super::dl_rules::DlRun,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    scan: &mut Scan,
    poll: &mut impl FnMut(&mut usize) -> bool,
) -> Option<super::dl_rules::SplitCand> {
    if !c.body().is_empty() || c.head().len() < 2 {
        return None;
    }
    let clause_id = u32::try_from(slot_id).ok()?;
    let head = c.head();
    let mut all_pos_const_eq = true;
    let mut best_pair: Option<(u32, u32, u32, u32)> = None; // (rank_lo, rank_hi, u_id, v_id)
    for (idx, &lit) in head.iter().enumerate() {
        scan.0 = scan.0.saturating_add(1);
        if idx > 0 && idx % HEAD_POLL_EVERY == 0 && !poll(&mut scan.0) {
            return None;
        }
        inspect_split_lit(lit, dl, inv, &mut all_pos_const_eq, &mut best_pair);
    }
    let (rank_lo, rank_hi, u_id, v_id) = best_pair?;
    Some(super::dl_rules::SplitCand {
        epoch: dl.split_epoch(),
        prio: if all_pos_const_eq { 2 } else { 1 },
        head_len: u32::try_from(head.len()).unwrap_or(u32::MAX),
        rank_lo,
        rank_hi,
        u_id,
        v_id,
        clause_id,
    })
}

/// Metered, cooperatively polled rescan of the root clause store under `root_st` when
/// `SplitSlot::best` was killed by subsumption or was empty at probe time.
pub(super) fn rescan_split_cand(
    ix: &Index,
    x: &DlSt,
    dl: &super::dl_rules::DlRun,
    poll: &mut impl FnMut() -> bool,
) -> Option<super::dl_rules::SplitCand> {
    let inv = inv_of(ix, dix(ix));
    let mut best: Option<super::dl_rules::SplitCand> = None;
    let mut scan = Scan(0);
    let mut charge_reads = |reads: &mut usize| -> bool {
        let work = *reads / WORK_READS;
        if work > 0 {
            dl.count(work);
            *reads %= WORK_READS;
        }
        poll()
    };
    for (slot_id, s) in x.store.clauses.iter().enumerate() {
        scan.0 = scan.0.saturating_add(1);
        if scan.0 >= M_SCAN && !charge_reads(&mut scan.0) {
            return None;
        }
        if s.dead {
            continue;
        }
        if let Some(cand) = disj_const_eq(&s.c, slot_id, dl, inv, &mut scan, &mut charge_reads)
            && best.is_none_or(|cur| cand.cmp_key() < cur.cmp_key())
        {
            best = Some(cand);
        }
        if scan.0 >= M_SCAN && !charge_reads(&mut scan.0) {
            return None;
        }
    }
    if scan.0 > 0 && !charge_reads(&mut scan.0) {
        return None;
    }
    best
}

/// Lists the processed clause `id` of a context by the constants of its maximal literals
/// `max`, in the context's indexes by constant, before the rules so that a clause meets
/// itself.
pub(super) fn index_consts<S: Sink>(
    ix: &Index,
    x: &mut super::dl_rules::DlSt,
    max: &[Lit],
    id: u32,
    out: &mut S,
) {
    let inv = inv_of(ix, dix(ix));
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    for &l in max {
        match out.dl().lits.view(l, inv) {
            XLit::Atom(_, RTerm::C(u)) => {
                if !RootSt::push_admit(&mut r.by_const, u, id, out) {
                    x.store.interrupted = true;
                    return;
                }
            }
            XLit::Role(_, a, b) => {
                for t in [a, b] {
                    if let RTerm::C(u) = t
                        && !RootSt::push_admit(&mut r.roles_of, u, id, out)
                    {
                        x.store.interrupted = true;
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

/// The rules on one clause of the root's store, whose maximal literals are `max`.
fn process<S: Sink>(ix: &Index, st: &mut State, c: &Clause, max: &[Lit], out: &mut S) {
    let dx = dix(ix);
    let me = dx.root.expect("a rooted run");
    // Declare `found_guard` strictly before `found` so LIFO drop always frees `found`
    // before releasing pre-admitted bytes, including on early return or panic unwind.
    let mut found_guard = Guard::new(out);
    let mut found = Found::default();
    found.root = true;
    if super::dl_rules::tracing() {
        let lits = &found_guard.dl().lits;
        eprintln!(
            "TR P R - {} -> {}",
            super::dl_rules::tr(ix, lits, c.body()),
            super::dl_rules::tr(ix, lits, c.head())
        );
    }
    if !join_body(ix, st, c, &mut found, &mut found_guard) {
        mark_interrupted(st);
        return;
    }
    for &l in max {
        if found_guard.stopped() {
            mark_interrupted(st);
            return;
        }
        let Some(prem_bytes) = super::dl_rules::side_bytes(c, l) else {
            found_guard.speculative_overflow();
            mark_interrupted(st);
            return;
        };
        let Some(prem_need) = found.held_bytes().checked_add(prem_bytes) else {
            found_guard.speculative_overflow();
            mark_interrupted(st);
            return;
        };
        if !found_guard.ensure_admitted(prem_need) {
            mark_interrupted(st);
            return;
        }
        let prem = side(c, l);
        debug_assert_eq!(super::dl_rules::side_cap_bytes(&prem), prem_bytes);
        found.set_extra_bytes(prem_bytes);
        let q = super::dl_eq::Eqs {
            ix,
            dx,
            root: true,
            query: None,
        };
        if !q.on_max(st, l, &prem, &mut found, &mut found_guard, false, false) {
            mark_interrupted(st);
            return;
        }
        if !join_head(ix, st, c, l, &prem, &mut found, &mut found_guard, false) {
            mark_interrupted(st);
            return;
        }
        let v = found_guard.dl().lits.view(l, inv_of(ix, dx));
        if ground_trigger(v) {
            ground_out(ix, st, l, &mut found_guard);
        }
        if !at_consts(ix, me, st, l, v, &prem, &mut found, &mut found_guard, false) {
            mark_interrupted(st);
            return;
        }
        drop(prem);
        found.set_extra_bytes(0);
        found_guard.release_to(found.held_bytes());
    }
    debug_assert_eq!(found.extra_bytes(), 0);
    drain_with_admitted(ix, st, found, &mut found_guard, &RootDerive);
}

/// The rules at the constants on a new premise `prem` of the context `me` whose maximal
/// literal is `l`, viewed as `v`: Succ over each successor term `f_e(u)` of v, with the
/// markers of an atom over it and Pred with the clauses the successor already sent, Hyper
/// with x bound to u on a ground atom `B(u)`, and the universal restrictions of the
/// constant's holders on a role atom with a constant at an end.
#[allow(clippy::too_many_arguments)]
fn at_consts<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    l: Lit,
    v: XLit,
    prem: &Side,
    found: &mut Found,
    out: &mut Guard<'_, S>,
    gbody: bool,
) -> bool {
    let dx = dix(ix);
    let (ts, n) = terms_of(v);
    for k in 0..n {
        let RTerm::Fc(e, u) = ts[k] else { continue };
        if ts[..k].contains(&ts[k]) {
            continue;
        }
        let w = succ_at(ix, me, st, e, u, found, out);
        if out.stopped() {
            return false;
        }
        if let XLit::Atom(a, _) = v {
            super::dl_rules::send_hyp(ix, st, w, a, out);
        }
        if let Some(cids) = dl(st).and_then(|x| x.pred_in.get(&w)) {
            for &cid in cids {
                if !root_pred(ix, st, e, u, cid, Some((l, prem)), found, out) {
                    mark_interrupted(st);
                    return false;
                }
            }
        }
    }
    match v {
        XLit::Atom(d, RTerm::C(u)) => hyper_at(ix, st, out, d, u, prem, found, gbody),
        v @ XLit::Role(..) => {
            let (ends, n) = from(v, inv_of(ix, dx));
            for &(u, s, t) in &ends[..n] {
                if !on_role(ix, st, out, s, u, t, prem, found, gbody) {
                    return false;
                }
            }
            if let Some((s, u)) = to_const(v, inv_of(ix, dx))
                && !univ_to_const(ix, st, out, s, u, prem, found)
            {
                return false;
            }
            true
        }
        _ => true,
    }
}

/// The role atom `v` read from `x` to a constant: `(s, u)` for `s(x, u)`, by the inverse
/// role when x is its second term.
fn to_const(v: XLit, inv: impl Fn(Role) -> Option<Role>) -> Option<(Role, u32)> {
    match v {
        XLit::Role(s, RTerm::X, RTerm::C(u)) => Some((s, u)),
        XLit::Role(s, RTerm::C(u), RTerm::X) => inv(s).map(|i| (i, u)),
        _ => None,
    }
}

/// Hyper on a new premise `prem` of the role atom `s(x, u)` with the universal restrictions
/// that the holders of `x` stand for: `∀t.C(x) ∧ s(x, u) → C(u)` for `s ⊑ t`, with the
/// unit holders and the clauses whose maximal literal is a holder over `x`. The inference
/// is local to the context, since no predecessor sees `s(x, u)`, so a selected premise
/// takes part.
fn univ_to_const<S: Sink>(
    ix: &Index,
    st: &State,
    out: &mut Guard<'_, S>,
    s: Role,
    u: u32,
    prem: &Side,
    found: &mut Found,
) -> bool {
    found.rule = "univc";
    let dx = dix(ix);
    let over = |b: Ex,
                pb: &[Lit],
                ph: &[Lit],
                without: Option<Lit>,
                found: &mut Found,
                out: &mut Guard<'_, S>|
     -> bool {
        for_each_all(ix, b, |t, c| {
            if dx.sub(ix, s, t) {
                let Some(made) = lit_admitted(ix, out, XLit::Atom(c, RTerm::C(u))) else {
                    return false;
                };
                if let Made::Lit(m) = made {
                    let Some(bytes) = side_parts_bytes(prem, pb, ph, without, Some(m)) else {
                        out.speculative_overflow();
                        return false;
                    };
                    if !found.push_admitted(
                        bytes,
                        || merge_parts(prem, pb, ph, without, Some(m)),
                        out,
                    ) {
                        return false;
                    }
                }
            }
            true
        })
    };
    if !over(TOP, &[], &[], None, found, out) {
        return false;
    }
    for &b in &st.subs {
        if b != TOP && !over(b, &[], &[], None, found, out) {
            return false;
        }
    }
    let Some(d) = dl(st) else { return true };
    let mut holders: Vec<Ex> = d.store.by_x.keys().copied().collect();
    holders.sort_unstable();
    for b in holders {
        if st.subs.contains(&b) {
            continue;
        }
        for i in d.store.with_x(b) {
            let cl = &d.store.clauses[i as usize].c;
            if !over(b, cl.body(), cl.head(), Some(Lit::x(b)), found, out) {
                return false;
            }
        }
    }
    true
}

/// Hyper on a new premise `prem` of a holder `d(x)` of universal restrictions with the
/// processed role atoms from `x` to a constant: `∀t.C(x) ∧ s(x, u) → C(u)` for `s ⊑ t`,
/// the other arrival order of [`univ_to_const`].
pub(super) fn univ_from_x<S: Sink>(
    ix: &Index,
    st: &State,
    d: Ex,
    prem: &Side,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    found.rule = "univx";
    let dx = dix(ix);
    let mut has_any = false;
    for_each_all(ix, d, |_, _| {
        has_any = true;
        false
    });
    if !has_any {
        return true;
    }
    let Some(x) = dl(st) else { return true };
    let Some(r) = x.root.as_deref() else {
        return true;
    };
    let inv = inv_of(ix, dx);
    let mut ids: Vec<u32> = r.roles_of.values().flatten().copied().collect();
    ids.sort_unstable();
    ids.dedup();
    for i in ids {
        let c = &x.store.clauses[i as usize];
        if c.dead {
            continue;
        }
        for &l in x.store.maxima(i) {
            let Some((s, u)) = to_const(out.dl().lits.view(l, inv), inv) else {
                continue;
            };
            if !for_each_all(ix, d, |t, b| {
                if dx.sub(ix, s, t) {
                    let Some(made) = lit_admitted(ix, out, XLit::Atom(b, RTerm::C(u))) else {
                        return false;
                    };
                    if let Made::Lit(m) = made {
                        let Some(bytes) =
                            side_parts_bytes(prem, c.c.body(), c.c.head(), Some(l), Some(m))
                        else {
                            out.speculative_overflow();
                            return false;
                        };
                        if !found.push_admitted(
                            bytes,
                            || merge_parts(prem, c.c.body(), c.c.head(), Some(l), Some(m)),
                            out,
                        ) {
                            return false;
                        }
                    }
                }
                true
            }) {
                return false;
            }
        }
    }
    true
}

/// Visits each live premise whose maximal literal is `l` directly from the borrowed store
/// without allocating an intermediate `Vec<Side>`, including selected clauses when `sel` is
/// set, stopping early when `cb` returns `false`. Passes `(body, head, without)`.
fn for_each_partner_of(
    st: &State,
    l: Lit,
    sel: bool,
    mut cb: impl FnMut(&[Lit], &[Lit], Option<Lit>) -> bool,
) -> bool {
    let Some(d) = dl(st) else {
        return true;
    };
    for &i in d.store.by_xl.get(&l).into_iter().flatten() {
        let s = &d.store.clauses[i as usize];
        if !s.dead && (sel || !s.sel) && !cb(s.c.body(), s.c.head(), Some(l)) {
            return false;
        }
    }
    true
}

/// Visits each live, un-`gbody`-cut premise of `a(u)` directly from the borrowed store in
/// store order without allocating an intermediate partner buffer or interning missing
/// literals, calling `f` with `(partner_body, partner_head, without_lit, fact)`. Each
/// entry of the store's index that it reads is a read of `meter` (item 1280), and the visit
/// is false once a check of the bounds stopped the meter.
fn for_each_partner_at<S: Sink>(
    ix: &Index,
    st: &State,
    out: &S,
    meter: &Meter,
    a: Ex,
    u: u32,
    mut f: impl FnMut(&[Lit], &[Lit], Option<Lit>, bool),
) -> bool {
    if a == TOP {
        f(&[], &[], None, true);
        return true;
    }
    let Some(d) = dl(st) else {
        return true;
    };
    let inv = inv_of(ix, dix(ix));
    let Some(l) = out.dl().lits.get(XLit::Atom(a, RTerm::C(u)), inv) else {
        return true;
    };
    for &i in d.store.by_xl.get(&l).into_iter().flatten() {
        if !meter.read(1, out) {
            return false;
        }
        let s = &d.store.clauses[i as usize];
        if !s.dead && !s.gbody {
            // the visit reads the partner's literals, which may be many
            if !meter.read(s.c.body().len() + s.c.head().len(), out) {
                return false;
            }
            f(s.c.body(), s.c.head(), Some(l), s.fact);
        }
    }
    true
}

/// Zero-allocation equivalent of [`restricted`] over borrowed `prem` and partner slices.
#[allow(clippy::too_many_arguments)]
pub(super) fn restricted_parts(
    lits: &super::dl_clause::Lits,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    prem_body: &[Lit],
    part_body: &[Lit],
    prem_head: &[Lit],
    part_head: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) -> bool {
    if !ground_restrict() || (prem_body.is_empty() && part_body.is_empty()) {
        return false;
    }
    prem_body
        .iter()
        .copied()
        .chain(part_body.iter().copied())
        .chain(prem_head.iter().copied())
        .chain(part_head.iter().copied().filter(|&l| Some(l) != without))
        .chain(add)
        .all(|l| l.kind() == XL && no_xy(lits.view(l, inv)))
}

/// Zero-allocation equivalent of [`routable_unit`] over borrowed `prem` and partner slices.
#[allow(clippy::too_many_arguments)]
pub(super) fn routable_unit_parts(
    ix: &Index,
    st: &State,
    lits: &super::dl_clause::Lits,
    prem_body: &[Lit],
    part_body: &[Lit],
    prem_head: &[Lit],
    part_head: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) -> bool {
    if !prem_body.is_empty() || !part_body.is_empty() {
        return false;
    }
    let Some(r) = dl(st).and_then(|x| x.root.as_deref()) else {
        return false;
    };
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut unit_lit: Option<Lit> = None;
    for l in prem_head
        .iter()
        .copied()
        .chain(part_head.iter().copied().filter(|&l| Some(l) != without))
        .chain(add)
    {
        if l.kind() != XL {
            return false;
        }
        match lits.view(l, inv) {
            XLit::Atom(TOP, _) => return false,
            XLit::Atom(BOTTOM, _) => continue,
            _ => match unit_lit {
                None => unit_lit = Some(l),
                Some(prev) if prev == l => {}
                Some(_) => return false,
            },
        }
    }
    let Some(l) = unit_lit else {
        return false;
    };
    let (ts, n) = terms_of(lits.view(l, inv));
    if n == 0 {
        return false;
    }
    for &t in &ts[..n] {
        match t {
            RTerm::C(u) | RTerm::Fc(_, u) => {
                if !r.edged.contains(&u) {
                    return false;
                }
            }
            RTerm::X | RTerm::Y | RTerm::Fx(_) => return false,
        }
    }
    true
}

/// Streams a D1-suppressed conclusion directly into `w` from borrowed literal slices
/// without allocating intermediate `Side`, `Vec`, or `String` buffers.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_shared_parts(
    mut w: impl std::io::Write,
    ix: &Index,
    st: &State,
    lits: &super::dl_clause::Lits,
    rule: &'static str,
    prem_body: &[Lit],
    part_body: &[Lit],
    prem_head: &[Lit],
    part_head: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) -> std::io::Result<()> {
    struct Body<'a>(&'a Index, &'a super::dl_clause::Lits, &'a [Lit], &'a [Lit]);
    impl std::fmt::Display for Body<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("[")?;
            let mut first = true;
            for l in self.2.iter().copied().chain(self.3.iter().copied()) {
                if !first {
                    f.write_str(", ")?;
                }
                first = false;
                super::dl_rules::write_lit(f, self.0, self.1, l)?;
            }
            f.write_str("]")
        }
    }
    struct Head<'a>(
        &'a Index,
        &'a super::dl_clause::Lits,
        &'a [Lit],
        &'a [Lit],
        Option<Lit>,
        Option<Lit>,
    );
    impl std::fmt::Display for Head<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("[")?;
            let mut first = true;
            for l in self
                .2
                .iter()
                .copied()
                .chain(self.3.iter().copied().filter(|&l| Some(l) != self.4))
                .chain(self.5)
            {
                if !first {
                    f.write_str(", ")?;
                }
                first = false;
                super::dl_rules::write_lit(f, self.0, self.1, l)?;
            }
            f.write_str("]")
        }
    }
    writeln!(
        w,
        "TR S {} {rule} {} -> {}",
        super::dl_rules::fmt_ctx(st, false),
        Body(ix, lits, prem_body, part_body),
        Head(ix, lits, prem_head, part_head, without, add),
    )
}

/// Logs a D1-suppressed conclusion lazily when `DLTRACE` is enabled without allocating
/// an intermediate `Side` or formatting scratch buffers.
#[allow(clippy::too_many_arguments)]
pub(super) fn trace_shared_parts(
    ix: &Index,
    st: &State,
    lits: &super::dl_clause::Lits,
    rule: &'static str,
    prem_body: &[Lit],
    part_body: &[Lit],
    prem_head: &[Lit],
    part_head: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) {
    #[cfg(test)]
    if super::dl_rules::test_trace_active() {
        let _ = write_shared_parts(
            std::io::sink(),
            ix,
            st,
            lits,
            rule,
            prem_body,
            part_body,
            prem_head,
            part_head,
            without,
            add,
        );
        return;
    }
    let _ = write_shared_parts(
        std::io::stderr().lock(),
        ix,
        st,
        lits,
        rule,
        prem_body,
        part_body,
        prem_head,
        part_head,
        without,
        add,
    );
}

/// Traverses live unselected partners of `partner_ex(u)` directly in store order without
/// allocating an intermediate partner buffer, pre-admitting `Found::one` growth and every
/// emitted `Side` buffer through `out` before allocation. Returns `false` on memory refusal
/// or overflow (marking `st` interrupted).
#[allow(clippy::too_many_arguments)]
pub(super) fn push_partners_admitted<S: Sink>(
    ix: &Index,
    st: &mut State,
    out: &mut Guard<'_, S>,
    partner_ex: Ex,
    u: u32,
    add_ex: Option<Ex>,
    check_fact: bool,
    prem: &Side,
    found: &mut Found,
) -> bool {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let meter = Meter::checks();
    let mut any_partner = false;
    if !for_each_partner_at(ix, st, out, &meter, partner_ex, u, |_, _, _, _| {
        any_partner = true;
    }) {
        mark_interrupted(st);
        return false;
    }
    if !any_partner {
        return true;
    }
    let add_lit = match add_ex {
        Some(k) => {
            let Some(l) = atom_admitted(ix, out, k, u) else {
                mark_interrupted(st);
                return false;
            };
            Some(l)
        }
        None => None,
    };
    let given_fact = check_fact && dl(st).is_some_and(|x| x.given_fact);
    let mut add_count = 0usize;
    let mut side_bytes = 0usize;
    let mut overflow = false;
    let go = for_each_partner_at(
        ix,
        st,
        out,
        &meter,
        partner_ex,
        u,
        |pb, ph, without, fact| {
            if overflow {
                return;
            }
            let lits = &out.dl().lits;
            if restricted_parts(lits, inv, &prem.0, pb, &prem.1, ph, without, add_lit) {
                return;
            }
            if given_fact
                && fact
                && routable_unit_parts(ix, st, lits, &prem.0, pb, &prem.1, ph, without, add_lit)
            {
                if super::dl_rules::tracing() {
                    trace_shared_parts(
                        ix, st, lits, found.rule, &prem.0, pb, &prem.1, ph, without, add_lit,
                    );
                }
                return;
            }
            let Some(bytes) = side_parts_bytes(prem, pb, ph, without, add_lit)
                .and_then(|b| side_bytes.checked_add(b))
            else {
                overflow = true;
                return;
            };
            let Some(next_count) = add_count.checked_add(1) else {
                overflow = true;
                return;
            };
            add_count = next_count;
            side_bytes = bytes;
        },
    );
    if !go {
        mark_interrupted(st);
        return false;
    }
    if overflow {
        out.speculative_overflow();
        mark_interrupted(st);
        return false;
    }
    if add_count == 0 {
        return true;
    }
    if !found.reserve_one_admitted(add_count, side_bytes, out) {
        mark_interrupted(st);
        return false;
    }
    let go = for_each_partner_at(
        ix,
        st,
        out,
        &meter,
        partner_ex,
        u,
        |pb, ph, without, fact| {
            let lits = &out.dl().lits;
            if restricted_parts(lits, inv, &prem.0, pb, &prem.1, ph, without, add_lit) {
                return;
            }
            if given_fact
                && fact
                && routable_unit_parts(ix, st, lits, &prem.0, pb, &prem.1, ph, without, add_lit)
            {
                return;
            }
            found.push(merge_parts(prem, pb, ph, without, add_lit));
        },
    );
    out.release_to(found.held_bytes());
    if !go {
        mark_interrupted(st);
    }
    go
}

/// Whether `a(u)` has a certified empty-body root-unit premise in `st` (including implicit
/// `owl:Thing`), without interning any new literal in `Lits`. Each index entry it reads is
/// a read of `meter`, and a check that stops the meter answers false.
fn has_fact_at<S: Sink>(ix: &Index, st: &State, out: &S, meter: &Meter, a: Ex, u: u32) -> bool {
    if a == TOP {
        return true;
    }
    let Some(d) = dl(st) else {
        return false;
    };
    let inv = inv_of(ix, dix(ix));
    let Some(l) = out.dl().lits.get(XLit::Atom(a, RTerm::C(u)), inv) else {
        return false;
    };
    for &i in d.store.by_xl.get(&l).into_iter().flatten() {
        if !meter.read(1, out) {
            return false;
        }
        let s = &d.store.clauses[i as usize];
        if !s.dead && !s.gbody && s.fact {
            return true;
        }
    }
    false
}

/// Whether `c` is a non-tautological empty-body normalized unit whose single head literal
/// mentions only constants or successor terms of constants (`C` or `Fc`, explicitly
/// excluding `X`, `Y` and `Fx`) and every such constant is registered with the root in
/// `r.edged`. Zero heap allocation.
fn routable_unit(ix: &Index, st: &State, lits: &super::dl_clause::Lits, c: &Side) -> bool {
    if !c.0.is_empty() {
        return false;
    }
    let Some(r) = dl(st).and_then(|x| x.root.as_deref()) else {
        return false;
    };
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut unit_lit: Option<Lit> = None;
    for &l in &c.1 {
        if l.kind() != XL {
            return false;
        }
        match lits.view(l, inv) {
            XLit::Atom(TOP, _) => return false,
            XLit::Atom(BOTTOM, _) => continue,
            _ => match unit_lit {
                None => unit_lit = Some(l),
                Some(prev) if prev == l => {}
                Some(_) => return false,
            },
        }
    }
    let Some(l) = unit_lit else {
        return false;
    };
    let (ts, n) = terms_of(lits.view(l, inv));
    if n == 0 {
        return false;
    }
    for &t in &ts[..n] {
        match t {
            RTerm::C(u) | RTerm::Fc(_, u) => {
                if !r.edged.contains(&u) {
                    return false;
                }
            }
            RTerm::X | RTerm::Y | RTerm::Fx(_) => return false,
        }
    }
    true
}

/// Whether a ground inference's conclusion `c` in a non-root context is already derived by
/// the root and delivered as a root unit: both the processed premise (`given_fact`) and the
/// partner (`partner_fact`) are certified root units, and `c` normalizes to an empty-body
/// ground unit over constants this context registered for.
pub(super) fn shared(
    ix: &Index,
    st: &State,
    lits: &super::dl_clause::Lits,
    partner_fact: bool,
    c: &Side,
) -> bool {
    partner_fact && dl(st).is_some_and(|x| x.given_fact) && routable_unit(ix, st, lits, c)
}

/// Logs a D1-suppressed conclusion lazily when `DLTRACE` is enabled without heap scratch.
pub(super) fn trace_shared(
    ix: &Index,
    st: &State,
    lits: &super::dl_clause::Lits,
    rule: &'static str,
    c: &Side,
) {
    trace_shared_parts(ix, st, lits, rule, &c.0, &[], &c.1, &[], None, None);
}

/// Pre-admits and pushes the conclusion formed by `(prem, pb, ph, without, add)` into
/// `found` through `out`, checking `restricted_parts` and D1 `routable_unit_parts` without
/// allocating the `Side` first. Returns `false` on memory refusal or overflow.
#[allow(clippy::too_many_arguments)]
fn put_with_fact_parts_admitted<S: Sink>(
    ix: &Index,
    st: &State,
    out: &mut Guard<'_, S>,
    found: &mut Found,
    ground: bool,
    fact: bool,
    prem: &Side,
    pb: &[Lit],
    ph: &[Lit],
    without: Option<Lit>,
    add: Option<Lit>,
) -> bool {
    let inv = inv_of(ix, dix(ix));
    let lits = &out.dl().lits;
    if ground && restricted_parts(lits, inv, &prem.0, pb, &prem.1, ph, without, add) {
        return true;
    }
    if ground
        && fact
        && dl(st).is_some_and(|x| x.given_fact)
        && routable_unit_parts(ix, st, lits, &prem.0, pb, &prem.1, ph, without, add)
    {
        if super::dl_rules::tracing() {
            trace_shared_parts(
                ix, st, lits, found.rule, &prem.0, pb, &prem.1, ph, without, add,
            );
        }
        return true;
    }
    let Some(bytes) = side_parts_bytes(prem, pb, ph, without, add) else {
        out.speculative_overflow();
        return false;
    };
    found.push_admitted(bytes, || merge_parts(prem, pb, ph, without, add), out)
}

/// Hyper on a new premise `prem` of `d(u)`: the ontology clauses with d in the body, with
/// their x bound to u. A conclusion is a ground inference's unless it holds x or y. A
/// premise cut by `gbody` (including a marker) takes only the universals over the role
/// atoms from u that are local to the context ([`super::dl_rules::local`]).
#[allow(clippy::too_many_arguments)]
fn hyper_at<S: Sink>(
    ix: &Index,
    st: &mut State,
    out: &mut Guard<'_, S>,
    d: Ex,
    u: u32,
    prem: &Side,
    found: &mut Found,
    gbody: bool,
) -> bool {
    found.rule = "hyperc";
    let dx = dix(ix);
    if gbody {
        return hyper_roles(ix, st, out, d, u, prem, found, gbody);
    }
    let h = ix.head(d);
    match h.node {
        Node::Conj(a, b) => {
            let Some(la) = atom_admitted(ix, out, a, u) else {
                mark_interrupted(st);
                return false;
            };
            let Some(lb) = atom_admitted(ix, out, b, u) else {
                mark_interrupted(st);
                return false;
            };
            if !put_with_fact_parts_admitted(
                ix,
                st,
                out,
                found,
                true,
                true,
                prem,
                &[],
                &[],
                None,
                Some(la),
            ) || !put_with_fact_parts_admitted(
                ix,
                st,
                out,
                found,
                true,
                true,
                prem,
                &[],
                &[],
                None,
                Some(lb),
            ) {
                mark_interrupted(st);
                return false;
            }
        }
        Node::Exist(r, _) => {
            let f = match super::dl_rules::into_nominal(ix, d) {
                Some((_, v)) => RTerm::C(v),
                None => RTerm::Fc(d, u),
            };
            for xl in [XLit::Role(r, RTerm::C(u), f), XLit::Atom(h.succ, f)] {
                let Some(made) = lit_admitted(ix, out, xl) else {
                    mark_interrupted(st);
                    return false;
                };
                if let Made::Lit(l) = made
                    && !put_with_fact_parts_admitted(
                        ix,
                        st,
                        out,
                        found,
                        true,
                        true,
                        prem,
                        &[],
                        &[],
                        None,
                        Some(l),
                    )
                {
                    mark_interrupted(st);
                    return false;
                }
            }
        }
        // N_v(x) → x ≈ v
        Node::Nominal(v) if v != u => {
            let Some(made) = lit_admitted(ix, out, XLit::Eq(RTerm::C(v), RTerm::C(u))) else {
                mark_interrupted(st);
                return false;
            };
            if let Made::Lit(l) = made
                && !put_with_fact_parts_admitted(
                    ix,
                    st,
                    out,
                    found,
                    true,
                    true,
                    prem,
                    &[],
                    &[],
                    None,
                    Some(l),
                )
            {
                mark_interrupted(st);
                return false;
            }
        }
        _ => {}
    }
    // M(x) → f_i(x) ≉ f_j(x) of a minimum's holder
    if let Some(es) = dx.ne_of.get(&d) {
        for (i, &a) in es.iter().enumerate() {
            for &b in &es[i + 1..] {
                let Some(made) = lit_admitted(ix, out, XLit::Ne(RTerm::Fc(a, u), RTerm::Fc(b, u)))
                else {
                    mark_interrupted(st);
                    return false;
                };
                if let Made::Lit(n) = made
                    && !put_with_fact_parts_admitted(
                        ix,
                        st,
                        out,
                        found,
                        true,
                        true,
                        prem,
                        &[],
                        &[],
                        None,
                        Some(n),
                    )
                {
                    mark_interrupted(st);
                    return false;
                }
            }
        }
    }
    for &t in ix.told_of(h) {
        let Some(lt) = atom_admitted(ix, out, t, u) else {
            mark_interrupted(st);
            return false;
        };
        if !put_with_fact_parts_admitted(
            ix,
            st,
            out,
            found,
            true,
            true,
            prem,
            &[],
            &[],
            None,
            Some(lt),
        ) {
            mark_interrupted(st);
            return false;
        }
    }
    for &(p, k) in ix.conj_of(h) {
        if !push_partners_admitted(ix, st, out, p, u, Some(k), true, prem, found) {
            return false;
        }
    }
    for &set in ix.disjoint_of(h) {
        for &m in &dx.sets[set as usize] {
            if m != d && !push_partners_admitted(ix, st, out, m, u, None, false, prem, found) {
                return false;
            }
        }
    }
    if let Some(ids) = dx.by_body.get(&d) {
        let given = dl(st).is_some_and(|x| x.given_fact);
        let empty_side = unit();
        // one meter for every clause of d, which reads each body position it visits
        let meter = Meter::checks();
        for &cl in ids {
            let (body, head) = &dx.clauses[cl as usize];
            // Unit-only specialization: every certified root unit (including implicit TOP)
            // has the empty residual, which subsumes every alternative at its position.
            // If every body position has such a witness, pruning leaves only `base`, which
            // we suppress iff `base` itself passes D1, avoiding any mask allocation.
            let all_facts = given
                && body.iter().all(|&a| {
                    meter.read(1, out) && (a == d || has_fact_at(ix, st, out, &meter, a, u))
                });
            if meter.stopped() {
                mark_interrupted(st);
                return false;
            }
            if all_facts {
                if head.len() == 1 {
                    let Some(l0) = atom_admitted(ix, out, head[0], u) else {
                        mark_interrupted(st);
                        return false;
                    };
                    if !put_with_fact_parts_admitted(
                        ix,
                        st,
                        out,
                        found,
                        true,
                        true,
                        &empty_side,
                        &[],
                        &[],
                        None,
                        Some(l0),
                    ) {
                        mark_interrupted(st);
                        return false;
                    }
                } else {
                    let Some(side_bytes) = side_exact_bytes(0, head.len()) else {
                        out.speculative_overflow();
                        mark_interrupted(st);
                        return false;
                    };
                    if !found.reserve_one_admitted(1, side_bytes, out) {
                        mark_interrupted(st);
                        return false;
                    }
                    let mut h_lits = Vec::with_capacity(head.len());
                    for &a in head.iter() {
                        let Some(la) = atom_admitted(ix, out, a, u) else {
                            drop(h_lits);
                            out.release_to(found.held_bytes());
                            mark_interrupted(st);
                            return false;
                        };
                        h_lits.push(la);
                    }
                    let base: Side = (Vec::new(), h_lits);
                    if !restricted(&out.dl().lits, inv_of(ix, dx), &base) {
                        if shared(ix, st, &out.dl().lits, true, &base) {
                            if super::dl_rules::tracing() {
                                trace_shared(ix, st, &out.dl().lits, found.rule, &base);
                            }
                        } else {
                            found.push(base);
                        }
                    }
                    out.release_to(found.held_bytes());
                }
                continue;
            }
            let Some(mut lists_bytes) = body.len().checked_mul(std::mem::size_of::<Vec<Side>>())
            else {
                out.speculative_overflow();
                mark_interrupted(st);
                return false;
            };
            let mut empty_pos = false;
            let mut max_list_len = 0usize;
            let mut overflow = false;
            for &a in body.iter() {
                if !meter.read(1, out) {
                    mark_interrupted(st);
                    return false;
                }
                let mut count = 0usize;
                let mut sides_bytes = 0usize;
                if a == d {
                    count = 1;
                    let Some(b) = side_exact_bytes(prem.0.len(), prem.1.len()) else {
                        overflow = true;
                        break;
                    };
                    sides_bytes = b;
                } else {
                    let go =
                        for_each_partner_at(ix, st, out, &meter, a, u, |pb, ph, without, _| {
                            if overflow {
                                return;
                            }
                            let Some(sb) = side_parts_bytes(&empty_side, pb, ph, without, None)
                                .and_then(|b| sides_bytes.checked_add(b))
                            else {
                                overflow = true;
                                return;
                            };
                            let Some(nc) = count.checked_add(1) else {
                                overflow = true;
                                return;
                            };
                            count = nc;
                            sides_bytes = sb;
                        });
                    if !go {
                        mark_interrupted(st);
                        return false;
                    }
                    if overflow {
                        break;
                    }
                }
                if count == 0 {
                    empty_pos = true;
                    break;
                }
                max_list_len = max_list_len.max(count);
                let Some(pos_bytes) = count
                    .checked_mul(std::mem::size_of::<Side>())
                    .and_then(|b| b.checked_add(sides_bytes))
                    .and_then(|b| lists_bytes.checked_add(b))
                else {
                    overflow = true;
                    break;
                };
                lists_bytes = pos_bytes;
            }
            if overflow {
                out.speculative_overflow();
                mark_interrupted(st);
                return false;
            }
            if empty_pos {
                continue;
            }
            let Some(base_bytes) = side_exact_bytes(0, head.len()) else {
                out.speculative_overflow();
                mark_interrupted(st);
                return false;
            };
            let Some(prod_bytes) = lists_bytes.checked_add(base_bytes) else {
                out.speculative_overflow();
                mark_interrupted(st);
                return false;
            };
            let prune_scratch = if max_list_len < 2 {
                0
            } else {
                let Some(ps) = max_list_len.checked_mul(PRUNE_SCRATCH_ELEM) else {
                    out.speculative_overflow();
                    mark_interrupted(st);
                    return false;
                };
                ps
            };
            if !found.reserve_prod_admitted(prod_bytes, prune_scratch, 0, out) {
                mark_interrupted(st);
                return false;
            }
            let mut h_lits = Vec::with_capacity(head.len());
            for &a in head.iter() {
                let Some(la) = atom_admitted(ix, out, a, u) else {
                    drop(h_lits);
                    out.release_to(found.held_bytes());
                    mark_interrupted(st);
                    return false;
                };
                h_lits.push(la);
            }
            let mut lists = Vec::with_capacity(body.len());
            for &a in body.iter() {
                if a == d {
                    lists.push(vec![(prem.0.clone(), prem.1.clone())]);
                } else {
                    let mut count = 0usize;
                    let mut v = Vec::new();
                    let go =
                        for_each_partner_at(ix, st, out, &meter, a, u, |_, _, _, _| count += 1)
                            && {
                                v.reserve_exact(count);
                                for_each_partner_at(
                                    ix,
                                    st,
                                    out,
                                    &meter,
                                    a,
                                    u,
                                    |pb, ph, without, _| {
                                        v.push(merge_parts(&empty_side, pb, ph, without, None));
                                    },
                                )
                            };
                    if !go {
                        drop((v, lists, h_lits));
                        out.release_to(found.held_bytes());
                        mark_interrupted(st);
                        return false;
                    }
                    lists.push(v);
                }
            }
            let base: Side = (Vec::new(), h_lits);
            debug_assert_eq!(
                super::dl_rules::prod_inner_bytes(lists.capacity(), &lists, &base),
                prod_bytes
            );
            found.ground_products(lists, base);
            out.release_to(found.held_bytes());
        }
    }
    hyper_roles(ix, st, out, d, u, prem, found, gbody)
}

/// The universals `d(u)` stands for, over the role atoms from u in the context. A role atom
/// whose other end is a constant or a successor term of one makes a ground inference, which
/// takes no `gbody`-cut premise, and any other is local and takes every premise.
#[allow(clippy::too_many_arguments)]
fn hyper_roles<S: Sink>(
    ix: &Index,
    st: &mut State,
    out: &mut Guard<'_, S>,
    d: Ex,
    u: u32,
    prem: &Side,
    found: &mut Found,
    gbody: bool,
) -> bool {
    found.rule = "hyperr";
    let dx = dix(ix);
    let mut has_any = false;
    for_each_all(ix, d, |_, _| {
        has_any = true;
        false
    });
    if !has_any {
        return true;
    }
    let inv = inv_of(ix, dx);
    let mut failed = false;
    if let Some(x) = dl(st)
        && let Some(r) = x.root.as_deref()
    {
        'outer: for &i in r.roles_of.get(&u).into_iter().flatten() {
            let s = &x.store.clauses[i as usize];
            if s.dead {
                continue;
            }
            for &l in x.store.maxima(i) {
                let (ends, n) = from(out.dl().lits.view(l, inv), inv);
                for &(w, t, f) in &ends[..n] {
                    if w != u {
                        continue;
                    }
                    if matches!(f, RTerm::C(_) | RTerm::Fc(..)) && (gbody || s.gbody) {
                        continue;
                    }
                    if !for_each_all(ix, d, |a, b| {
                        if dx.sub(ix, t, a) {
                            let Some(made) = lit_admitted(ix, out, XLit::Atom(b, f)) else {
                                return false;
                            };
                            if let Made::Lit(m) = made
                                && !put_with_fact_parts_admitted(
                                    ix,
                                    st,
                                    out,
                                    found,
                                    true,
                                    s.fact,
                                    prem,
                                    s.c.body(),
                                    s.c.head(),
                                    Some(l),
                                    Some(m),
                                )
                            {
                                return false;
                            }
                        }
                        true
                    }) {
                        failed = true;
                        break 'outer;
                    }
                }
            }
        }
    }
    if failed {
        mark_interrupted(st);
        return false;
    }
    true
}

/// The role atom `v` read from each constant u at one of its ends: `(u, s, t)` for
/// `s(u, t)`, by the inverse role when u is its second term, on the stack without heap
/// allocation.
fn from(v: XLit, inv: impl Fn(Role) -> Option<Role>) -> ([(u32, Role, RTerm); 2], usize) {
    let mut out = [(0, 0, RTerm::X); 2];
    let mut n = 0;
    if let XLit::Role(s, a, b) = v {
        if let RTerm::C(u) = a {
            out[n] = (u, s, b);
            n += 1;
        }
        if let RTerm::C(u) = b
            && let Some(i) = inv(s)
        {
            out[n] = (u, i, a);
            n += 1;
        }
    }
    (out, n)
}

/// Hyper on a new premise `prem` of the role atom `s(u, f)`: the universals of the holders
/// of u, `owl:Thing` included, and the disjoint roles. The inference is ground when f is a
/// constant or a successor term of one, and the restriction then applies and no `gbody`-cut
/// premise takes part; otherwise it is local and every premise takes part.
#[allow(clippy::too_many_arguments)]
fn on_role<S: Sink>(
    ix: &Index,
    st: &State,
    out: &mut Guard<'_, S>,
    s: Role,
    u: u32,
    f: RTerm,
    prem: &Side,
    found: &mut Found,
    gbody: bool,
) -> bool {
    found.rule = "onrole";
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let ground = matches!(f, RTerm::C(_) | RTerm::Fc(..));
    if ground && gbody {
        return true;
    }
    let over = |b: Ex,
                pb: &[Lit],
                ph: &[Lit],
                without: Option<Lit>,
                fact: bool,
                found: &mut Found,
                out: &mut Guard<'_, S>|
     -> bool {
        for_each_all(ix, b, |t, c| {
            if dx.sub(ix, s, t) {
                let Some(made) = lit_admitted(ix, out, XLit::Atom(c, f)) else {
                    return false;
                };
                if let Made::Lit(m) = made
                    && !put_with_fact_parts_admitted(
                        ix,
                        st,
                        out,
                        found,
                        ground,
                        fact,
                        prem,
                        pb,
                        ph,
                        without,
                        Some(m),
                    )
                {
                    return false;
                }
            }
            true
        })
    };
    if !over(TOP, &[], &[], None, true, found, out) {
        return false;
    }
    let Some(x) = dl(st) else { return true };
    if let Some(r) = x.root.as_deref() {
        for &i in r.by_const.get(&u).into_iter().flatten() {
            let c = &x.store.clauses[i as usize];
            if c.dead || ground && c.gbody {
                continue;
            }
            for &l in x.store.maxima(i) {
                if let XLit::Atom(b, RTerm::C(w)) = out.dl().lits.view(l, inv)
                    && w == u
                    && !over(b, c.c.body(), c.c.head(), Some(l), c.fact, found, out)
                {
                    return false;
                }
            }
        }
        for &(a, b) in &dx.rdis {
            if !dx.sub(ix, s, a) {
                continue;
            }
            for &i in r.roles_of.get(&u).into_iter().flatten() {
                let c = &x.store.clauses[i as usize];
                if c.dead || ground && c.gbody {
                    continue;
                }
                for &l in x.store.maxima(i) {
                    let (ends, n) = from(out.dl().lits.view(l, inv), inv);
                    for &(w, t, g) in &ends[..n] {
                        if w == u
                            && g == f
                            && dx.sub(ix, t, b)
                            && !put_with_fact_parts_admitted(
                                ix,
                                st,
                                out,
                                found,
                                ground,
                                c.fact,
                                prem,
                                c.c.body(),
                                c.c.head(),
                                Some(l),
                                None,
                            )
                        {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

/// Adds a clause the root's rules derived: the empty clause makes the root unsatisfiable,
/// and any other clause goes to the store unless it is redundant. The root has no units
/// over x, so even a unit is stored.
pub(super) fn derive<S: Sink>(ix: &Index, st: &mut State, side: Side, out: &mut S) {
    let _ = derive_covered(ix, st, side, 0, out);
}

/// [`derive`], carrying `covered_payload` bytes of an active [`Sink::admit_dl`] reservation
/// for the source literal buffer into the store. Returns `true` when `covered_payload` was
/// settled by `commit_dl`.
fn derive_covered<S: Sink>(
    ix: &Index,
    st: &mut State,
    (mut body, mut head): Side,
    covered_payload: usize,
    out: &mut S,
) -> bool {
    if st.bottom {
        return false;
    }
    out.dl().count_root(1);
    out.worked(1);
    let inv = inv_of(ix, dix(ix));
    let lits = &out.dl().lits;
    body.retain(|&l| !matches!(lits.view(l, inv), XLit::Atom(TOP, _)));
    let mut holds = false;
    head.retain(|&l| match lits.view(l, inv) {
        XLit::Atom(BOTTOM, _) => false,
        XLit::Atom(TOP, _) => {
            holds = true;
            true
        }
        _ => true,
    });
    if holds {
        return false;
    }
    body.sort_unstable();
    body.dedup();
    head.sort_unstable();
    head.dedup();
    // Γ ∧ A → Δ ∨ A and Γ → Δ ∨ s ≈ t ∨ s ≉ t
    if head.iter().any(|l| body.binary_search(l).is_ok()) || lits.complementary(&head, inv) {
        return false;
    }
    if body.is_empty() && head.is_empty() {
        out.me(Concl::Sub(BOTTOM));
        return false;
    }
    if let Some(x) = ds_admit(st, out) {
        x.store
            .add_parts_admitted_covered(body, head, covered_payload, out)
            .1
    } else {
        false
    }
}

/// Whether a clause is a marker `A → A`.
pub(super) fn is_marker(c: &Clause) -> bool {
    c.body().len() == 1 && c.body() == c.head()
}

/// Join on a new premise `prem` of the clause `c`, whose maximal literal is `l`: when l is a
/// ground atom or an equality of constants, the processed clauses with l in the body. A
/// marker is no premise on either side, since its conclusion would be the other premise,
/// and a selected clause is no premise on the side of the head, as ordered resolution
/// with selection has it. A Join of two clauses that hold neither x nor y is a ground
/// inference, and its conclusion then holds neither, so the restriction applies to the
/// conclusion as [`super::dl_rules::join_exempt`] says.
#[allow(clippy::too_many_arguments)]
pub(super) fn join_head<S: Sink>(
    ix: &Index,
    st: &State,
    c: &Clause,
    l: Lit,
    prem: &Side,
    found: &mut Found,
    out: &mut Guard<'_, S>,
    gbody: bool,
) -> bool {
    found.rule = "joinh";
    if is_marker(c) || gbody {
        return true;
    }
    let inv = inv_of(ix, dix(ix));
    let Some(x) = dl(st) else { return true };
    for &i in x.store.by_ground.get(&l).into_iter().flatten() {
        let s = &x.store.clauses[i as usize];
        if s.dead || is_marker(&s.c) {
            continue;
        }
        if super::dl_rules::join_restricted_parts(
            &out.dl().lits,
            inv,
            &prem.0,
            &prem.0,
            s.c.body(),
            Some(l),
            &prem.1,
            None,
            s.c.head(),
        ) {
            continue;
        }
        let extra_b = s.c.body().iter().filter(|&&m| m != l).count();
        let Some(bytes) = side_exact_bytes(prem.0.len() + extra_b, prem.1.len() + s.c.head().len())
        else {
            out.speculative_overflow();
            return false;
        };
        if !found.push_admitted(
            bytes,
            || {
                let mut b = Vec::with_capacity(prem.0.len() + extra_b);
                b.extend_from_slice(&prem.0);
                b.extend(s.c.body().iter().copied().filter(|&m| m != l));
                let mut h = Vec::with_capacity(prem.1.len() + s.c.head().len());
                h.extend_from_slice(&prem.1);
                h.extend_from_slice(s.c.head());
                (b, h)
            },
            out,
        ) {
            return false;
        }
    }
    true
}

/// Join on a new clause `c` with ground literals in its body: for each, the processed
/// clauses where it is maximal. The restriction on ground inferences applies to its
/// conclusions as [`super::dl_rules::join_exempt`] says.
pub(super) fn join_body<S: Sink>(
    ix: &Index,
    st: &State,
    c: &Clause,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    found.rule = "joinb";
    if is_marker(c) {
        return true;
    }
    let inv = inv_of(ix, dix(ix));
    // each body literal tested, each index entry read and each literal of a premise that
    // the restriction and the copy read is a read of the meter (item 1280), and a stop is
    // false
    let meter = Meter::checks();
    // a body has no repeats, so the residual body of each of its literals is one shorter
    let rest_b_len = c.body().len().saturating_sub(1);
    let rest_h_len = c.head().len();
    for &l in c.body() {
        if !meter.read(1, &*out) {
            return false;
        }
        if l.kind() != XL || !out.dl().lits.view(l, inv).is_ground() {
            continue;
        }
        let Some(x) = dl(st) else { return true };
        for &i in x.store.by_xl.get(&l).into_iter().flatten() {
            if !meter.read(1, &*out) {
                return false;
            }
            let s = &x.store.clauses[i as usize];
            if !s.dead && !s.gbody && !is_marker(&s.c) {
                let width = s.c.body().len() + s.c.head().len() + rest_b_len + rest_h_len;
                if !meter.read(width, &*out) {
                    return false;
                }
                if super::dl_rules::join_restricted_parts(
                    &out.dl().lits,
                    inv,
                    s.c.body(),
                    s.c.body(),
                    c.body(),
                    Some(l),
                    s.c.head(),
                    Some(l),
                    c.head(),
                ) {
                    continue;
                }
                let s_h_len = s.c.head().iter().filter(|&&m| m != l).count();
                let Some(bytes) =
                    side_exact_bytes(s.c.body().len() + rest_b_len, s_h_len + rest_h_len)
                else {
                    out.speculative_overflow();
                    return false;
                };
                if !found.push_admitted(
                    bytes,
                    || {
                        let mut b = Vec::with_capacity(s.c.body().len() + rest_b_len);
                        b.extend_from_slice(s.c.body());
                        b.extend(c.body().iter().copied().filter(|&m| m != l));
                        let mut h = Vec::with_capacity(s_h_len + rest_h_len);
                        h.extend(s.c.head().iter().copied().filter(|&m| m != l));
                        h.extend_from_slice(c.head());
                        (b, h)
                    },
                    out,
                ) {
                    return false;
                }
            }
        }
    }
    true
}

/// The constants of a literal, inside `f_e(u)` included.
fn consts(v: XLit, out: &mut Vec<u32>) {
    let (ts, n) = terms_of(v);
    for t in &ts[..n] {
        if let RTerm::C(u) | RTerm::Fc(_, u) = t {
            out.push(*u);
        }
    }
}

/// The rules of a context other than the root on a new premise `prem` of the clause `c`,
/// whose maximal literal is the interned literal `l`: the context registers with the root
/// for the constants of l, sends its successors the marker of a ground l, resolves a ground
/// l with the clauses that have it in the body (Join), and runs the rules at the constants
/// of l, Hyper with x bound to a constant and Succ over `f_e(u)`, with its own clauses.
#[allow(clippy::too_many_arguments)]
pub(super) fn on_max<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    c: &Clause,
    l: Lit,
    prem: &Side,
    found: &mut Found,
    out: &mut Guard<'_, S>,
    gbody: bool,
) -> bool {
    let dx = dix(ix);
    let v = out.dl().lits.view(l, inv_of(ix, dx));
    if let Some(root) = dx.root {
        let mut us = Vec::new();
        consts(v, &mut us);
        edge_to(root, me, st, us, out);
    }
    if ground_trigger(v) {
        ground_out(ix, st, l, out);
        if out.dl().mrepair & MREPAIR_ON != 0
            && !is_marker(c)
            && let Some(root) = dx.root
            && !m_note(ix, root, me, st, l, prem, out)
        {
            return true;
        }
    }
    if !join_head(ix, st, c, l, prem, found, out, gbody) {
        return false;
    }
    at_consts(ix, me, st, l, v, prem, found, out, gbody)
}

/// A context other than the root takes a clause the root sent, a processed root clause with
/// an empty body over one of the constants the context registered for, once.
pub fn rpred_c<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    match RootSt::ins_admit(&mut r.seen, cid, out) {
        Some(true) => {}
        Some(false) => return false,
        None => {
            x.store.interrupted = true;
            return false;
        }
    }
    let c = out.dl().table.get(cid);
    debug_assert!(c.body().is_empty(), "the root sends its facts");
    let mut found = Found::default();
    found.rule = "rpred";
    found.push((Vec::new(), c.head().to_vec()));
    ds(st).taking_fact = true;
    drain(ix, st, found, out);
    ds(st).taking_fact = false;
    true
}

/// The root takes a fact that a hypothetical successor derived, a clause with an empty
/// body whose literals hold neither `x` nor `y`: the successor has the empty core and no
/// assumption, so its clauses hold of every element, and this one holds in every model of
/// the ontology. The root derives it as its own, and the redundancy of its store leaves out
/// one that it holds already.
pub fn rfact<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let c = out.dl().table.get(cid);
    debug_assert!(c.body().is_empty(), "a promoted fact has no body");
    let mut found = Found::default();
    found.root = true;
    found.rule = "rfact";
    found.push((Vec::new(), c.head().to_vec()));
    drain_with(ix, st, found, out, &RootDerive);
    true
}

/// The context v registers with the root for the constant u, which a maximal literal of
/// one of its clauses or a ground literal in one of its bodies mentions.
pub fn redge<S: Sink>(st: &mut State, v: Ex, u: u32, out: &mut S) -> bool {
    register(st, v, vec![u], out)
}

/// The root registers v for the constants `us`, and v gets the root's facts over the new
/// ones.
fn register<S: Sink>(st: &mut State, v: Ex, mut us: Vec<u32>, out: &mut S) -> bool {
    us.sort_unstable();
    us.dedup();
    let bottom = st.bottom;
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    let mut new = Vec::new();
    let pairs = out.dl().mrepair & (MREPAIR_ON | MREPAIR_WIDE) == MREPAIR_ON;
    for &u in &us {
        if r.edge_set.contains(&(v, u)) {
            continue;
        }
        if pairs && !mrep_admit(r, out) {
            x.store.interrupted = true;
            return false;
        }
        let need_reg = pairs && !r.mr().reach_set.contains(&v);
        let Some(bound) = (|| {
            let mut b = set_insert_bound(&r.edge_set, &(v, u))?;
            b = b.checked_add(map_vec_push_bound(&r.edges, &u)?)?;
            if need_reg {
                b = b.checked_add(map_vec_push_bound(&r.mr().regs, &v)?)?;
            }
            Some(b)
        })() else {
            out.speculative_overflow();
            x.store.interrupted = true;
            return false;
        };
        if bound > 0 && !out.admit_dl(bound) {
            x.store.interrupted = true;
            return false;
        }
        let mut delta = set_insert_delta(&mut r.edge_set, (v, u)).unwrap_or(0)
            + map_vec_push_dedup(&mut r.edges, u, v);
        if need_reg {
            delta += map_vec_push_dedup(&mut r.mw().regs, v, u);
        }
        if bound > 0 {
            out.commit_dl(bound, delta);
        }
        new.push(u);
    }
    if new.is_empty() {
        return false;
    }
    // an unsatisfiable root sends the empty clause to every context registered with it
    if bottom {
        let Some(id) = super::dl_rules::DlRun::empty_admitted(out) else {
            x.store.interrupted = true;
            return false;
        };
        out.send(v, Concl::RPred(id));
        return true;
    }
    let mut ids: Vec<u32> = Vec::new();
    {
        let r = x.root.as_deref().expect("made above");
        for &u in &new {
            for &i in r.ready.get(&u).into_iter().flatten() {
                if !x.store.clauses[i as usize].dead {
                    ids.push(i);
                }
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    for i in ids {
        if let Some(cid) = pred_id(x, i, out) {
            out.send(v, Concl::RPred(cid));
        }
    }
    if out.dl().mrepair & MREPAIR_ON != 0
        && (!pairs
            || x.root
                .as_deref()
                .and_then(|r| r.m.as_deref())
                .is_some_and(|m| m.reach_set.contains(&v)))
    {
        m_registered(x, v, &new, out);
    }
    true
}

/// A processed root clause with an empty body goes to the contexts registered for one of
/// the constants of its head, and is listed for the contexts that register later.
fn offer<S: Sink>(ix: &Index, st: &mut State, id: u32, c: &Clause, out: &mut S) {
    if st.bottom || !c.body().is_empty() {
        return;
    }
    let inv = inv_of(ix, dix(ix));
    // the head's literals, the constants' lookups and their registrations are each a read
    // of the meter (item 1280), and the destinations are admitted before they are listed
    let meter = Meter::checks();
    let lits = &out.dl().lits;
    let mut keys = Vec::new();
    for &l in c.head() {
        if !meter.read(1, &*out) {
            mark_interrupted(st);
            return;
        }
        consts(lits.view(l, inv), &mut keys);
    }
    keys.sort_unstable();
    keys.dedup();
    let Some(x) = ds_admit(st, out) else { return };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    for &u in &keys {
        if !RootSt::push_admit(&mut r.ready, u, id, out) {
            x.store.interrupted = true;
            return;
        }
    }
    let mut n = Some(0usize);
    for u in &keys {
        if !meter.read(1, &*out) {
            x.store.interrupted = true;
            return;
        }
        n = n.and_then(|n| n.checked_add(r.edges.get(u).map_or(0, Vec::len)));
    }
    let Some(bytes) = n.and_then(|n| n.checked_mul(std::mem::size_of::<Ex>())) else {
        out.speculative_overflow();
        x.store.interrupted = true;
        return;
    };
    let mut guard = Guard::new(out);
    if !guard.admit_add(bytes) {
        x.store.interrupted = true;
        return;
    }
    let mut to: Vec<Ex> = Vec::with_capacity(n.unwrap_or(0));
    for u in &keys {
        for &v in r.edges.get(u).into_iter().flatten() {
            if !meter.read(1, &guard) {
                x.store.interrupted = true;
                return;
            }
            to.push(v);
        }
    }
    to.sort_unstable();
    to.dedup();
    if to.is_empty() {
        return;
    }
    let Some(cid) = pred_id(x, id, &mut guard) else {
        return;
    };
    for v in to {
        guard.send(v, Concl::RPred(cid));
    }
}

/// The root became unsatisfiable: every context registered with it takes the empty clause.
pub(super) fn bottom<S: Sink>(st: &State, out: &mut S) {
    let Some(r) = dl(st).and_then(|x| x.root.as_deref()) else {
        return;
    };
    if r.edges.is_empty() {
        return;
    }
    let mut to: Vec<Ex> = r.edge_set.iter().map(|&(v, _)| v).collect();
    to.sort_unstable();
    to.dedup();
    let Some(id) = super::dl_rules::DlRun::empty_admitted(out) else {
        return;
    };
    for v in to {
        out.send(v, Concl::RPred(id));
    }
}

/// Whether a literal is a successor trigger of Definition 4 that is ground: an atom or a
/// role atom over constants, or an equality of constants. Every predicate is taken as one
/// that a body of the ontology mentions.
fn ground_trigger(v: XLit) -> bool {
    use RTerm::C;
    matches!(
        v,
        XLit::Atom(_, C(_)) | XLit::Role(_, C(_), C(_)) | XLit::Eq(C(_), C(_))
    )
}

/// The context's state for the rules that involve the root, made with its first use and
/// counted then.
#[cfg(test)]
fn rst<'a, S: Sink>(st: &'a mut State, out: &S) -> &'a mut RootSt {
    root_of(ds(st), out)
}

/// [`rst`] for a context's state for Tier D.
#[cfg(test)]
fn root_of<'a, S: Sink>(x: &'a mut super::dl_rules::DlSt, out: &S) -> &'a mut RootSt {
    if x.root.is_none() {
        out.dl().grow(std::mem::size_of::<RootSt>());
    }
    x.root.get_or_insert_with(Box::default)
}

/// Pre-admits and ensures `x.root` is allocated.
pub(super) fn root_admit<'a, S: Sink>(
    x: &'a mut super::dl_rules::DlSt,
    out: &mut S,
) -> Option<&'a mut RootSt> {
    if x.root.is_some() {
        return x.root.as_deref_mut();
    }
    let need = std::mem::size_of::<RootSt>();
    if !out.admit_dl(need) {
        return None;
    }
    let r = x.root.get_or_insert_with(Box::default);
    out.commit_dl(need, need);
    Some(r)
}

/// Pre-admits and ensures `r.m` is allocated.
fn mrep_admit<S: Sink>(r: &mut RootSt, out: &mut S) -> bool {
    if r.m.is_some() {
        return true;
    }
    let need = std::mem::size_of::<MRep>();
    if !out.admit_dl(need) {
        return false;
    }
    r.m = Some(Box::default());
    out.commit_dl(need, need);
    true
}

/// The successors of a context: those it registered with over `f_e(x)` and over `f_e(u)`,
/// and the EL successors of its unit existential restrictions, other than those into a
/// singleton, which name their constant in place of a successor (item 822).
fn successors(ix: &Index, st: &State) -> Vec<Ex> {
    let mut v: Vec<Ex> = Vec::new();
    if let Some(x) = dl(st) {
        v.extend(x.edges().iter().map(|&(_, w)| w));
        if let Some(r) = x.root.as_deref() {
            v.extend(r.by_succ.keys().copied());
        }
    }
    for &d in &st.subs {
        let h = ix.head(d);
        if let Node::Exist(..) = h.node
            && super::dl_rules::into_nominal(ix, d).is_none()
        {
            v.push(h.succ);
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Succ's ground triggers: a new maximal ground atom or equality of constants of the
/// context goes to each of its successors as a marker.
fn ground_out<S: Sink>(ix: &Index, st: &mut State, l: Lit, out: &mut S) {
    let Some(x) = ds_admit(st, out) else { return };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    if r.ground_set.contains(&l) {
        return;
    }
    let Some(bound) = set_insert_bound(&r.ground_set, &l)
        .and_then(|a| vec_grow_bound(&r.ground).and_then(|b| a.checked_add(b)))
    else {
        out.speculative_overflow();
        x.store.interrupted = true;
        return;
    };
    if bound > 0 && !out.admit_dl(bound) {
        x.store.interrupted = true;
        return;
    }
    let delta =
        set_insert_delta(&mut r.ground_set, l).unwrap_or(0) + vec_push_delta(&mut r.ground, l);
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    for w in successors(ix, st) {
        send_ground(st, w, l, out);
    }
}

/// Sends the successor w the marker of the ground literal l, once.
fn send_ground<S: Sink>(st: &mut State, w: Ex, l: Lit, out: &mut S) {
    let Some(x) = ds_admit(st, out) else { return };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    match RootSt::ins_admit(&mut r.gsent, (w, l), out) {
        Some(true) => out.send(w, Concl::HypL(l.xid())),
        Some(false) => {}
        None => x.store.interrupted = true,
    }
}

/// A new successor w of the context takes the markers of the context's maximal ground
/// literals.
pub(super) fn new_succ<S: Sink>(st: &mut State, w: Ex, out: &mut S) {
    if out.dl().mrepair & (MREPAIR_ON | MREPAIR_WIDE) == MREPAIR_ON
        && dl(st)
            .and_then(|x| x.root.as_deref())
            .and_then(|r| r.m.as_deref())
            .is_some_and(|m| m.reached)
    {
        m_tell(st, w, out);
    }
    let Some(ls) = dl(st)
        .and_then(|x| x.root.as_deref())
        .map(|r| r.ground.clone())
    else {
        return;
    };
    for l in ls {
        send_ground(st, w, l, out);
    }
}

/// The marker `l → l` of a ground literal that a predecessor holds as a maximal literal.
pub fn hyp_l<S: Sink>(st: &mut State, xid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let l = Lit::xl(xid);
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    match RootSt::ins_admit(&mut r.gmarks, l, out) {
        Some(true) => {}
        Some(false) => return false,
        None => {
            x.store.interrupted = true;
            return false;
        }
    }
    let _ = x.store.add_admitted(Clause::new(vec![l], vec![l]), out);
    true
}

/// A context other than the root registers with the root for the constants of the ground
/// literals in the body of its new clause c.
pub(super) fn register_body<S: Sink>(ix: &Index, me: Ex, st: &mut State, c: &Clause, out: &mut S) {
    let dx = dix(ix);
    let Some(root) = dx.root else { return };
    let inv = inv_of(ix, dx);
    let mut us = Vec::new();
    for &l in c.body() {
        if l.kind() == XL {
            let v = out.dl().lits.view(l, inv);
            if v.is_ground() {
                consts(v, &mut us);
            }
        }
    }
    edge_to(root, me, st, us, out);
}

/// Registers `me` (other than the root) with the root for every constant appearing in
/// `max` (inside `f_e(u)` included), without scratch allocation, so `r.edged` holds them
/// before `Eqs::on_max` and `dl_root::on_max` check [`shared`].
pub(super) fn register_max<S: Sink>(ix: &Index, me: Ex, st: &mut State, max: &[Lit], out: &mut S) {
    let dx = dix(ix);
    let Some(root) = dx.root else { return };
    let inv = inv_of(ix, dx);
    for &l in max {
        if l.kind() != XL {
            continue;
        }
        let (ts, n) = terms_of(out.dl().lits.view(l, inv));
        for &t in &ts[..n] {
            if let RTerm::C(u) | RTerm::Fc(_, u) = t {
                let Some(x) = ds_admit(st, out) else { return };
                let Some(r) = root_admit(x, out) else {
                    x.store.interrupted = true;
                    return;
                };
                match RootSt::ins_admit(&mut r.edged, u, out) {
                    Some(true) => out.send(root, Concl::REdge(me, u)),
                    Some(false) => {}
                    None => {
                        x.store.interrupted = true;
                        return;
                    }
                }
            }
        }
    }
}

/// The context `me`, other than the root, registers with the root for the constants
/// `us`, once each.
fn edge_to<S: Sink>(root: Ex, me: Ex, st: &mut State, us: Vec<u32>, out: &mut S) {
    if us.is_empty() {
        return;
    }
    let Some(x) = ds_admit(st, out) else { return };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    for u in us {
        match RootSt::ins_admit(&mut r.edged, u, out) {
            Some(true) => out.send(root, Concl::REdge(me, u)),
            Some(false) => {}
            None => {
                x.store.interrupted = true;
                return;
            }
        }
    }
}

/// Succ in the context `me` for the successor term `f_e(u)`: the successor is e's
/// hypothetical successor, which has the empty core. A new edge registers the context with
/// it, sends it the markers of the context's ground literals when it is a new successor,
/// and replays the clauses it already sent for the new constant.
fn succ_at<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    e: Ex,
    u: u32,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> Ex {
    found.rule = "succ";
    let dx = dix(ix);
    let w = dx.hyp.get(&e).copied().unwrap_or(TOP);
    let Some(x) = ds_admit(st, out) else {
        return w;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return w;
    };
    if r.succ_set.contains(&(e, u)) {
        return w;
    }
    let Some(bound) = set_insert_bound(&r.succ_set, &(e, u))
        .and_then(|a| map_vec_push_bound(&r.by_succ, &w).and_then(|b| a.checked_add(b)))
    else {
        out.speculative_overflow();
        x.store.interrupted = true;
        return w;
    };
    if bound > 0 && !out.admit_dl(bound) {
        x.store.interrupted = true;
        return w;
    }
    let fresh = !r.by_succ.contains_key(&w);
    let delta = set_insert_delta(&mut r.succ_set, (e, u)).unwrap_or(0)
        + map_vec_push_dedup(&mut r.by_succ, w, (e, u));
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    out.send(w, Concl::DEdge(me, e));
    if fresh {
        new_succ(st, w, out);
    }
    let share = out.dl().shared_facts;
    if share {
        let mut failed = false;
        if let Some(pend) = st
            .extra
            .as_mut()
            .and_then(|x| x.dl.as_deref_mut())
            .and_then(|x| x.root.as_deref_mut())
            .and_then(|r| r.pred_pending.get_mut(&w))
        {
            let mut idx = 0;
            while idx < pend.len() {
                let cid = pend[idx];
                let bytes = {
                    let c = out.dl().table.get(cid);
                    side_exact_bytes(c.body().len(), c.head().len())
                };
                let Some(bytes) = bytes else {
                    out.speculative_overflow();
                    failed = true;
                    break;
                };
                found.rule = "predg";
                if !found.reserve_one_admitted(1, bytes, out) {
                    failed = true;
                    break;
                }
                let side = {
                    let c = out.dl().table.get(cid);
                    (c.body().to_vec(), c.head().to_vec())
                };
                found.push(side);
                out.release_to(found.held_bytes());
                idx += 1;
            }
            pend.drain(..idx);
        }
        if failed {
            mark_interrupted(st);
            return w;
        }
    }
    if let Some(cids) = dl(st).and_then(|x| x.pred_in.get(&w)) {
        for &cid in cids {
            if share
                && super::dl_rules::constants_only(ix, dx, &out.dl().lits, out.dl().table.get(cid))
            {
                continue;
            }
            if !root_pred(ix, st, e, u, cid, None, found, out) {
                mark_interrupted(st);
                return w;
            }
        }
    }
    w
}

#[derive(Clone, Copy)]
enum PredClause<'a> {
    ById(u32),
    #[allow(dead_code)]
    Ref(&'a Clause),
}

fn resolve_pred_clause<'a, S: Sink>(
    g: &'a Option<&mut Guard<'_, S>>,
    fb: Option<&'a super::dl_clause::Lits>,
    clause: PredClause<'a>,
) -> (&'a super::dl_clause::Lits, &'a Clause) {
    let lits = match (g, fb) {
        (Some(g), _) => &g.dl().lits,
        (None, Some(l)) => l,
        (None, None) => unreachable!(),
    };
    let c = match clause {
        PredClause::ById(cid) => g.as_ref().expect("guard for cid").dl().table.get(cid),
        PredClause::Ref(c) => c,
    };
    (lits, c)
}

/// Pred along the edge over `f_e(u)` with the clause `cid` of its successor, in any context:
/// y is read as u and x as `f_e(u)`. Each body atom `B(x)` takes a premise whose maximal
/// literal is `B(f_e(u))`, each role atom `s(y, x)` one whose maximal literal is a role
/// atom below s from u to `f_e(u)`, and the ground body literals stay. `fixed` is the
/// premise that a new clause gives for one of these literals. The successor's core is
/// empty, so no core atom is resolved.
#[allow(clippy::too_many_arguments)]
pub(super) fn root_pred<S: Sink>(
    ix: &Index,
    st: &State,
    e: Ex,
    u: u32,
    cid: u32,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    root_pred_of_admitted(
        ix,
        st,
        e,
        u,
        PredClause::ById(cid),
        fixed,
        found,
        Some(guard),
        None,
        super::dl_rules::sel_partners_x(),
    )
}

/// [`root_pred`], whose premises over `f_e(u)` include the fully selected clauses (`s.sel`)
/// when `include_selected` is set, as Hyper over x has them under
/// [`super::dl_rules::sel_partners_x`].
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn root_pred_of(
    ix: &Index,
    st: &State,
    e: Ex,
    u: u32,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
    include_selected: bool,
) {
    struct NoSink;
    impl Sink for NoSink {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &super::dl_rules::DlRun {
            unreachable!()
        }
    }
    let _ = root_pred_of_admitted::<NoSink>(
        ix,
        st,
        e,
        u,
        PredClause::Ref(c),
        fixed,
        found,
        None,
        Some(lits),
        include_selected,
    );
}

#[allow(clippy::too_many_arguments)]
fn root_pred_of_admitted<S: Sink>(
    ix: &Index,
    st: &State,
    e: Ex,
    u: u32,
    clause: PredClause<'_>,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    mut guard: Option<&mut Guard<'_, S>>,
    lits_fallback: Option<&super::dl_clause::Lits>,
    sel: bool,
) -> bool {
    use super::dl_clause::{RY, X as KX};
    use std::mem::size_of;

    found.rule = "rpredc";
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let f = RTerm::Fc(e, u);
    let at = RTerm::C(u);
    let map = |t: RTerm| match t {
        RTerm::X => Some(f),
        RTerm::Y => Some(at),
        RTerm::C(_) => Some(t),
        _ => None,
    };
    let map_head_xl = |v: XLit| match v {
        XLit::Atom(a, b) => map(b).map(|b| XLit::Atom(a, b)),
        XLit::Role(r, a, b) => map(a).zip(map(b)).map(|(a, b)| XLit::Role(r, a, b)),
        XLit::Eq(a, b) => map(a).zip(map(b)).map(|(a, b)| XLit::Eq(a, b)),
        XLit::Ne(a, b) => map(a).zip(map(b)).map(|(a, b)| XLit::Ne(a, b)),
    };
    enum Need {
        Atom(Lit),
        Role(Role),
    }
    let role_of = |lits: &super::dl_clause::Lits, p: Lit| match lits.view(p, inv) {
        XLit::Role(t, a, b) if a == at && b == f => Some(t),
        XLit::Role(t, a, b) if a == f && b == at => dx.inv(ix, t),
        _ => None,
    };
    let fits = |lits: &super::dl_clause::Lits, n: &Need, p: Lit| match n {
        Need::Atom(k) => *k == p,
        Need::Role(s) => role_of(lits, p).is_some_and(|t| dx.sub(ix, t, *s)),
    };

    // Pass 0 (zero allocations): validate body/head, count `ground_len`, `head_len`,
    // `need_len`, and verify `fixed` fits at least one body place. Uses `lits.get` for
    // body atoms so uninterned atoms short-circuit with zero `Lits` growth.
    let (ground_len, head_len, need_len, c_head_len) = {
        let (lits, c) = resolve_pred_clause(&guard, lits_fallback, clause);
        let mut ground_len = 0usize;
        let mut need_len = 0usize;
        let mut any_fit = fixed.is_none();
        for &l in c.body() {
            match l.kind() {
                KX => {
                    let Some(k) = lits.get(XLit::Atom(l.atom(), f), inv) else {
                        return true;
                    };
                    if let Some((p, _)) = fixed
                        && k == p
                    {
                        any_fit = true;
                    }
                    need_len += 1;
                }
                RY => {
                    if let Some((p, _)) = fixed
                        && role_of(lits, p).is_some_and(|t| dx.sub(ix, t, l.atom()))
                    {
                        any_fit = true;
                    }
                    need_len += 1;
                }
                XL if lits.view(l, inv).is_ground() => ground_len += 1,
                _ => return true,
            }
        }
        if !any_fit {
            return true;
        }
        let mut head_len = 0usize;
        for &l in c.head() {
            let Some(w) = map_head_xl(lits.view(l, inv)) else {
                return true;
            };
            match w {
                XLit::Eq(a, b) if a == b => return true,
                XLit::Ne(a, b) if a == b => {}
                _ => head_len += 1,
            }
        }
        (ground_len, head_len, need_len, c.head().len())
    };

    let Some(base_bytes) = side_exact_bytes(ground_len, head_len) else {
        if let Some(g) = guard.as_deref_mut() {
            g.speculative_overflow();
        }
        return false;
    };

    let for_each_need =
        |lits: &super::dl_clause::Lits, c: &Clause, cb: &mut dyn FnMut(usize, Need) -> bool| {
            let mut idx = 0usize;
            for &l in c.body() {
                let item = match l.kind() {
                    KX => Need::Atom(
                        lits.get(XLit::Atom(l.atom(), f), inv)
                            .expect("validated in pass 0"),
                    ),
                    RY => Need::Role(l.atom()),
                    _ => continue,
                };
                if !cb(idx, item) {
                    return;
                }
                idx += 1;
            }
        };

    type RolePartnerCb<'a> = dyn FnMut(&[Lit], &[Lit], Option<Lit>) -> bool + 'a;
    let for_each_role_partner =
        |lits: &super::dl_clause::Lits, s: Role, cb: &mut RolePartnerCb<'_>| -> bool {
            let Some(x) = dl(st) else { return true };
            let Some(t) = x.store.terms.as_deref() else {
                return true;
            };
            for &i in t.any.get(&f).into_iter().flatten() {
                let sc = &x.store.clauses[i as usize];
                if sc.dead {
                    continue;
                }
                for &m in x.store.maxima(i) {
                    if role_of(lits, m).is_some_and(|rt| dx.sub(ix, rt, s))
                        && !cb(sc.c.body(), sc.c.head(), Some(m))
                    {
                        return false;
                    }
                }
            }
            true
        };

    let empty_side = unit();
    let emit_place = |fixed_place: Option<(usize, &Side)>,
                      found: &mut Found,
                      guard: &mut Option<&mut Guard<'_, S>>|
     -> bool {
        // Pass 1 (zero allocations): count borrowed partners and exact bytes.
        let (prod_bytes, prune_scratch, empty_pos) = {
            let (lits, c) = resolve_pred_clause(guard, lits_fallback, clause);
            let Some(mut lists_bytes) = need_len.checked_mul(size_of::<Vec<Side>>()) else {
                if let Some(g) = guard.as_deref_mut() {
                    g.speculative_overflow();
                }
                return false;
            };
            let mut max_list_len = 0usize;
            let mut empty_pos = false;
            let mut overflow = false;
            for_each_need(lits, c, &mut |j, n| {
                let mut count = 0usize;
                let mut sides_bytes = 0usize;
                if let Some((k, prem)) = fixed_place
                    && j == k
                {
                    count = 1;
                    let Some(b) = side_exact_bytes(prem.0.len(), prem.1.len()) else {
                        overflow = true;
                        return false;
                    };
                    sides_bytes = b;
                } else {
                    let mut acc = |pb: &[Lit], ph: &[Lit], without: Option<Lit>| -> bool {
                        let Some(sb) = side_parts_bytes(&empty_side, pb, ph, without, None)
                            .and_then(|b| sides_bytes.checked_add(b))
                        else {
                            overflow = true;
                            return false;
                        };
                        let Some(nc) = count.checked_add(1) else {
                            overflow = true;
                            return false;
                        };
                        count = nc;
                        sides_bytes = sb;
                        true
                    };
                    match n {
                        Need::Atom(k) => {
                            for_each_partner_of(st, k, sel, &mut acc);
                        }
                        Need::Role(s) => {
                            for_each_role_partner(lits, s, &mut acc);
                        }
                    }
                    if overflow {
                        return false;
                    }
                }
                if count == 0 {
                    empty_pos = true;
                    return false;
                }
                max_list_len = max_list_len.max(count);
                let Some(pos_bytes) = count
                    .checked_mul(size_of::<Side>())
                    .and_then(|b| b.checked_add(sides_bytes))
                    .and_then(|b| lists_bytes.checked_add(b))
                else {
                    overflow = true;
                    return false;
                };
                lists_bytes = pos_bytes;
                true
            });
            if overflow {
                if let Some(g) = guard.as_deref_mut() {
                    g.speculative_overflow();
                }
                return false;
            }
            if empty_pos {
                (0, 0, true)
            } else {
                let Some(prod_bytes) = lists_bytes.checked_add(base_bytes) else {
                    if let Some(g) = guard.as_deref_mut() {
                        g.speculative_overflow();
                    }
                    return false;
                };
                let prune_scratch = if max_list_len < 2 {
                    0
                } else {
                    let Some(ps) = max_list_len.checked_mul(PRUNE_SCRATCH_ELEM) else {
                        if let Some(g) = guard.as_deref_mut() {
                            g.speculative_overflow();
                        }
                        return false;
                    };
                    ps
                };
                (prod_bytes, prune_scratch, false)
            }
        };
        if empty_pos {
            return true;
        }
        if let Some(g) = guard.as_deref_mut()
            && !found.reserve_prod_admitted(prod_bytes, prune_scratch, 0, g)
        {
            return false;
        }
        // Pass 2 (post-admission): build exact-capacity `(ground, head)` and `lists`.
        let mut ground = Vec::with_capacity(ground_len);
        {
            let (lits, c) = resolve_pred_clause(guard, lits_fallback, clause);
            for &l in c.body() {
                if l.kind() == XL && lits.view(l, inv).is_ground() {
                    ground.push(l);
                }
            }
        }
        let mut head = Vec::with_capacity(head_len);
        for idx in 0..c_head_len {
            let w = {
                let (lits, c) = resolve_pred_clause(guard, lits_fallback, clause);
                map_head_xl(lits.view(c.head()[idx], inv)).expect("validated in pass 0")
            };
            let made = match guard.as_deref_mut() {
                Some(g) => {
                    let Some(m) = super::dl_clause::admit_lit(w, inv, g) else {
                        drop(ground);
                        drop(head);
                        g.release_to(found.held_bytes());
                        return false;
                    };
                    m
                }
                None => lits_fallback.expect("lits fallback").lit(w, inv),
            };
            if let Made::Lit(m) = made {
                head.push(m);
            }
        }
        let mut lists = Vec::with_capacity(need_len);
        {
            let (lits, c) = resolve_pred_clause(guard, lits_fallback, clause);
            for_each_need(lits, c, &mut |j, n| {
                if let Some((k, prem)) = fixed_place
                    && j == k
                {
                    lists.push(vec![(prem.0.clone(), prem.1.clone())]);
                } else {
                    let mut count = 0usize;
                    let mut count_cb = |_: &[Lit], _: &[Lit], _: Option<Lit>| {
                        count += 1;
                        true
                    };
                    match n {
                        Need::Atom(k) => {
                            for_each_partner_of(st, k, sel, &mut count_cb);
                        }
                        Need::Role(s) => {
                            for_each_role_partner(lits, s, &mut count_cb);
                        }
                    }
                    let mut v = Vec::with_capacity(count);
                    let mut push_cb = |pb: &[Lit], ph: &[Lit], without: Option<Lit>| {
                        v.push(merge_parts(&empty_side, pb, ph, without, None));
                        true
                    };
                    match n {
                        Need::Atom(k) => {
                            for_each_partner_of(st, k, sel, &mut push_cb);
                        }
                        Need::Role(s) => {
                            for_each_role_partner(lits, s, &mut push_cb);
                        }
                    }
                    lists.push(v);
                }
                true
            });
        }
        let base = (ground, head);
        debug_assert_eq!(
            super::dl_rules::prod_inner_bytes(lists.capacity(), &lists, &base),
            prod_bytes
        );
        found.products(lists, base);
        if let Some(g) = guard.as_deref_mut() {
            g.release_to(found.held_bytes());
        }
        true
    };
    match fixed {
        None => emit_place(None, found, &mut guard),
        Some((p, prem)) => {
            for k in 0..need_len {
                let mut matches_k = false;
                {
                    let (lits, c) = resolve_pred_clause(&guard, lits_fallback, clause);
                    for_each_need(lits, c, &mut |j, n| {
                        if j == k {
                            matches_k = fits(lits, &n, p);
                            return false;
                        }
                        true
                    });
                }
                if !matches_k {
                    continue;
                }
                if !emit_place(Some((k, prem)), found, &mut guard) {
                    return false;
                }
            }
            true
        }
    }
}

/// Resolves the active marker-repair bitmask from explicit environment override `env`,
/// test override `test` (`0` = unset), and [`Limits::rooted_marker_repair`](super::Limits)
/// flag `on`.
fn marker_repair_mode(env: Option<u8>, test: u8, on: bool) -> u8 {
    env.unwrap_or(if test != 0 {
        test
    } else if on {
        MREPAIR_ON | MREPAIR_CF
    } else {
        0
    })
}

/// Row 1231's marker repair for a run, as [`DlRun::mrepair`](super::dl_rules::DlRun)
/// holds it: `on` is [`Limits::rooted_marker_repair`](super::Limits), which defaults to
/// [`MREPAIR_ON`] | [`MREPAIR_CF`] (M-return-cf, item 1250), and
/// `SPARKLES_ROOTED_MARKER_REPAIR` overrides it for measurements with `0` (off), `1`
/// (unfiltered [`MREPAIR_ON`]) or a sum of the bits [`MREPAIR_ON`], [`MREPAIR_HOLDER`],
/// [`MREPAIR_UNITS`], [`MREPAIR_WIDE`] and [`MREPAIR_CF`].
pub(super) fn marker_repair(on: bool) -> u8 {
    static ENV: std::sync::OnceLock<Option<u8>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| {
        std::env::var("SPARKLES_ROOTED_MARKER_REPAIR")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    let test = TEST_MODE.load(std::sync::atomic::Ordering::Relaxed);
    marker_repair_mode(env, test, on)
}

/// The repair's mode and the diagnostic that [`marker_test_mode`] sets, for the deficits'
/// tests, which run in a test binary of their own.
static TEST_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static TEST_DIAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sets the repair's mode for the runs that start from here on, which the environment
/// overrides, `0` for [`Limits::rooted_marker_repair`](super::Limits)'s, and turns on the
/// diagnostic's deficits in [`Stats::dl_marker_deficits`](super::Stats), for tests.
pub fn marker_test_mode(mode: u8, diag: bool) {
    TEST_MODE.store(mode, std::sync::atomic::Ordering::Relaxed);
    TEST_DIAG.store(diag, std::sync::atomic::Ordering::Relaxed);
}

/// M-send in the context `me`, other than the root, on its new premise `prem` whose maximal
/// literal `l` is a ground trigger of a processed clause other than a marker: the context
/// tells the root once that it holds l, and the root sends l's marker to the contexts
/// registered for l's constants. The premise also meets the clauses the root returned
/// with l in their bodies (M-return's late premise, [`m_late`]). It is false when a stop of
/// the run cut the replay off, which marks the store interrupted.
fn m_note<S: Sink>(
    ix: &Index,
    root: Ex,
    me: Ex,
    st: &mut State,
    l: Lit,
    prem: &Side,
    out: &mut S,
) -> bool {
    if m_stopped(st, out) {
        return false;
    }
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    if !mrep_admit(r, out) {
        x.store.interrupted = true;
        return false;
    }
    match RootSt::ins_admit(&mut r.mw().mheld, l, out) {
        Some(true) => {
            out.dl().count_mrepair(1);
            out.worked(1);
            out.send(root, Concl::RMark(me, l.xid()));
        }
        Some(false) => {}
        None => {
            x.store.interrupted = true;
            return false;
        }
    }
    let cf = cf_of(ix, out);
    if cf.is_some_and(|cf| ground_body(cf, &prem.0)) {
        return true;
    }
    m_late(ix, st, l, prem, out)
}

/// M-return's late premise `prem`, whose maximal literal is `l`: the clauses the root
/// returned with l in their bodies are resolved with it in batches, each drained before the
/// next is made, so that the many returns of a literal are never prepared whole. A batch
/// closes after [`M_POLL`] returns or once its products hold [`RECORD_SLACK`] bytes. Each
/// return's premise lists are read through one [`Meter`] for the whole replay and admitted
/// before they are built ([`ret_found`]), and [`m_flush`] checks the run's bounds before it
/// drains a batch. A stop, a refusal or a drain that stopped ends the replay with the store
/// interrupted, as [`drain`] leaves it, and the function is false. The conclusions are
/// charged as `drain` charges them, and the tables of a product that is not ground are
/// charged by the product as they grow.
fn m_late<S: Sink>(ix: &Index, st: &mut State, l: Lit, prem: &Side, out: &mut S) -> bool {
    let meter = Meter::checks();
    let mut guard = Guard::new(out);
    let mut batch = Found::default();
    batch.rule = "mret";
    let mut since = 0;
    for k in 0.. {
        if st.bottom {
            return true;
        }
        let Some(cid) = dl(st)
            .and_then(|x| x.root.as_deref())
            .and_then(|r| r.m.as_deref())
            .and_then(|m| m.rets.get(&l))
            .and_then(|v| v.get(k).copied())
        else {
            break;
        };
        let c = guard.dl().table.get(cid).clone();
        if !ret_found(ix, st, &c, Some((l, prem)), &meter, &mut batch, &mut guard) {
            drop(batch);
            guard.release_to(0);
            mark_interrupted(st);
            return false;
        }
        since += 1;
        if since >= M_POLL || batch.held_bytes() >= RECORD_SLACK {
            since = 0;
            if !m_flush(ix, st, &mut batch, &mut guard) {
                return false;
            }
        }
    }
    since == 0 || m_flush(ix, st, &mut batch, &mut guard)
}

/// Drains a batch whose held bytes `guard` holds admitted, after a check of the run's
/// bounds, and releases them, false when the run stopped.
fn m_flush<S: Sink>(
    ix: &Index,
    st: &mut State,
    batch: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    let b = std::mem::take(batch);
    // The next batch's products keep the rule of this one, M-return's in a replay.
    batch.rule = b.rule;
    guard.release_to(b.held_bytes());
    if !guard.go_on() {
        drop(b);
        guard.release_to(0);
        ds(st).store.interrupted = true;
        return false;
    }
    let was = ds(st).store.interrupted;
    drain_admitted(ix, st, b, guard);
    guard.release_to(0);
    was || !ds(st).store.interrupted
}

/// The repair's pair set: the root reaches the context `me` by successors, from a
/// predecessor that it reaches. The context tells the root, which sends it the markers of
/// the held literals over the constants it registered for, and tells its successors in
/// turn. In the root, the message is a context's, `e`, that the root now reaches.
pub fn rreach<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, out: &mut S) -> bool {
    let Some(root) = dix(ix).root else {
        return false;
    };
    if m_stopped(st, out) {
        return false;
    }
    if me == root {
        let bottom = st.bottom;
        let Some(x) = ds_admit(st, out) else {
            return false;
        };
        let Some(r) = root_admit(x, out) else {
            x.store.interrupted = true;
            return false;
        };
        if !mrep_admit(r, out) {
            x.store.interrupted = true;
            return false;
        }
        match RootSt::ins_admit(&mut r.mw().reach_set, e, out) {
            Some(true) => {}
            Some(false) => return false,
            None => {
                x.store.interrupted = true;
                return false;
            }
        }
        if bottom {
            return true;
        }
        // the registrations that waited for the reach are replayed once, and the ones
        // that come later go to the context as they come ([`register`]). Empty the slot in
        // place instead of removing the key so `regs` stays append-only with no hashbrown
        // tombstones. Declare `credit` before `us` so LIFO drop always frees `us` before
        // crediting its retained capacity back on both normal return and callback unwind.
        let mut credit = CreditGuard::new(out, 0);
        let us = r
            .mw()
            .regs
            .get_mut(&e)
            .map(std::mem::take)
            .unwrap_or_default();
        credit.arm(us.capacity() * std::mem::size_of::<u32>());
        m_registered(x, e, &us, &mut credit);
        return true;
    }
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    if !mrep_admit(r, out) {
        x.store.interrupted = true;
        return false;
    }
    if r.mr().reached {
        return false;
    }
    r.mw().reached = true;
    out.send(root, Concl::RReach(me));
    out.dl().count_mrepair(1);
    out.worked(1);
    for (k, w) in successors(ix, st).into_iter().enumerate() {
        if !m_poll(k, st, out) {
            break;
        }
        m_tell(st, w, out);
    }
    true
}

/// A context that the root reaches tells its successor `w` so, once.
fn m_tell<S: Sink>(st: &mut State, w: Ex, out: &mut S) {
    let Some(x) = ds_admit(st, out) else { return };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    if !mrep_admit(r, out) {
        x.store.interrupted = true;
        return;
    }
    match RootSt::ins_admit(&mut r.mw().told, w, out) {
        Some(true) => {
            out.send(w, Concl::RReach(TOP));
            out.dl().count_mrepair(1);
            out.worked(1);
        }
        Some(false) => {}
        None => x.store.interrupted = true,
    }
}

/// The repair's handlers take no notice once the run has stopped, which ends it without a
/// claim: the context's store, when it has one, is interrupted, as [`drain`] leaves it, and
/// the handler makes nothing, so that a stopped run's queued messages cost no more than
/// this test.
fn m_stopped<S: Sink>(st: &mut State, out: &S) -> bool {
    if !out.stopped() {
        return false;
    }
    st.interrupted = true;
    if let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) {
        x.store.interrupted = true;
    }
    true
}

/// A scan of the repair whose reads mostly send nothing: it counts them, charges a
/// conclusion for each [`WORK_READS`] of them and checks the run's bounds after every
/// [`M_SCAN`] of them.
pub(crate) struct Scan(usize);

impl Scan {
    /// Counts `n` reads, false when a check of the bounds stopped the scan.
    fn read<S: Sink>(&mut self, n: usize, out: &mut S) -> bool {
        let before = self.0 / M_SCAN;
        self.0 += n;
        let polls = self.0 / M_SCAN - before;
        if polls == 0 {
            return true;
        }
        let charge = polls * (M_SCAN / WORK_READS);
        out.dl().count_mrepair(charge);
        out.worked(charge);
        out.go_on()
    }

    /// Charges the reads since the last check.
    fn end<S: Sink>(self, out: &mut S) {
        let charge = self.0 % M_SCAN / WORK_READS;
        out.dl().count_mrepair(charge);
        out.worked(charge);
    }

    /// Charges every unbilled work unit accumulated in `self.0` and keeps only the
    /// remainder below `WORK_READS`, for scans whose poll callback charges and reduces
    /// `self.0` directly rather than going through [`Scan::read`].
    fn charge_pending<S: Sink>(&mut self, out: &mut S) {
        let work = self.0 / WORK_READS;
        if work > 0 {
            out.worked(work);
            self.0 %= WORK_READS;
        }
    }
}

/// The repair's sends poll the run's bounds after every [`M_POLL`] of them, as a rule whose
/// one inference sends many messages does: `k` counts the loop's sends. A stop ends the
/// loop and marks the context's store interrupted, as [`drain`] does. It comes only when the
/// run has stopped or an activation of a run in steps is past the bound, which both end the
/// run without a claim, so a send left undone never hides behind a complete answer.
fn m_poll<S: Sink>(k: usize, st: &mut State, out: &mut S) -> bool {
    let go = if k == 0 {
        // the first send of a loop tests the stop without a check of the bounds
        !out.stopped()
    } else {
        !k.is_multiple_of(M_POLL) || out.sent_go_on()
    };
    if go {
        return true;
    }
    ds(st).store.interrupted = true;
    false
}

/// The sends between two polls of the run's bounds in the repair's loops.
const M_POLL: usize = 64;

/// The reads between two polls of the run's bounds in the repair's scans.
const M_SCAN: usize = 1024;

/// Makes the repair's state of a context with its first use, and counts it.
#[cfg(test)]
fn mrep_new<S: Sink>(r: &mut RootSt, out: &S) {
    if r.m.is_none() {
        out.dl().grow(std::mem::size_of::<MRep>());
        r.m = Some(Box::default());
    }
}

/// Whether the ground literal `l` is a live unit of the root, by the index of units that
/// [`m_offer`] keeps.
fn root_unit(x: &DlSt, l: Lit) -> bool {
    x.root
        .as_deref()
        .and_then(|r| r.m.as_deref())
        .and_then(|m| m.units.get(&l))
        .is_some_and(|v| v.iter().any(|&i| !x.store.clauses[i as usize].dead))
}

/// The root sends the marker of `l` to the context `d`, once. Returns `false` if pre-admission
/// refused the marker-tracking table growth.
fn m_fwd<S: Sink>(r: &mut RootSt, d: Ex, l: Lit, out: &mut S) -> bool {
    match RootSt::ins_admit(&mut r.gsent, (d, l), out) {
        Some(true) => {
            out.send(d, Concl::HypL(l.xid()));
            out.dl().count_mrepair(1);
            out.worked(1);
            out.dl().mcount[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        }
        Some(false) => true,
        None => false,
    }
}

/// The table id of the root's processed clause `i`, which a returned clause keeps whether
/// or not it was subsumed since, since it holds in every model either way.
fn ret_id<S: Sink>(x: &mut DlSt, i: u32, out: &mut S) -> Option<u32> {
    if let Some(id) = x.store.clauses[i as usize].sent {
        return Some(id);
    }
    let clause_bytes = x.store.clauses[i as usize].c.bytes();
    let Some(id) = super::dl_clause::push_clause_admitted(out, clause_bytes, || {
        x.store.clauses[i as usize].c.clone()
    }) else {
        x.store.interrupted = true;
        return None;
    };
    x.store.clauses[i as usize].sent = Some(id);
    Some(id)
}

/// The root takes M-send's notice that the context `h` holds the ground literal `xid`:
/// the contexts registered for one of its constants take its marker, unless it is a live
/// unit of the root, which they take as a fact already, and `h` takes the root's clauses
/// whose ground bodies it now holds every literal of (M-return).
pub fn rmark<S: Sink>(ix: &Index, st: &mut State, h: Ex, xid: u32, out: &mut S) -> bool {
    if st.bottom || m_stopped(st, out) {
        return false;
    }
    let dx = dix(ix);
    let Some(root) = dx.root else { return false };
    let l = Lit::xl(xid);
    let mut us = Vec::new();
    consts(out.dl().lits.view(l, inv_of(ix, dx)), &mut us);
    us.sort_unstable();
    us.dedup();
    let mode = out.dl().mrepair;
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    if !mrep_admit(r, out) {
        x.store.interrupted = true;
        return false;
    }
    if r.mr().hold_set.contains(&(h, l)) {
        return false;
    }
    let first = !r.mr().holders.contains_key(&l);
    let Some(bound) = (|| {
        let mut b = set_insert_bound(&r.mr().hold_set, &(h, l))?;
        b = b.checked_add(map_vec_push_bound(&r.mr().holders, &l)?)?;
        if first {
            b = b.checked_add(map_vec_push_keys_bound(&r.mr().held_at, &us)?)?;
        }
        Some(b)
    })() else {
        out.speculative_overflow();
        x.store.interrupted = true;
        return false;
    };
    if bound > 0 && !out.admit_dl(bound) {
        x.store.interrupted = true;
        return false;
    }
    let mut delta = set_insert_delta(&mut r.mw().hold_set, (h, l)).unwrap_or(0)
        + map_vec_push_dedup(&mut r.mw().holders, l, h);
    if first {
        for &u in &us {
            delta += map_vec_push_dedup(&mut r.mw().held_at, u, xid);
        }
    }
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    out.dl().mcount[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let unit = mode & MREPAIR_UNITS == 0 && root_unit(x, l);
    if !unit {
        // the destinations, admitted before they are listed
        let r = x.root.as_deref().expect("made above");
        let n: usize = us.iter().map(|u| r.edges.get(u).map_or(0, Vec::len)).sum();
        let bytes = n * std::mem::size_of::<Ex>();
        let mut cut = false;
        {
            let mut guard = Guard::new(out);
            if !guard.reserve_add(bytes) {
                x.store.interrupted = true;
                return true;
            }
            // the registrations are read as a scan, charged and polled, and the destinations
            // are built in the capacity admitted for them
            let r = x.root.as_deref_mut().expect("made above");
            let scan = Meter::new(Bill::Repair);
            let mut to: Vec<Ex> = Vec::with_capacity(n);
            'scan: for u in &us {
                for &d in r.edges.get(u).into_iter().flatten() {
                    if !scan.read_mut(1, &mut guard) {
                        cut = true;
                        break 'scan;
                    }
                    if d != root
                        && (d != h || mode & MREPAIR_HOLDER != 0)
                        && (mode & MREPAIR_WIDE != 0 || r.mr().reach_set.contains(&d))
                    {
                        to.push(d);
                    }
                }
            }
            scan.end(&mut guard);
            if !cut {
                to.sort_unstable();
                to.dedup();
                for (k, d) in to.into_iter().enumerate() {
                    if k > 0 && k.is_multiple_of(M_POLL) && !guard.sent_go_on() {
                        cut = true;
                        break;
                    }
                    if !m_fwd(r, d, l, &mut guard) {
                        cut = true;
                        break;
                    }
                }
            }
        }
        if cut {
            x.store.interrupted = true;
            return true;
        }
    }
    if mode & MREPAIR_NO_RETURN != 0 {
        return true;
    }
    // the returns: the root's clauses with l in the body whose other body literals h
    // holds, their ids admitted before they are listed and the scan charged and polled
    let g = x
        .root
        .as_deref()
        .and_then(|r| r.mr().gbody.get(&l))
        .map_or(0, Vec::len);
    let bytes = g * 4;
    let mut guard = Guard::new(out);
    if !guard.reserve_add(bytes) {
        x.store.interrupted = true;
        return true;
    }
    let r = x.root.as_deref().expect("made above");
    let scan = Meter::new(Bill::Repair);
    let mut ids: Vec<u32> = Vec::with_capacity(g);
    let mut cut = false;
    'scan: for &i in r.mr().gbody.get(&l).into_iter().flatten() {
        let mut all = true;
        for &m in x.store.clauses[i as usize].c.body() {
            // each body literal checked is a read
            if !scan.read_mut(1, &mut guard) {
                cut = true;
                break 'scan;
            }
            if m != l && !r.mr().hold_set.contains(&(h, m)) {
                all = false;
                break;
            }
        }
        if all {
            ids.push(i);
        }
    }
    scan.end(&mut guard);
    if cut {
        x.store.interrupted = true;
        ids.clear();
    }
    for (k, i) in ids.into_iter().enumerate() {
        if !m_poll(k, st, &mut guard) {
            break;
        }
        let Some(cid) = ret_id(ds(st), i, &mut guard) else {
            break;
        };
        guard.send(h, Concl::RRet(cid));
        guard.dl().count_mrepair(1);
        guard.worked(1);
        guard.dl().mcount[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    true
}

/// The root registered the context `v` for the constants `new`: v takes the markers of the
/// literals over them that a context other than v holds, unless they are live units.
fn m_registered<S: Sink>(x: &mut DlSt, v: Ex, new: &[u32], out: &mut S) {
    let mode = out.dl().mrepair;
    if mode & MREPAIR_NO_REPLAY != 0 {
        return;
    }
    if out.stopped() {
        x.store.interrupted = true;
        return;
    }
    let Some(r) = x.root.as_deref().filter(|r| r.m.is_some()) else {
        return;
    };
    // the held literals over the constants, admitted before they are listed; the sizing,
    // the listing and the sends are read as one scan, a read per constant and per pair
    let scan = Meter::new(Bill::Repair);
    let mut n = 0;
    let mut cut = false;
    for u in new {
        if !scan.read_mut(1, out) {
            cut = true;
            break;
        }
        n += r.mr().held_at.get(u).map_or(0, Vec::len);
    }
    if cut {
        scan.end(out);
        x.store.interrupted = true;
        return;
    }
    let bytes = n * std::mem::size_of::<(u32, u32)>();
    let mut guard = Guard::new(out);
    if !guard.reserve_add(bytes) {
        scan.end(&mut guard);
        x.store.interrupted = true;
        return;
    }
    let r = x.root.as_deref().expect("read above");
    let mut ls: Vec<(u32, u32)> = Vec::with_capacity(n);
    'list: for &u in new {
        if !scan.read_mut(1, &mut guard) {
            cut = true;
            break;
        }
        for &i in r.mr().held_at.get(&u).into_iter().flatten() {
            if !scan.read_mut(1, &mut guard) {
                cut = true;
                break 'list;
            }
            ls.push((i, u));
        }
    }
    if cut {
        ls.clear();
    }
    ls.sort_unstable();
    ls.dedup_by_key(|p| p.0);
    for (k, (i, _)) in ls.into_iter().enumerate() {
        if (k > 0 && k.is_multiple_of(M_POLL) && !guard.sent_go_on())
            || !scan.read_mut(1, &mut guard)
        {
            cut = true;
            break;
        }
        let l = Lit::xl(i);
        let r = x.root.as_deref().expect("read above");
        let held = r.mr().holders.get(&l).is_some_and(|hs| {
            hs.iter().any(|&h| h != v) || (mode & MREPAIR_HOLDER != 0 && !hs.is_empty())
        });
        if !held || (mode & MREPAIR_UNITS == 0 && root_unit(x, l)) {
            continue;
        }
        if !m_fwd(x.root.as_deref_mut().expect("read above"), v, l, &mut guard) {
            cut = true;
            break;
        }
    }
    scan.end(&mut guard);
    if cut {
        x.store.interrupted = true;
    }
}

/// M-return in the root: a processed clause `c` whose body is ground triggers goes to each
/// context that holds every body literal, and is listed under its body literals for the
/// holders that come later. The head may be empty, so a denial returns too.
fn m_offer<S: Sink>(ix: &Index, st: &mut State, id: u32, c: &Clause, out: &mut S) {
    let mode = out.dl().mrepair;
    if mode & MREPAIR_ON == 0 || st.bottom || m_stopped(st, out) {
        return;
    }
    if c.body().is_empty() {
        // a unit of the root, for [`root_unit`]
        if let &[l] = c.head()
            && l.kind() == XL
        {
            let Some(x) = ds_admit(st, out) else { return };
            let Some(r) = root_admit(x, out) else {
                x.store.interrupted = true;
                return;
            };
            if !mrep_admit(r, out) || !RootSt::push_admit(&mut r.mw().units, l, id, out) {
                x.store.interrupted = true;
            }
        }
        return;
    }
    if mode & MREPAIR_NO_RETURN != 0 {
        return;
    }
    // the index of the clause and its holders are read through one meter (item 1280), a
    // read for each body literal tested, indexed and compared, which charges nothing, and
    // the scan of the holders, which is charged as the repair's scans are
    let scan = Meter::new(Bill::Repair);
    let inv = inv_of(ix, dix(ix));
    for &l in c.body() {
        if !scan.read(1, &*out) {
            mark_interrupted(st);
            return;
        }
        if !(l.kind() == XL && ground_trigger(out.dl().lits.view(l, inv))) {
            return;
        }
    }
    let Some(x) = ds_admit(st, out) else { return };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return;
    };
    if !mrep_admit(r, out) {
        x.store.interrupted = true;
        return;
    }
    let Some(gbody_bound) =
        map_vec_push_keys_bound_with(&r.mr().gbody, c.body(), || scan.read(1, &*out))
    else {
        x.store.interrupted = true;
        return;
    };
    let Some(gbody_bound) = gbody_bound else {
        out.speculative_overflow();
        x.store.interrupted = true;
        return;
    };
    if gbody_bound > 0 && !out.admit_dl(gbody_bound) {
        x.store.interrupted = true;
        return;
    }
    // the index grows by the body once it is admitted, and a check that stops the scan
    // leaves the rest of the body unindexed, in a store that the stop interrupts, with
    // what the index grew by committed
    let mut gbody_delta = 0usize;
    let mut go = true;
    for &l in c.body() {
        if !scan.read(1, &*out) {
            go = false;
            break;
        }
        gbody_delta += map_vec_push_dedup(&mut r.mw().gbody, l, id);
    }
    if gbody_bound > 0 {
        out.commit_dl(gbody_bound, gbody_delta);
    }
    if !go {
        x.store.interrupted = true;
        return;
    }
    // the holders of the body literal with the fewest, admitted before they are listed,
    // and the scan of the others' charged and polled
    let len = |l: &Lit| r.mr().holders.get(l).map_or(0, Vec::len);
    let mut first = c.body()[0];
    for &l in c.body() {
        if !scan.read(1, &*out) {
            x.store.interrupted = true;
            return;
        }
        if len(&l) < len(&first) {
            first = l;
        }
    }
    let cap = len(&first);
    let bytes = cap * std::mem::size_of::<Ex>();
    let mut guard = Guard::new(out);
    if !guard.reserve_add(bytes) {
        x.store.interrupted = true;
        return;
    }
    let r = x.root.as_deref().expect("made above");
    let mut hs: Vec<Ex> = Vec::with_capacity(cap);
    let mut cut = false;
    'scan: for &h in r.mr().holders.get(&first).into_iter().flatten() {
        let mut all = true;
        for &m in c.body() {
            // each body literal checked is a read
            if !scan.read_mut(1, &mut guard) {
                cut = true;
                break 'scan;
            }
            if m != first && !r.mr().hold_set.contains(&(h, m)) {
                all = false;
                break;
            }
        }
        if all {
            hs.push(h);
        }
    }
    scan.end(&mut guard);
    if cut {
        x.store.interrupted = true;
        hs.clear();
    }
    if !hs.is_empty() {
        let Some(cid) = ret_id(x, id, &mut guard) else {
            return;
        };
        for (k, h) in hs.into_iter().enumerate() {
            if !m_poll(k, st, &mut guard) {
                break;
            }
            guard.send(h, Concl::RRet(cid));
            guard.dl().count_mrepair(1);
            guard.worked(1);
            guard.dl().mcount[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// A context other than the root takes a clause the root returned (M-return), once: each
/// choice of one of its processed clauses, other than a marker, with each body literal
/// maximal, resolved with the returned clause on those literals, keeping the premises'
/// bodies and residual heads. The clause is kept under its body literals for the premises
/// that come later. Its index and its premise lists are read through one [`Meter`], and the
/// lists are admitted before they are built ([`ret_found`]). The run's bounds are checked
/// in full before the product is drained, as [`m_flush`] checks them before a late batch,
/// and the product charges its own tables as they grow.
pub fn rret<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom || m_stopped(st, out) {
        return false;
    }
    let Some(x) = ds_admit(st, out) else {
        return false;
    };
    let Some(r) = root_admit(x, out) else {
        x.store.interrupted = true;
        return false;
    };
    if r.seen.contains(&cid) {
        return false;
    }
    let c = out.dl().table.get(cid).clone();
    // each body literal sized and indexed is a read of the meter (item 1280)
    let meter = Meter::checks();
    let sized = {
        let empty: FxHashMap<Lit, Vec<u32>> = FxHashMap::default();
        let rets = r.m.as_deref().map_or(&empty, |m| &m.rets);
        map_vec_push_keys_bound_with(rets, c.body(), || meter.read(1, &*out))
    };
    let Some(sized) = sized else {
        x.store.interrupted = true;
        return false;
    };
    let Some(bound) = sized.and_then(|b| {
        let b = b.checked_add(set_insert_bound(&r.seen, &cid)?)?;
        if r.m.is_none() {
            b.checked_add(std::mem::size_of::<MRep>())
        } else {
            Some(b)
        }
    }) else {
        out.speculative_overflow();
        x.store.interrupted = true;
        return false;
    };
    if bound > 0 && !out.admit_dl(bound) {
        x.store.interrupted = true;
        return false;
    }
    let mut delta = set_insert_delta(&mut r.seen, cid).unwrap_or(0);
    if r.m.is_none() {
        r.m = Some(Box::default());
        delta += std::mem::size_of::<MRep>();
    }
    // a check that stops the index leaves the rest of the body unindexed, in a store that
    // the stop interrupts, with what the index grew by committed
    let mut go = true;
    for &l in c.body() {
        if !meter.read(1, &*out) {
            go = false;
            break;
        }
        delta += map_vec_push_dedup(&mut r.mw().rets, l, cid);
    }
    if bound > 0 {
        out.commit_dl(bound, delta);
    }
    if !go {
        x.store.interrupted = true;
        return true;
    }
    // the premise lists are read through the meter and admitted before they are built,
    // and the drain charges the product's tables as they grow (item 1280)
    let mut guard = Guard::new(out);
    let mut found = Found::default();
    found.rule = "mret";
    if !ret_found(ix, st, &c, None, &meter, &mut found, &mut guard) || !guard.go_on() {
        drop(found);
        guard.release_to(0);
        mark_interrupted(st);
        return true;
    }
    drain_admitted(ix, st, found, &mut guard);
    true
}

/// M-return's resolution of the returned clause `c` in a context, pushed to `found`: for
/// each body literal, the context's processed clauses other than markers where it is
/// maximal, selected ones included, or the premise `fixed` for its literal, when `fixed`
/// names one of the body's literals ([`ret_premises`]). The lists are read through `meter`
/// twice, once to size them and once to build them, with each body literal and each
/// literal of a premise that they copy, and admitted through `guard` with the product's
/// push and its pruning's scratch before they are built, as the rooted rules admit their
/// products. The buffer of the lists' sizes is admitted before it is made, and the pruning
/// reads through the meter too. `guard` holds `found`'s held bytes before and after. False
/// when a check of the bounds, a refusal or an overflow stopped the resolution, which
/// leaves `found` as it was.
fn ret_found<S: Sink>(
    ix: &Index,
    st: &State,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    meter: &Meter,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
) -> bool {
    use std::mem::size_of;
    let body = c.body();
    if let Some((p, _)) = fixed {
        // the late premise's literal, searched for in the body
        let mut has = false;
        for &l in body {
            if !meter.read(1, &*guard) {
                return false;
            }
            if l == p {
                has = true;
                break;
            }
        }
        if !has {
            return true;
        }
    }
    let Some(counts_bytes) = body.len().checked_mul(size_of::<usize>()) else {
        guard.speculative_overflow();
        return false;
    };
    let Some(live) = found.held_bytes().checked_add(counts_bytes) else {
        guard.speculative_overflow();
        return false;
    };
    if !guard.ensure_admitted(live) {
        guard.release_to(found.held_bytes());
        return false;
    }
    let mut counts: Vec<usize> = Vec::with_capacity(body.len());
    debug_assert_eq!(counts.capacity() * size_of::<usize>(), counts_bytes);
    let go = ret_lists(
        ix,
        st,
        c,
        fixed,
        meter,
        found,
        guard,
        &mut counts,
        counts_bytes,
    );
    // the lists' payload went with the inner call, and the sizes go before their charge
    drop(counts);
    guard.release_to(found.held_bytes());
    go
}

/// [`ret_found`]'s two passes over the lists, with `counts` the admitted buffer of their
/// sizes, which the caller drops and releases after it, whatever the answer.
#[allow(clippy::too_many_arguments)]
fn ret_lists<S: Sink>(
    ix: &Index,
    st: &State,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    meter: &Meter,
    found: &mut Found,
    guard: &mut Guard<'_, S>,
    counts: &mut Vec<usize>,
    counts_bytes: usize,
) -> bool {
    use std::mem::size_of;
    let body = c.body();
    let is_fixed = |l: Lit| fixed.filter(|&(p, _)| p == l).map(|(_, prem)| prem);
    // the sizes of the lists, read through the meter
    let Some(mut bytes) = body.len().checked_mul(size_of::<Vec<Side>>()) else {
        guard.speculative_overflow();
        return false;
    };
    let mut most = 0usize;
    {
        let cf = cf_of(ix, &*guard);
        let out: &Guard<'_, S> = guard;
        let mut overflow = false;
        let mut read = |n: usize| meter.read(n, out);
        for &l in body {
            if !read(1) {
                return false;
            }
            let (n, b) = match is_fixed(l) {
                Some(prem) => (1, side_exact_bytes(prem.0.len(), prem.1.len())),
                None => {
                    let (mut n, mut b) = (0usize, Some(0usize));
                    if !ret_premises(st, l, cf, &mut read, &mut |s| {
                        n += 1;
                        b = b.and_then(|b| {
                            super::dl_rules::side_bytes(s, l).and_then(|x| b.checked_add(x))
                        });
                    }) {
                        return false;
                    }
                    (n, b)
                }
            };
            if n == 0 {
                return true;
            }
            let Some(b) = b.and_then(|b| {
                n.checked_mul(size_of::<Side>())
                    .and_then(|x| x.checked_add(b))
                    .and_then(|x| bytes.checked_add(x))
            }) else {
                overflow = true;
                break;
            };
            bytes = b;
            most = most.max(n);
            counts.push(n);
        }
        if overflow {
            guard.speculative_overflow();
            return false;
        }
    }
    let Some(prod_bytes) = side_exact_bytes(0, c.head().len()).and_then(|b| bytes.checked_add(b))
    else {
        guard.speculative_overflow();
        return false;
    };
    let scratch = if most < 2 {
        Some(0)
    } else {
        most.checked_mul(PRUNE_SCRATCH_ELEM)
    };
    let Some(scratch) = scratch else {
        guard.speculative_overflow();
        return false;
    };
    if !found.reserve_prod_admitted(prod_bytes, scratch, counts_bytes, guard) {
        return false;
    }
    // the lists, built in the capacity admitted for them
    let cf = cf_of(ix, &*guard);
    let out: &Guard<'_, S> = guard;
    let mut read = |n: usize| meter.read(n, out);
    let mut lists: Vec<Vec<Side>> = Vec::with_capacity(body.len());
    for (&l, &n) in body.iter().zip(counts.iter()) {
        if !read(1) {
            return false;
        }
        match is_fixed(l) {
            Some(prem) => {
                // the copy of the fixed premise reads its literals
                if !read(prem.0.len() + prem.1.len()) {
                    return false;
                }
                lists.push(vec![prem.clone()]);
            }
            None => {
                let mut v = Vec::with_capacity(n);
                if !ret_premises(st, l, cf, &mut read, &mut |s| v.push(side(s, l))) {
                    return false;
                }
                debug_assert_eq!(v.len(), n, "the store did not change between the reads");
                lists.push(v);
            }
        }
    }
    let base: Side = (Vec::new(), c.head().to_vec());
    debug_assert_eq!(
        super::dl_rules::prod_inner_bytes(lists.capacity(), &lists, &base),
        prod_bytes
    );
    found.products_read(lists, base, &mut read)
}

/// Visits M-return's premises on the body literal `l` of a returned clause: the context's
/// processed clauses other than markers where l is maximal, selected ones included, and with
/// `cf`, M-return-cf ([`MREPAIR_CF`]), only those whose body has no ground literal
/// ([`ground_body`]). It tells `read` of each index entry, of each literal it tests, and of
/// the literals of each premise it keeps, which the caller sizes or copies, and is false
/// once read answers false.
fn ret_premises<'s>(
    st: &'s State,
    l: Lit,
    cf: Option<Cf>,
    read: &mut dyn FnMut(usize) -> bool,
    keep: &mut dyn FnMut(&'s Clause),
) -> bool {
    let Some(d) = dl(st) else { return true };
    for &i in d.store.by_xl.get(&l).into_iter().flatten() {
        if !read(1) {
            return false;
        }
        let s = &d.store.clauses[i as usize];
        if s.dead {
            continue;
        }
        // the clause l → l, whose premise is l's own unit
        let mut rest = false;
        for &m in s.c.head() {
            if !read(1) {
                return false;
            }
            rest |= m != l;
        }
        if !rest && s.c.body() == [l] {
            continue;
        }
        if let Some(cf) = cf {
            match ground_body_with(cf, s.c.body(), read) {
                None => return false,
                Some(true) => continue,
                Some(false) => {}
            }
        }
        // the caller sizes or copies the premise, which reads each of its literals
        if !read(s.c.body().len() + s.c.head().len()) {
            return false;
        }
        keep(&s.c);
    }
    true
}

/// The premises of M-return's resolution on each literal of `body`, for the diagnostic,
/// which tells `read` of what it reads ([`ret_premises`]) and is `None` once read answers
/// false.
fn ret_lists_with(
    st: &State,
    body: &[Lit],
    cf: Option<Cf>,
    read: &mut dyn FnMut(usize) -> bool,
) -> Option<Vec<Vec<Side>>> {
    let mut lists = Vec::with_capacity(body.len());
    for &l in body {
        let mut v = Vec::new();
        if !ret_premises(st, l, cf, read, &mut |s| v.push(side(s, l))) {
            return None;
        }
        lists.push(v);
    }
    Some(lists)
}

/// What M-return-cf's test of a premise's body reads: the index and the run's interned
/// literals.
type Cf<'a> = (&'a Index, &'a super::dl_clause::Lits);

/// M-return-cf's test, `Some` when the run's repair has [`MREPAIR_CF`].
fn cf_of<'a, S: Sink + ?Sized>(ix: &'a Index, out: &'a S) -> Option<Cf<'a>> {
    (out.dl().mrepair & MREPAIR_CF != 0).then(|| (ix, &out.dl().lits))
}

/// Whether a premise's body holds a ground literal, for M-return-cf: an interned literal
/// whose terms are constants or function terms of constants, so that it mentions neither x
/// nor y.
fn ground_body(cf: Cf, body: &[Lit]) -> bool {
    ground_body_with(cf, body, &mut |_| true).expect("a read that never stops")
}

/// [`ground_body`], telling `read` of each literal it tests, `None` once read answers false.
fn ground_body_with(
    (ix, lits): Cf,
    body: &[Lit],
    read: &mut dyn FnMut(usize) -> bool,
) -> Option<bool> {
    let inv = inv_of(ix, dix(ix));
    for &l in body {
        if !read(1) {
            return None;
        }
        if l.kind() == XL && super::dl_rules::no_xy(lits.view(l, inv)) {
            return Some(true);
        }
    }
    Some(false)
}

/// Row 1231's diagnostic, which `SPARKLES_ROOTED_MARKER_DIAG=1` turns on: after a rooted
/// run saturates, the run reports on stderr how often a context that is registered with the
/// root for a constant of a conditional ground trigger lies outside the successors of the
/// trigger's holder, so that the trigger's marker never reaches it.
pub(super) fn marker_diag() -> bool {
    #[cfg(test)]
    if let Some(on) = MARKER_DIAG_TEST.with(|c| c.get()) {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("SPARKLES_ROOTED_MARKER_DIAG").is_some_and(|v| v == "1" || v == "2")
    }) || TEST_DIAG.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    static MARKER_DIAG_TEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_marker_diag_override<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            MARKER_DIAG_TEST.with(|c| c.set(self.0));
        }
    }
    let prev = MARKER_DIAG_TEST.with(|c| c.replace(Some(on)));
    let _reset = Reset(prev);
    f()
}

/// Whether row 1231's diagnostic also prints each pair, which `SPARKLES_ROOTED_MARKER_DIAG=2`
/// turns on.
fn marker_detail() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_MARKER_DIAG").is_some_and(|v| v == "2"))
}

/// The tier of [`marker_trips`] at which row 1231's decline withdraws a rooted run's answer,
/// or `None` for no decline. A finished rooted run with a pair at that tier makes no claim:
/// the classifier starts again without the rooted rules and reports
/// `Stats::dl_rooted_declined`. `on` is [`Limits::rooted_decline`](super::Limits), which
/// gives tier 4, and `SPARKLES_ROOTED_MARKER_DECLINE` overrides it for measurements, with
/// `off` for no decline or a tier.
pub(super) fn marker_decline(on: bool) -> Option<usize> {
    static ENV: std::sync::OnceLock<Option<Option<usize>>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| {
        let v = std::env::var("SPARKLES_ROOTED_MARKER_DECLINE").ok()?;
        Some(if v == "off" {
            None
        } else {
            v.parse().ok().filter(|&t| t < MARKER_TIERS)
        })
    });
    env.unwrap_or(if on { Some(MARKER_DECLINE_TIER) } else { None })
}

/// The tier of [`marker_trips`] at which the decline is on by default.
pub(super) const MARKER_DECLINE_TIER: usize = 4;

/// The tiers of [`marker_trips`].
pub(super) const MARKER_TIERS: usize = 5;

/// What one context of a rooted run holds for [`marker_trips`].
pub(super) struct MarkerCtx {
    pub(super) me: Ex,
    /// the context's successors
    pub(super) succ: Vec<Ex>,
    /// the context's maximal ground triggers that a processed clause other than a marker has
    pub(super) held: Vec<Lit>,
    /// in the root, the contexts registered for each constant
    pub(super) edges: Vec<(u32, Vec<Ex>)>,
    /// in the root, the heads of its live facts with one literal
    pub(super) units: Vec<Lit>,
}

/// What [`marker_deficits`] reads of one context, which [`marker_def`] copies only with the
/// diagnostic, beside the context's [`MarkerCtx`], so that a copy without it is no larger.
#[derive(Default)]
pub(super) struct MarkerDef {
    /// whether the context is unsatisfiable, which discharges what it is owed
    bottom: bool,
    /// in another context, the ground literals whose markers it holds or has a live clause
    /// that subsumes, in order
    marks: Vec<Lit>,
    /// in another context, the live root clauses with ground bodies that it holds every body
    /// literal of and lacks a product of, and those whose products are too many to check
    owed: usize,
    unchecked: usize,
}

/// The factor by which a vector's capacity or a hash table's buckets can pass the
/// entries they hold: a vector doubles, and a table keeps 7 of 8 buckets at most and
/// rounds them up to a power of two.
const SLACK: usize = 3;

/// A bound on the bytes of the maps, sets and buffers that [`marker_trips`] builds over
/// the copies `cs` with a `stop`, which the run admits before the search: the index by
/// context, the predecessors with their lists, the root's reach, its successors and the
/// singleton contexts, the root's units and registrations, one holder's successors and
/// predecessors at a time, and the copies of successor lists that the searches take.
pub(super) fn marker_search_bytes(cs: &[MarkerCtx]) -> usize {
    use std::mem::size_of;
    let n = cs.len();
    let succ: usize = cs.iter().map(|c| c.succ.len()).sum();
    let root: usize = cs.iter().map(|c| c.units.len() + c.edges.len()).sum();
    // a hash entry of a context, of a context and its index, of a context and its list
    let set = size_of::<Ex>() + 1;
    let map = size_of::<(Ex, usize)>() + 1;
    let list = size_of::<(Ex, Vec<Ex>)>() + 1;
    let tables = n * (map + list + 4 * set) + root * (size_of::<(u32, &[Ex])>() + 1);
    // the predecessor lists and the copies of successor lists taken while searching
    let buffers = succ * size_of::<Ex>() * 2;
    SLACK * (tables + buffers)
}

/// What [`marker_trips`] needs of the context `me` with the state `st`.
/// `admit` takes the bytes the copy will hold, and with a negative count gives back
/// those of its scratch sets, before anything is allocated. A refusal ends the copy with
/// `Err`, and the run stops as past its memory budget.
pub(super) fn marker_ctx(
    ix: &Index,
    me: Ex,
    st: &State,
    lits: &super::dl_clause::Lits,
    admit: &mut dyn FnMut(isize) -> bool,
) -> Result<Option<MarkerCtx>, ()> {
    let Some(x) = dl(st) else { return Ok(None) };
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let is_root = dx.root == Some(me);
    // bounds on what the copy holds and on its two scratch sets of ground literals, with
    // the slack of a vector's or a table's capacity
    let ground = x.root.as_deref().map_or(0, |r| r.ground.len());
    let scratch = (SLACK * ground * 2 * (std::mem::size_of::<Lit>() + 1)) as isize;
    let mut kept = ground * std::mem::size_of::<Lit>()
        + (x.edges().len() + x.root.as_deref().map_or(0, |r| r.by_succ.len()) + st.subs.len())
            * std::mem::size_of::<Ex>();
    if is_root {
        kept += x.store.clauses.len() * std::mem::size_of::<Lit>();
        if let Some(r) = x.root.as_deref() {
            kept += r.edges.len() * std::mem::size_of::<(u32, Vec<Ex>)>()
                + r.edges.values().map(|v| v.len()).sum::<usize>() * std::mem::size_of::<Ex>();
        }
    }
    let kept = SLACK * kept;
    if !admit(kept as isize + scratch) {
        return Err(());
    }
    let mut held = Vec::new();
    if let Some(r) = x.root.as_deref()
        && !r.ground.is_empty()
    {
        // every maximal literal of a processed clause other than a marker, dead or not,
        // since a clause can have several and Stored::max holds the first
        let ground: FxHashSet<Lit> = r.ground.iter().copied().collect();
        let mut own: FxHashSet<Lit> = FxHashSet::default();
        for i in 0..x.store.clauses.len() as u32 {
            if is_marker(&x.store.clauses[i as usize].c) {
                continue;
            }
            own.extend(x.store.maxes(i).filter(|m| ground.contains(m)));
        }
        for &l in &r.ground {
            if own.contains(&l) && ground_trigger(lits.view(l, inv)) {
                held.push(l);
            }
        }
    }
    let mut edges = Vec::new();
    let mut units = Vec::new();
    if is_root {
        if let Some(r) = x.root.as_deref() {
            edges = r.edges.iter().map(|(&u, v)| (u, v.clone())).collect();
        }
        for s in &x.store.clauses {
            if !s.dead && s.c.body().is_empty() && s.c.head().len() == 1 {
                units.push(s.c.head()[0]);
            }
        }
    }
    admit(-scratch);
    Ok(Some(MarkerCtx {
        me,
        succ: successors(ix, st),
        held,
        edges,
        units,
    }))
}

/// Whether `e` is the root of the rooted run.
pub(super) fn is_root(ix: &Index, e: Ex) -> bool {
    dix(ix).root == Some(e)
}

/// The root's live clauses whose bodies are ground triggers, which [`marker_def`] reads, for
/// the diagnostic.
pub(super) fn marker_root_gbody(
    ix: &Index,
    me: Ex,
    st: &State,
    lits: &super::dl_clause::Lits,
    poll: &mut dyn FnMut(usize) -> bool,
) -> Option<Vec<Clause>> {
    let inv = inv_of(ix, dix(ix));
    let Some(x) = dl(st).filter(|_| is_root(ix, me)) else {
        return Some(Vec::new());
    };
    let mut rd = Reads::new(poll);
    let mut rg = Vec::new();
    for s in &x.store.clauses {
        // a read for the clause and one for each literal of its body
        if !rd.read(1) {
            return None;
        }
        let mut triggers = !s.dead && !s.c.body().is_empty();
        for &l in s.c.body() {
            if !rd.read(1) {
                return None;
            }
            triggers = triggers && l.kind() == XL && ground_trigger(lits.view(l, inv));
        }
        if triggers {
            rg.push(s.c.clone());
        }
    }
    rd.end().then_some(rg)
}

/// The choices of one product of [`marker_def`] past which a pair counts as unchecked.
const DEF_CHOICES: usize = 1 << 16;

/// The deficits' copy of the context `m` with the state `st`, for the diagnostic and outside
/// the run's memory budget, as the diagnostic's sets are. `rg` holds the root's live clauses
/// with ground bodies ([`marker_root_gbody`]). A clause is owed to the context when the
/// context holds every body literal, is satisfiable, and lacks one of the products that
/// M-return derives from it, each choice of a live premise with each body literal maximal
/// merged with the clause's head: the product is neither a tautology, after the sorting and
/// the tests that `derive` applies first in a rooted run (owl:Thing and owl:Nothing over
/// constants, a head literal in the body, an equality beside its inequality), nor subsumed
/// by a live clause of the context. derive's other simplifications are not replayed, so a
/// product they drop can count. With `cf`, the run's M-return-cf ([`MREPAIR_CF`]), the
/// products take only the premises that M-return-cf takes, so the deficit checks the
/// delivery of the rule the run applies, not what M-return-cf leaves out. A product that
/// derive promoted to the root as a fact ([`super::dl_rules::DlSt`]'s promoted heads) is
/// held. The copy charges `poll` with its reads and choices, after every
/// [`METER_CHECK`](super::dl_rules::METER_CHECK) of them, and is `Err` once poll answers
/// false, so that the run's deadline and cancellation bound it.
pub(super) fn marker_def(
    ix: &Index,
    m: &MarkerCtx,
    st: &State,
    rg: &[Clause],
    lits: &super::dl_clause::Lits,
    cf: bool,
    poll: &mut dyn FnMut(usize) -> bool,
) -> Result<Option<MarkerDef>, ()> {
    let Some(x) = dl(st) else { return Ok(None) };
    let is_root = is_root(ix, m.me);
    let mut rd = Reads::new(poll);
    let mut marks = Vec::new();
    for &l in x.root.as_deref().into_iter().flat_map(|r| &r.gmarks) {
        if !rd.read(1) {
            return Err(());
        }
        marks.push(l);
    }
    for s in &x.store.clauses {
        if !rd.read(1) {
            return Err(());
        }
        if s.dead {
            continue;
        }
        let (b, h) = (s.c.body(), s.c.head());
        let one: Vec<Lit> = b.iter().chain(h).copied().collect();
        if one.len() <= 2
            && b.len() <= 1
            && h.len() <= 1
            && one.windows(2).all(|w| w[0] == w[1])
            && let Some(&l) = one.first()
            && l.kind() == XL
        {
            marks.push(l);
        }
    }
    marks.sort_unstable();
    marks.dedup();
    let (mut owed, mut unchecked) = (0, 0);
    if !is_root && !st.bottom && !rg.is_empty() {
        let mut held: FxHashSet<Lit> = FxHashSet::default();
        for &l in &m.held {
            if !rd.read(1) {
                return Err(());
            }
            held.insert(l);
        }
        for c in rg {
            // a read for the clause and one for each literal of its body
            if !rd.read(1) {
                return Err(());
            }
            let mut all = true;
            for l in c.body() {
                if !rd.read(1) {
                    return Err(());
                }
                all = all && held.contains(l);
            }
            if !all {
                continue;
            }
            // the premises, each candidate and each literal it tests read
            let lists = ret_lists_with(st, c.body(), cf.then_some((ix, lits)), &mut |n| rd.read(n))
                .ok_or(())?;
            let n = lists
                .iter()
                .try_fold(1usize, |n, l| n.checked_mul(l.len()))
                .unwrap_or(usize::MAX);
            if n > DEF_CHOICES {
                unchecked += 1;
                continue;
            }
            if n > 0 && !products_held(ix, st, &lists, c.head(), lits, &mut rd).ok_or(())? {
                owed += 1;
            }
        }
    }
    if !rd.end() {
        return Err(());
    }
    Ok(Some(MarkerDef {
        bottom: st.bottom,
        marks,
        owed,
        unchecked,
    }))
}

/// Whether [`marker_def`] takes M-return-cf's premises for the repair's mode `mode`: only
/// where M-return ran with [`MREPAIR_CF`], since without [`MREPAIR_ON`] no rule restricted
/// them.
pub(super) fn marker_def_cf(mode: u8) -> bool {
    mode & (MREPAIR_ON | MREPAIR_CF) == MREPAIR_ON | MREPAIR_CF
}

/// The diagnostic's count of what it reads, item 1280's [`Meter`] over a `poll`: it charges
/// poll a conclusion for each [`WORK_READS`] reads and checks the bounds through it after
/// every [`METER_CHECK`](super::dl_rules::METER_CHECK) reads, and stops once poll answers
/// false. The redundancy test of
/// each product in [`products_held`] reads through it too, and [`marker_trips`] counts
/// nothing (roadmap item 1260).
struct Reads<'a> {
    meter: Meter,
    poll: &'a mut dyn FnMut(usize) -> bool,
}

impl<'a> Reads<'a> {
    fn new(poll: &'a mut dyn FnMut(usize) -> bool) -> Self {
        Reads {
            meter: Meter::new(Bill::Work),
            poll,
        }
    }

    /// Counts `k` reads, false when a check of the bounds stopped the diagnostic.
    fn read(&mut self, k: usize) -> bool {
        self.meter.read_poll(k, self.poll)
    }

    /// Charges `w` conclusions and checks the bounds at once.
    fn check(&mut self, w: usize) -> bool {
        (self.poll)(w)
    }

    /// Charges the reads since the last check and checks the bounds, false when the check
    /// stopped the diagnostic.
    fn end(self) -> bool {
        self.meter.end_poll(self.poll)
    }
}

/// Whether the context holds every product of `lists` with the head `head`: each is a
/// tautology or a live clause of the context subsumes it.
fn products_held(
    ix: &Index,
    st: &State,
    lists: &[Vec<Side>],
    head: &[Lit],
    lits: &super::dl_clause::Lits,
    rd: &mut Reads,
) -> Option<bool> {
    use super::dl_clause::X;
    let Some(x) = dl(st) else { return Some(false) };
    let inv = inv_of(ix, dix(ix));
    let is =
        |l: Lit, a: Ex| l.kind() == XL && matches!(lits.view(l, inv), XLit::Atom(b, _) if b == a);
    let mut at = vec![0usize; lists.len()];
    loop {
        // a choice counts as a conclusion
        if !rd.read(WORK_READS) {
            return None;
        }
        // the copy of the base's head reads each of its literals
        if !rd.read(head.len()) {
            return None;
        }
        let mut body: Vec<Lit> = Vec::new();
        let mut hd: Vec<Lit> = head.to_vec();
        for (l, &i) in lists.iter().zip(&at) {
            // each literal of a chosen premise is a read
            for &m in &l[i].0 {
                if !rd.read(1) {
                    return None;
                }
                body.push(m);
            }
            for &m in &l[i].1 {
                if !rd.read(1) {
                    return None;
                }
                hd.push(m);
            }
        }
        // the normalization of the assembled clause and its test of a tautology read each
        // of its literals
        if !rd.read(body.len() + hd.len()) {
            return None;
        }
        body.retain(|l| !(l.kind() == X && (l.atom() == TOP || st.subs.contains(&l.atom()))));
        body.retain(|&l| !is(l, TOP));
        hd.retain(|&l| !is(l, BOTTOM));
        body.sort_unstable();
        body.dedup();
        hd.sort_unstable();
        hd.dedup();
        let taut = lits.complementary(&hd, inv)
            || hd.iter().any(|&l| {
                (l.kind() == X && (l.atom() == TOP || st.subs.contains(&l.atom())))
                    || is(l, TOP)
                    || body.binary_search(&l).is_ok()
            });
        // a fact that derive sent the root ([`super::dl_rules::derive`]) is held
        let promoted = body.is_empty() && x.promoted.contains(hd.as_slice());
        if !taut && !promoted {
            let c = Clause::new(body, hd);
            if !x.store.subsumed_with(&c, c.sig(), &mut |n| rd.read(n))? {
                return Some(false);
            }
        }
        let mut k = lists.len();
        loop {
            if k == 0 {
                return Some(true);
            }
            k -= 1;
            at[k] += 1;
            if at[k] < lists[k].len() {
                break;
            }
            at[k] = 0;
        }
    }
}

/// Counts, for each tier, the conditional ground triggers, their constants and the pairs
/// of a trigger's holder and a context registered with the root for one of the trigger's
/// constants, that the holder's markers do not reach. A trigger is conditional when it is
/// no fact of the root. The tiers leave out more contexts in turn: tier 0 leaves out only
/// the holder and its successors, tier 1 also the holder's predecessors, tier 2 also the
/// constant's singleton context, tier 3 also the contexts that the root does not reach,
/// and tier 4 also the root's own successors. With `stop`, and without the pairs on
/// stderr, the count ends at the first pair of that tier, which is all a decline needs.
/// The answer also counts the pair set P_R of the repair (PLAN v3.1, D3), the pairs that
/// tier 0 keeps whose context the root reaches, as triples of holder, trigger and context
/// and as sends, the distinct pairs of context and trigger. Those two counts are complete
/// only without `stop`, and the run builds the sets of distinct triggers, constants and
/// sends only then, for the diagnostic, outside the run's memory budget.
pub(super) fn marker_trips(
    ix: &Index,
    cs: &[MarkerCtx],
    lits: &super::dl_clause::Lits,
    stop: Option<usize>,
) -> ([[usize; 3]; MARKER_TIERS], [usize; 2]) {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut out = [[0usize; 3]; MARKER_TIERS];
    let mut pr = [0usize; 2];
    let mut sends: FxHashSet<(Ex, Lit)> = FxHashSet::default();
    let Some(root) = dx.root else {
        return (out, pr);
    };
    let by: FxHashMap<Ex, usize> = cs.iter().enumerate().map(|(i, c)| (c.me, i)).collect();
    let Some(&ri) = by.get(&root) else {
        return (out, pr);
    };
    let detail = marker_detail();
    let stop = stop.filter(|_| !detail);
    let units: FxHashSet<Lit> = cs[ri].units.iter().copied().collect();
    let edges: FxHashMap<u32, &[Ex]> = cs[ri].edges.iter().map(|(u, v)| (*u, &v[..])).collect();
    let mut preds: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    for c in cs {
        for &w in &c.succ {
            preds.entry(w).or_default().push(c.me);
        }
    }
    let reach = |from: Ex, next: &dyn Fn(Ex) -> Vec<Ex>| {
        let mut seen: FxHashSet<Ex> = FxHashSet::default();
        let mut todo = vec![from];
        seen.insert(from);
        while let Some(e) = todo.pop() {
            for w in next(e) {
                if seen.insert(w) {
                    todo.push(w);
                }
            }
        }
        seen
    };
    let succ_of = |e: Ex| by.get(&e).map_or(Vec::new(), |&i| cs[i].succ.clone());
    let pred_of = |e: Ex| preds.get(&e).cloned().unwrap_or_default();
    let from_root = reach(root, &succ_of);
    let root_succ: FxHashSet<Ex> = cs[ri].succ.iter().copied().collect();
    let singleton: FxHashMap<u32, Ex> = dx.consts.iter().copied().collect();
    let mut trig: [FxHashSet<Lit>; MARKER_TIERS] = Default::default();
    let mut kons: [FxHashSet<u32>; MARKER_TIERS] = Default::default();
    for c in cs {
        if c.me == root || c.held.is_empty() {
            continue;
        }
        // the holder's successors and predecessors, found only when a pair needs them
        let down: std::cell::OnceCell<FxHashSet<Ex>> = std::cell::OnceCell::new();
        let up: std::cell::OnceCell<FxHashSet<Ex>> = std::cell::OnceCell::new();
        for &l in &c.held {
            if units.contains(&l) {
                continue;
            }
            let mut us = Vec::new();
            consts(lits.view(l, inv), &mut us);
            us.sort_unstable();
            us.dedup();
            let mut regs: Vec<(u32, Ex)> = Vec::new();
            for &u in &us {
                for &d in edges.get(&u).copied().unwrap_or(&[]) {
                    regs.push((u, d));
                }
            }
            regs.sort_unstable_by_key(|&(_, d)| d);
            regs.dedup_by_key(|p| p.1);
            for (_, d) in regs {
                if d == root {
                    continue;
                }
                // the tiers that need no search first, so that a decline skips the
                // searches of the pairs it cannot use
                let single = !us.iter().any(|v| {
                    singleton.get(v) == Some(&d) || super::dl_rules::singleton_of(ix, d) == Some(*v)
                });
                let reached = from_root.contains(&d);
                let direct = !root_succ.contains(&d);
                if let Some(t) = stop
                    && ![single, reached, direct][..t.saturating_sub(1).min(3)]
                        .iter()
                        .all(|&k| k)
                {
                    continue;
                }
                if down.get_or_init(|| reach(c.me, &succ_of)).contains(&d) {
                    continue;
                }
                if reached && stop.is_none() {
                    pr[0] += 1;
                    sends.insert((d, l));
                }
                let keep = [
                    true,
                    !up.get_or_init(|| reach(c.me, &pred_of)).contains(&d),
                    single,
                    reached,
                    direct,
                ];
                if detail {
                    let t = keep.iter().take_while(|&&k| k).count();
                    eprintln!(
                        "MARKER-PAIR tier<={} holder={} lit={} ctx={d}",
                        t.saturating_sub(1),
                        c.me,
                        super::dl_rules::tr(ix, lits, &[l])
                    );
                }
                let mut tier = 0;
                while tier < MARKER_TIERS && keep[..=tier].iter().all(|&k| k) {
                    out[tier][2] += 1;
                    // the sets of the diagnostic, which a decline does not build
                    if stop.is_none() {
                        trig[tier].insert(l);
                        kons[tier].extend(us.iter().copied());
                    }
                    tier += 1;
                }
                if stop.is_some_and(|t| out[t][2] > 0) {
                    pr[1] = sends.len();
                    return (out, pr);
                }
            }
        }
    }
    for t in 0..MARKER_TIERS {
        out[t][0] = trig[t].len();
        out[t][1] = kons[t].len();
    }
    pr[1] = sends.len();
    (out, pr)
}

/// The two deficits of the marker repair (PLAN v3.1, D4), found at quiescence from the
/// contexts' clauses and not from the sends: the triples of P_R, holder, trigger and context,
/// whose context holds neither the trigger's marker nor a clause that subsumes it, and the
/// pairs of a live root clause whose body is ground triggers and a context that holds every
/// body literal and lacks one of the clause's M-return products ([`marker_def`]), and the
/// pairs whose products are too many to check. `defs` holds marker_def's copy of each
/// context of `cs`, in its order. The count charges `poll` with the contexts, successors,
/// literals, registrations and destinations it reads, checks it for each context and
/// after every [`METER_CHECK`](super::dl_rules::METER_CHECK) reads, and is `None` once
/// poll answers false.
pub(super) fn marker_deficits(
    ix: &Index,
    cs: &[MarkerCtx],
    defs: &[Option<MarkerDef>],
    lits: &super::dl_clause::Lits,
    poll: &mut dyn FnMut(usize) -> bool,
) -> Option<[usize; 3]> {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut out = [0usize; 3];
    let Some(root) = dx.root else {
        return Some(out);
    };
    let mut rd = Reads::new(poll);
    let mut by: FxHashMap<Ex, usize> = FxHashMap::default();
    for (i, c) in cs.iter().enumerate() {
        if !rd.read(1) {
            return None;
        }
        by.insert(c.me, i);
    }
    let Some(&ri) = by.get(&root) else {
        return rd.end().then_some(out);
    };
    let mut units: FxHashSet<Lit> = FxHashSet::default();
    for &l in &cs[ri].units {
        if !rd.read(1) {
            return None;
        }
        units.insert(l);
    }
    let mut edges: FxHashMap<u32, &[Ex]> = FxHashMap::default();
    for (u, v) in &cs[ri].edges {
        if !rd.read(1) {
            return None;
        }
        edges.insert(*u, &v[..]);
    }
    // the contexts that `from` reaches, each successor read a read
    let reach = |from: Ex, rd: &mut Reads| {
        let mut seen: FxHashSet<Ex> = FxHashSet::default();
        let mut todo = vec![from];
        seen.insert(from);
        while let Some(e) = todo.pop() {
            let succ = by.get(&e).map_or(&[][..], |&i| &cs[i].succ[..]);
            if !rd.read(1) {
                return None;
            }
            for &w in succ {
                if !rd.read(1) {
                    return None;
                }
                if seen.insert(w) {
                    todo.push(w);
                }
            }
        }
        Some(seen)
    };
    let from_root = reach(root, &mut rd)?;
    for c in cs {
        if !rd.check(1) {
            return None;
        }
        if c.me == root || c.held.is_empty() {
            continue;
        }
        let down = reach(c.me, &mut rd)?;
        let mut us = Vec::new();
        let mut to: Vec<Ex> = Vec::new();
        for &l in &c.held {
            if !rd.read(1) {
                return None;
            }
            if units.contains(&l) {
                continue;
            }
            us.clear();
            consts(lits.view(l, inv), &mut us);
            to.clear();
            for &d in us
                .iter()
                .flat_map(|u| edges.get(u).copied().unwrap_or(&[]).iter())
            {
                if !rd.read(1) {
                    return None;
                }
                if d != root && from_root.contains(&d) && !down.contains(&d) {
                    to.push(d);
                }
            }
            to.sort_unstable();
            to.dedup();
            for &d in &to {
                if !rd.read(1) {
                    return None;
                }
                let Some(&i) = by.get(&d) else { continue };
                let Some(dd) = defs.get(i).and_then(Option::as_ref) else {
                    continue;
                };
                if !dd.bottom && dd.marks.binary_search(&l).is_err() {
                    out[0] += 1;
                }
            }
        }
    }
    for d in defs.iter().flatten() {
        if !rd.read(1) {
            return None;
        }
        out[1] += d.owed;
        out[2] += d.unchecked;
    }
    rd.end().then_some(out)
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::dl_norm::DlAx;
    use super::super::dl_rules::{DlRun, METER_CHECK};
    use super::super::index::{Axioms, Interner, RoleName};
    use super::super::saturate::{Bounds, Contexts, saturate};
    use super::*;
    use crate::model::{Class, NamedIndividual, ObjectProperty};
    use std::sync::OnceLock;

    /// A sink for M-return's late replay: its answer to every check of the run's bounds, the
    /// checks it was asked, and the bytes it holds reserved and their most.
    struct Bounded<'a> {
        run: &'a DlRun,
        go: bool,
        polls: std::cell::Cell<usize>,
        held: usize,
        peak: usize,
    }

    impl Sink for Bounded<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn go_on(&self) -> bool {
            self.polls.set(self.polls.get() + 1);
            self.go
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.polls.set(self.polls.get() + 1);
            self.held += bytes;
            self.peak = self.peak.max(self.held);
            if !self.go {
                self.held -= bytes;
            }
            self.go
        }
        fn release_data(&mut self, bytes: usize) {
            self.held -= bytes;
        }
    }

    /// What a late replay did, [`late_replay`]'s answer.
    #[derive(Debug)]
    struct Late {
        /// the checks of the bounds before the caller drained the rule's other conclusions
        polls: usize,
        /// whether the store was interrupted then
        interrupted: bool,
        /// the conclusions derived in all
        work: usize,
        /// the most bytes reserved at once, and the bytes still reserved at the end
        peak: usize,
        held: usize,
    }

    /// A context other than the root holds `rets` returned clauses A(u) ∧ M(u) → H_n(u) and
    /// `partners` processed premises B_j(x) → M(u), with ground-free bodies so that
    /// M-return-cf keeps them, and takes the late premise K(x) → A(u) with the repair in
    /// `mode`, under a sink whose checks of the bounds all answer `go`.
    fn late_replay(mode: u8, rets: usize, partners: usize, go: bool) -> Late {
        let mut sig = Sig::new();
        let [a, m, k] = ["A", "M", "K"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let bs: Vec<Ex> = (0..partners).map(|j| sig.class(&format!("B{j}"))).collect();
        let hs: Vec<Ex> = (0..rets).map(|n| sig.class(&format!("H{n}"))).collect();
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let inv = inv_of(&ix, dix(&ix));
        let lit = |a: Ex| match run.lits.lit(XLit::Atom(a, c(u, &ix)), inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let (la, lm) = (lit(a), lit(m));
        let ids: Vec<u32> = hs
            .iter()
            .map(|&h| run.table.push(Clause::new(vec![la, lm], vec![lit(h)])))
            .collect();
        let mut st = State {
            init: true,
            ..State::default()
        };
        for &b in &bs {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(b)], vec![lm]));
        }
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        while ds(&mut st)
            .store
            .next(&ix, Some(&order), &mut max)
            .is_some()
        {}
        let mut out = Bounded {
            run: &run,
            go,
            polls: std::cell::Cell::new(0),
            held: 0,
            peak: 0,
        };
        let r = rst(&mut st, &out);
        mrep_new(r, &out);
        r.mw().mheld.insert(la);
        r.mw().rets.insert(la, ids);
        let work = run.work();
        let premise = Clause::new(vec![Lit::x(k)], vec![la]);
        let prem = side(&premise, la);
        let mut found_guard = Guard::new(&mut out);
        let mut found = Found::default();
        on_max(
            &ix,
            k,
            &mut st,
            &premise,
            la,
            &prem,
            &mut found,
            &mut found_guard,
            true,
        );
        let interrupted = ds(&mut st).store.interrupted;
        drain_admitted(&ix, &mut st, found, &mut found_guard);
        drop(found_guard);
        let polls = out.polls.get();
        Late {
            polls,
            interrupted,
            work: run.work() - work,
            peak: out.peak,
            held: out.held,
        }
    }

    #[test]
    fn m_return_s_late_replay_is_bounded_and_complete() {
        // the replay of many saved returns checks the run's bounds and admits what it
        // prepares batch by batch, so a refusal stops it before the returns are all
        // expanded, whatever their number, and with every check passed it derives each
        // return's resolvent with each partner, a last part batch included
        for mode in [MREPAIR_ON, MREPAIR_ON | MREPAIR_CF] {
            let (rets, partners) = (4 * M_POLL + 5, 3);
            let all = late_replay(mode, rets, partners, true);
            assert_eq!(all.work, rets * partners, "mode {mode}: {all:?}");
            assert!(
                !all.interrupted && all.peak > 0 && all.held == 0,
                "mode {mode}: {all:?}"
            );
            let more = late_replay(mode, 4 * rets, partners, true);
            assert_eq!(more.work, 4 * rets * partners, "mode {mode}: {more:?}");
            assert_eq!(more.peak, all.peak, "mode {mode}: one batch at a time");
            for rets in [rets, 4 * rets] {
                let cut = late_replay(mode, rets, partners, false);
                assert!(cut.polls >= 1 && cut.interrupted, "mode {mode}: {cut:?}");
                assert!(cut.work < M_POLL * partners, "mode {mode}: {cut:?}");
                assert!(
                    cut.peak <= all.peak && cut.held == 0,
                    "mode {mode}: {cut:?}"
                );
            }
        }
    }

    #[test]
    fn m_return_s_late_replay_closes_a_batch_on_its_bytes() {
        // with many partners a batch closes on RECORD_SLACK after a few returns, well before
        // M_POLL of them, so what the replay and its drain hold at once stays near the slack,
        // far below the returns' products together, whatever the number of returns
        for mode in [MREPAIR_ON, MREPAIR_ON | MREPAIR_CF] {
            let partners = 100;
            let one = late_replay(mode, 1, partners, true).peak;
            assert!(
                one > 0 && one < RECORD_SLACK / 2,
                "mode {mode}: one return {one}"
            );
            let rets = M_POLL - 1;
            let all = late_replay(mode, rets, partners, true);
            assert_eq!(all.work, rets * partners, "mode {mode}: {all:?}");
            assert!(
                all.peak <= 2 * RECORD_SLACK,
                "mode {mode}: {all:?}, one {one}"
            );
            assert!(2 * all.peak < rets * one, "mode {mode}: {all:?}, one {one}");
            let more = late_replay(mode, 4 * rets, partners, true);
            assert_eq!(more.work, 4 * rets * partners, "mode {mode}: {more:?}");
            assert!(
                more.peak <= 2 * RECORD_SLACK,
                "mode {mode}: {more:?}, {all:?}"
            );
            let cut = late_replay(mode, 4 * rets, partners, false);
            assert!(
                cut.polls >= 1 && cut.interrupted && cut.held == 0,
                "mode {mode}: {cut:?}"
            );
            assert!(cut.peak <= 2 * RECORD_SLACK, "mode {mode}: {cut:?}");
        }
    }

    /// The repair's modes that the bounds tests run: M-return alone and with M-return-cf.
    const MODES: [u8; 2] = [MREPAIR_ON, MREPAIR_ON | MREPAIR_CF];

    /// A sink for the repair's bounds: it records what the rules send, counts the checks
    /// of the run's bounds, answers each with `refuse` unset or among the first `after`,
    /// and stays stopped after a refusal or once `stop` is set, as a run does. It tracks
    /// the bytes reserved. With `grant`, a reservation checks only the budget, as the
    /// production sink grants memory without the full check of the bounds, and with
    /// `cadence` the sink keeps the activation's count of the meters' reads.
    struct Stops<'a> {
        run: &'a DlRun,
        polls: std::cell::Cell<usize>,
        stop: std::cell::Cell<bool>,
        refuse: bool,
        after: usize,
        budget: Option<usize>,
        overflow: bool,
        sent: Vec<(Ex, Concl)>,
        asked: usize,
        held: usize,
        peak: usize,
        grant: bool,
        cadence: Option<std::cell::Cell<usize>>,
    }

    impl<'a> Stops<'a> {
        fn new(run: &'a DlRun) -> Self {
            Stops {
                run,
                polls: Default::default(),
                stop: Default::default(),
                refuse: false,
                after: 0,
                budget: None,
                overflow: false,
                sent: Vec::new(),
                asked: 0,
                held: 0,
                peak: 0,
                grant: false,
                cadence: None,
            }
        }
    }

    impl Sink for Stops<'_> {
        fn meter_reads(&self) -> Option<&std::cell::Cell<usize>> {
            self.cadence.as_ref()
        }
        fn send(&mut self, e: Ex, c: Concl) {
            self.sent.push((e, c));
        }
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn go_on(&self) -> bool {
            self.polls.set(self.polls.get() + 1);
            if self.refuse && self.polls.get() > self.after {
                self.stop.set(true);
            }
            !self.stop.get()
        }
        fn stopped(&self) -> bool {
            self.stop.get()
        }
        fn speculative_overflow(&mut self) {
            self.overflow = true;
            self.stop.set(true);
        }
        fn retained_overflow(&mut self) {
            self.overflow = true;
            self.stop.set(true);
        }
        fn reserve_data(&mut self, n: usize) -> bool {
            self.asked += n;
            self.held += n;
            self.peak = self.peak.max(self.held);
            if self.budget.is_some_and(|b| self.held > b) {
                self.stop.set(true);
            }
            let ok = if self.grant {
                !self.stop.get()
            } else {
                self.go_on()
            };
            if !ok {
                self.held -= n;
            }
            ok
        }
        fn release_data(&mut self, n: usize) {
            self.held -= n;
        }
    }

    /// The literal `a(u)` of the run.
    fn glit(run: &DlRun, ix: &Index, a: Ex, u: Ex) -> Lit {
        match run.lits.lit(XLit::Atom(a, c(u, ix)), inv_of(ix, dix(ix))) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        }
    }

    #[test]
    fn the_repair_s_handlers_do_nothing_once_the_run_stopped() {
        // a run that stopped takes no further notice, reach or return: the handlers neither
        // record, allocate, charge nor send, and M-send's replay is cut off at its entry
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, k] = ["A", "B", "K"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, root) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.edges.insert(cu, vec![500]);
            r.mw().reach_set.insert(500);
            r.mw().holders.insert(ga, vec![501]);
            r.mw().hold_set.insert((501, ga));
            r.mw().held_at.insert(cu, vec![ga.xid()]);
            let cid = run.table.push(Clause::new(vec![ga], vec![gb]));
            out.stop.set(true);
            let (work, bytes) = (run.work(), run.bytes());
            assert!(!rmark(&ix, &mut st, k, gb.xid(), &mut out), "mode {mode}");
            assert!(!rreach(&ix, root, &mut st, 600, &mut out), "mode {mode}");
            m_offer(&ix, &mut st, 0, &Clause::new(vec![ga], vec![gb]), &mut out);
            m_registered(ds(&mut st), 500, &[cu], &mut out);
            let m = dl(&st).unwrap().root.as_deref().unwrap().mr();
            assert!(!m.hold_set.contains(&(k, gb)), "mode {mode}");
            assert!(
                m.gbody.is_empty() && !m.reach_set.contains(&600),
                "mode {mode}"
            );
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            // a context other than the root that the stop finds without state keeps none
            let mut other = State::default();
            assert!(!rreach(&ix, k, &mut other, TOP, &mut out), "mode {mode}");
            assert!(!rret(&ix, &mut other, cid, &mut out), "mode {mode}");
            assert!(other.extra.is_none(), "mode {mode}");
            // M-send's notice and its late replay
            let mut other = State::default();
            ds(&mut other);
            let premise = Clause::new(vec![Lit::x(k)], vec![ga]);
            let prem = side(&premise, ga);
            assert!(
                !m_note(&ix, root, k, &mut other, ga, &prem, &mut out),
                "mode {mode}"
            );
            assert!(dl(&other).unwrap().root.is_none(), "mode {mode}");
            assert!(!m_poll(0, &mut other, &mut out), "mode {mode}");
            assert!(out.sent.is_empty(), "mode {mode}: {:?}", out.sent);
            assert_eq!((run.work(), run.bytes()), (work, bytes), "mode {mode}");
        }
    }

    /// A context that holds four premises with `A(u)` in their heads and four with `B(u)`,
    /// each with a body `K(x)` of its own, and the run's returned clause `A(u) ∧ B(u) → H(u)`,
    /// whose M-return is a product of two lists of four.
    fn wide_return(mode: u8) -> (Index, DlRun, State, u32) {
        let mut sig = Sig::new();
        let [a, b, h] = ["A", "B", "H"].map(|n| sig.class(n));
        let ks: Vec<Ex> = (0..8).map(|i| sig.class(&format!("K{i}"))).collect();
        let u = sig.ind("u");
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let (ga, gb, gh) = (
            glit(&run, &ix, a, u),
            glit(&run, &ix, b, u),
            glit(&run, &ix, h, u),
        );
        let mut st = State::default();
        for (i, &k) in ks.iter().enumerate() {
            let l = if i < 4 { ga } else { gb };
            let id = ds(&mut st).store.clauses.len() as u32;
            assert!(
                ds(&mut st)
                    .store
                    .add(Clause::new(vec![Lit::x(k)], vec![l]))
                    .is_some()
            );
            ds(&mut st).store.by_xl.entry(l).or_default().push(id);
        }
        let cid = run.table.push(Clause::new(vec![ga, gb], vec![gh]));
        (ix, run, st, cid)
    }

    #[test]
    fn m_return_reserves_its_fold_tables() {
        for mode in MODES {
            // granted: the reservation covers the product's tables and is released after
            let (ix, run, mut st, cid) = wide_return(mode);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            assert!(rret(&ix, &mut st, cid, &mut out), "mode {mode}");
            let derived = dl(&st).unwrap().store.clauses.len() - stored;
            assert!(derived > 0, "mode {mode}: the return derives");
            assert!(!dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}: everything reserved is released");
            // 16 unions in a table of 32 buckets, which grew from 16, beyond 48 bytes each
            assert!(out.peak >= 32 * 49 - 16 * 48, "mode {mode}: {}", out.peak);
        }
    }

    #[test]
    fn a_refused_m_return_reservation_stops_the_return() {
        // refused at its first check: nothing is derived and the store is interrupted
        for mode in MODES {
            let (ix, run, mut st, cid) = wide_return(mode);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            out.refuse = true;
            assert!(rret(&ix, &mut st, cid, &mut out), "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len(),
                stored,
                "mode {mode}: nothing derived after the refusal"
            );
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert!(out.stop.get() && out.asked > 0, "mode {mode}");
            assert_eq!(
                out.held, 0,
                "mode {mode}: the refused reservation is released"
            );
        }
    }

    /// A context holding `counts[i]` distinct non-ground singleton premises for body
    /// literal `i` of a returned clause `A0(u) ∧ ... ∧ A_{n-1}(u) → H(u)`.
    pub(in crate::classify) fn multi_list_return(
        mode: u8,
        counts: &[usize],
    ) -> (Index, DlRun, State, u32, Vec<Lit>, Lit) {
        let mut sig = Sig::new();
        let h = sig.class("H");
        let atoms: Vec<Ex> = (0..counts.len())
            .map(|i| sig.class(&format!("A{i}")))
            .collect();
        let total: usize = counts.iter().sum();
        let ks: Vec<Ex> = (0..total).map(|i| sig.class(&format!("K{i}"))).collect();
        let u = sig.ind("u");
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let lits: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
        let gh = glit(&run, &ix, h, u);
        let mut st = State::default();
        let mut ki = 0usize;
        for (&l, &cnt) in lits.iter().zip(counts) {
            for _ in 0..cnt {
                let k = ks[ki];
                ki += 1;
                let id = ds(&mut st).store.clauses.len() as u32;
                assert!(
                    ds(&mut st)
                        .store
                        .add(Clause::new(vec![Lit::x(k)], vec![l]))
                        .is_some()
                );
                ds(&mut st).store.by_xl.entry(l).or_default().push(id);
            }
        }
        let cid = run.table.push(Clause::new(lits.clone(), vec![gh]));
        (ix, run, st, cid, lits, gh)
    }

    #[test]
    fn a_return_charges_what_its_product_holds() {
        // A unit H(u) subsumes every conclusion of the return, so the run stores nothing and
        // what the heap holds past the start is what the return builds, its premise lists
        // and its product's tables. The charge covers the heap and stays within a small
        // factor of it, where the bound that item 1280 removed took one union per choice.
        for mode in MODES {
            let (ix, run, mut st, cid, _, gh) = multi_list_return(mode, &[64, 64]);
            assert!(
                ds(&mut st)
                    .store
                    .add(Clause::new(vec![], vec![gh]))
                    .is_some()
            );
            let stored = dl(&st).unwrap().store.clauses.len();
            let before = st.bytes();
            let copy = run.table.get(cid).bytes();
            let mut out = Stops::new(&run);
            let (go, heap) =
                super::super::numeric::tests::measure(|| rret(&ix, &mut st, cid, &mut out));
            // the sink counts no retained growth (`admit_dl`), so the return's record of the
            // clause under its body literals is added from the state's own count, with the
            // copy of the clause that the return holds while it runs
            let retained = st.bytes() - before;
            assert!(go, "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len(),
                stored,
                "mode {mode}: every conclusion is subsumed"
            );
            assert!(!dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(
                out.peak + retained + copy >= heap.peak,
                "mode {mode}: charged {} with {retained} retained and a copy of {copy} below \
                 the heap's {}",
                out.peak,
                heap.peak
            );
            assert!(
                out.peak <= 4 * heap.peak,
                "mode {mode}: charged {} against the heap's {}",
                out.peak,
                heap.peak
            );
        }
    }

    #[test]
    fn a_wide_m_return_stops_on_its_tables_before_it_derives() {
        // Six lists of 512 premises followed by 8 give 2^57 choices from only 3,080
        // distinct premises. Since item 1280 the return reserves no bound on the product's
        // tables before it drains, which here would pass any budget, and the product's
        // steps charge their tables as they grow, so the budget stops the return within
        // its second step, before its last step derives anything, without an overflow.
        for mode in MODES {
            let (ix, run, mut st, cid, _, _) =
                multi_list_return(mode, &[512, 512, 512, 512, 512, 512, 8]);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            out.budget = Some(4 << 20);
            assert!(rret(&ix, &mut st, cid, &mut out), "mode {mode}");
            assert!(!out.overflow, "mode {mode}: a representable charge");
            assert!(out.stop.get(), "mode {mode}: the budget stops the return");
            // the refused request is the last one, so what was asked stays near the budget
            assert!(out.peak < 16 << 20, "mode {mode}: {}", out.peak);
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len(),
                stored,
                "mode {mode}: nothing derived"
            );
        }
    }

    /// Item 1280's harness: one run of a metered step on a fixture of size `n`, which
    /// answers the reads it must make at least and the checks of the bounds that its sink
    /// counted. The harness grows `n` eight times over and asserts that the meters never
    /// count more than `most` reads between two checks, that every read the fixture needs
    /// was counted, and that the sink saw a check for every `METER_CHECK` reads.
    fn meter_grows(
        what: &str,
        most: usize,
        sizes: [usize; 4],
        run: impl Fn(usize) -> (usize, usize),
    ) {
        let mut last = 0;
        for n in sizes {
            let _ = super::super::dl_rules::meter_probe::take();
            let (need, polls) = run(n);
            let (reads, checks, between) = super::super::dl_rules::meter_probe::take();
            assert!(
                between <= most,
                "{what} at {n}: {between} reads between two checks, past {most}"
            );
            assert!(
                reads >= need,
                "{what} at {n}: counted {reads} reads of {need}"
            );
            assert!(
                checks >= reads / METER_CHECK && polls >= checks,
                "{what} at {n}: {checks} checks and {polls} polls for {reads} reads"
            );
            assert!(reads > last, "{what} at {n}: the reads grow with the input");
            last = reads;
        }
        assert!(
            last >= 2 * METER_CHECK,
            "{what}: the largest input checks the bounds"
        );
    }

    /// A context in which each of the literals `A_j(u)`, `j < lists`, has `per` premises
    /// `P_0(x) ∧ ... ∧ P_{side-2}(x) ∧ K(x) → A_j(u)` that share all body literals but the
    /// last, and the returned clause `A_0(u) ∧ ... → H(u)`.
    fn meter_fixture(
        mode: u8,
        lists: usize,
        per: usize,
        side: usize,
    ) -> (Index, DlRun, State, Clause) {
        let mut sig = Sig::new();
        let h = sig.class("H");
        let ps: Vec<Ex> = (0..side.saturating_sub(1))
            .map(|i| sig.class(&format!("P{i}")))
            .collect();
        let atoms: Vec<Ex> = (0..lists).map(|i| sig.class(&format!("A{i}"))).collect();
        let ks: Vec<Ex> = (0..lists * per)
            .map(|i| sig.class(&format!("K{i}")))
            .collect();
        let u = sig.ind("u");
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let lits: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
        let gh = glit(&run, &ix, h, u);
        let mut st = State::default();
        for (j, &l) in lits.iter().enumerate() {
            for &k in &ks[j * per..(j + 1) * per] {
                let mut body: Vec<Lit> = ps.iter().map(|&p| Lit::x(p)).collect();
                body.push(Lit::x(k));
                let id = ds(&mut st).store.clauses.len() as u32;
                assert!(ds(&mut st).store.add(Clause::new(body, vec![l])).is_some());
                ds(&mut st).store.by_xl.entry(l).or_default().push(id);
            }
        }
        (ix, run, st, Clause::new(lits, vec![gh]))
    }

    /// The premise lists of a returned clause, read through one meter, with their pruning.
    fn meter_ret_found(mode: u8, lists: usize, per: usize, side: usize) -> usize {
        let (ix, run, st, c) = meter_fixture(mode, lists, per, side);
        let mut out = Stops::new(&run);
        let mut found = Found::default();
        let meter = Meter::checks();
        let mut guard = Guard::new(&mut out);
        assert!(ret_found(
            &ix, &st, &c, None, &meter, &mut found, &mut guard
        ));
        assert_eq!(found.products_len(), 1);
        drop(found);
        drop(guard);
        out.polls.get()
    }

    #[test]
    fn a_stop_at_a_check_of_the_meter_leaves_nothing_held() {
        // A refusal at each check of the bounds in turn, the meter's own checks and the
        // admissions alike, stops the step with nothing held and nothing pushed or sent.
        for mode in MODES {
            // the premise lists of a return, through the count pass, the admission, the
            // build pass and the pruning's signatures and comparisons, refused at every check
            let granted = meter_ret_found(mode, 1, 600, 2);
            assert!(granted >= 100, "mode {mode}: {granted}");
            let (ix, run, st, c) = meter_fixture(mode, 1, 600, 2);
            for k in 0..granted {
                let mut out = Stops::new(&run);
                out.refuse = true;
                out.after = k;
                let mut found = Found::default();
                let meter = Meter::checks();
                let _ = super::super::dl_rules::meter_probe::take_refused();
                let go = {
                    let mut guard = Guard::new(&mut out);
                    let go = ret_found(&ix, &st, &c, None, &meter, &mut found, &mut guard);
                    // the guard holds only what `found` keeps, the capacity of its
                    // products' slots, and none of the lists' payload
                    assert_eq!(
                        guard.bytes(),
                        found.held_bytes(),
                        "mode {mode} at {k}: held with the guard"
                    );
                    go
                };
                assert!(!go, "mode {mode} at {k}");
                assert_eq!(found.products_len(), 0, "mode {mode} at {k}");
                assert_eq!(out.held, 0, "mode {mode} at {k}");
                assert_eq!(
                    super::super::dl_rules::meter_probe::take_refused(),
                    0,
                    "mode {mode} at {k}: reads asked of a stopped meter"
                );
            }
            // the index and the holders of an offered clause
            let offer = |after: Option<usize>| {
                let mut sig = Sig::new();
                let h = sig.class("H");
                let atoms: Vec<Ex> = (0..4).map(|i| sig.class(&format!("A{i}"))).collect();
                let u = sig.ind("u");
                let (ix, _) = probe_ix(sig);
                let mut run = DlRun::new(false, Default::default());
                run.mrepair = mode;
                let ls: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
                let c = Clause::new(ls.clone(), vec![glit(&run, &ix, h, u)]);
                let mut st = State::default();
                let id = ds(&mut st).store.clauses.len() as u32;
                assert!(ds(&mut st).store.add(c.clone()).is_some());
                let mut out = Stops::new(&run);
                let r = rst(&mut st, &out);
                mrep_new(r, &out);
                let hs: Vec<Ex> = (0..1200).map(|i| 1000 + i).collect();
                for &l in &ls {
                    r.mw().holders.insert(l, hs.clone());
                    for &e in &hs {
                        r.mw().hold_set.insert((e, l));
                    }
                }
                if let Some(k) = after {
                    out.refuse = true;
                    out.after = k;
                }
                m_offer(&ix, &mut st, id, &c, &mut out);
                let sent = out
                    .sent
                    .iter()
                    .filter(|(_, c)| matches!(c, Concl::RRet(_)))
                    .count();
                (
                    out.polls.get(),
                    sent,
                    dl(&st).unwrap().store.interrupted,
                    out.held,
                )
            };
            let (granted, sent, cut, _) = offer(None);
            assert!(
                !cut && sent == 1200 && granted >= 4,
                "mode {mode}: {granted}"
            );
            for k in 0..granted {
                let (_, sent, cut, held) = offer(Some(k));
                assert!(
                    cut && sent < 1200 && held == 0,
                    "mode {mode} at {k}: {sent}"
                );
            }
        }
        // Join's premises on the default path
        let mut sig = Sig::new();
        let [b, z] = ["B", "Z"].map(|n| sig.class(n));
        let ks: Vec<Ex> = (0..3000).map(|i| sig.class(&format!("K{i}"))).collect();
        let u = sig.ind("u");
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let gb = glit(&run, &ix, b, u);
        let mut st = State::default();
        for &k in &ks {
            let id = ds(&mut st).store.clauses.len() as u32;
            assert!(
                ds(&mut st)
                    .store
                    .add(Clause::new(vec![Lit::x(k)], vec![gb]))
                    .is_some()
            );
            ds(&mut st).store.by_xl.entry(gb).or_default().push(id);
        }
        let target = Clause::new(vec![gb], vec![Lit::x(z)]);
        let join = |after: Option<usize>| {
            let mut out = Stops::new(&run);
            if let Some(k) = after {
                out.refuse = true;
                out.after = k;
            }
            let mut found = Found::default();
            let _ = super::super::dl_rules::meter_probe::take_refused();
            let go = {
                let mut guard = Guard::new(&mut out);
                let go = join_body(&ix, &st, &target, &mut found, &mut guard);
                assert_eq!(guard.bytes(), found.held_bytes(), "held with the guard");
                go
            };
            let refused = super::super::dl_rules::meter_probe::take_refused();
            (
                go,
                found.one_sides().len(),
                out.polls.get(),
                out.held,
                refused,
            )
        };
        let (go, n, granted, _, _) = join(None);
        assert!(go && n == 3000 && granted >= 2, "{granted}");
        for k in 0..granted {
            let (go, n, _, held, refused) = join(Some(k));
            assert!(!go && n < 3000 && held == 0 && refused == 0, "at {k}: {n}");
        }
    }

    #[test]
    fn the_premise_lists_of_a_return_check_the_bounds_at_a_fixed_count_of_reads() {
        for mode in MODES {
            meter_grows(
                "premises per literal",
                METER_CHECK,
                [256, 512, 1024, 2048],
                |n| (2 * n + n * (n - 1) / 2, meter_ret_found(mode, 1, n, 2)),
            );
            meter_grows("premise length", METER_CHECK, [32, 64, 128, 256], |n| {
                let body = if mode & MREPAIR_CF != 0 {
                    2 * 32 * n
                } else {
                    0
                };
                (32 * 31 / 2 + body, meter_ret_found(mode, 1, 32, n))
            });
            meter_grows("returned body", METER_CHECK, [512, 1024, 2048, 4096], |n| {
                (4 * n, meter_ret_found(mode, n, 1, 1))
            });
            // one premise of n literals, sized and copied once in each pass
            meter_grows(
                "one wide premise",
                METER_CHECK,
                [512, 1024, 2048, 4096],
                |n| (2 * n, meter_ret_found(mode, 1, 1, n)),
            );
            // a late premise on the last of n body literals, the first of which has no
            // premise: the search for its literal reads the body
            meter_grows(
                "late premise's literal",
                METER_CHECK,
                [1024, 2048, 4096, 8192],
                |n| {
                    let (ix, run, st, c) = meter_fixture(mode, n, 0, 1);
                    let last = *c.body().last().unwrap();
                    let prem = unit();
                    let mut out = Stops::new(&run);
                    let mut found = Found::default();
                    let meter = Meter::checks();
                    {
                        let mut guard = Guard::new(&mut out);
                        assert!(ret_found(
                            &ix,
                            &st,
                            &c,
                            Some((last, &prem)),
                            &meter,
                            &mut found,
                            &mut guard
                        ));
                    }
                    assert_eq!(found.products_len(), 0);
                    (n, out.polls.get())
                },
            );
        }
    }

    #[test]
    fn a_returned_clause_s_index_ends_at_a_refused_read() {
        use super::super::dl_rules::meter_probe;
        // A returned clause of n body literals, none with a premise in the context: a
        // refusal while the clause is listed under its body literals leaves the rest
        // unlisted, asks nothing more of the meter and holds nothing.
        for mode in MODES {
            let n = 4 * METER_CHECK;
            let ret = |after: Option<usize>| {
                let (ix, run, mut st, c) = meter_fixture(mode, n, 0, 1);
                let _ = ds(&mut st);
                let cid = run.table.push(c);
                let mut out = Stops::new(&run);
                if let Some(k) = after {
                    out.refuse = true;
                    out.after = k;
                }
                let _ = meter_probe::take_refused();
                let _ = rret(&ix, &mut st, cid, &mut out);
                let refused = meter_probe::take_refused();
                let listed = dl(&st)
                    .and_then(|x| x.root.as_deref())
                    .and_then(|r| r.m.as_deref())
                    .map_or(0, |m| m.rets.values().filter(|v| v.contains(&cid)).count());
                (
                    out.polls.get(),
                    dl(&st).is_some_and(|x| x.store.interrupted),
                    out.held,
                    listed,
                    refused,
                )
            };
            let (granted, cut, _, listed, _) = ret(None);
            assert!(
                !cut && listed == n && granted >= 8,
                "mode {mode}: {granted}"
            );
            let mut partial = 0;
            for k in 0..granted {
                let (_, cut, held, listed, refused) = ret(Some(k));
                assert!(cut && held == 0, "mode {mode} at {k}");
                assert_eq!(
                    refused, 0,
                    "mode {mode} at {k}: reads asked of a stopped meter"
                );
                if listed > 0 && listed < n {
                    partial += 1;
                }
            }
            assert!(
                partial >= 3,
                "mode {mode}: {partial} stops inside the index"
            );
        }
    }

    #[test]
    fn pruning_reads_each_side_s_literals_and_ends_at_a_refused_read() {
        // Two sides of n literals each whose signatures differ, so that one comparison
        // decides them: the signatures and the tests of order read every literal.
        for n in [64, 512, 4096] {
            let side =
                |o: u32| -> Side { ((0..n as u32).map(|i| Lit::x(o + i)).collect(), Vec::new()) };
            let mut l = vec![side(0), side(1 << 20)];
            let mut reads = 0usize;
            assert!(super::super::dl_rules::prune_with(&mut l, &mut |k| {
                reads += k;
                true
            }));
            assert_eq!(l.len(), 2, "{n}");
            assert!(reads >= 4 * n, "{n}: {reads} reads");
            let mut l = vec![side(0), side(1 << 20)];
            let mut asked = 0usize;
            assert!(!super::super::dl_rules::prune_with(&mut l, &mut |_| {
                asked += 1;
                false
            }));
            assert_eq!(asked, 1, "{n}: a refused read ends the pruning");
        }
    }

    #[test]
    fn a_late_premise_s_replay_checks_the_bounds_at_a_fixed_count_of_reads() {
        for mode in MODES {
            meter_grows(
                "returns per late premise",
                METER_CHECK,
                [512, 1024, 2048, 4096],
                |n| {
                    let late = late_replay(mode, n, 2, true);
                    assert!(!late.interrupted && late.held == 0, "{late:?}");
                    (2 * n, late.polls)
                },
            );
        }
    }

    #[test]
    fn the_index_of_an_offered_clause_checks_the_bounds_at_a_fixed_count_of_reads() {
        for mode in MODES {
            let offer = |holders: usize, body: usize| {
                let mut sig = Sig::new();
                let h = sig.class("H");
                let atoms: Vec<Ex> = (0..body).map(|i| sig.class(&format!("A{i}"))).collect();
                let u = sig.ind("u");
                let (ix, _) = probe_ix(sig);
                let mut run = DlRun::new(false, Default::default());
                run.mrepair = mode;
                let ls: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
                let c = Clause::new(ls.clone(), vec![glit(&run, &ix, h, u)]);
                let mut st = State::default();
                let id = ds(&mut st).store.clauses.len() as u32;
                assert!(ds(&mut st).store.add(c.clone()).is_some());
                let mut out = Stops::new(&run);
                let r = rst(&mut st, &out);
                mrep_new(r, &out);
                let hs: Vec<Ex> = (0..holders as Ex).map(|i| 1000 + i).collect();
                for &l in &ls {
                    r.mw().holders.insert(l, hs.clone());
                    for &e in &hs {
                        r.mw().hold_set.insert((e, l));
                    }
                }
                m_offer(&ix, &mut st, id, &c, &mut out);
                assert!(!dl(&st).unwrap().store.interrupted);
                let sent = out
                    .sent
                    .iter()
                    .filter(|(_, c)| matches!(c, Concl::RRet(_)))
                    .count();
                assert_eq!(sent, holders);
                out.polls.get()
            };
            meter_grows("holders", METER_CHECK, [512, 1024, 2048, 4096], |n| {
                (4 * n, offer(n, 4))
            });
            meter_grows("offered body", METER_CHECK, [512, 1024, 2048, 4096], |n| {
                (3 * n, offer(1, n))
            });
        }
    }

    #[test]
    fn the_diagnostic_s_redundancy_test_checks_the_bounds_at_a_fixed_count_of_reads() {
        let held = |n: usize| {
            let mut sig = Sig::new();
            let [h, z] = ["H", "Z"].map(|n| sig.class(n));
            let ks: Vec<Ex> = (0..n).map(|i| sig.class(&format!("K{i}"))).collect();
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let run = DlRun::new(false, Default::default());
            let gh = glit(&run, &ix, h, u);
            let mut st = State::default();
            for &k in &ks {
                assert!(
                    ds(&mut st)
                        .store
                        .add(Clause::new(vec![Lit::x(k)], vec![gh]))
                        .is_some()
                );
            }
            let lists = [vec![(vec![Lit::x(z)], vec![])]];
            let polls = std::cell::Cell::new(0);
            let mut poll = |_| {
                polls.set(polls.get() + 1);
                true
            };
            let mut rd = Reads::new(&mut poll);
            let got = products_held(&ix, &st, &lists, &[gh], &run.lits, &mut rd);
            assert_eq!(got, Some(false), "no stored clause subsumes the product");
            assert!(rd.end());
            polls.get()
        };
        meter_grows(
            "stored clauses",
            METER_CHECK + WORK_READS,
            [1024, 2048, 4096, 8192],
            |n| (n, held(n)),
        );
    }

    #[test]
    fn the_rooted_rules_premise_lists_check_the_bounds_at_a_fixed_count_of_reads() {
        // Join's premises over a ground body literal, on the default path
        let join = |n: usize| {
            let mut sig = Sig::new();
            let [b, z] = ["B", "Z"].map(|n| sig.class(n));
            let ks: Vec<Ex> = (0..n).map(|i| sig.class(&format!("K{i}"))).collect();
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let run = DlRun::new(false, Default::default());
            let gb = glit(&run, &ix, b, u);
            let mut st = State::default();
            for &k in &ks {
                let id = ds(&mut st).store.clauses.len() as u32;
                assert!(
                    ds(&mut st)
                        .store
                        .add(Clause::new(vec![Lit::x(k)], vec![gb]))
                        .is_some()
                );
                ds(&mut st).store.by_xl.entry(gb).or_default().push(id);
            }
            let target = Clause::new(vec![gb], vec![Lit::x(z)]);
            let mut out = Stops::new(&run);
            let mut guard = Guard::new(&mut out);
            let mut found = Found::default();
            assert!(join_body(&ix, &st, &target, &mut found, &mut guard));
            assert_eq!(found.one_sides().len(), n);
            drop(found);
            drop(guard);
            out.polls.get()
        };
        meter_grows(
            "Join's premises",
            METER_CHECK,
            [1024, 2048, 4096, 8192],
            |n| (n, join(n)),
        );
        // a given clause of n ground body literals with no premise in the store
        meter_grows("Join's body", METER_CHECK, [1024, 2048, 4096, 8192], |n| {
            let mut sig = Sig::new();
            let z = sig.class("Z");
            let bs: Vec<Ex> = (0..n).map(|i| sig.class(&format!("B{i}"))).collect();
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let run = DlRun::new(false, Default::default());
            let body: Vec<Lit> = bs.iter().map(|&b| glit(&run, &ix, b, u)).collect();
            let mut st = State::default();
            let _ = ds(&mut st);
            let target = Clause::new(body, vec![Lit::x(z)]);
            let mut out = Stops::new(&run);
            let mut found = Found::default();
            {
                let mut guard = Guard::new(&mut out);
                assert!(join_body(&ix, &st, &target, &mut found, &mut guard));
            }
            assert!(found.one_sides().is_empty());
            (n, out.polls.get())
        });
    }

    /// A context in which the literal `A(u)` has `n` stored premises `K_i(x) → A(u)`, none a
    /// fact, the literal and the constant u.
    fn wide_index(n: usize) -> (Index, DlRun, State, Ex, Ex, u32, Lit) {
        let mut sig = Sig::new();
        let [a, d, h] = ["A", "D", "H"].map(|n| sig.class(n));
        let ks: Vec<Ex> = (0..n).map(|i| sig.class(&format!("K{i}"))).collect();
        let u = sig.ind("u");
        sig.dl.clauses.push((vec![d, a], vec![h]));
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let ga = glit(&run, &ix, a, u);
        let mut st = State::default();
        for &k in &ks {
            let id = ds(&mut st).store.clauses.len() as u32;
            assert!(
                ds(&mut st)
                    .store
                    .add(Clause::new(vec![Lit::x(k)], vec![ga]))
                    .is_some()
            );
            ds(&mut st).store.by_xl.entry(ga).or_default().push(id);
        }
        let RTerm::C(cu) = c(u, &ix) else {
            unreachable!()
        };
        (ix, run, st, a, d, cu, ga)
    }

    #[test]
    fn a_refused_read_ends_the_scan_that_made_it() {
        use super::super::dl_rules::meter_probe;
        // The search for a fact of A(u) over a wide index of premises that are no facts: a
        // refusal at any check returns at once, with no read asked of the stopped meter and
        // no entry read past the check.
        let n = 4 * METER_CHECK + 100;
        let (ix, run, st, a, _, u, _) = wide_index(n);
        let fact = |after: Option<usize>| {
            let mut out = Stops::new(&run);
            if let Some(k) = after {
                out.refuse = true;
                out.after = k;
            }
            let _ = meter_probe::take();
            let meter = Meter::checks();
            let got = has_fact_at(&ix, &st, &out, &meter, a, u);
            let refused = meter_probe::take_refused();
            let (reads, _, _) = meter_probe::take();
            (got, meter.stopped(), refused, reads, out.polls.get())
        };
        let (got, stopped, refused, reads, granted) = fact(None);
        assert!(!got && !stopped && refused == 0 && reads == n, "{reads}");
        assert_eq!(granted, n / METER_CHECK);
        for k in 0..granted {
            let (got, stopped, refused, reads, _) = fact(Some(k));
            assert!(!got && stopped, "at {k}");
            assert_eq!(refused, 0, "at {k}: reads asked of a stopped meter");
            assert_eq!(reads, (k + 1) * METER_CHECK, "at {k}");
        }
        // The index of a wide offered clause: a refusal while the clause is indexed leaves
        // the rest of its body unindexed, sends nothing and holds nothing.
        for mode in MODES {
            let n = 4 * METER_CHECK;
            let offer = |after: Option<usize>| {
                let mut sig = Sig::new();
                let h = sig.class("H");
                let atoms: Vec<Ex> = (0..n).map(|i| sig.class(&format!("A{i}"))).collect();
                let u = sig.ind("u");
                let (ix, _) = probe_ix(sig);
                let mut run = DlRun::new(false, Default::default());
                run.mrepair = mode;
                let ls: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
                let c = Clause::new(ls.clone(), vec![glit(&run, &ix, h, u)]);
                let mut st = State::default();
                let id = ds(&mut st).store.clauses.len() as u32;
                assert!(ds(&mut st).store.add(c.clone()).is_some());
                let mut out = Stops::new(&run);
                let r = rst(&mut st, &out);
                mrep_new(r, &out);
                for &l in &ls {
                    r.mw().holders.insert(l, vec![1000]);
                    r.mw().hold_set.insert((1000, l));
                }
                if let Some(k) = after {
                    out.refuse = true;
                    out.after = k;
                }
                let _ = meter_probe::take();
                m_offer(&ix, &mut st, id, &c, &mut out);
                let refused = meter_probe::take_refused();
                let indexed = rst(&mut st, &out)
                    .mr()
                    .gbody
                    .values()
                    .filter(|v| v.contains(&id))
                    .count();
                (
                    out.polls.get(),
                    out.sent.len(),
                    dl(&st).unwrap().store.interrupted,
                    out.held,
                    indexed,
                    refused,
                )
            };
            let (granted, sent, cut, _, indexed, _) = offer(None);
            assert!(!cut && sent == 1 && indexed == n, "mode {mode}: {granted}");
            let mut partial = 0;
            for k in 0..granted {
                let (_, sent, cut, held, indexed, refused) = offer(Some(k));
                assert!(cut && held == 0, "mode {mode} at {k}");
                assert_eq!(
                    refused, 0,
                    "mode {mode} at {k}: reads asked of a stopped meter"
                );
                if indexed > 0 && indexed < n {
                    partial += 1;
                    assert_eq!(sent, 0, "mode {mode} at {k}");
                }
            }
            assert!(
                partial >= 3,
                "mode {mode}: {partial} stops inside the index"
            );
        }
    }

    #[test]
    fn an_initial_return_checks_the_bounds_in_full_before_it_derives() {
        // One premise of one body literal makes no check on the meter's cadence, and a sink
        // that grants memory without the full check, as the production sink does, would
        // let the return derive after a stop that was already asked for.
        for mode in MODES {
            let (ix, run, mut st, cid, _, _) = multi_list_return(mode, &[1]);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            out.grant = true;
            assert!(rret(&ix, &mut st, cid, &mut out), "mode {mode}");
            assert_eq!(out.polls.get(), 1, "mode {mode}: one full check");
            assert!(
                dl(&st).unwrap().store.clauses.len() > stored,
                "mode {mode}: the return derives"
            );
            let (ix, run, mut st, cid, _, _) = multi_list_return(mode, &[1]);
            let mut out = Stops::new(&run);
            out.grant = true;
            out.refuse = true;
            assert!(rret(&ix, &mut st, cid, &mut out), "mode {mode}");
            assert!(out.polls.get() >= 1 && out.held == 0, "mode {mode}");
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len(),
                stored,
                "mode {mode}: nothing derived after the stop"
            );
        }
    }

    #[test]
    fn the_meters_of_an_activation_share_one_cadence_through_its_guards() {
        // A check-only meter and a repair's meter read through guarded views of one sink
        // that keeps the activation's count: the check falls on the 1,024th read of the
        // two together, and the repair's charge is a conclusion for every 16 of its own
        // reads, whatever the cadence's phase.
        let run = DlRun::new(false, Default::default());
        let mut out = Stops::new(&run);
        out.cadence = Some(std::cell::Cell::new(0));
        let work = run.work();
        let a = Meter::checks();
        let b = Meter::new(Bill::Repair);
        {
            let g = Guard::new(&mut out);
            for _ in 0..1000 {
                assert!(a.read(1, &g));
            }
        }
        assert_eq!(out.polls.get(), 0);
        {
            let mut g = Guard::new(&mut out);
            for _ in 0..23 {
                assert!(b.read_mut(1, &mut g));
            }
        }
        assert_eq!(out.polls.get(), 0, "1,023 reads make no check");
        {
            let mut g = Guard::new(&mut out);
            assert!(b.read_mut(1, &mut g));
        }
        assert_eq!(out.polls.get(), 1, "the 1,024th read checks");
        assert_eq!(
            run.work() - work,
            1,
            "24 of the repair's reads charged at the check"
        );
        {
            let mut g = Guard::new(&mut out);
            for _ in 0..76 {
                assert!(b.read_mut(1, &mut g));
            }
            b.end(&mut g);
        }
        assert_eq!(
            run.work() - work,
            100 / WORK_READS,
            "the repair's 100 reads"
        );
        assert_eq!(out.cadence.as_ref().unwrap().get(), 76);
        // a wide read is split at each check and carries the rest to the next
        let _ = super::super::dl_rules::meter_probe::take();
        {
            let g = Guard::new(&mut out);
            assert!(a.read(METER_CHECK - 76, &g));
            assert!(a.read(2 * METER_CHECK + 5, &g));
        }
        let (_, checks, most) = super::super::dl_rules::meter_probe::take();
        assert_eq!((out.polls.get(), checks), (4, 3));
        assert_eq!(most, METER_CHECK);
        assert_eq!(out.cadence.as_ref().unwrap().get(), 5);
    }

    #[test]
    fn a_return_admits_the_sizes_of_its_lists_before_it_makes_them() {
        // A wide returned body whose first literal has no premise: the buffer of the lists'
        // sizes is admitted before it is made, so that a grant below it refuses the return
        // before the heap grows by it.
        for mode in MODES {
            let n = 1 << 14;
            let (ix, run, st, c) = meter_fixture(mode, n, 0, 1);
            let sizes = n * std::mem::size_of::<usize>();
            let mut out = Stops::new(&run);
            let mut found = Found::default();
            let meter = Meter::checks();
            let (go, heap) = super::super::numeric::tests::measure(|| {
                let mut g = Guard::new(&mut out);
                let go = ret_found(&ix, &st, &c, None, &meter, &mut found, &mut g);
                assert_eq!(g.bytes(), found.held_bytes(), "mode {mode}");
                go
            });
            assert!(go && found.products_len() == 0, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(out.peak >= sizes, "mode {mode}: charged {}", out.peak);
            assert!(
                heap.peak <= out.peak + 4096,
                "mode {mode}: the heap's {} past the charge of {}",
                heap.peak,
                out.peak
            );
            let mut out = Stops::new(&run);
            out.budget = Some(sizes / 2);
            let mut found = Found::default();
            let meter = Meter::checks();
            let (go, heap) = super::super::numeric::tests::measure(|| {
                let mut g = Guard::new(&mut out);
                let go = ret_found(&ix, &st, &c, None, &meter, &mut found, &mut g);
                assert_eq!(g.bytes(), found.held_bytes(), "mode {mode}");
                go
            });
            assert!(!go && found.products_len() == 0, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(
                heap.peak < sizes / 2,
                "mode {mode}: the heap grew {}",
                heap.peak
            );
        }
    }

    #[test]
    fn a_late_replay_keeps_m_return_s_rule_across_its_batches() {
        // The batch that a flush starts carries M-return's rule, so that the derivation
        // trace names it for every product of the replay.
        for mode in MODES {
            let (ix, run, mut st, cid, _, _) = multi_list_return(mode, &[1, 1]);
            let c = run.table.get(cid).clone();
            let mut out = Stops::new(&run);
            let mut guard = Guard::new(&mut out);
            let meter = Meter::checks();
            let mut batch = Found::default();
            batch.rule = "mret";
            assert!(ret_found(
                &ix, &st, &c, None, &meter, &mut batch, &mut guard
            ));
            assert_eq!(batch.product_rules(), ["mret"], "mode {mode}");
            assert!(m_flush(&ix, &mut st, &mut batch, &mut guard), "mode {mode}");
            assert_eq!(batch.rule, "mret", "mode {mode}");
            assert!(ret_found(
                &ix, &st, &c, None, &meter, &mut batch, &mut guard
            ));
            assert_eq!(batch.product_rules(), ["mret"], "mode {mode}");
        }
    }

    #[test]
    fn hyper_and_its_partners_check_the_bounds_at_a_fixed_count_of_reads() {
        // Hyper on D(u) with the ontology clause D ∧ A → H and `n` premises of A(u), and the
        // partners of A(u) that Hyper's conjunctions and disjointness sets push
        let prem: Side = (vec![Lit::x(0)], vec![]);
        let hyper = |n: usize, after: Option<usize>| {
            let (ix, run, mut st, _, d, u, _) = wide_index(n);
            let mut out = Stops::new(&run);
            if let Some(k) = after {
                out.refuse = true;
                out.after = k;
            }
            let mut found = Found::default();
            let go = {
                let mut g = Guard::new(&mut out);
                let go = hyper_at(&ix, &mut st, &mut g, d, u, &prem, &mut found, false);
                assert_eq!(g.bytes(), found.held_bytes());
                go
            };
            let refused = super::super::dl_rules::meter_probe::take_refused();
            (go, found.products_len(), out.polls.get(), out.held, refused)
        };
        let partners = |n: usize, after: Option<usize>| {
            let (ix, run, mut st, a, _, u, _) = wide_index(n);
            let mut out = Stops::new(&run);
            if let Some(k) = after {
                out.refuse = true;
                out.after = k;
            }
            let mut found = Found::default();
            let go = {
                let mut g = Guard::new(&mut out);
                let go = push_partners_admitted(
                    &ix, &mut st, &mut g, a, u, None, false, &prem, &mut found,
                );
                assert_eq!(g.bytes(), found.held_bytes());
                go
            };
            let refused = super::super::dl_rules::meter_probe::take_refused();
            (
                go,
                found.one_sides().len(),
                out.polls.get(),
                out.held,
                refused,
            )
        };
        meter_grows(
            "Hyper's premises",
            METER_CHECK,
            [1024, 2048, 4096, 8192],
            |n| {
                let (go, prods, polls, held, _) = hyper(n, None);
                assert!(go && prods == 1 && held == 0, "{n}");
                (3 * n, polls)
            },
        );
        meter_grows(
            "Hyper's partners",
            METER_CHECK,
            [1024, 2048, 4096, 8192],
            |n| {
                let (go, sides, polls, held, _) = partners(n, None);
                assert!(go && sides == n && held == 0, "{n}");
                (2 * n, polls)
            },
        );
        let n = 3 * METER_CHECK;
        let (_, _, granted, _, _) = hyper(n, None);
        for k in 0..granted {
            let (go, prods, _, held, refused) = hyper(n, Some(k));
            assert!(
                !go && prods == 0 && held == 0 && refused == 0,
                "Hyper at {k}"
            );
        }
        let (_, _, granted, _, _) = partners(n, None);
        for k in 0..granted {
            let (go, sides, _, held, refused) = partners(n, Some(k));
            assert!(
                !go && sides < n && held == 0 && refused == 0,
                "partners at {k}"
            );
        }
    }

    #[test]
    fn a_root_fact_s_offer_checks_the_bounds_at_a_fixed_count_of_reads() {
        // A root fact over the constant u with `n` contexts registered for u, and `h` head
        // literals over u
        let offer = |n: usize, h: usize, after: Option<usize>| {
            let mut sig = Sig::new();
            let atoms: Vec<Ex> = (0..h).map(|i| sig.class(&format!("A{i}"))).collect();
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let run = DlRun::new(false, Default::default());
            let ls: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
            let fact = Clause::new(vec![], ls);
            let RTerm::C(cu) = c(u, &ix) else {
                unreachable!()
            };
            let mut st = State::default();
            let id = ds(&mut st).store.clauses.len() as u32;
            assert!(ds(&mut st).store.add(fact.clone()).is_some());
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            r.edges.insert(cu, (0..n as Ex).map(|i| 1000 + i).collect());
            if let Some(k) = after {
                out.refuse = true;
                out.after = k;
            }
            let _ = super::super::dl_rules::meter_probe::take_refused();
            offer(&ix, &mut st, id, &fact, &mut out);
            let refused = super::super::dl_rules::meter_probe::take_refused();
            (
                out.polls.get(),
                out.sent.len(),
                dl(&st).unwrap().store.interrupted,
                out.held,
                refused,
            )
        };
        meter_grows(
            "registrations",
            METER_CHECK,
            [1024, 2048, 4096, 8192],
            |n| {
                let (polls, sent, cut, held, _) = offer(n, 1, None);
                assert!(sent == n && !cut && held == 0, "{n}");
                (n, polls)
            },
        );
        meter_grows("head", METER_CHECK, [1024, 2048, 4096, 8192], |n| {
            let (polls, sent, cut, held, _) = offer(1, n, None);
            assert!(sent == 1 && !cut && held == 0, "{n}");
            (n, polls)
        });
        let (granted, _, _, _, _) = offer(3 * METER_CHECK, 1, None);
        for k in 0..granted {
            let (_, sent, cut, held, refused) = offer(3 * METER_CHECK, 1, Some(k));
            assert!(cut && sent == 0 && held == 0 && refused == 0, "at {k}");
        }
    }

    #[test]
    fn m_return_s_late_replay_admits_its_lists_and_refuses_over_budget() {
        for mode in MODES {
            // 1. 3-literal returned clause A(u) ∧ B(u) ∧ C(u) → H(u) with 4 premises each
            // for B and C; late premise K(x) → A(u) yields a 1 × 4 × 4 product below fold's
            // internal charging thresholds.
            let setup = |counts: &[usize], extra_ret: bool| {
                let mut sig = Sig::new();
                let h = sig.class("H");
                let h2 = sig.class("H2");
                let late_k = sig.class("LateK");
                let atoms: Vec<Ex> = (0..counts.len())
                    .map(|i| sig.class(&format!("A{i}")))
                    .collect();
                let total: usize = counts.iter().sum();
                let ks: Vec<Ex> = (0..total).map(|i| sig.class(&format!("K{i}"))).collect();
                let u = sig.ind("u");
                let (ix, _) = probe_ix(sig);
                let mut run = DlRun::new(false, Default::default());
                run.mrepair = mode;
                let lits: Vec<Lit> = atoms.iter().map(|&a| glit(&run, &ix, a, u)).collect();
                let gh = glit(&run, &ix, h, u);
                let gh2 = glit(&run, &ix, h2, u);
                let mut st = State::default();
                let mut ki = 0usize;
                for (&l, &cnt) in lits.iter().zip(counts) {
                    for _ in 0..cnt {
                        let k = ks[ki];
                        ki += 1;
                        let id = ds(&mut st).store.clauses.len() as u32;
                        assert!(
                            ds(&mut st)
                                .store
                                .add(Clause::new(vec![Lit::x(k)], vec![l]))
                                .is_some()
                        );
                        ds(&mut st).store.by_xl.entry(l).or_default().push(id);
                    }
                }
                let mut ids = vec![run.table.push(Clause::new(lits.clone(), vec![gh]))];
                if extra_ret {
                    ids.push(run.table.push(Clause::new(lits.clone(), vec![gh2])));
                }
                {
                    let out = Stops::new(&run);
                    let r = rst(&mut st, &out);
                    mrep_new(r, &out);
                    r.mw().rets.insert(lits[0], ids);
                }
                let premise = Clause::new(vec![Lit::x(late_k)], vec![lits[0]]);
                let prem = side(&premise, lits[0]);
                (ix, run, st, lits[0], prem)
            };

            // The lists of one 1 × 4 × 4 late product, admitted exactly before they are
            // built: the guard holds what the product holds.
            let (ix, run, mut st, la, prem) = setup(&[0, 4, 4], false);
            let c0 = run
                .table
                .get(rst(&mut st, &Stops::new(&run)).mr().rets[&la][0])
                .clone();
            let held_one = {
                let mut probe = Found::default();
                let mut o = Stops::new(&run);
                let mut g = Guard::new(&mut o);
                let meter = Meter::checks();
                assert!(
                    ret_found(&ix, &st, &c0, Some((la, &prem)), &meter, &mut probe, &mut g),
                    "mode {mode}"
                );
                assert_eq!(probe.products_len(), 1, "mode {mode}");
                assert_eq!(g.bytes(), probe.held_bytes(), "mode {mode}");
                probe.held_bytes()
            };
            assert!(held_one > 0, "mode {mode}");

            // 1. A granted late replay admits the lists, derives 16 clauses, charges the
            // product's tables as they grow, and releases everything.
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            assert!(m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len() - stored,
                16,
                "mode {mode}"
            );
            assert!(!dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(out.peak > held_one, "mode {mode}: {} {held_one}", out.peak);
            let one_peak = out.peak;

            // 2. A budget below the lists refuses before the drain, derives nothing,
            // interrupts the store and holds nothing.
            let (ix, run, mut st, la, prem) = setup(&[0, 4, 4], false);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            out.budget = Some(held_one - 1);
            assert!(!m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len(),
                stored,
                "mode {mode}: nothing derived on refusal"
            );
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");

            // 3. Two returns in one batch hold both products' lists and one product's
            // tables at a time.
            let (ix, run, mut st, la, prem) = setup(&[0, 4, 4], true);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            assert!(m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert_eq!(
                dl(&st).unwrap().store.clauses.len() - stored,
                32,
                "mode {mode}"
            );
            assert!(
                out.peak > one_peak && out.peak < 2 * one_peak,
                "mode {mode}: {} against one product's {one_peak}",
                out.peak
            );
            assert_eq!(out.held, 0, "mode {mode}");

            // 4. Empty replay (missing partner list): the only reservation is the buffer
            // of the three lists' sizes, admitted before it is made, and the bounds are
            // checked by that admission and before the batch drains.
            let sizes = 3 * std::mem::size_of::<usize>();
            let (ix, run, mut st, la, prem) = setup(&[0, 4, 0], false);
            let mut out = Stops::new(&run);
            assert!(m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert_eq!(out.asked, sizes, "mode {mode}");
            assert_eq!(out.polls.get(), 2, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            let mut out = Stops::new(&run);
            out.refuse = true;
            assert!(!m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert_eq!(out.asked, sizes, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");

            // 5. A late product of 1 × 512^6 × 8 choices stops on its tables' charge within
            // the budget, without an overflow and before it derives anything.
            let (ix, run, mut st, la, prem) = setup(&[0, 512, 512, 512, 512, 512, 512, 8], false);
            let stored = dl(&st).unwrap().store.clauses.len();
            let mut out = Stops::new(&run);
            out.budget = Some(4 << 20);
            assert!(!m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert!(!out.overflow, "mode {mode}");
            assert!(out.peak < 16 << 20, "mode {mode}: {}", out.peak);
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(dl(&st).unwrap().store.clauses.len(), stored, "mode {mode}");

            // 6. Interruption during drain releases the batch's admission.
            let (ix, run, mut st, la, prem) = setup(&[0, 16, 17], false);
            let mut out = Stops::new(&run);
            out.refuse = true;
            // grant the lists' admission and m_flush's check, refuse fold's first charge
            out.after = 2;
            assert!(!m_late(&ix, &mut st, la, &prem, &mut out), "mode {mode}");
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            assert_eq!(
                out.held, 0,
                "mode {mode}: interrupted drain must release batch"
            );
        }
    }

    /// A root whose repair state lists 256 constants, each with 256 ground literals that
    /// the context `k` alone holds, for [`m_registered`]'s scratch.
    fn many_held(mode: u8) -> (Index, DlRun, State, Ex, Vec<u32>) {
        let mut sig = Sig::new();
        let k = sig.class("K");
        let atoms: Vec<Ex> = (0..256).map(|i| sig.class(&format!("S{i}"))).collect();
        let inds: Vec<Ex> = (0..256).map(|i| sig.ind(&format!("v{i}"))).collect();
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let mut st = State::default();
        let mut us = Vec::new();
        {
            let out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            for &ind in &inds {
                let RTerm::C(cu) = c(ind, &ix) else { panic!() };
                us.push(cu);
                let mut held = Vec::new();
                for &a in &atoms {
                    let l = glit(&run, &ix, a, ind);
                    held.push(l.xid());
                    r.mw().holders.insert(l, vec![k]);
                    r.mw().hold_set.insert((k, l));
                }
                r.mw().held_at.insert(cu, held);
            }
        }
        (ix, run, st, k, us)
    }

    #[test]
    fn the_repair_s_scratch_is_admitted_before_it_is_built() {
        // the replay of a context's registrations admits the pairs it lists before it lists
        // them, so a refusal stops it before the scratch is built, and with every check
        // passed it charges and polls the scan of pairs it sends nothing for
        for mode in MODES {
            let (_ix, run, mut st, k, us) = many_held(mode);
            let mut out = Stops::new(&run);
            out.refuse = true;
            let ((), alloc) = super::super::numeric::tests::measure(|| {
                m_registered(ds(&mut st), k, &us, &mut out)
            });
            assert!(out.stopped() && out.asked >= 65_536 * 8, "mode {mode}");
            assert!(alloc.peak < 65_536, "mode {mode}: {alloc:?}");
            assert!(
                ds(&mut st).store.interrupted && out.held == 0,
                "mode {mode}"
            );
            let (_ix, run, mut st, k, us) = many_held(mode);
            let mut out = Stops::new(&run);
            let work = run.work();
            m_registered(ds(&mut st), k, &us, &mut out);
            assert!(out.sent.is_empty() && out.held == 0, "mode {mode}");
            assert!(out.asked >= 65_536 * 8, "mode {mode}");
            assert!(run.work() - work >= 65_536 / 16 - 1, "mode {mode}");
            assert!(out.polls.get() >= 65_536 / M_SCAN, "mode {mode}");
        }
    }

    #[test]
    fn the_repair_s_scans_without_output_are_charged_and_polled() {
        // a notice whose literal is in the bodies of 50,000 root clauses that the holder
        // holds no other literal of, and a root clause offered to the holders of its body,
        // which share none, each charge their reads and check the run's bounds
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, k] = ["A", "B", "K"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let heads: Vec<Ex> = (0..64).map(|i| sig.class(&format!("H{i}"))).collect();
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let mut st = State::default();
            let out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            ds(&mut st).store.add(Clause::new(
                vec![ga, gb],
                vec![glit(&run, &ix, heads[0], u)],
            ));
            let sample = ds(&mut st).store.clauses[0].clone();
            for _ in 1..50_000 {
                ds(&mut st).store.clauses.push(sample.clone());
            }
            let r = ds(&mut st).root.as_deref_mut().unwrap();
            r.mw().gbody.insert(ga, (0..50_000).collect());
            let mut out = Stops::new(&run);
            let work = run.work();
            assert!(rmark(&ix, &mut st, k, ga.xid(), &mut out), "mode {mode}");
            let notice = run.work() - work;
            assert!(notice >= 50_000 / 16 - 1, "mode {mode}: {notice}");
            assert!(out.polls.get() >= 50_000 / M_SCAN, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(
                out.asked >= 50_000 * 4,
                "mode {mode}: the returns' ids admitted"
            );
            // two body literals held by 50,000 contexts each, none by both
            let r = ds(&mut st).root.as_deref_mut().unwrap();
            r.mw().holders.insert(ga, (100..50_100).collect());
            r.mw().holders.insert(gb, (60_000..110_000).collect());
            let c = Clause::new(vec![ga, gb], vec![glit(&run, &ix, heads[1], u)]);
            let mut out = Stops::new(&run);
            let work = run.work();
            m_offer(&ix, &mut st, 1, &c, &mut out);
            assert!(run.work() - work >= 50_000 / 16 - 1, "mode {mode}");
            assert!(out.polls.get() >= 1 && out.sent.is_empty(), "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            // with one of them held once, the offer reads the shorter list
            let r = ds(&mut st).root.as_deref_mut().unwrap();
            r.mw().holders.insert(gb, vec![60_000]);
            let mut out = Stops::new(&run);
            let work = run.work();
            m_offer(&ix, &mut st, 2, &c, &mut out);
            assert!(run.work() - work <= 1, "mode {mode}: the shorter list");
        }
    }

    #[test]
    fn a_root_unit_is_found_by_its_literal_while_it_lives() {
        // the root's units are indexed by literal as the root processes them, so the test
        // reads no list of the root's facts, and a unit that died is no unit
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, cc] = ["A", "B", "C"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let gc = glit(&run, &ix, cc, u);
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            let mut st = State::default();
            let mut out = Stops::new(&run);
            mrep_new(rst(&mut st, &out), &out);
            let unit = Clause::new(vec![], vec![ga]);
            let two = Clause::new(vec![], vec![gb, gc]);
            ds(&mut st).store.add(unit.clone());
            ds(&mut st).store.add(two.clone());
            m_offer(&ix, &mut st, 0, &unit, &mut out);
            m_offer(&ix, &mut st, 1, &two, &mut out);
            // facts the index does not list are not read
            ds(&mut st)
                .root
                .as_deref_mut()
                .unwrap()
                .ready
                .insert(cu, vec![0, 1]);
            assert!(root_unit(dl(&st).unwrap(), ga), "mode {mode}");
            assert!(!root_unit(dl(&st).unwrap(), gb), "mode {mode}");
            ds(&mut st).store.clauses[0].dead = true;
            assert!(!root_unit(dl(&st).unwrap(), ga), "mode {mode}");
            let mut fresh = State::default();
            mrep_new(rst(&mut fresh, &out), &out);
            ds(&mut fresh).store.add(unit.clone());
            ds(&mut fresh)
                .root
                .as_deref_mut()
                .unwrap()
                .ready
                .insert(cu, vec![0]);
            assert!(
                !root_unit(dl(&fresh).unwrap(), ga),
                "mode {mode}: not indexed"
            );
        }
    }

    #[test]
    fn a_reach_charges_the_state_it_makes() {
        // the first reach of a context that has no state for Tier D makes it, and the run's
        // memory takes all of it
        for mode in MODES {
            let mut sig = Sig::new();
            let k = sig.class("K");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let mut out = Stops::new(&run);
            let mut st = State::default();
            let (before, held) = (run.bytes(), st.bytes());
            assert!(rreach(&ix, k, &mut st, TOP, &mut out), "mode {mode}");
            let (charged, made) = (run.bytes() - before, st.bytes() - held);
            assert!(
                charged >= made,
                "mode {mode}: charged {charged}, made {made}"
            );
        }
    }

    #[test]
    fn a_reach_replays_registrations_once_and_tells_a_successor_once() {
        // the registrations that wait for a context's reach go with it, and a reached
        // context tells each successor of its reach once, however often it meets it
        for mode in MODES {
            let mut sig = Sig::new();
            let k = sig.class("K");
            let (ix, root) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let mut out = Stops::new(&run);
            let mut st = State::default();
            register(&mut st, k, vec![0], &mut out);
            rreach(&ix, root, &mut st, k, &mut out);
            register(&mut st, k, vec![1], &mut out);
            let m = dl(&st).unwrap().root.as_deref().unwrap().mr();
            assert!(m.regs.get(&k).is_none_or(|v| v.is_empty()), "mode {mode}");
            let mut st = State::default();
            rreach(&ix, k, &mut st, TOP, &mut out);
            out.sent.clear();
            new_succ(&mut st, 77, &mut out);
            new_succ(&mut st, 77, &mut out);
            let n = out
                .sent
                .iter()
                .filter(|(_, c)| matches!(c, Concl::RReach(_)))
                .count();
            assert_eq!(n, 1, "mode {mode}");
        }
    }

    #[test]
    fn the_deficits_hold_promoted_facts_and_take_cf_only_with_the_repair() {
        // a product that derive sent the root as a fact is held, and the diagnostic reads
        // M-return-cf's premises only where M-return ran with them
        let mut sig = Sig::new();
        let [b, k] = ["B", "K"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let gb = glit(&run, &ix, b, u);
        let mut st = State::default();
        st.subs.insert(k);
        ds(&mut st).empty_core = true;
        let mut out = Stops::new(&run);
        super::super::dl_rules::derive(&ix, &mut st, (vec![], vec![gb]), &mut out);
        assert!(dl(&st).unwrap().promoted.contains(&[gb][..]));
        let lists = [vec![(vec![Lit::x(k)], vec![])]];
        let held = products_held(
            &ix,
            &st,
            &lists,
            &[gb],
            &run.lits,
            &mut Reads::new(&mut |_| true),
        );
        assert_eq!(held, Some(true));
        assert!(!marker_def_cf(MREPAIR_CF) && !marker_def_cf(0));
        assert!(marker_def_cf(MREPAIR_ON | MREPAIR_CF) && !marker_def_cf(MREPAIR_ON));
    }

    #[test]
    fn the_deficits_charge_and_poll_their_products() {
        // 256 premises on each of two literals give 65,536 products, all tautologies, which
        // the copy charges and polls as it goes, and a refusal of a poll ends it
        let mut sig = Sig::new();
        let [a, b, k, z] = ["A", "B", "K", "Z"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let hs: Vec<Ex> = (0..512).map(|i| sig.class(&format!("H{i}"))).collect();
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
        let mut st = State::default();
        for &h in &hs[..256] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![ga, Lit::x(z), Lit::x(h)]));
        }
        for &h in &hs[256..] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(z)], vec![gb, Lit::x(k), Lit::x(h)]));
        }
        let inv = inv_of(&ix, dix(&ix));
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        while ds(&mut st)
            .store
            .next(&ix, Some(&order), &mut max)
            .is_some()
        {}
        let m = MarkerCtx {
            me: k,
            held: vec![ga, gb],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        let rg = [Clause::new(vec![ga, gb], vec![])];
        let (mut polls, mut work) = (0, 0);
        let def = marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |n| {
            polls += 1;
            work += n;
            true
        });
        let def = def.unwrap().unwrap();
        assert_eq!((def.owed, def.unchecked), (0, 0));
        assert!(
            polls >= 65_536 / METER_CHECK && work >= 65_536,
            "{polls} {work}"
        );
        let cut = marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |_| false);
        assert!(cut.is_err());
    }

    /// Reports the failures that a test over [`MODES`] collected, so that one run shows
    /// every mode's.
    fn report(fails: &[String]) {
        assert!(fails.is_empty(), "{}", fails.join("\n"));
    }

    #[test]
    fn the_repair_s_scratch_stays_within_its_admission() {
        // the destinations of a notice, the returns of a notice and the holders of an
        // offered clause, 40,000 of each and all passing, are built in the bytes admitted
        // for them: what the handler allocates at its most is the admission and the state
        // it charged
        const N: u32 = 40_000;
        let mut fails = Vec::new();
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, k, hd] = ["A", "B", "K", "H"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let gh = glit(&run, &ix, hd, u);
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            let many: Vec<Ex> = (100_000..100_000 + N).collect();
            let within = |fails: &mut Vec<String>, what: &str, peak, out: &Stops, grown| {
                if peak > out.peak + grown + 4096 {
                    fails.push(format!(
                        "mode {mode} {what}: allocated {peak}, admitted {} and charged {grown}",
                        out.peak
                    ));
                }
            };
            // the destinations of the notice of ga, each sent the marker already
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.edges.insert(cu, many.clone());
            for &d in &many {
                r.mw().reach_set.insert(d);
                r.gsent.insert((d, ga));
            }
            let before = run.bytes();
            let (_, alloc) = super::super::numeric::tests::measure(|| {
                rmark(&ix, &mut st, k, ga.xid(), &mut out)
            });
            within(
                &mut fails,
                "destinations",
                alloc.peak,
                &out,
                run.bytes() - before,
            );
            // the returns of the notice of ga: N root clauses ga → H whose ids it sent
            let mut st = State::default();
            let mut out = Stops::new(&run);
            out.sent.reserve(N as usize + 16);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.mw().gbody.insert(ga, (0..N).collect());
            let mut one = Clause::new(vec![ga], vec![gh]);
            ds(&mut st).store.add(one.clone());
            ds(&mut st).store.clauses[0].sent = Some(0);
            let sample = ds(&mut st).store.clauses[0].clone();
            for _ in 1..N {
                ds(&mut st).store.clauses.push(sample.clone());
            }
            let before = run.bytes();
            let (_, alloc) = super::super::numeric::tests::measure(|| {
                rmark(&ix, &mut st, k, ga.xid(), &mut out)
            });
            within(
                &mut fails,
                "returns",
                alloc.peak,
                &out,
                run.bytes() - before,
            );
            if out.sent.len() != N as usize {
                fails.push(format!("mode {mode} returns: {} sent", out.sent.len()));
            }
            // the holders of ga and gb, N contexts that hold both, for the offer of ga ∧ gb
            let mut st = State::default();
            let mut out = Stops::new(&run);
            out.sent.reserve(N as usize + 16);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.mw().holders.insert(ga, many.clone());
            r.mw().holders.insert(gb, many.clone());
            for &h in &many {
                r.mw().hold_set.insert((h, ga));
                r.mw().hold_set.insert((h, gb));
            }
            one = Clause::new(vec![ga, gb], vec![gh]);
            ds(&mut st).store.add(one.clone());
            ds(&mut st).store.clauses[0].sent = Some(0);
            let before = run.bytes();
            let (_, alloc) =
                super::super::numeric::tests::measure(|| m_offer(&ix, &mut st, 0, &one, &mut out));
            within(
                &mut fails,
                "holders",
                alloc.peak,
                &out,
                run.bytes() - before,
            );
            if out.sent.len() != N as usize {
                fails.push(format!("mode {mode} holders: {} sent", out.sent.len()));
            }
        }
        report(&fails);
    }

    #[test]
    fn the_destination_scan_is_charged_and_polled() {
        // a notice over a constant that 200,000 contexts the root does not reach registered
        // for sends nothing, charges its reads, checks the run's bounds as it goes, and
        // stops at a refusal after its admission
        const N: u32 = 200_000;
        let mut fails = Vec::new();
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, k] = ["A", "K"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let ga = glit(&run, &ix, a, u);
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            for after in [usize::MAX, 1] {
                let mut st = State::default();
                let mut out = Stops::new(&run);
                let r = rst(&mut st, &out);
                mrep_new(r, &out);
                r.edges.insert(cu, (100_000..100_000 + N).collect());
                out.refuse = true;
                out.after = after;
                let work = run.work();
                rmark(&ix, &mut st, k, ga.xid(), &mut out);
                let (work, polls) = (run.work() - work, out.polls.get());
                let cut = ds(&mut st).store.interrupted;
                if !out.sent.is_empty() || out.held != 0 {
                    fails.push(format!("mode {mode} after {after}: sent or held"));
                }
                if after == 1 {
                    if !cut || polls != 2 {
                        fails.push(format!(
                            "mode {mode}: a refusal after the admission, interrupted {cut}, {polls} checks"
                        ));
                    }
                } else if cut || work < N as usize / WORK_READS - 1 || polls < N as usize / M_SCAN {
                    fails.push(format!(
                        "mode {mode}: {work} work and {polls} checks for {N} reads"
                    ));
                }
            }
        }
        report(&fails);
    }

    #[test]
    fn the_body_checks_are_charged_per_literal() {
        // 256 root clauses with the same 2,048 ground body literals, of which the holder
        // holds all but the last in the body's order, and an offer of the clause to 256
        // such holders: each check of a body literal counts as a read
        const B: usize = 2048;
        const H: u32 = 256;
        let reads = H as usize * (B - 1);
        let mut fails = Vec::new();
        for mode in MODES {
            let mut sig = Sig::new();
            let [k, hd] = ["K", "H"].map(|n| sig.class(n));
            let ss: Vec<Ex> = (0..B).map(|i| sig.class(&format!("S{i}"))).collect();
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let body: Vec<Lit> = ss.iter().map(|&s| glit(&run, &ix, s, u)).collect();
            let one = Clause::new(body, vec![glit(&run, &ix, hd, u)]);
            let (&l, &last) = (one.body().first().unwrap(), one.body().last().unwrap());
            // the notice of the body's first literal
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.mw().gbody.insert(l, (0..H).collect());
            for &m in &one.body()[1..B - 1] {
                r.mw().hold_set.insert((k, m));
            }
            for _ in 0..H {
                ds(&mut st).store.add(one.clone());
            }
            let sample = ds(&mut st).store.clauses[0].clone();
            ds(&mut st).store.clauses.resize(H as usize, sample);
            let work = run.work();
            rmark(&ix, &mut st, k, l.xid(), &mut out);
            let (work, polls) = (run.work() - work, out.polls.get());
            if !out.sent.is_empty() || work < reads / WORK_READS - 1 || polls < reads / M_SCAN {
                fails.push(format!(
                    "mode {mode} notice: {work} work, {polls} checks, {} sent",
                    out.sent.len()
                ));
            }
            // the offer to 256 holders of every body literal but the last
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            let hs: Vec<Ex> = (100_000..100_000 + H).collect();
            for &m in one.body() {
                r.mw().holders.insert(m, hs.clone());
                if m != last {
                    for &h in &hs {
                        r.mw().hold_set.insert((h, m));
                    }
                }
            }
            ds(&mut st).store.add(one.clone());
            let work = run.work();
            m_offer(&ix, &mut st, 0, &one, &mut out);
            let (work, polls) = (run.work() - work, out.polls.get());
            if !out.sent.is_empty() || work < reads / WORK_READS - 1 || polls < reads / M_SCAN {
                fails.push(format!(
                    "mode {mode} offer: {work} work, {polls} checks, {} sent",
                    out.sent.len()
                ));
            }
        }
        report(&fails);
    }

    #[test]
    fn the_deficit_count_polls_within_a_context() {
        // one holder of 4,096 ground literals over a constant that 256 contexts the root
        // reaches registered for, none of which holds a marker: the count charges the
        // 1,048,576 pairs it checks and polls among them, and a refusal of the first poll
        // inside the holder ends it
        const L: usize = 4096;
        const D: u32 = 256;
        let mut sig = Sig::new();
        let k = sig.class("K");
        let ss: Vec<Ex> = (0..L).map(|i| sig.class(&format!("S{i}"))).collect();
        let u = sig.ind("u");
        let (ix, root) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let RTerm::C(cu) = c(u, &ix) else { panic!() };
        let dests: Vec<Ex> = (100_000..100_000 + D).collect();
        let ctx = |me: Ex| MarkerCtx {
            me,
            held: vec![],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        let mut cs = vec![MarkerCtx {
            succ: dests.iter().copied().chain([k]).collect(),
            edges: vec![(cu, dests.clone())],
            ..ctx(root)
        }];
        cs.extend(dests.iter().map(|&d| ctx(d)));
        cs.push(MarkerCtx {
            held: ss.iter().map(|&s| glit(&run, &ix, s, u)).collect(),
            ..ctx(k)
        });
        let def = || MarkerDef {
            bottom: false,
            marks: vec![],
            owed: 0,
            unchecked: 0,
        };
        let defs: Vec<Option<MarkerDef>> = cs.iter().map(|_| Some(def())).collect();
        let (mut polls, mut work) = (0, 0);
        let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |w| {
            polls += 1;
            work += w;
            true
        });
        let pairs = L * D as usize;
        assert_eq!(n, Some([pairs, 0, 0]));
        assert!(
            work >= pairs / WORK_READS && polls >= pairs / METER_CHECK,
            "{work} work, {polls} checks"
        );
        // a check per context passes, and the first one inside the holder refuses
        let mut calls = 0;
        let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
            calls += 1;
            calls <= cs.len()
        });
        assert_eq!((n, calls), (None, cs.len() + 1));
    }

    /// A context K holding a(u) and b(u), with `na` premises K → a(u) ∨ Z ∨ H_i and `nb`
    /// premises Z → b(u) ∨ K ∨ H_j, whose products with the root clause a(u) ∧ b(u) → ⊥ are
    /// all tautologies, and `dead` further premises on a(u) that died, for the diagnostic.
    fn def_ctx(na: usize, nb: usize, dead: usize) -> (Index, DlRun, State, MarkerCtx, Clause) {
        let mut sig = Sig::new();
        let [a, b, k, z] = ["A", "B", "K", "Z"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let hs: Vec<Ex> = (0..na + nb + dead)
            .map(|i| sig.class(&format!("H{i}")))
            .collect();
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
        let mut st = State::default();
        for &h in &hs[..na] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![ga, Lit::x(z), Lit::x(h)]));
        }
        for &h in &hs[na..na + nb] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(z)], vec![gb, Lit::x(k), Lit::x(h)]));
        }
        for &h in &hs[na + nb..] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![ga, Lit::x(h)]));
        }
        let inv = inv_of(&ix, dix(&ix));
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        while ds(&mut st)
            .store
            .next(&ix, Some(&order), &mut max)
            .is_some()
        {}
        let n = ds(&mut st).store.clauses.len();
        for s in &mut ds(&mut st).store.clauses[n - dead..] {
            s.dead = true;
        }
        let m = MarkerCtx {
            me: k,
            held: vec![ga, gb],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        (ix, run, st, m, Clause::new(vec![ga, gb], vec![]))
    }

    #[test]
    fn the_diagnostic_charges_what_it_reads_where_it_reads_it() {
        // 63 x 65 tautological products, short of a multiple of the poll, charge every
        // choice; 8,192 dead premises charge their reads though no list keeps them; a
        // holder of 100,000 literals is read under the run's checks
        let (ix, run, st, m, rg) = def_ctx(63, 65, 0);
        let mut work = 0;
        let def = marker_def(&ix, &m, &st, &[rg], &run.lits, false, &mut |n| {
            work += n;
            true
        });
        assert_eq!(
            def.map(|d| d.map(|d| (d.owed, d.unchecked))),
            Ok(Some((0, 0)))
        );
        assert!(work >= 63 * 65, "{work} work for {} products", 63 * 65);
        // the dead premises are read twice, as stored clauses and as candidates
        let read = |dead| {
            let (ix, run, st, m, rg) = def_ctx(1, 1, dead);
            let mut work = 0;
            let def = marker_def(&ix, &m, &st, &[rg], &run.lits, false, &mut |n| {
                work += n;
                true
            });
            assert!(def.is_ok_and(|d| d.is_some()));
            work
        };
        let (none, dead) = (read(0), read(8192));
        assert!(
            dead - none >= 2 * 8192 / WORK_READS - 16,
            "{} work for 8,192 dead premises",
            dead - none
        );
        let (ix, run, st, mut m, rg) = def_ctx(1, 1, 0);
        m.held.extend((0..100_000).map(Lit::xl));
        let mut polls = 0;
        let def = marker_def(&ix, &m, &st, &[rg], &run.lits, false, &mut |_| {
            polls += 1;
            true
        });
        assert!(def.is_ok_and(|d| d.is_some()));
        assert!(
            polls >= 100_000 / METER_CHECK,
            "{polls} checks for 100,000 held literals"
        );
    }

    #[test]
    fn a_refusal_at_any_check_of_the_diagnostic_ends_it() {
        // for each check k of a complete count, a run whose check k refuses gets no count
        let (ix, run, st, m, rg) = def_ctx(16, 16, 64);
        let rg = [rg];
        let mut total = 0;
        assert!(
            marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |_| {
                total += 1;
                true
            })
            .is_ok_and(|d| d.is_some())
        );
        for k in 1..=total {
            let mut calls = 0;
            let def = marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |_| {
                calls += 1;
                calls < k
            });
            assert!(def.is_err(), "marker_def, check {k} of {total}");
        }
        // the count over a holder of 64 literals and 64 destinations
        let mut sig = Sig::new();
        let k = sig.class("K");
        let ss: Vec<Ex> = (0..64).map(|i| sig.class(&format!("S{i}"))).collect();
        let u = sig.ind("u");
        let (ix, root) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let RTerm::C(cu) = c(u, &ix) else { panic!() };
        let dests: Vec<Ex> = (100_000..100_064).collect();
        let ctx = |me: Ex| MarkerCtx {
            me,
            held: vec![],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        let mut cs = vec![MarkerCtx {
            succ: dests.iter().copied().chain([k]).collect(),
            edges: vec![(cu, dests.clone())],
            ..ctx(root)
        }];
        cs.extend(dests.iter().map(|&d| ctx(d)));
        cs.push(MarkerCtx {
            held: ss.iter().map(|&s| glit(&run, &ix, s, u)).collect(),
            ..ctx(k)
        });
        let defs: Vec<Option<MarkerDef>> = cs
            .iter()
            .map(|_| {
                Some(MarkerDef {
                    bottom: false,
                    marks: vec![],
                    owed: 0,
                    unchecked: 0,
                })
            })
            .collect();
        let mut total = 0;
        assert_eq!(
            marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
                total += 1;
                true
            }),
            Some([64 * 64, 0, 0])
        );
        for k in 1..=total {
            let mut calls = 0;
            let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
                calls += 1;
                calls < k
            });
            assert_eq!(n, None, "marker_deficits, check {k} of {total}");
        }
    }

    #[test]
    fn a_replay_over_many_constants_checks_the_bounds() {
        // a registration replay for 200,000 constants that hold no literal reads each, so a
        // stopped run's check ends it before any admission
        for mode in MODES {
            let (_ix, run, mut st, k, _) = many_held(mode);
            let new: Vec<u32> = (1_000_000..1_200_000).collect();
            let mut out = Stops::new(&run);
            let work = run.work();
            m_registered(ds(&mut st), k, &new, &mut out);
            assert!(
                out.polls.get() >= 200_000 / M_SCAN,
                "mode {mode}: {}",
                out.polls.get()
            );
            assert!(run.work() - work >= 200_000 / WORK_READS - 1, "mode {mode}");
            let (_ix, run, mut st, k, _) = many_held(mode);
            let mut out = Stops::new(&run);
            out.refuse = true;
            m_registered(ds(&mut st), k, &new, &mut out);
            assert!(out.asked == 0 && out.held == 0, "mode {mode}");
            assert!(ds(&mut st).store.interrupted, "mode {mode}");
        }
    }

    #[test]
    fn the_deficit_count_reads_each_successor_and_copy() {
        // a root's 50,000 successor entries, and 50,000 copies to total, are read one by one
        // under the run's checks
        let sig = Sig::new();
        let (ix, root) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let none = |me: Ex, succ: Vec<Ex>| MarkerCtx {
            me,
            held: vec![],
            succ,
            edges: vec![],
            units: vec![],
        };
        let def = || {
            Some(MarkerDef {
                bottom: false,
                marks: vec![],
                owed: 1,
                unchecked: 0,
            })
        };
        for (succ, copies) in [(50_000, 1), (0, 50_000)] {
            // one successor listed 50,000 times, which the reach meets once
            let cs = [none(root, std::iter::repeat_n(100_000, succ).collect())];
            let defs: Vec<Option<MarkerDef>> = (0..copies).map(|_| def()).collect();
            let mut polls = 0;
            let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
                polls += 1;
                true
            });
            assert_eq!(n, Some([0, copies as usize, 0]));
            assert!(
                polls >= 50_000 / METER_CHECK,
                "{polls} checks for {succ} successors and {copies} copies"
            );
        }
    }

    /// A small signature: classes, individuals and roles by name.
    struct Sig {
        it: Interner,
        ax: Axioms,
        dl: DlAx,
    }

    impl Sig {
        fn new() -> Sig {
            Sig {
                it: Interner::new(),
                ax: Axioms::default(),
                dl: DlAx::default(),
            }
        }
        fn class(&mut self, n: &str) -> Ex {
            self.it.class(&Class::new(format!("urn:t:{n}")))
        }
        fn ind(&mut self, n: &str) -> Ex {
            self.it.nominal(&NamedIndividual::new(format!("urn:t:{n}")))
        }
        fn role(&mut self, n: &str) -> Role {
            self.it
                .role(RoleName::Object(ObjectProperty::new(format!("urn:t:{n}"))))
        }
        fn some(&mut self, r: Role, c: Ex) -> Ex {
            self.it.node(Node::Exist(r, c))
        }
        /// An existential restriction with its hypothetical successor, as normalization
        /// gives every existential restriction of a run.
        fn some_h(&mut self, r: Role, c: Ex) -> Ex {
            let e = self.some(r, c);
            let h = self.it.fresh_class();
            self.dl.hyp.insert(e, h);
            e
        }
        fn all(&mut self, r: Role, c: Ex) -> Ex {
            self.it.node(Node::Forall(r, c))
        }
        fn sub(&mut self, a: Ex, b: Ex) {
            self.ax.subs.push((a, b));
        }
    }

    /// The state after a saturation of one context, the root unless the test names
    /// another, as `root` here.
    struct Done {
        ix: Index,
        ctx: Contexts,
        root: Ex,
        run: DlRun,
    }

    impl Done {
        fn bottom(&self) -> bool {
            self.ctx[self.root as usize].get().unwrap().st.lock().bottom
        }
        /// The root's live clauses, each as its body and head in general form.
        fn clauses(&self) -> Vec<(Vec<XLit>, Vec<XLit>)> {
            self.clauses_of(self.root)
        }
        /// The live clauses of the context of `at`.
        fn clauses_of(&self, at: Ex) -> Vec<(Vec<XLit>, Vec<XLit>)> {
            let dx = dix(&self.ix);
            let inv = inv_of(&self.ix, dx);
            let Some(c) = self.ctx[at as usize].get() else {
                return Vec::new();
            };
            let st = c.st.lock();
            let Some(d) = dl(&st) else { return Vec::new() };
            d.store
                .clauses
                .iter()
                .filter(|s| !s.dead)
                .map(|s| {
                    let v = |ls: &[Lit]| ls.iter().map(|&l| self.run.lits.view(l, inv)).collect();
                    (v(s.c.body()), v(s.c.head()))
                })
                .collect()
        }
        /// Whether the root holds the clause with an empty body and this head, or one that
        /// subsumes it.
        fn has(&self, head: &[XLit]) -> bool {
            self.clauses()
                .iter()
                .any(|(b, h)| b.is_empty() && h.iter().all(|l| head.contains(l)))
        }
        /// Whether the context holds the unit `a(x)`.
        fn subs(&self, a: Ex) -> bool {
            self.ctx[self.root as usize]
                .get()
                .unwrap()
                .st
                .lock()
                .subs
                .contains(&a)
        }
    }

    /// In `before`, the root, which [`saturate_after`] makes.
    const ROOT: Ex = Ex::MAX;

    fn saturate_root(s: Sig) -> Done {
        saturate_at(s, None)
    }

    /// Saturates the context of `at`, or of the root.
    fn saturate_at(s: Sig, at: Option<Ex>) -> Done {
        saturate_seeded(s, at, |_, _| Vec::new())
    }

    /// Saturates as [`saturate_at`] does, with the root's store holding the clauses of
    /// `seed`, as body and head in general form, in place of its start.
    fn saturate_seeded(
        s: Sig,
        at: Option<Ex>,
        seed: impl FnOnce(&Index, &mut Interner) -> Vec<(Vec<XLit>, Vec<XLit>)>,
    ) -> Done {
        saturate_after(s, &[], at, seed)
    }

    /// Saturates as [`saturate_seeded`] does, after saturating the contexts of `before`
    /// one at a time.
    fn saturate_after(
        mut s: Sig,
        before: &[Ex],
        at: Option<Ex>,
        seed: impl FnOnce(&Index, &mut Interner) -> Vec<(Vec<XLit>, Vec<XLit>)>,
    ) -> Done {
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        for set in &mut s.ax.disjoint {
            set.sort_unstable();
        }
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let ctx: Contexts = (0..s.it.nodes.len()).map(|_| OnceLock::new()).collect();
        let run = DlRun::new(false, Default::default());
        let bounds = Bounds {
            threads: 1,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            base_queued: 0,
            peak_memory: 0,
            paid_memory: 0,
            dynamic: None,
            dl: Some(&run),
            partial: false,
            steps: false,
        };
        let seeds = seed(&ix, &mut s.it);
        if !seeds.is_empty() {
            let dx = dix(&ix);
            let inv = inv_of(&ix, dx);
            let l = |v: &XLit| match run.lits.lit(*v, inv) {
                Made::Lit(l) => l,
                _ => panic!("a literal"),
            };
            // the seeds go to the first context saturated
            let to = match before.first().copied() {
                Some(ROOT) | None => root,
                Some(e) => e,
            };
            let c = ctx[to as usize].get_or_init(Box::default);
            let mut st = c.st.lock();
            st.init = true;
            let x = ds(&mut st);
            if to == root {
                x.root.get_or_insert_with(Box::default);
            }
            for (b, h) in &seeds {
                x.store.add(Clause::new(
                    b.iter().map(l).collect(),
                    h.iter().map(l).collect(),
                ));
            }
        }
        let root_ex = root;
        let root = at.unwrap_or(root);
        for &e in before {
            let e = if e == ROOT { root_ex } else { e };
            saturate(&ix, &ctx, &[e], &[], &[], &bounds).unwrap();
        }
        saturate(&ix, &ctx, &[root], &[], &[], &bounds).unwrap();
        Done { ix, ctx, root, run }
    }

    fn c(u: Ex, ix: &Index) -> RTerm {
        match ix.head(u).node {
            Node::Nominal(k) => RTerm::C(k),
            _ => panic!("not a nominal"),
        }
    }

    /// Item 1220's repair R1 in Pred from a successor over `f_e(u)`: two selected clauses
    /// `H(u) → A(f_e(u)) ∨ Z(x)` and `H(u) → B(f_e(u)) ∨ Z(x)` are the premises of the
    /// successor's `A(x) ∧ B(x) → Z(y)` only when selected partners count, with no fixed
    /// premise and with either one fixed. The clauses cannot go to a predecessor, since
    /// their heads hold an atom over x and one over `f_e(u)`.
    #[test]
    fn root_pred_takes_selected_partners_under_r1() {
        use super::super::dl_clause::Rooted;
        let mut sig = Sig::new();
        let a = sig.class("A");
        let b = sig.class("B");
        let h = sig.class("H");
        let z = sig.class("Z");
        let v = sig.ind("v");
        let r = sig.role("r");
        let e = sig.some_h(r, a);
        let done = saturate_root(sig);
        let ix = &done.ix;
        let lits = &done.run.lits;
        let inv = inv_of(ix, dix(ix));
        let RTerm::C(u) = c(v, ix) else {
            unreachable!()
        };
        let literal = |x| match lits.lit(x, inv) {
            Made::Lit(l) => l,
            _ => panic!("literal"),
        };
        let hyp = literal(XLit::Atom(h, RTerm::C(u)));
        let fa = literal(XLit::Atom(a, RTerm::Fc(e, u)));
        let fb = literal(XLit::Atom(b, RTerm::Fc(e, u)));
        let answer = literal(XLit::Atom(z, RTerm::C(u)));
        let mut st = State::default();
        let o = Rooted {
            ix,
            lits,
            inv: &inv,
            ground_total: true,
        };
        for l in [fa, fb] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![hyp], vec![l, Lit::x(z)]));
        }
        let mut maxima = Vec::new();
        while let Some(i) = ds(&mut st).store.next(ix, Some(&o), &mut maxima) {
            let s = &mut ds(&mut st).store.clauses[i as usize];
            s.sel = super::super::dl_rules::selected_always(lits, inv, &s.c);
        }
        assert!(ds(&mut st).store.clauses.iter().all(|s| s.sel));
        let succ = Clause::new(vec![Lit::x(a), Lit::x(b)], vec![answer]);
        let fixed = side(&ds(&mut st).store.clauses[0].c, fa);
        for (sel, want) in [(false, 0), (true, 1)] {
            let mut found = Found::default();
            root_pred_of(ix, &st, e, u, &succ, None, &mut found, lits, sel);
            assert_eq!(found.products_len(), want, "no fixed premise, sel {sel}");
            let mut found = Found::default();
            root_pred_of(
                ix,
                &st,
                e,
                u,
                &succ,
                Some((fa, &fixed)),
                &mut found,
                lits,
                sel,
            );
            assert_eq!(found.products_len(), want, "fixed premise, sel {sel}");
        }
    }

    /// Item 1250 Layer 1: split partner policies between Ground Hyper (`!s.gbody`) and Fc
    /// Pred (`include_selected || !s.sel`), plus `gbody` gating on `hyper_at`,
    /// `hyper_roles`, `on_role`, and `join_head`/`join_body` when `sel = false, gbody = true`.
    #[test]
    fn gbody_and_sel_partner_policies_differ() {
        use super::super::dl_clause::Rooted;
        let mut sig = Sig::new();
        let a = sig.class("A");
        let b = sig.class("B");
        let h = sig.class("H");
        let k = sig.class("K");
        let z = sig.class("Z");
        let t_cls = sig.class("T");
        let v = sig.ind("v");
        let w = sig.ind("w");
        let r = sig.role("r");
        let e = sig.some_h(r, a);
        let all_r_t = sig.all(r, t_cls);
        sig.sub(a, t_cls);
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let lits = &run.lits;
        let inv = inv_of(&ix, dix(&ix));
        let RTerm::C(u) = c(v, &ix) else {
            unreachable!()
        };
        let RTerm::C(u2) = c(w, &ix) else {
            unreachable!()
        };
        let literal = |x| match lits.lit(x, inv) {
            Made::Lit(l) => l,
            _ => panic!("literal"),
        };
        let hyp = literal(XLit::Atom(h, RTerm::C(u)));
        let fa = literal(XLit::Atom(a, RTerm::Fc(e, u)));
        let fb = literal(XLit::Atom(b, RTerm::Fc(e, u)));
        let ga = literal(XLit::Atom(a, RTerm::C(u)));
        let gb = literal(XLit::Atom(b, RTerm::C(u)));
        let answer = literal(XLit::Atom(z, RTerm::C(u)));
        let r_uv = literal(XLit::Role(r, RTerm::C(u), RTerm::C(u2)));
        let r_ux = literal(XLit::Role(r, RTerm::C(u), RTerm::X));
        let all_u = literal(XLit::Atom(all_r_t, RTerm::C(u)));

        let mut st = State::default();
        let o = Rooted {
            ix: &ix,
            lits,
            inv: &inv,
            ground_total: true,
        };
        // Two Fc-headed clauses with ground bodies: sel = false, gbody = true.
        for l in [fa, fb] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![hyp], vec![l, Lit::x(z)]));
        }
        // A ground-bodied clause with maximal A(u) and a ground-bodied role clause r(u, u2).
        ds(&mut st)
            .store
            .add(Clause::new(vec![hyp, Lit::x(k)], vec![ga]));
        ds(&mut st)
            .store
            .add(Clause::new(vec![hyp, Lit::x(k)], vec![r_uv]));
        ds(&mut st)
            .store
            .add(Clause::new(vec![hyp, Lit::x(k)], vec![r_ux]));
        ds(&mut st)
            .store
            .add(Clause::new(vec![hyp, Lit::x(k)], vec![all_u]));
        // A body-side Join target: B(u) -> Z(x), and two live, mutually incomparable
        // producers of B(u): K(x) -> B(u) (gbody=false) and H(u) -> B(u) (gbody=true).
        ds(&mut st)
            .store
            .add(Clause::new(vec![gb], vec![Lit::x(z)]));
        let prod_clean_id = {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![gb]));
            ds(&mut st).store.clauses.len() - 1
        };
        let prod_gbody_id = {
            ds(&mut st).store.add(Clause::new(vec![hyp], vec![gb]));
            ds(&mut st).store.clauses.len() - 1
        };

        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let mut maxima = Vec::new();
        while let Some(i) = ds(&mut st).store.next(&ix, Some(&o), &mut maxima) {
            index_consts(&ix, ds(&mut st), &maxima, i, &mut out);
            let s = &mut ds(&mut st).store.clauses[i as usize];
            let has_gbody = super::super::dl_rules::selected_always(lits, inv, &s.c);
            s.sel = false;
            s.gbody = has_gbody;
        }
        let clauses = &dl(&st).unwrap().store.clauses;
        let prod_clean = &clauses[prod_clean_id];
        let prod_gbody = &clauses[prod_gbody_id];
        assert!(prod_clean.done && !prod_clean.dead && !prod_clean.gbody && !prod_clean.sel);
        assert!(prod_gbody.done && !prod_gbody.dead && prod_gbody.gbody && !prod_gbody.sel);

        // 1. Fc Pred with sel=false, gbody=true, include_selected=false STILL takes both
        //    partners: no fixed premise, first fixed, and second fixed.
        let succ = Clause::new(vec![Lit::x(a), Lit::x(b)], vec![answer]);
        let fixed_a = side(&ds(&mut st).store.clauses[0].c, fa);
        let fixed_b = side(&ds(&mut st).store.clauses[1].c, fb);
        let mut found = Found::default();
        root_pred_of(&ix, &st, e, u, &succ, None, &mut found, lits, false);
        assert_eq!(
            found.products_len(),
            1,
            "Fc Pred must keep gbody-only partners with no fixed premise"
        );
        for (lit, fixed) in [(fa, &fixed_a), (fb, &fixed_b)] {
            let mut found = Found::default();
            root_pred_of(
                &ix,
                &st,
                e,
                u,
                &succ,
                Some((lit, fixed)),
                &mut found,
                lits,
                false,
            );
            assert_eq!(
                found.products_len(),
                1,
                "Fc Pred must keep gbody-only partners with either fixed premise"
            );
        }

        // 2. Ground Hyper partners in the EXACT same flag state (sel=false, gbody=true)
        //    must exclude the gbody-cut clause.
        let mut ga_count = 0usize;
        for_each_partner_at(&ix, &st, &out, &Meter::checks(), a, u, |_, _, _, _| {
            ga_count += 1
        });
        assert_eq!(
            ga_count, 0,
            "ground Hyper partners must exclude gbody=true clauses even when sel=false"
        );
        let mut gb_count = 0usize;
        for_each_partner_at(&ix, &st, &out, &Meter::checks(), b, u, |_, _, _, _| {
            gb_count += 1
        });
        assert_eq!(
            gb_count, 1,
            "ground Hyper partners must still accept empty-body (gbody=false) clauses"
        );

        // 3. hyper_at with gbody=true skips told_of (A ⊑ T) and skips C-filler universals,
        //    while still firing local X-filler universals.
        let prem_a: Side = (vec![hyp, Lit::x(k)], vec![]);
        let mut found = Found::default();
        let mut guard = Guard::new(&mut out);
        assert!(hyper_at(
            &ix, &mut st, &mut guard, a, u, &prem_a, &mut found, true
        ));
        assert!(
            found.one_sides().is_empty(),
            "hyper_at(gbody=true) must skip told_of A(u)->T(u)"
        );
        let mut found = Found::default();
        assert!(hyper_at(
            &ix, &mut st, &mut guard, all_r_t, u, &prem_a, &mut found, true
        ));
        let t_x = literal(XLit::Atom(t_cls, RTerm::X));
        let t_u2 = literal(XLit::Atom(t_cls, RTerm::C(u2)));
        let sides = found.one_sides();
        assert!(
            sides.iter().any(|s| s.1.contains(&t_x)),
            "hyper_roles(gbody=true) must still fire local X-filler universal"
        );
        assert!(
            !sides.iter().any(|s| s.1.contains(&t_u2)),
            "hyper_roles(gbody=true) must skip C-filler universal"
        );

        // 4. on_role with gbody=true skips C-filler but keeps local X-filler.
        let mut found = Found::default();
        assert!(on_role(
            &ix,
            &st,
            &mut guard,
            r,
            u,
            RTerm::C(u2),
            &prem_a,
            &mut found,
            true,
        ));
        assert!(
            found.one_sides().is_empty(),
            "on_role(gbody=true) must skip C-filler"
        );
        let mut found = Found::default();
        assert!(on_role(
            &ix,
            &st,
            &mut guard,
            r,
            u,
            RTerm::X,
            &prem_a,
            &mut found,
            true
        ));
        assert!(
            found.one_sides().iter().any(|s| s.1.contains(&t_x)),
            "on_role(gbody=true) must still fire local X-filler"
        );

        // 5. Join: gbody=true is blocked on head side (join_head & join_body) while still
        //    discharged on body side against a gbody=false producer.
        let head_gbody = Clause::new(vec![hyp, Lit::x(k)], vec![gb]);
        let mut found = Found::default();
        assert!(join_head(
            &ix,
            &st,
            &head_gbody,
            gb,
            &side(&head_gbody, gb),
            &mut found,
            &mut guard,
            true,
        ));
        assert!(
            found.one_sides().is_empty(),
            "join_head(gbody=true) must block gbody head premise"
        );
        let mut found = Found::default();
        assert!(join_head(
            &ix,
            &st,
            &head_gbody,
            gb,
            &side(&head_gbody, gb),
            &mut found,
            &mut guard,
            false,
        ));
        let mut want_head_body = vec![hyp, Lit::x(k)];
        want_head_body.sort();
        assert_eq!(
            found.one_sides(),
            &[(want_head_body, vec![Lit::x(z)])],
            "join_head(gbody=false) must resolve head premise against body target"
        );

        let body_target = Clause::new(vec![gb], vec![Lit::x(z)]);
        let mut found = Found::default();
        assert!(join_body(&ix, &st, &body_target, &mut found, &mut guard));
        assert_eq!(
            found.one_sides(),
            &[(vec![Lit::x(k)], vec![Lit::x(z)])],
            "join_body must resolve only against the live gbody=false producer, excluding the live gbody=true producer"
        );
        // Clearing gbody on the second live producer makes its distinct resolvent appear.
        ds(&mut st).store.clauses[prod_gbody_id].gbody = false;
        let mut found = Found::default();
        assert!(join_body(&ix, &st, &body_target, &mut found, &mut guard));
        assert_eq!(
            found.one_sides().len(),
            2,
            "clearing gbody on stored head producer must admit its join_body resolvent"
        );
        assert!(
            found
                .one_sides()
                .contains(&(vec![Lit::x(k)], vec![Lit::x(z)])),
            "clean producer resolvent must remain present"
        );
        assert!(
            found.one_sides().contains(&(vec![hyp], vec![Lit::x(z)])),
            "previously gated producer resolvent must appear once gbody is cleared"
        );
    }

    /// Item 1250 Layer 1 (`dl_eq.rs`): classify binary Eq by both participating literals
    /// in both arrival directions, preserve implicit unit `(false, false)` flags, classify
    /// Factor purity per pair inside the loop (so mixed `x ≈ b ∨ a ≈ b` stays available for
    /// either maximal pivot under `sel=false, gbody=true`), and preserve full selection's
    /// asymmetry (testing locality only on the maximal equality) when `sel=true`.
    #[test]
    fn eq_and_factor_split_pure_no_xy_from_mixed_non_local() {
        use super::super::dl_clause::Rooted;
        use super::super::dl_eq::Eqs;
        let mut sig = Sig::new();
        let [k, a_cls, b_cls, h_cls] = ["K", "A", "B", "H"].map(|n| sig.class(n));
        let r = sig.role("r");
        let e = sig.some_h(r, a_cls);
        let [na, nb, nc] = ["a", "b", "c"].map(|n| sig.ind(n));
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let lits = &run.lits;
        let dx = dix(&ix);
        let inv = inv_of(&ix, dx);
        let (ca, cb, cc) = (c(na, &ix), c(nb, &ix), c(nc, &ix));
        // Order constants c > b > a so orientations are deterministic.
        let mut cs = [ca, cb, cc];
        cs.sort_by(|&x, &y| {
            if Rooted::a_gt(x, y) {
                std::cmp::Ordering::Less
            } else if Rooted::a_gt(y, x) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        let [hi, mid, lo] = cs;
        assert!(Rooted::a_gt(hi, mid) && Rooted::a_gt(mid, lo));

        let lit_of = |v: XLit| match lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("literal"),
        };
        let hyp = lit_of(XLit::Atom(h_cls, lo));
        let eq_hi_mid = lit_of(XLit::Eq(hi, mid));
        let eq_hi_lo = lit_of(XLit::Eq(hi, lo));
        let eq_x_hi = lit_of(XLit::Eq(RTerm::X, hi));
        let eq_fx_hi = lit_of(XLit::Eq(RTerm::Fx(e), hi));
        let eq_fx_mid = lit_of(XLit::Eq(RTerm::Fx(e), mid));
        let b_hi = lit_of(XLit::Atom(b_cls, hi));
        let r_x_hi = lit_of(XLit::Role(r, RTerm::X, hi));

        let eqs = Eqs {
            ix: &ix,
            dx,
            root: false,
            query: Some(k),
        };
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let mut guard = Guard::new(&mut out);
        let o = Rooted {
            ix: &ix,
            lits,
            inv: &inv,
            ground_total: true,
        };

        // Helper: build a store containing one processed stored clause with chosen flags.
        let mk_store = |clause: Clause, sel: bool, gbody: bool| -> State {
            let mut st = State::default();
            ds(&mut st).store.add(clause);
            let mut maxima = Vec::new();
            while let Some(i) = ds(&mut st).store.next(&ix, Some(&o), &mut maxima) {
                let s = &mut ds(&mut st).store.clauses[i as usize];
                s.sel = sel;
                s.gbody = gbody;
            }
            st
        };

        // --- A. Pure no_xy Eq (hi ≈ mid rewriting B(hi)), both arrival directions ---
        let prem_eq: Side = (vec![hyp, Lit::x(k)], vec![]);
        let prem_b: Side = (vec![hyp, Lit::x(k)], vec![]);
        // Direction 1 (on_max): new eq rewrites stored B(hi)
        let st_b_gbody = mk_store(Clause::new(vec![hyp, Lit::x(k)], vec![b_hi]), false, true);
        let st_b_plain = mk_store(Clause::new(vec![Lit::x(k)], vec![b_hi]), false, false);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_b_plain,
            eq_hi_mid,
            &prem_eq,
            &mut f,
            &mut guard,
            false,
            true
        ));
        assert!(
            f.one_sides().is_empty(),
            "pure Eq blocked when new premise has gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_b_gbody,
            eq_hi_mid,
            &(vec![Lit::x(k)], vec![]),
            &mut f,
            &mut guard,
            false,
            false,
        ));
        assert!(
            f.one_sides().is_empty(),
            "pure Eq blocked when target has gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_b_plain,
            eq_hi_mid,
            &(vec![Lit::x(k)], vec![]),
            &mut f,
            &mut guard,
            false,
            false,
        ));
        assert!(
            !f.one_sides().is_empty(),
            "pure Eq allowed when both have sel=false, gbody=false"
        );

        // Direction 2 (into): new B(hi) rewritten by stored eq hi ≈ mid
        let st_eq_gbody = mk_store(
            Clause::new(vec![hyp, Lit::x(k)], vec![eq_hi_mid]),
            false,
            true,
        );
        let st_eq_plain = mk_store(Clause::new(vec![Lit::x(k)], vec![eq_hi_mid]), false, false);
        let mut f = Found::default();
        assert!(eqs.on_max(&st_eq_plain, b_hi, &prem_b, &mut f, &mut guard, false, true));
        assert!(
            f.one_sides().is_empty(),
            "pure Eq(into) blocked when new target has gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_eq_gbody,
            b_hi,
            &(vec![Lit::x(k)], vec![]),
            &mut f,
            &mut guard,
            false,
            false,
        ));
        assert!(
            f.one_sides().is_empty(),
            "pure Eq(into) blocked when stored eq has gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_eq_plain,
            b_hi,
            &(vec![Lit::x(k)], vec![]),
            &mut f,
            &mut guard,
            false,
            false,
        ));
        assert!(
            !f.one_sides().is_empty(),
            "pure Eq(into) allowed when both have sel=false, gbody=false"
        );

        // --- B. Mixed non-local Eq (plain-ground hi ≈ mid rewriting x ≈ hi), both directions ---
        let st_xhi_gbody = mk_store(
            Clause::new(vec![hyp, Lit::x(k)], vec![eq_x_hi]),
            false,
            true,
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_xhi_gbody,
            eq_hi_mid,
            &prem_eq,
            &mut f,
            &mut guard,
            false,
            true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "mixed non-local Eq (hi≈mid into x≈hi) must remain allowed under sel=false, gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_xhi_gbody,
            eq_hi_mid,
            &prem_eq,
            &mut f,
            &mut guard,
            true,
            true
        ));
        assert!(
            f.one_sides().is_empty(),
            "mixed non-local Eq must be blocked under sel=true"
        );
        // Reverse arrival direction (into): new x ≈ hi rewritten by stored hi ≈ mid
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_eq_gbody,
            eq_x_hi,
            &prem_eq,
            &mut f,
            &mut guard,
            false,
            true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "mixed non-local Eq(into) must remain allowed under sel=false, gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_eq_gbody,
            eq_x_hi,
            &prem_eq,
            &mut f,
            &mut guard,
            true,
            true
        ));
        assert!(
            f.one_sides().is_empty(),
            "mixed non-local Eq(into) must be blocked under sel=true"
        );

        // --- C. Query x ≈ hi rewriting implicit unit K(x) (unit has flags (false, false)) ---
        let mut st_unit = State::default();
        st_unit.subs.insert(k);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_unit,
            eq_x_hi,
            &(vec![hyp], vec![]),
            &mut f,
            &mut guard,
            false,
            true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "query x≈hi rewriting implicit unit K(x) must remain allowed under sel=false, gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_unit,
            eq_x_hi,
            &(vec![hyp], vec![]),
            &mut f,
            &mut guard,
            true,
            true
        ));
        assert!(
            f.one_sides().is_empty(),
            "query x≈hi rewriting implicit unit K(x) must be blocked under sel=true"
        );

        // --- D. Local Eq (hi ≈ mid rewriting r(x, hi)), allowed even under sel=true ---
        let st_local = mk_store(Clause::new(vec![hyp, Lit::x(k)], vec![r_x_hi]), true, true);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_local, eq_hi_mid, &prem_eq, &mut f, &mut guard, true, true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "local Eq allowed under sel=true, gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &st_eq_gbody,
            r_x_hi,
            &prem_eq,
            &mut f,
            &mut guard,
            true,
            true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "local Eq(into) allowed under sel=true, gbody=true"
        );

        // --- E. Factor: pure no_xy vs mixed x ≈ hi ∨ hi ≈ mid vs sel=true asymmetry ---
        let empty_st = State::default();
        // Pure no_xy Factor: hi ≈ mid ∨ hi ≈ lo (prem has hi ≈ lo in head, pivot is hi ≈ mid)
        let prem_pure: Side = (vec![hyp, Lit::x(k)], vec![eq_hi_lo]);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st, eq_hi_mid, &prem_pure, &mut f, &mut guard, false, true
        ));
        assert!(
            f.one_sides().is_empty(),
            "pure no_xy Factor must be blocked when gbody=true (even if sel=false)"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st, eq_hi_mid, &prem_pure, &mut f, &mut guard, false, false
        ));
        assert!(
            !f.one_sides().is_empty(),
            "pure no_xy Factor allowed when sel=false, gbody=false"
        );

        // Mixed Factor: x ≈ hi ∨ hi ≈ mid under sel=false, gbody=true for BOTH pivots!
        // Pivot 1: x ≈ hi maximal, second is hi ≈ mid
        let prem_mix1: Side = (vec![hyp, Lit::x(k)], vec![eq_hi_mid]);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st, eq_x_hi, &prem_mix1, &mut f, &mut guard, false, true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "mixed Factor with x≈hi maximal must stay allowed under sel=false, gbody=true"
        );
        // Pivot 2: hi ≈ mid (plain ground!) maximal, second is x ≈ hi
        let prem_mix2: Side = (vec![hyp, Lit::x(k)], vec![eq_x_hi]);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st, eq_hi_mid, &prem_mix2, &mut f, &mut guard, false, true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "mixed Factor with plain-ground hi≈mid maximal must stay allowed under sel=false, gbody=true"
        );

        // Full-selection asymmetry under sel=true, gbody=true:
        // Maximal pivot hi ≈ mid (non-local, s=hi) + second f(x) ≈ hi (local, shares hi):
        // fires under sel=false, gbody=true (non-vacuous), MUST be blocked under sel=true
        // because locality of the second equality does not exempt Factor!
        let prem_asym_blocked: Side = (vec![hyp, Lit::x(k)], vec![eq_fx_hi]);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st,
            eq_hi_mid,
            &prem_asym_blocked,
            &mut f,
            &mut guard,
            false,
            true,
        ));
        assert!(
            !f.one_sides().is_empty(),
            "non-vacuous control: hi≈mid + f(x)≈hi factors under sel=false, gbody=true"
        );
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st,
            eq_hi_mid,
            &prem_asym_blocked,
            &mut f,
            &mut guard,
            true,
            true
        ));
        assert!(
            f.one_sides().is_empty(),
            "under sel=true, locality of the second equality must NOT exempt Factor"
        );
        // Maximal pivot f(x) ≈ hi (local, s=f(x)) + second f(x) ≈ mid -> allowed under sel=true!
        let prem_asym_ok: Side = (vec![hyp, Lit::x(k)], vec![eq_fx_mid]);
        let mut f = Found::default();
        assert!(eqs.on_max(
            &empty_st,
            eq_fx_hi,
            &prem_asym_ok,
            &mut f,
            &mut guard,
            true,
            true
        ));
        assert!(
            !f.one_sides().is_empty(),
            "under sel=true, locality of the maximal equality exempts Factor"
        );
    }

    /// Root clauses must never set `sel` or `gbody` even when their bodies contain ground
    /// literals, and `Stored` keeps its layout size and `Store::owned_bytes == scan_bytes`.
    #[test]
    fn root_flags_stay_false_and_stored_owned_bytes_matches_scan() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            std::mem::size_of::<super::super::dl_clause::Stored>(),
            64,
            "adding gbody: bool must fit inside Stored's existing alignment padding"
        );
        let mut sig = Sig::new();
        let [a, b, h, z] = ["A", "B", "H", "Z"].map(|n| sig.class(n));
        let u = sig.ind("u");
        sig.sub(u, a);
        sig.dl.clauses.push((vec![a], vec![b, z]));
        let done = saturate_seeded(sig, None, |ix, _| {
            let cu = c(u, ix);
            vec![
                // Unsubsumed ground-bodied clause that survives until dl_root::flush processes it.
                (vec![XLit::Atom(h, cu)], vec![XLit::Atom(b, cu)]),
            ]
        });
        let st = done.ctx[done.root as usize].get().unwrap().st.lock();
        let store = &dl(&st).expect("root store").store;
        assert_eq!(store.bytes(), store.scan_bytes());
        assert!(
            store
                .clauses
                .iter()
                .any(|s| s.done && !s.dead && !s.c.body().is_empty()),
            "root store must have processed a live ground-bodied clause"
        );
        assert!(
            store.clauses.iter().all(|s| !s.sel && !s.gbody),
            "root clauses must keep sel=false and gbody=false"
        );
    }

    /// Item 1250 Layer 3: `marker_repair(true)` defaults to `MREPAIR_ON | MREPAIR_CF` (129),
    /// preserves mode 0 / mode 1 / TEST_MODE precedence, and handles all three M-return-cf
    /// arrival orders: denial-before-producer, producer-before-denial, and ineligible
    /// ground/Fc-bodied holder followed by an eligible holder.
    #[test]
    fn marker_repair_defaults_to_cf_and_handles_denial_arrival_orders() {
        use super::super::dl_clause::Rooted;
        assert_eq!(marker_repair_mode(None, 0, true), MREPAIR_ON | MREPAIR_CF);
        assert_eq!(marker_repair_mode(None, 0, false), 0);
        assert_eq!(marker_repair_mode(None, MREPAIR_ON, true), MREPAIR_ON);
        assert_eq!(marker_repair_mode(None, MREPAIR_ON, false), MREPAIR_ON);
        assert_eq!(marker_repair_mode(Some(0), MREPAIR_ON, true), 0);
        assert_eq!(marker_repair_mode(Some(MREPAIR_ON), 0, true), MREPAIR_ON);
        assert_eq!(
            marker_repair_mode(Some(MREPAIR_ON | MREPAIR_CF), MREPAIR_ON, false),
            MREPAIR_ON | MREPAIR_CF
        );
        if std::env::var_os("SPARKLES_ROOTED_MARKER_REPAIR").is_none() {
            assert_eq!(marker_repair(true), MREPAIR_ON | MREPAIR_CF);
            assert_eq!(marker_repair(false), 0);
        }

        let mut sig = Sig::new();
        let [a, b, h, k] = ["A", "B", "H", "K"].map(|n| sig.class(n));
        let r = sig.role("r");
        let e = sig.some_h(r, a);
        let u = sig.ind("u");
        let (ix, root) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = MREPAIR_ON | MREPAIR_CF;
        let inv = inv_of(&ix, dix(&ix));
        let RTerm::C(cu) = c(u, &ix) else {
            unreachable!()
        };
        let lit_of = |v: XLit| match run.lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("literal"),
        };
        let ga = lit_of(XLit::Atom(a, RTerm::C(cu)));
        let gh = lit_of(XLit::Atom(h, RTerm::C(cu)));
        let gfc = lit_of(XLit::Atom(b, RTerm::Fc(e, cu)));
        let denial = Clause::new(vec![ga], vec![]);
        let cid = run.table.push(denial);
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };

        // Order 1: denial returned BEFORE eligible empty-body producer arrives.
        {
            let mut st = State::default();
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            assert!(rret(&ix, &mut st, cid, &mut out));
            assert!(out.mine.is_empty(), "no product before any producer exists");
            assert!(
                m_note(&ix, root, k, &mut st, ga, &(vec![], vec![]), &mut out),
                "late eligible producer must replay saved denial"
            );
            assert!(
                out.mine
                    .iter()
                    .any(|m| matches!(m, Concl::Sub(x) if *x == super::super::index::BOTTOM)),
                "denial-before-producer must derive BOTTOM"
            );
        }

        // Order 2: eligible empty-body producer processed BEFORE denial returns.
        {
            let mut st = State::default();
            ds(&mut st).store.add(Clause::new(vec![], vec![ga]));
            let mut max = Vec::new();
            while ds(&mut st)
                .store
                .next(&ix, Some(&order), &mut max)
                .is_some()
            {}
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            assert!(rret(&ix, &mut st, cid, &mut out));
            assert!(
                out.mine
                    .iter()
                    .any(|m| matches!(m, Concl::Sub(x) if *x == super::super::index::BOTTOM)),
                "producer-before-denial must derive BOTTOM immediately"
            );
        }

        // Order 3: ineligible ground-bodied (H(u) -> A(u)) and Fc-bodied (B(f_e(u)) -> A(u))
        // holders arrive FIRST, then denial returns (producing nothing, keeping `rets`),
        // then an eligible x-only holder (K(x) -> A(u)) arrives and derives K(x) -> ⊥!
        {
            let mut st = State::default();
            let inelig_g = Clause::new(vec![gh], vec![ga]);
            let inelig_fc = Clause::new(vec![gfc], vec![ga]);
            ds(&mut st).store.add(inelig_g.clone());
            ds(&mut st).store.add(inelig_fc.clone());
            let mut max = Vec::new();
            while ds(&mut st)
                .store
                .next(&ix, Some(&order), &mut max)
                .is_some()
            {}
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            // Ineligible holders still register in `mheld` (sending RMark to root).
            assert!(m_note(
                &ix,
                root,
                k,
                &mut st,
                ga,
                &side(&inelig_g, ga),
                &mut out
            ));
            assert!(m_note(
                &ix,
                root,
                k,
                &mut st,
                ga,
                &side(&inelig_fc, ga),
                &mut out
            ));
            assert!(
                out.sent
                    .iter()
                    .any(|(to, m)| *to == root && matches!(m, Concl::RMark(from, _) if *from == k)),
                "m_note must register holder before rejecting ineligible ground-bodied premise"
            );
            // Denial returns: recorded in `rets`, but produces NO ground-bodied conclusion.
            assert!(rret(&ix, &mut st, cid, &mut out));
            assert!(
                dl(&st)
                    .unwrap()
                    .store
                    .clauses
                    .iter()
                    .all(|s| !s.c.head().is_empty()),
                "M-return-cf must not derive ground-bodied denials from pre-existing ineligible holders"
            );
            // Late ineligible ground-bodied and Fc-bodied premises arrive AFTER `rret` has
            // saved `cid` in `rets`: m_note's late fixed-premise filter must reject both.
            assert!(m_note(
                &ix,
                root,
                k,
                &mut st,
                ga,
                &(vec![gh, Lit::x(k)], vec![]),
                &mut out
            ));
            assert!(m_note(
                &ix,
                root,
                k,
                &mut st,
                ga,
                &(vec![gfc, Lit::x(k)], vec![]),
                &mut out
            ));
            assert!(
                dl(&st)
                    .unwrap()
                    .store
                    .clauses
                    .iter()
                    .all(|s| !s.c.head().is_empty()),
                "M-return-cf must not replay saved returns against late ineligible ground/Fc-bodied premises"
            );
            // Eligible x-only holder arrives later: m_late derives K(x) -> ⊥.
            let x_only: Side = (vec![Lit::x(k)], vec![]);
            assert!(m_note(&ix, root, k, &mut st, ga, &x_only, &mut out));
            assert!(
                dl(&st)
                    .unwrap()
                    .store
                    .clauses
                    .iter()
                    .any(|s| s.c.body() == [Lit::x(k)] && s.c.head().is_empty()),
                "late x-only eligible holder must derive K(x) -> ⊥ under M-return-cf"
            );
        }
    }

    #[test]
    fn every_constant_exists_and_takes_the_global_axioms() {
        let mut s = Sig::new();
        let a = s.ind("a");
        let b = s.ind("b");
        let g = s.class("g");
        s.sub(TOP, g);
        let d = saturate_root(s);
        assert!(!d.bottom());
        for n in [a, b] {
            let u = c(n, &d.ix);
            assert!(d.has(&[XLit::Atom(n, u)]), "N_u(u) for {n}");
            assert!(d.has(&[XLit::Atom(g, u)]), "⊤ ⊑ G gives G(u) for {n}");
        }
    }

    #[test]
    fn an_assertion_through_a_disjunction_can_clash() {
        // A(a), A ⊑ B ⊔ C, B ⊑ D, C ⊑ D, A ⊓ D ⊑ ⊥
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, cc, dd] = ["A", "B", "C", "D"].map(|n| s.class(n));
        s.sub(na, a);
        s.sub(b, dd);
        s.sub(cc, dd);
        s.ax.disjoint.push(vec![a, dd]);
        s.dl.clauses.push((vec![a], vec![b, cc]));
        let d = saturate_root(s);
        assert!(d.bottom(), "the root derives the empty clause");
    }

    #[test]
    fn an_assertion_through_a_disjunction_keeps_both_cases() {
        // A(a), A ⊑ B ⊔ C: B(a) ∨ C(a), and no B(a) or C(a) alone
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, cc] = ["A", "B", "C"].map(|n| s.class(n));
        s.sub(na, a);
        s.dl.clauses.push((vec![a], vec![b, cc]));
        let d = saturate_root(s);
        assert!(!d.bottom());
        let u = c(na, &d.ix);
        assert!(d.has(&[XLit::Atom(b, u), XLit::Atom(cc, u)]));
        assert!(!d.has(&[XLit::Atom(b, u)]));
        assert!(!d.has(&[XLit::Atom(cc, u)]));
    }

    #[test]
    fn a_conjunction_of_ground_atoms_meets_at_one_constant() {
        // A(a), B(a), B(b), A ⊓ B ⊑ E: E(a) and not E(b)
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [a, b, e] = ["A", "B", "E"].map(|n| s.class(n));
        let ab = s.it.conj(a, b);
        s.sub(na, a);
        s.sub(na, b);
        s.sub(nb, b);
        s.sub(ab, e);
        let d = saturate_root(s);
        assert!(d.has(&[XLit::Atom(e, c(na, &d.ix))]));
        assert!(!d.has(&[XLit::Atom(e, c(nb, &d.ix))]));
    }

    #[test]
    fn an_existential_meets_the_universals_in_either_order() {
        // {a} ⊑ ∃r.E, ⊤ ⊑ ∀r.G, {a} ⊑ ∀r.H: r(a, f(a)), E(f(a)), G(f(a)), H(f(a))
        let mut s = Sig::new();
        let na = s.ind("a");
        let [e, g, h] = ["E", "G", "H"].map(|n| s.class(n));
        let r = s.role("r");
        let some = s.some(r, e);
        let all_g = s.all(r, g);
        let all_h = s.all(r, h);
        s.sub(na, some);
        s.sub(TOP, all_g);
        s.sub(na, all_h);
        let d = saturate_root(s);
        let u = c(na, &d.ix);
        let f = RTerm::Fc(
            some,
            match u {
                RTerm::C(k) => k,
                _ => unreachable!(),
            },
        );
        assert!(d.has(&[XLit::Role(r, u, f)]));
        for x in [e, g, h] {
            assert!(d.has(&[XLit::Atom(x, f)]), "{x} over f(a)");
        }
    }

    #[test]
    fn a_universal_over_a_disjunction_keeps_its_case() {
        // {a} ⊑ ∃r.E, {a} ⊑ A ⊔ B, A ⊑ ∀r.H: A(a) ∨ B(a) gives H(f(a)) ∨ B(a), with A
        // declared after B so that A(a) is the maximal literal under the shared ground
        // order of item 804
        let mut s = Sig::new();
        let na = s.ind("a");
        let [e, b, a, h] = ["E", "B", "A", "H"].map(|n| s.class(n));
        let r = s.role("r");
        let some = s.some(r, e);
        let all_h = s.all(r, h);
        s.sub(na, some);
        s.sub(a, all_h);
        s.dl.clauses.push((vec![na], vec![a, b]));
        let d = saturate_root(s);
        let u = c(na, &d.ix);
        let RTerm::C(k) = u else { unreachable!() };
        let f = RTerm::Fc(some, k);
        assert!(d.has(&[XLit::Atom(h, f), XLit::Atom(b, u)]));
        assert!(!d.has(&[XLit::Atom(h, f)]));
    }

    #[test]
    fn disjoint_roles_clash_over_one_successor_term() {
        // {a} ⊑ ∃r.E, r ⊑ s, r and s disjoint
        let mut s = Sig::new();
        let na = s.ind("a");
        let e = s.class("E");
        let r = s.role("r");
        let t = s.role("s");
        let some = s.some(r, e);
        s.sub(na, some);
        s.ax.role_subs.push((r, t));
        s.dl.rdis.push((r, t));
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn a_singleton_below_another_gives_an_equality_of_constants() {
        // {a} ⊑ {b}: a ≈ b
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        s.sub(na, nb);
        let d = saturate_root(s);
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        assert!(d.has(&[XLit::Eq(ua.min(ub), ua.max(ub))]));
        assert!(!d.bottom());
    }

    fn k(u: RTerm) -> u32 {
        match u {
            RTerm::C(k) => k,
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_equality_of_constants_moves_the_greater_constants_facts() {
        // {a} ⊑ {b}, A(a), B(b): A and B hold of the smaller constant
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        s.sub(na, nb);
        s.sub(na, a);
        s.sub(nb, b);
        let d = saturate_root(s);
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        let lo = ua.min(ub);
        assert!(d.has(&[XLit::Atom(a, lo)]));
        assert!(d.has(&[XLit::Atom(b, lo)]));
        assert!(!d.bottom());
    }

    #[test]
    fn different_individuals_made_equal_clash() {
        // {a} ⊑ {b}, and a and b are different
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        s.sub(na, nb);
        s.ax.disjoint.push(vec![na, nb]);
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn an_equality_below_a_ground_atom_waits_for_the_atom() {
        // {a} ⊑ {b} ⊔ C, B(b): under the shared ground order of item 804 the atom C(a) is
        // above a ≈ b, so a ≈ b ∨ C(a) stands, and once C ⊑ ⊥ resolves C(a) away the unit
        // a ≈ b rewrites B(b) to B of the smaller constant
        for bottom in [false, true] {
            let mut s = Sig::new();
            // B and C before the individuals, so that N_b(a) is maximal beside C(a)
            let [b, cc] = ["B", "C"].map(|n| s.class(n));
            let na = s.ind("a");
            let nb = s.ind("b");
            s.sub(nb, b);
            if bottom {
                s.sub(cc, BOTTOM);
            }
            s.dl.clauses.push((vec![na], vec![nb, cc]));
            let d = saturate_root(s);
            let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
            let lo = ua.min(ub);
            let hi = ua.max(ub);
            if !bottom {
                assert!(
                    d.clauses().iter().any(|(bd, h)| bd.is_empty()
                        && h.contains(&XLit::Eq(lo, hi))
                        && h.contains(&XLit::Atom(cc, ua))),
                    "a ≈ b ∨ C(a)"
                );
                assert!(!d.bottom());
                continue;
            }
            assert!(d.has(&[XLit::Eq(lo, hi)]));
            assert!(d.has(&[XLit::Atom(b, lo)]));
            assert!(!d.bottom());
        }
    }

    #[test]
    fn a_minimum_s_successors_differ_in_the_root() {
        // {a} ⊑ M, M ⊑ ∃r.B1 ⊓ ∃r.B2 with f1(a) ≉ f2(a)
        let mut s = Sig::new();
        let na = s.ind("a");
        let [m, b1, b2] = ["M", "B1", "B2"].map(|n| s.class(n));
        let r = s.role("r");
        let e1 = s.some(r, b1);
        let e2 = s.some(r, b2);
        s.sub(na, m);
        s.sub(m, e1);
        s.sub(m, e2);
        s.dl.ne.push((m, vec![e1, e2]));
        s.dl.counting = true;
        let d = saturate_root(s);
        let u = k(c(na, &d.ix));
        let (f1, f2) = (RTerm::Fc(e1, u), RTerm::Fc(e2, u));
        assert!(d.has(&[XLit::Ne(f1.min(f2), f1.max(f2))]));
    }

    #[test]
    fn a_context_in_a_singleton_holds_its_facts_of_the_constant() {
        // A ⊑ {o}, A ⊑ B ⊔ C: in A's context A(o), and B(o) ∨ C(x), B(x) ∨ C(o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc] = ["A", "B", "C"].map(|n| s.class(n));
        s.sub(a, no);
        s.dl.clauses.push((vec![a], vec![b, cc]));
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Eq(RTerm::X, o)]), "x ≈ o");
        assert!(d.has(&[XLit::Atom(a, o)]), "A(o)");
        assert!(d.has(&[XLit::Atom(no, o)]), "N_o(o)");
        // with the blocking of item 804's third departure x ≈ o rewrites the core unit
        // A(x) only, and the disjunction over x stays over x
        let mixed = d.has(&[XLit::Atom(b, o), XLit::Atom(cc, RTerm::X)])
            && d.has(&[XLit::Atom(b, RTerm::X), XLit::Atom(cc, o)]);
        assert_eq!(mixed, !super::super::dl_eq::eq_block());
        assert!(!d.has(&[XLit::Atom(b, o)]));
    }

    #[test]
    fn a_root_fact_of_a_named_class_is_rewritten_back_over_x_in_a_query_context() {
        // A ⊑ {o}, and the root holds B(o) and F(o) for a fresh F: A's context takes both
        // and rewrites B(o) to the unit B(x) with o ↦ x. With the blocking, the fresh atom
        // stays over o, since only a named-class atom is rewritten back.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        let f = s.it.fresh_class();
        s.sub(a, no);
        let d = saturate_after(s, &[ROOT], Some(a), |ix, _| {
            let o = c(no, ix);
            vec![
                (vec![], vec![XLit::Atom(b, o)]),
                (vec![], vec![XLit::Atom(f, o)]),
            ]
        });
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Atom(a, o)]), "A(o)");
        assert!(d.has(&[XLit::Atom(b, o)]), "B(o) reached A's context");
        assert!(d.has(&[XLit::Atom(f, o)]), "F(o) reached A's context");
        assert!(d.subs(b), "B(x)");
        assert_eq!(d.subs(f), !super::super::dl_eq::eq_block());
    }

    #[test]
    fn a_hypothetical_successor_rewrites_nothing_with_an_equality_to_a_constant() {
        // A ⊑ ∃r.B ⊔ C, B ⊑ {o} as a clause, B ⊑ G: the hypothetical successor of ∃r.B
        // holds the marker B(x) → B(x), B(x) → x ≈ o and B(x) → G(x). With the blocking,
        // x ≈ o rewrites nothing there, since the successor is no query context; without
        // it, B(x) → G(o) and B(x) → B(o) follow. A told B ⊑ {o} would name o in place
        // of the successor (item 822), so the singleton is a clause here.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc, g] = ["A", "B", "C", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, b);
        s.dl.clauses.push((vec![a], vec![e, cc]));
        s.dl.clauses.push((vec![b], vec![no]));
        s.sub(b, g);
        let d = saturate_at(s, Some(a));
        let h = dix(&d.ix).hyp[&e];
        let o = c(no, &d.ix);
        let cs = d.clauses_of(h);
        let bx = XLit::Atom(b, RTerm::X);
        let has = |head: XLit| {
            cs.iter()
                .any(|(bd, hd)| bd == &vec![bx] && hd == &vec![head])
        };
        assert!(has(XLit::Eq(RTerm::X, o)), "B(x) → x ≈ o in {cs:?}");
        assert!(has(XLit::Atom(g, RTerm::X)), "B(x) → G(x)");
        let ground = has(XLit::Atom(g, o)) || has(XLit::Atom(b, o));
        assert_eq!(ground, !super::super::dl_eq::eq_block(), "{cs:?}");
    }

    #[test]
    fn a_restriction_into_a_singleton_names_its_constant_whichever_comes_first() {
        // A ⊑ ∀r.C, A ⊑ B, B ⊑ ∃r.{o}, and D ⊑ ∃r.{o} ⊔ E with D ⊑ ∀r.C: A's context
        // holds the restriction as a unit after its universal, D's in a clause, and each
        // holds r(x, o) and C(o) and no literal over the restriction's successor term
        // (item 822)
        if !super::super::dl_rules::nominal_succ() {
            return;
        }
        for named in ["A", "D"] {
            let mut s = Sig::new();
            let no = s.ind("o");
            let [a, b, cc, dd, ee] = ["A", "B", "C", "D", "E"].map(|n| s.class(n));
            let r = s.role("r");
            let e = s.some_h(r, no);
            let all = s.all(r, cc);
            s.sub(a, all);
            s.sub(a, b);
            s.sub(b, e);
            s.sub(dd, all);
            s.dl.clauses.push((vec![dd], vec![e, ee]));
            let at = if named == "A" { a } else { dd };
            let d = saturate_at(s, Some(at));
            let o = c(no, &d.ix);
            let cs = d.clauses_of(at);
            let over_f = cs
                .iter()
                .flat_map(|(bd, hd)| bd.iter().chain(hd))
                .any(|&l| {
                    let (ts, n) = super::super::dl_clause::terms_of(l);
                    ts[..n]
                        .iter()
                        .any(|t| matches!(t, RTerm::Fx(f) | RTerm::Fc(f, _) if *f == e))
                });
            assert!(!over_f, "{named}: a literal over f(x) in {cs:?}");
            let head = |l: XLit| cs.iter().any(|(_, hd)| hd.contains(&l));
            let rxo = XLit::Role(r, RTerm::X, o).canon(inv_of(&d.ix, dix(&d.ix)));
            assert!(head(rxo), "{named}: r(x, o) in {cs:?}");
            assert!(head(XLit::Atom(cc, o)), "{named}: C(o) in {cs:?}");
        }
    }

    #[test]
    fn a_clause_over_x_is_rewritten_at_each_maximal_atom() {
        // A ⊑ {o} and ⊤ → B(x) ∨ C(x): in A's context x ≈ o rewrites each of the two
        // incomparable atoms, and then the other, to B(o) ∨ C(o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc] = ["A", "B", "C"].map(|n| s.class(n));
        s.sub(a, no);
        s.dl.clauses.push((vec![], vec![b, cc]));
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Atom(b, RTerm::X), XLit::Atom(cc, RTerm::X)]));
        assert!(d.has(&[XLit::Atom(b, o), XLit::Atom(cc, o)]));
    }

    #[test]
    fn a_universal_meets_a_role_atom_between_constants_from_either_end() {
        // ⊤ → r(a, b), ⊤ → (∀r.B)(a), ⊤ → (∀r⁻.C)(b): B(b) and C(a), in both arrival
        // orders of the role atom and the universal
        for role_first in [true, false] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let nb = s.ind("b");
            let [b, cc] = ["B", "C"].map(|n| s.class(n));
            let r = s.role("r");
            let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
            let all_b = s.all(r, b);
            let all_c = s.all(ri, cc);
            let d = saturate_seeded(s, None, |ix, _| {
                let (ua, ub) = (c(na, ix), c(nb, ix));
                let role = (vec![], vec![XLit::Role(r, ua, ub)]);
                let alls = [
                    (vec![], vec![XLit::Atom(all_b, ua)]),
                    (vec![], vec![XLit::Atom(all_c, ub)]),
                ];
                let mut v = Vec::new();
                if role_first {
                    v.push(role.clone());
                }
                v.extend(alls);
                if !role_first {
                    v.push(role);
                }
                v
            });
            let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
            assert!(d.has(&[XLit::Atom(b, ub)]), "B(b), role first {role_first}");
            assert!(
                d.has(&[XLit::Atom(cc, ua)]),
                "C(a), role first {role_first}"
            );
        }
    }

    #[test]
    fn disjoint_roles_clash_between_constants() {
        // ⊤ → r(a, b), ⊤ → s(b, a)... with s⁻ below t, r and t disjoint
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let r = s.role("r");
        let t = s.role("t");
        s.dl.rdis.push((r, t));
        let d = saturate_seeded(s, None, |ix, _| {
            let (ua, ub) = (c(na, ix), c(nb, ix));
            vec![
                (vec![], vec![XLit::Role(r, ua, ub)]),
                (vec![], vec![XLit::Role(t, ua, ub)]),
            ]
        });
        assert!(d.bottom());
    }

    #[test]
    fn a_successor_in_a_singleton_sends_its_equality_and_ground_facts_back() {
        // A ⊑ ∃r.B, B ⊑ {o}, B ⊑ G: B's context has x ≈ o and G(o), and A's context
        // gets f(x) ≈ o and G(o), and through Eq r(x, o). The singleton is a clause, since
        // a told B ⊑ {o} names o in place of the successor (item 822).
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, g] = ["A", "B", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![no]));
        s.sub(b, g);
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Eq(RTerm::Fx(e), o)]), "f(x) ≈ o");
        assert!(d.has(&[XLit::Atom(g, o)]), "G(o)");
        assert!(d.has(&[XLit::Role(r, RTerm::X, o)]), "r(x, o)");
    }

    #[test]
    fn a_conditional_ground_fact_keeps_its_condition_through_pred() {
        // A ⊑ ∃r.B, B ⊑ C ⊔ {o}, B ⊑ G: G(o) holds only when B's instance is o, so A's
        // context has no G(o) alone. With {o} ⊑ G in place of B ⊑ G, G(o) would hold in
        // every model, and the root would give it to A's context.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc, g] = ["A", "B", "C", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![cc, no]));
        s.sub(b, g);
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(!d.has(&[XLit::Atom(g, o)]));
        assert!(!d.has(&[XLit::Eq(RTerm::Fx(e), o)]));
        assert!(!d.bottom());
    }

    #[test]
    fn factor_takes_two_equalities_of_y() {
        // ⊤ → y ≈ a ∨ y ≈ b, b above a: a ≉ b ∨ y ≈ b
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let d = saturate_seeded(s, None, |ix, _| {
            let (ua, ub) = (c(na, ix), c(nb, ix));
            vec![(vec![], vec![XLit::Eq(RTerm::Y, ua), XLit::Eq(RTerm::Y, ub)])]
        });
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        let (lo, hi) = (ua.min(ub), ua.max(ub));
        assert!(d.has(&[XLit::Ne(lo, hi), XLit::Eq(RTerm::Y, hi)]));
    }

    #[test]
    fn the_index_by_term_is_counted_from_its_first_allocation() {
        let mut st = super::super::dl_clause::Store::default();
        let mut s = Sig::new();
        let _ = s.ind("a");
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let lits = super::super::dl_clause::Lits::default();
        let inv = |_: Role| None;
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let Made::Lit(l) = lits.lit(XLit::Atom(TOP, RTerm::C(0)), inv) else {
            unreachable!()
        };
        st.add(Clause::new(Vec::new(), vec![l]));
        let before = st.bytes();
        let added = st.take_grown();
        let mut max = Vec::new();
        st.next(&ix, Some(&o), &mut max);
        let grown = st.take_grown();
        assert!(added == 0);
        assert!(grown >= std::mem::size_of::<super::super::dl_clause::ByTerm>());
        assert!(
            st.bytes() - before <= grown,
            "growth {grown} covers {}",
            st.bytes() - before
        );
    }

    #[test]
    fn a_predecessor_that_comes_later_gets_the_clauses_with_maxima() {
        // B ⊑ {o}, B ⊑ G, A ⊑ ∃r.B, with B's context saturated before A reaches it, and
        // the singleton a clause, as above
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, g] = ["A", "B", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![no]));
        s.sub(b, g);
        let d = saturate_after(s, &[b], Some(a), |_, _| Vec::new());
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Eq(RTerm::Fx(e), o)]), "f(x) ≈ o");
        assert!(d.has(&[XLit::Atom(g, o)]), "G(o)");
    }

    #[test]
    fn only_predecessor_triggers_go_to_the_predecessor() {
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let bb = s.class("B");
        let r = s.role("r");
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let dx = dix(&ix);
        let lits = super::super::dl_clause::Lits::default();
        let inv = inv_of(&ix, dx);
        let (ua, ub) = (c(na, &ix), c(nb, &ix));
        let l = |v: XLit| match lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!(),
        };
        let ready = |h: Vec<XLit>| {
            super::super::dl_rules::pred_ready(
                &ix,
                dx,
                &lits,
                &Clause::new(Vec::new(), h.into_iter().map(l).collect()),
            )
        };
        assert!(ready(vec![XLit::Atom(bb, RTerm::Y)]));
        assert!(ready(vec![XLit::Atom(bb, ua)]));
        assert!(ready(vec![XLit::Role(r, ua, ub)]));
        assert!(ready(vec![XLit::Eq(RTerm::X, ua)]));
        assert!(ready(vec![XLit::Eq(RTerm::Y, ua)]));
        assert!(ready(vec![XLit::Eq(RTerm::X, RTerm::Y)]));
        assert!(ready(vec![XLit::Eq(ua, ub)]));
        assert!(!ready(vec![XLit::Role(r, RTerm::Y, ua)]));
        assert!(!ready(vec![XLit::Role(r, ua, RTerm::Y)]));
        assert!(!ready(vec![XLit::Role(r, RTerm::Y, RTerm::Y)]));
        assert!(!ready(vec![XLit::Ne(RTerm::Y, ua)]));
        // a ground literal goes whatever its form
        assert!(ready(vec![XLit::Ne(ua, ub)]));
        // a role atom between x and a constant goes, and only the root takes it
        assert!(ready(vec![XLit::Role(r, RTerm::X, ua)]));
        assert!(!ready(vec![XLit::Atom(bb, RTerm::X)]));
    }

    #[test]
    fn a_later_predecessor_replays_no_clause_outside_the_triggers() {
        // B's context holds ⊤ → s(y, y) before A ⊑ ∃r.B reaches it: A gets no s(x, x)
        let mut s = Sig::new();
        let _ = s.ind("o");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("s");
        let e = s.some(r, b);
        s.sub(a, e);
        let d = saturate_after(s, &[b], Some(a), |_, _| {
            vec![(vec![], vec![XLit::Role(t, RTerm::Y, RTerm::Y)])]
        });
        assert!(!d.has(&[XLit::Role(t, RTerm::X, RTerm::X)]));
        assert!(
            !d.clauses()
                .iter()
                .any(|(_, h)| h.contains(&XLit::Role(t, RTerm::X, RTerm::X)))
        );
    }

    #[test]
    fn a_fact_of_an_equal_constant_reaches_a_context_in_the_singleton() {
        // {p} ⊑ {o}, {p} ⊑ C, A ⊑ {o}: A ⊑ C, whichever constant is greater and whichever
        // of the root and A's context saturates first
        for (o_first, root_first) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut s = Sig::new();
            let (no, np) = if o_first {
                let o = s.ind("o");
                (o, s.ind("p"))
            } else {
                let p = s.ind("p");
                (s.ind("o"), p)
            };
            let [a, cc] = ["A", "C"].map(|n| s.class(n));
            s.sub(np, no);
            s.sub(np, cc);
            s.sub(a, no);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(
                d.subs(cc),
                "A ⊑ C, o first {o_first}, root first {root_first}"
            );
            assert!(!d.bottom());
        }
    }

    #[test]
    fn a_root_clause_over_y_returns_to_the_context_with_the_edge() {
        // A ⊑ ∃r.{o}, {p} ⊑ {o}, {p} ⊑ ∀r⁻.B: A's context has r(x, o), the root the marker
        // r(y, o) and r(y, o) → B(y), and A ⊑ B, in every order
        for (o_first, root_first) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut s = Sig::new();
            let (no, np) = if o_first {
                let o = s.ind("o");
                (o, s.ind("p"))
            } else {
                let p = s.ind("p");
                (s.ind("o"), p)
            };
            let [a, b] = ["A", "B"].map(|n| s.class(n));
            let r = s.role("r");
            let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
            let e = s.some(r, no);
            let all = s.all(ri, b);
            s.sub(a, e);
            s.sub(np, no);
            s.sub(np, all);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(
                d.subs(b),
                "A ⊑ B, o first {o_first}, root first {root_first}"
            );
        }
    }

    #[test]
    fn join_resolves_a_ground_atom_in_either_arrival_order() {
        // the Horn clause first: ⊤ → B(o) with B(o) → C(o) ∨ D(o), and B(o) → C(o) with
        // ⊤ → B(o) ∨ E(o)
        for head_first in [true, false] {
            let mut s = Sig::new();
            let no = s.ind("o");
            // E before B, so that B(o) is maximal beside E(o) under the shared ground order
            let [ee, b, cc, dd] = ["E", "B", "C", "D"].map(|n| s.class(n));
            let d = saturate_seeded(s, None, |ix, _| {
                let o = c(no, ix);
                let at = |a: Ex| XLit::Atom(a, o);
                if head_first {
                    vec![(vec![], vec![at(b)]), (vec![at(b)], vec![at(cc), at(dd)])]
                } else {
                    vec![(vec![], vec![at(b), at(ee)]), (vec![at(b)], vec![at(cc)])]
                }
            });
            let o = c(no, &d.ix);
            let at = |a: Ex| XLit::Atom(a, o);
            if head_first {
                assert!(d.has(&[at(cc), at(dd)]));
            } else {
                assert!(d.has(&[at(cc), at(ee)]));
            }
        }
    }

    #[test]
    fn the_root_s_bottom_reaches_a_context_with_an_edge() {
        // {p} ⊑ {o}, {p} ⊑ C, {o} and C disjoint: the ontology is inconsistent, and
        // A ⊑ {q}, which reaches the root by q, is unsatisfiable
        for root_first in [true, false] {
            let mut s = Sig::new();
            let no = s.ind("o");
            let np = s.ind("p");
            let nq = s.ind("q");
            let [a, cc] = ["A", "C"].map(|n| s.class(n));
            s.sub(np, no);
            s.sub(np, cc);
            s.ax.disjoint.push(vec![no, cc]);
            s.sub(a, nq);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(d.bottom(), "root first {root_first}");
        }
    }

    #[test]
    fn root_clauses_meet_the_cases_of_a_disjunction() {
        // A ⊑ D ⊔ E, D ⊑ ∃r.{o}, E ⊑ ∃s.{o}, {p} ⊑ {o}, {p} ⊑ ∀r⁻.B ⊓ ∀s⁻.B: A ⊑ B, which
        // takes the root's clauses r(y, o) → B(y) and s(y, o) → B(y) in each case
        for (o_first, root_first) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut s = Sig::new();
            let (no, np) = if o_first {
                let o = s.ind("o");
                (o, s.ind("p"))
            } else {
                let p = s.ind("p");
                (s.ind("o"), p)
            };
            let [a, b, dd, ee] = ["A", "B", "D", "E"].map(|n| s.class(n));
            let r = s.role("r");
            let t = s.role("s");
            let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
            let ti = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:s")));
            let er = s.some(r, no);
            let et = s.some(t, no);
            let ar = s.all(ri, b);
            let at = s.all(ti, b);
            s.dl.clauses.push((vec![a], vec![dd, ee]));
            s.sub(dd, er);
            s.sub(ee, et);
            s.sub(np, no);
            s.sub(np, ar);
            s.sub(np, at);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(
                d.subs(b),
                "A ⊑ B, o first {o_first}, root first {root_first}"
            );
        }
    }

    /// A sink that keeps what the rules send.
    struct Probe<'a> {
        run: &'a DlRun,
        sent: Vec<(Ex, Concl)>,
        mine: Vec<Concl>,
    }

    impl Sink for Probe<'_> {
        fn send(&mut self, to: Ex, c: Concl) {
            self.sent.push((to, c));
        }
        fn me(&mut self, c: Concl) {
            self.mine.push(c);
        }
        fn dl(&self) -> &DlRun {
            self.run
        }
    }

    #[test]
    fn a_context_derives_the_consequences_of_its_own_ground_disjunction() {
        // Z's context holds ⊤ → C(a) ∨ C(b) and C ⊑ D: Hyper at the greater constant gives
        // ⊤ → D(a) ∨ C(b) or ⊤ → C(a) ∨ D(b) in Z's context itself, and the root holds no
        // clause with a body, since the run has no markers in the root
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [cc, dd, z] = ["C", "D", "Z"].map(|n| s.class(n));
        s.sub(cc, dd);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            vec![(
                vec![],
                vec![XLit::Atom(cc, c(na, ix)), XLit::Atom(cc, c(nb, ix))],
            )]
        });
        let (a, b) = (c(na, &d.ix), c(nb, &d.ix));
        let cs = d.clauses_of(z);
        let has = |h: &[XLit]| {
            cs.iter().any(|(bd, hd)| {
                bd.is_empty() && hd.len() == h.len() && h.iter().all(|l| hd.contains(l))
            })
        };
        let (hi, lo) = if Rooted::a_gt(a, b) { (a, b) } else { (b, a) };
        assert!(has(&[XLit::Atom(dd, hi), XLit::Atom(cc, lo)]), "{cs:?}");
        if super::super::dl_clause::ground_total() {
            assert!(!has(&[XLit::Atom(cc, hi), XLit::Atom(dd, lo)]), "{cs:?}");
        }
        let root = dix(&d.ix).root.unwrap();
        assert!(
            d.clauses_of(root).iter().all(|(bd, _)| bd.is_empty()),
            "{:?}",
            d.clauses_of(root)
        );
    }

    #[test]
    fn a_root_fact_reaches_a_context_that_mentions_the_constant_inside_a_successor_term() {
        // {o} ⊑ G, and Z's context holds ⊤ → K(f_e(o)): Z registers for o and takes the
        // root's G(o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [g, k, z] = ["G", "K", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, k);
        s.sub(no, g);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            let RTerm::C(o) = c(no, ix) else { panic!() };
            vec![(vec![], vec![XLit::Atom(k, RTerm::Fc(e, o))])]
        });
        let o = c(no, &d.ix);
        let cs = d.clauses_of(z);
        assert!(
            cs.iter()
                .any(|(bd, hd)| bd.is_empty() && hd == &vec![XLit::Atom(g, o)]),
            "{cs:?}"
        );
    }

    #[test]
    fn a_context_takes_a_successor_over_a_constant_through_its_own_edge() {
        // Z's context holds ⊤ → B(a) ∨ E(x), B ⊑ ∃r.C and C ⊑ ⊥: Z's own Succ over f(a)
        // reaches the hypothetical successor, which refutes C, and Pred along the edge
        // gives ⊤ → E(x), so Z ⊑ E
        let mut s = Sig::new();
        let na = s.ind("a");
        let [b, cc, ee, z] = ["B", "C", "E", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, cc);
        s.sub(b, e);
        s.sub(cc, BOTTOM);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            vec![(
                vec![],
                vec![XLit::Atom(b, c(na, ix)), XLit::Atom(ee, RTerm::X)],
            )]
        });
        assert!(d.subs(ee), "{:?}", d.clauses_of(z));
        assert!(!d.bottom());
    }

    #[test]
    fn pred_along_an_edge_over_a_constant_replays_in_either_order() {
        // the successor's clauses C(x) → ⊥ and C(x) → s(v, x) reach Z before or after Z
        // has ⊤ → B(a) ∨ E(x) with B ⊑ ∃r.C: Z derives ⊤ → E(x) and ⊤ → s(v, f(a)) ∨ E(x)
        // either way, the role atom between a constant and x returning along an edge over
        // a constant as Table 3 admits for the root
        for clause_first in [true, false] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let nv = s.ind("v");
            let [b, cc, ee, z] = ["B", "C", "E", "Z"].map(|n| s.class(n));
            let r = s.role("r");
            let t = s.role("s");
            let e = s.some_h(r, cc);
            s.sub(b, e);
            let (ix, _) = probe_ix(s);
            let h = dix(&ix).hyp[&e];
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let (a, v) = (c(na, &ix), c(nv, &ix));
            let inv = inv_of(&ix, dix(&ix));
            let l = |w: XLit| match run.lits.lit(w, inv) {
                Made::Lit(l) => l,
                _ => panic!("a literal"),
            };
            let k1 = run.table.push(Clause::new(vec![Lit::x(cc)], Vec::new()));
            let k2 = run.table.push(Clause::new(
                vec![Lit::x(cc)],
                vec![l(XLit::Role(t, v, RTerm::X))],
            ));
            let mut st = State {
                init: true,
                ..State::default()
            };
            let send = |st: &mut State, out: &mut Probe| {
                assert!(super::super::dl_rules::pred_c(&ix, z, st, h, k1, out));
                assert!(super::super::dl_rules::pred_c(&ix, z, st, h, k2, out));
                assert!(
                    !super::super::dl_rules::pred_c(&ix, z, st, h, k1, out),
                    "a clause is taken once"
                );
            };
            if clause_first {
                send(&mut st, &mut out);
                assert!(out.mine.is_empty());
            }
            ds(&mut st).store.add(Clause::new(
                Vec::new(),
                vec![l(XLit::Atom(b, a)), Lit::x(ee)],
            ));
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            if !clause_first {
                send(&mut st, &mut out);
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            }
            assert!(
                out.mine.contains(&Concl::Sub(ee)),
                "clause first {clause_first}: {:?}",
                out.mine
            );
            assert!(
                out.sent.contains(&(h, Concl::DEdge(z, e))),
                "the edge, clause first {clause_first}"
            );
            let RTerm::C(ua) = a else { panic!() };
            let want = l(XLit::Role(t, v, RTerm::Fc(e, ua)));
            let store = &dl(&st).unwrap().store;
            assert!(
                store
                    .clauses
                    .iter()
                    .any(|c| c.c.body().is_empty() && c.c.head().contains(&want)),
                "s(v, f(a)) ∨ E(x), clause first {clause_first}"
            );
        }
    }

    #[test]
    fn an_assertion_s_successor_meets_its_constant() {
        // r(a, o) as {a} ⊑ ∃r.{o}, {a} ⊑ ∀r.C, A ⊑ {o}: the root's successor for f(a) gives
        // f(a) ≈ o, the root then has r(a, o) and C(o), and A ⊑ C
        for root_first in [true, false] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let no = s.ind("o");
            let [a, cc] = ["A", "C"].map(|n| s.class(n));
            let r = s.role("r");
            let e = s.some_h(r, no);
            let all = s.all(r, cc);
            s.sub(na, e);
            s.sub(na, all);
            s.sub(a, no);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(d.subs(cc), "A ⊑ C, root first {root_first}");
            assert!(!d.bottom());
        }
    }

    #[test]
    fn a_successor_of_the_root_sends_back_what_holds_of_the_constant() {
        // {a} ⊑ ∃r.B, B ⊑ ∃s.{o}, {o} ⊑ ∀s⁻.G, G ⊑ ∀r⁻.K: K(a), and the root's successor
        // took the marker of the ground fact N_a(a), which the root's ⊤ → N_a(a) then
        // subsumes
        let mut s = Sig::new();
        let na = s.ind("a");
        let no = s.ind("o");
        let [b, g, k] = ["B", "G", "K"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("s");
        let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
        let ti = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:s")));
        let er = s.some_h(r, b);
        let et = s.some_h(t, no);
        let ag = s.all(ti, g);
        let ak = s.all(ri, k);
        s.sub(na, er);
        s.sub(b, et);
        s.sub(no, ag);
        s.sub(g, ak);
        let h = s.dl.hyp[&er];
        let d = saturate_root(s);
        let ua = c(na, &d.ix);
        assert!(d.has(&[XLit::Atom(k, ua)]), "K(a)");
        let m = XLit::Atom(na, ua);
        assert!(
            d.clauses_of(h)
                .iter()
                .any(|(bd, hd)| bd.iter().all(|l| *l == m) && hd == &vec![m]),
            "the marker of N_a(a)"
        );
        assert!(!d.bottom());
    }

    #[test]
    fn an_unsatisfiable_successor_of_the_root_makes_it_unsatisfiable() {
        // {a} ⊑ ∃r.D, D ⊑ E ⊔ F, E and F unsatisfiable
        let mut s = Sig::new();
        let na = s.ind("a");
        let [dd, ee, ff] = ["D", "E", "F"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, dd);
        s.sub(na, e);
        s.dl.clauses.push((vec![dd], vec![ee, ff]));
        s.sub(ee, BOTTOM);
        s.sub(ff, BOTTOM);
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn a_hypothetical_successor_s_unconditional_ground_fact_goes_to_the_root() {
        // the hypothetical successor of ∃r.D holds ⊤ → t(x, b) ∨ E(x) and ⊤ → t(x, b) under
        // ⊤ ⊑ ∀t.G: the clause with E(x) gives ⊤ → G(b) ∨ E(x), which the context keeps,
        // and the other gives the fact ⊤ → G(b), which goes to the root and is not kept
        let mut s = Sig::new();
        let nb = s.ind("b");
        let [dd, ee, g] = ["D", "E", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("t");
        let er = s.some_h(r, dd);
        let ag = s.all(t, g);
        s.sub(TOP, ag);
        let (ix, root) = probe_ix(s);
        let h = dix(&ix).hyp[&er];
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let ub = c(nb, &ix);
        let inv = inv_of(&ix, dix(&ix));
        let l = |w: XLit| match run.lits.lit(w, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        st.subs.insert(TOP);
        st.subs.insert(ag);
        let tb = l(XLit::Role(t, RTerm::X, ub));
        let gb = l(XLit::Atom(g, ub));
        let kept = |st: &State, head: &[Lit]| {
            dl(st).unwrap().store.clauses.iter().any(|c| {
                !c.dead && c.c.body().is_empty() && {
                    let mut h = c.c.head().to_vec();
                    h.sort_unstable();
                    let mut w = head.to_vec();
                    w.sort_unstable();
                    h == w
                }
            })
        };
        ds(&mut st)
            .store
            .add(Clause::new(Vec::new(), vec![tb, Lit::x(ee)]));
        super::super::dl_rules::flush(&ix, h, &mut st, &mut out, 100);
        assert!(kept(&st, &[gb, Lit::x(ee)]), "⊤ → G(b) ∨ E(x) is kept");
        assert!(
            !out.sent.iter().any(|(_, c)| matches!(c, Concl::RFact(_))),
            "a clause over x stays: {:?}",
            out.sent
        );
        ds(&mut st).store.add(Clause::new(Vec::new(), vec![tb]));
        super::super::dl_rules::flush(&ix, h, &mut st, &mut out, 100);
        let facts: Vec<&Clause> = out
            .sent
            .iter()
            .filter_map(|(to, c)| match c {
                Concl::RFact(cid) if *to == root => Some(run.table.get(*cid)),
                _ => None,
            })
            .collect();
        assert_eq!(facts.len(), 1, "one fact to the root: {:?}", out.sent);
        assert!(
            facts[0].body().is_empty() && facts[0].head() == [gb],
            "⊤ → G(b)"
        );
        assert!(!kept(&st, &[gb]), "the fact is not kept");
    }

    #[test]
    fn an_inconsistency_a_hypothetical_successor_finds_without_assumptions_reaches_the_root() {
        // {a} ⊑ ∃r.D, ⊤ ⊑ ∃t.{b}, ⊤ ⊑ ∀t.E, E ⊑ ⊥: every element has b as a t-successor, so
        // b is in E and the ontology is inconsistent, which the root's successor for f(a)
        // finds as the empty clause with an empty body
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [dd, ee] = ["D", "E"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("t");
        let er = s.some_h(r, dd);
        let et = s.some_h(t, nb);
        let ae = s.all(t, ee);
        s.sub(na, er);
        s.sub(TOP, et);
        s.sub(TOP, ae);
        s.sub(ee, BOTTOM);
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn a_selected_clause_headed_by_an_atom_over_x_still_takes_the_hyper_over_x() {
        // Q ⊑ ∃s.D, Q ⊑ ∃t.{a}, Q ⊑ ∀t.∀r.A, D ⊑ ∃r⁻.{a}, A ⊑ ⊥: a Q has ∀r.A at a and a D
        // reached from a by r, which is in A, so Q is unsatisfiable. The D context holds
        // the marker of ∀r.A(a) and the local role atom r(a, x), whose Hyper gives the
        // selected clause ∀r.A(a) → A(x), which Pred cannot carry, so the context itself
        // takes the Hyper over x to ∀r.A(a) → ⊥
        let mut s = Sig::new();
        let na = s.ind("a");
        let [q, dd, aa] = ["Q", "D", "A"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("t");
        let sr = s.role("s");
        let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
        let es = s.some_h(sr, dd);
        let et = s.some_h(t, na);
        let ara = s.all(r, aa);
        let atra = s.all(t, ara);
        let eri = s.some_h(ri, na);
        s.sub(q, es);
        s.sub(q, et);
        s.sub(q, atra);
        s.sub(dd, eri);
        s.sub(aa, BOTTOM);
        let d = saturate_at(s, Some(q));
        assert!(d.bottom(), "Q ⊑ ⊥");
    }

    /// The index and a probe over a signature whose root is made here.
    fn probe_ix(mut s: Sig) -> (Index, Ex) {
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        (
            Index::build(&s.ax, &mut s.it, None, &|| false).unwrap(),
            root,
        )
    }

    #[test]
    fn a_marker_is_no_premise_of_join() {
        // A(o) → A(o) and A(o) → B(o) in a context other than the root, in both orders:
        // Join derives nothing
        for marker_first in [true, false] {
            let mut s = Sig::new();
            let no = s.ind("o");
            let [a, b] = ["A", "B"].map(|n| s.class(n));
            let (ix, _) = probe_ix(s);
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let o = c(no, &ix);
            let inv = inv_of(&ix, dix(&ix));
            let l = |v: XLit| match run.lits.lit(v, inv) {
                Made::Lit(l) => l,
                _ => panic!("a literal"),
            };
            let (ao, bo) = (l(XLit::Atom(a, o)), l(XLit::Atom(b, o)));
            let mut st = State {
                init: true,
                ..State::default()
            };
            let mut cs = vec![
                Clause::new(vec![ao], vec![ao]),
                Clause::new(vec![ao], vec![bo]),
            ];
            if !marker_first {
                cs.reverse();
            }
            for c in cs {
                ds(&mut st).store.add(c);
            }
            super::super::dl_rules::flush(&ix, a, &mut st, &mut out, 100);
            assert_eq!(run.work(), 0, "marker first {marker_first}");
        }
    }

    #[test]
    fn the_root_s_state_is_counted_when_made() {
        let s = Sig::new();
        let (ix, _) = probe_ix(s);
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let mut st = State::default();
        init(&ix, &mut st, &mut out);
        assert!(dl(&st).unwrap().root.is_some());
        assert!(run.bytes() >= std::mem::size_of::<RootSt>());
    }

    #[test]
    fn every_predecessor_of_a_rooted_run_takes_a_clause_over_any_role_of_its_successor() {
        // a successor by r, with r not below s, holds s(y, x) → K(y): the root and another
        // predecessor by r get it whether they register before or after the clause, since
        // a predecessor may reach the successor over f_e(u) and finds the role premises
        // itself, and Pred over f_e(x) refuses the clause at the predecessor
        for register_first in [true, false] {
            let mut s = Sig::new();
            let _ = s.ind("a");
            let [a, b, k] = ["A", "B", "K"].map(|n| s.class(n));
            let r = s.role("r");
            let t = s.role("s");
            let e = s.some(r, b);
            let (ix, root) = probe_ix(s);
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let mut st = State {
                init: true,
                ..State::default()
            };
            let add = |st: &mut State, out: &mut Probe| {
                ds(st)
                    .store
                    .add(Clause::new(vec![Lit::ry(t)], vec![Lit::y(k)]));
                super::super::dl_rules::flush(&ix, b, st, out, 100);
            };
            if !register_first {
                add(&mut st, &mut out);
            }
            super::super::dl_rules::dedge(&ix, b, &mut st, root, e, &mut out);
            super::super::dl_rules::dedge(&ix, b, &mut st, a, e, &mut out);
            if register_first {
                add(&mut st, &mut out);
            }
            let to = |v: Ex| {
                out.sent
                    .iter()
                    .any(|&(w, c)| w == v && matches!(c, Concl::PredC(f, _) if f == b))
            };
            assert!(to(root), "the root, register first {register_first}");
            assert!(
                to(a),
                "another predecessor, register first {register_first}"
            );
        }
    }

    #[test]
    fn the_root_counts_the_clauses_its_successors_send() {
        let mut s = Sig::new();
        let _ = s.ind("a");
        let b = s.class("B");
        let (ix, _) = probe_ix(s);
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let cid = run.table.push(Clause::new(Vec::new(), vec![Lit::y(b)]));
        let mut st = State {
            init: true,
            ..State::default()
        };
        ds(&mut st);
        let before = run.bytes();
        let root = dix(&ix).root.unwrap();
        assert!(super::super::dl_rules::pred_c(
            &ix, root, &mut st, b, cid, &mut out
        ));
        assert!(
            !super::super::dl_rules::pred_c(&ix, root, &mut st, b, cid, &mut out),
            "a clause is taken once"
        );
        assert!(run.bytes() > before);
    }

    #[test]
    fn every_predecessor_of_a_rooted_run_takes_a_propagation_over_any_role_of_its_successor() {
        // B's context by r, with r not below s, holds ∀s⁻.K as a unit: the root and another
        // predecessor by r get s(y, x) → K(y) whether they register before or after the
        // unit, as above
        for register_first in [true, false] {
            let mut s = Sig::new();
            let _ = s.ind("a");
            let [a, b, k] = ["A", "B", "K"].map(|n| s.class(n));
            let r = s.role("r");
            let _ = s.role("s");
            let si = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:s")));
            let e = s.some(r, b);
            let all = s.all(si, k);
            let (ix, root) = probe_ix(s);
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let mut st = State {
                init: true,
                ..State::default()
            };
            ds(&mut st);
            if !register_first {
                super::super::dl_rules::sub(&ix, b, &mut st, all, &mut out);
            }
            super::super::dl_rules::dedge(&ix, b, &mut st, root, e, &mut out);
            super::super::dl_rules::dedge(&ix, b, &mut st, a, e, &mut out);
            if register_first {
                super::super::dl_rules::sub(&ix, b, &mut st, all, &mut out);
            }
            let to = |v: Ex| {
                out.sent
                    .iter()
                    .any(|&(w, c)| w == v && matches!(c, Concl::PredC(f, _) if f == b))
            };
            assert!(to(root), "the root, register first {register_first}");
            assert!(
                to(a),
                "another predecessor, register first {register_first}"
            );
        }
    }

    /// A probe run of the context `me` over the clauses `seed`, flushed, with the live
    /// clauses of its store in general form.
    fn probe_flush(
        s: Sig,
        me: impl Fn(Ex) -> Ex,
        seed: impl Fn(&Index, &dyn Fn(XLit) -> Lit) -> Vec<(Vec<XLit>, Vec<XLit>)>,
    ) -> Vec<(Vec<XLit>, Vec<XLit>)> {
        let (ix, root) = probe_ix(s);
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let inv = inv_of(&ix, dix(&ix));
        let l = |v: XLit| match run.lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        for (b, h) in seed(&ix, &l) {
            ds(&mut st).store.add(Clause::new(
                b.iter().copied().map(l).collect(),
                h.iter().copied().map(l).collect(),
            ));
        }
        let me = me(root);
        if me == root {
            flush(&ix, &mut st, &mut out, 100);
        } else {
            super::super::dl_rules::flush(&ix, me, &mut st, &mut out, 100);
        }
        let v = |ls: &[Lit]| ls.iter().map(|&l| run.lits.view(l, inv)).collect();
        dl(&st)
            .unwrap()
            .store
            .clauses
            .iter()
            .filter(|c| !c.dead)
            .map(|c| (v(c.c.body()), v(c.c.head())))
            .collect()
    }

    #[test]
    fn a_context_makes_no_ground_clause_with_a_body_from_its_markers() {
        // the markers A(a) → A(a) and B(a) → B(a) and A ⊓ B ⊑ H: Hyper at a would give
        // A(a) ∧ B(a) → H(a), a ground clause with a body, which the restriction on ground
        // inferences leaves out
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, h, z] = ["A", "B", "H", "Z"].map(|n| s.class(n));
        s.dl.clauses.push((vec![a, b], vec![h]));
        let cs = probe_flush(
            s,
            |_| z,
            |ix, _| {
                let u = c(na, ix);
                vec![
                    (vec![XLit::Atom(a, u)], vec![XLit::Atom(a, u)]),
                    (vec![XLit::Atom(b, u)], vec![XLit::Atom(b, u)]),
                ]
            },
        );
        let made = cs.iter().any(|(bd, hd)| {
            bd.len() == 2 && hd.iter().any(|l| matches!(l, XLit::Atom(k, _) if *k == h))
        });
        assert_eq!(made, !super::super::dl_rules::ground_restrict(), "{cs:?}");
    }

    #[test]
    fn a_disjoint_role_clash_over_x_keeps_its_ground_condition() {
        // A(a) → s(a, x) and ⊤ → t(a, x) with s and t disjoint: the Hyper binds y to x, so
        // it is no ground inference, and A(a) → ⊥ is made with the restriction on
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, z] = ["A", "Z"].map(|n| s.class(n));
        let r = s.role("s");
        let t = s.role("t");
        s.dl.rdis.push((r, t));
        let cs = probe_flush(
            s,
            |_| z,
            |ix, _| {
                let u = c(na, ix);
                vec![
                    (vec![XLit::Atom(a, u)], vec![XLit::Role(r, u, RTerm::X)]),
                    (vec![], vec![XLit::Role(t, u, RTerm::X)]),
                ]
            },
        );
        assert!(
            cs.iter().any(|(bd, hd)| hd.is_empty()
                && bd.len() == 1
                && matches!(bd[0], XLit::Atom(k, _) if k == a)),
            "{cs:?}"
        );
    }

    #[test]
    fn an_equality_over_a_successor_term_keeps_its_ground_condition() {
        // A(a) → f(x) ≈ u and ⊤ → B(f(x)): the equality holds x, so Eq is no ground
        // inference, and A(a) → B(u) is made with the restriction on
        let mut s = Sig::new();
        let na = s.ind("a");
        let nu = s.ind("u");
        let [a, b, k, z] = ["A", "B", "K", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, k);
        let cs = probe_flush(
            s,
            |_| z,
            |ix, _| {
                let (ua, uu) = (c(na, ix), c(nu, ix));
                vec![
                    (vec![XLit::Atom(a, ua)], vec![XLit::Eq(RTerm::Fx(e), uu)]),
                    (vec![], vec![XLit::Atom(b, RTerm::Fx(e))]),
                ]
            },
        );
        assert!(
            cs.iter().any(|(bd, hd)| {
                bd.len() == 1
                    && matches!(bd[0], XLit::Atom(k, _) if k == a)
                    && matches!(hd.as_slice(), [XLit::Atom(k, RTerm::C(_))] if *k == b)
            }),
            "{cs:?}"
        );
    }

    #[test]
    fn the_root_discharges_a_ground_body_by_join() {
        // ⊤ → A(a) and A(a) → D(a) in the root, as Pred along an edge over a constant
        // leaves a successor's marker in the body: Join gives ⊤ → D(a), which has no body
        // and so passes the restriction on ground inferences
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, dd] = ["A", "D"].map(|n| s.class(n));
        let cs = probe_flush(
            s,
            |root| root,
            |ix, _| {
                let u = c(na, ix);
                vec![
                    (vec![], vec![XLit::Atom(a, u)]),
                    (vec![XLit::Atom(a, u)], vec![XLit::Atom(dd, u)]),
                ]
            },
        );
        assert!(
            cs.iter()
                .any(|(bd, hd)| bd.is_empty()
                    && matches!(hd.as_slice(), [XLit::Atom(k, _)] if *k == dd)),
            "{cs:?}"
        );
    }

    /// A context of the class `at`, or the root, seeded with `seeds` and flushed, with its
    /// live clauses in general form. o is the greater constant of o and p.
    fn flushed_tautologies(at_root: bool) -> Vec<(Vec<XLit>, Vec<XLit>)> {
        let mut s = Sig::new();
        let np = s.ind("p");
        let no = s.ind("o");
        let [a, cc, g, z] = ["A", "C", "G", "Z"].map(|n| s.class(n));
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let (o, p) = (c(no, &ix), c(np, &ix));
        assert!(Rooted::a_gt(o, p));
        let inv = inv_of(&ix, dix(&ix));
        let l = |v: XLit| match run.lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        if at_root {
            ds(&mut st).root.get_or_insert_with(Box::default);
        }
        let seeds = [
            // Join on A(o) gives ⊤ → o ≈ p ∨ o ≉ p
            (vec![], vec![XLit::Atom(a, o), XLit::Eq(o, p)]),
            (vec![XLit::Atom(a, o)], vec![XLit::Ne(o, p)]),
            // Join on A(o) gives G(p) → C(o) ∨ G(p)
            (
                vec![XLit::Atom(g, p)],
                vec![XLit::Atom(a, o), XLit::Atom(cc, o)],
            ),
            (vec![XLit::Atom(a, o)], vec![XLit::Atom(g, p)]),
        ];
        for (b, h) in seeds {
            ds(&mut st).store.add(Clause::new(
                b.into_iter().map(l).collect(),
                h.into_iter().map(l).collect(),
            ));
        }
        if at_root {
            flush(&ix, &mut st, &mut out, 1000);
        } else {
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 1000);
        }
        let x = dl(&st).unwrap();
        x.store
            .clauses
            .iter()
            .filter(|s| !s.dead)
            .map(|s| {
                let v = |ls: &[Lit]| ls.iter().map(|&l| run.lits.view(l, inv)).collect();
                (v(s.c.body()), v(s.c.head()))
            })
            .collect()
    }

    #[test]
    fn a_ground_tautology_is_no_clause() {
        // Elim drops Γ ∧ A → Δ ∨ A and Γ → Δ ∨ s ≈ t ∨ s ≉ t whatever their literals, in
        // the root and in every other context, with counting off
        for at_root in [true, false] {
            let kept = flushed_tautologies(at_root);
            assert!(
                kept.iter().any(|(b, _)| b.is_empty()),
                "the seeds are kept, root {at_root}"
            );
            for (b, h) in &kept {
                assert!(
                    !h.iter().any(|l| b.contains(l)),
                    "{b:?} → {h:?} is a tautology, root {at_root}"
                );
            }
            for (b, h) in &kept {
                assert!(
                    !h.iter()
                        .any(|&e| matches!(e, XLit::Eq(s, t) if h.contains(&XLit::Ne(s, t)))),
                    "{b:?} → {h:?} is a tautology, root {at_root}"
                );
            }
        }
    }

    #[test]
    fn a_maximal_equality_to_a_constant_registers_for_it() {
        // {o} ⊑ {p} with o above p gives the root ⊤ → o ≈ p. A context whose only clause is
        // ⊤ → x ≈ o takes it, as the journal's r-Pred gives every context a root clause
        // without a body, and Eq gives it ⊤ → x ≈ p
        let mut s = Sig::new();
        let np = s.ind("p");
        let no = s.ind("o");
        let z = s.class("Z");
        s.sub(no, np);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            vec![(vec![], vec![XLit::Eq(RTerm::X, c(no, ix))])]
        });
        let (o, p) = (c(no, &d.ix), c(np, &d.ix));
        assert!(Rooted::a_gt(o, p));
        let has = |h: XLit| {
            d.clauses_of(z)
                .iter()
                .any(|(b, hs)| b.is_empty() && hs == &vec![h])
        };
        assert!(has(XLit::Eq(RTerm::X, o)), "the seed is kept");
        assert!(has(XLit::Eq(RTerm::X, p)), "{:?}", d.clauses_of(z));
    }

    #[test]
    fn stored_fact_adds_zero_padding_bytes() {
        // Stored::fact: bool must fit inside existing struct padding (0 B growth vs 1291).
        assert_eq!(
            std::mem::size_of::<super::super::dl_clause::Stored>(),
            64,
            "Stored should stay 64 bytes with fact: bool in existing padding"
        );
    }

    #[test]
    fn exact_root_unit_provenance_and_duplicate_marking() {
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, g, z] = ["A", "B", "G", "Z"].map(|n| s.class(n));
        let (ix, _) = probe_ix(s);
        let inv = inv_of(&ix, dix(&ix));
        let RTerm::C(u) = c(na, &ix) else {
            unreachable!()
        };

        // 1. Switch OFF: even an exact root unit does not acquire `fact = true`.
        {
            let run = DlRun::new(false, Default::default()).with_shared_facts(false);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let la = atom(&ix, &out, a, u);
            let cid = run.table.push(Clause::new(Vec::new(), vec![la]));
            let mut st = State {
                init: true,
                ..State::default()
            };
            assert!(rpred_c(&ix, &mut st, cid, &mut out));
            let x = dl(&st).unwrap();
            assert_eq!(x.store.clauses.len(), 1);
            assert!(!x.store.clauses[0].fact);
        }

        // 2. Switch ON: exact root unit acquires `fact = true`; disjunctive root clause
        //    and conditional unit do NOT; pre-existing live exact duplicate (both queued
        //    and already processed) is marked `fact = true` in place with zero byte growth.
        for already_processed in [false, true] {
            let run = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let la = atom(&ix, &out, a, u);
            let lb = atom(&ix, &out, b, u);
            let lg = atom(&ix, &out, g, u);
            let mut st = State {
                init: true,
                ..State::default()
            };
            // Seed a local copy of `→ A(u)` (`fact = false`) and a conditional `G(u) → B(u)`.
            assert!(
                ds(&mut st)
                    .store
                    .add_parts_admitted(vec![], vec![la], &mut out)
            );
            assert!(
                ds(&mut st)
                    .store
                    .add_parts_admitted(vec![lg], vec![lb], &mut out)
            );
            assert!(!dl(&st).unwrap().store.clauses[0].fact);
            assert!(!dl(&st).unwrap().store.clauses[1].fact);

            if already_processed {
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
                assert!(dl(&st).unwrap().store.clauses[0].done);
            }

            let store_bytes_before = dl(&st).unwrap().store.bytes();
            let cid_unit = run.table.push(Clause::new(Vec::new(), vec![la]));
            assert!(rpred_c(&ix, &mut st, cid_unit, &mut out));
            assert_eq!(
                dl(&st).unwrap().store.bytes(),
                store_bytes_before,
                "duplicate unit marking must allocate zero store bytes"
            );
            assert!(
                dl(&st).unwrap().store.clauses[0].fact,
                "live exact duplicate unit must be marked fact=true (processed={already_processed})"
            );
            assert!(
                !dl(&st).unwrap().store.clauses[1].fact,
                "conditional unit must never become fact=true"
            );

            // A disjunctive empty-body root clause `→ B(u) ∨ G(u)` stays `fact = false`.
            let cid_disj = run.table.push(Clause::new(Vec::new(), vec![lb, lg]));
            assert!(rpred_c(&ix, &mut st, cid_disj, &mut out));
            let disj_clause = dl(&st)
                .unwrap()
                .store
                .clauses
                .iter()
                .find(|cl| !cl.dead && cl.c.body().is_empty() && cl.c.head().len() == 2)
                .expect("disjunctive root clause stored");
            assert!(
                !disj_clause.fact,
                "disjunctive root fact must NOT acquire unit-only fact=true"
            );
            assert!(
                dl(&st).unwrap().store.scan_bytes() == dl(&st).unwrap().store.bytes(),
                "store owned_bytes must stay exact"
            );
            let _ = inv;
        }
    }

    #[test]
    fn a_fact_of_the_root_alone_gives_no_inference_in_another_context() {
        // A ⊑ B: a certified root unit A(u) produces no local B(u) when shared_facts is ON,
        // and DOES produce B(u) when shared_facts is OFF or when A(u) is a local clause.
        for (shared_on, via_rpred, want_b) in [
            (true, true, false),
            (false, true, true),
            (true, false, true),
        ] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let [a, b, z] = ["A", "B", "Z"].map(|n| s.class(n));
            s.sub(a, b);
            let (ix, _) = probe_ix(s);
            let run = DlRun::new(false, Default::default()).with_shared_facts(shared_on);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let RTerm::C(u) = c(na, &ix) else {
                unreachable!()
            };
            let la = atom(&ix, &out, a, u);
            let lb = atom(&ix, &out, b, u);
            let mut st = State {
                init: true,
                ..State::default()
            };
            if via_rpred {
                let cid = run.table.push(Clause::new(Vec::new(), vec![la]));
                rpred_c(&ix, &mut st, cid, &mut out);
            } else {
                ds(&mut st).store.add(Clause::new(Vec::new(), vec![la]));
            }
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            assert!(!dl(&st).unwrap().given_fact, "given_fact cleared on exit");
            let has_b = dl(&st)
                .unwrap()
                .store
                .clauses
                .iter()
                .any(|cl| !cl.dead && cl.c.body().is_empty() && cl.c.head() == [lb]);
            assert_eq!(
                has_b, want_b,
                "shared_on={shared_on}, via_rpred={via_rpred}"
            );
        }
    }

    #[test]
    fn two_root_units_suppress_unit_conclusions_but_keep_mixed_multihead_and_bottom() {
        // Test:
        // - A ⊓ B ⊑ C (conj_of) -> unit C(u) suppressed for root+root (both orders), kept for mixed.
        // - by_body Horn `A ∧ B → H` -> unit H(u) suppressed for root+root, kept for mixed.
        // - by_body Disj `A ∧ B → D1 ∨ D2` -> multihead ALWAYS kept locally (Astra #2).
        // - A ⊓ E ⊑ ⊥ (disjoint_of) -> bottom ALWAYS kept locally (Astra #2).
        for (a_root, b_root, swap_order) in [
            (true, true, false),
            (true, true, true),
            (true, false, false),
            (false, true, false),
        ] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let [a, b, cc, hh, d1, d2, z] =
                ["A", "B", "C", "H", "D1", "D2", "Z"].map(|n| s.class(n));
            let ab = s.it.conj(a, b);
            s.sub(ab, cc);
            s.dl.clauses.push((vec![a, b], vec![hh]));
            s.dl.clauses.push((vec![a, b], vec![d1, d2]));
            let (ix, _) = probe_ix(s);
            let run = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let RTerm::C(u) = c(na, &ix) else {
                unreachable!()
            };
            let la = atom(&ix, &out, a, u);
            let lb = atom(&ix, &out, b, u);
            let lc = atom(&ix, &out, cc, u);
            let lh = atom(&ix, &out, hh, u);
            let ld1 = atom(&ix, &out, d1, u);
            let ld2 = atom(&ix, &out, d2, u);
            let mut st = State {
                init: true,
                ..State::default()
            };
            let add_one = |st: &mut State, out: &mut Probe<'_>, lit: Lit, as_root: bool| {
                if as_root {
                    let cid = run.table.push(Clause::new(Vec::new(), vec![lit]));
                    rpred_c(&ix, st, cid, out);
                } else {
                    ds(st).store.add(Clause::new(Vec::new(), vec![lit]));
                }
            };
            if swap_order {
                add_one(&mut st, &mut out, lb, b_root);
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
                add_one(&mut st, &mut out, la, a_root);
            } else {
                add_one(&mut st, &mut out, la, a_root);
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
                add_one(&mut st, &mut out, lb, b_root);
            }
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            let has_unit = |l: Lit| {
                dl(&st)
                    .unwrap()
                    .store
                    .clauses
                    .iter()
                    .any(|cl| !cl.dead && cl.c.body().is_empty() && cl.c.head() == [l])
            };
            let has_disj = dl(&st).unwrap().store.clauses.iter().any(|cl| {
                !cl.dead
                    && cl.c.body().is_empty()
                    && cl.c.head().len() == 2
                    && cl.c.head().contains(&ld1)
                    && cl.c.head().contains(&ld2)
            });
            let both_root = a_root && b_root;
            assert_eq!(
                has_unit(lc),
                !both_root,
                "conj_of C(u): a_root={a_root}, b_root={b_root}, swap={swap_order}"
            );
            assert_eq!(
                has_unit(lh),
                !both_root,
                "by_body Horn H(u): a_root={a_root}, b_root={b_root}, swap={swap_order}"
            );
            assert!(
                has_disj,
                "by_body multihead D1(u) ∨ D2(u) must stay local: a_root={a_root}, b_root={b_root}"
            );
        }

        // Disjoint root units keep bottom locally.
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, e, z] = ["A", "E", "Z"].map(|n| s.class(n));
        s.ax.disjoint.push(vec![a, e]);
        let (ix, _) = probe_ix(s);
        let run = DlRun::new(false, Default::default()).with_shared_facts(true);
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let RTerm::C(u) = c(na, &ix) else {
            unreachable!()
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        for cl in [a, e] {
            let cid = run
                .table
                .push(Clause::new(Vec::new(), vec![atom(&ix, &out, cl, u)]));
            rpred_c(&ix, &mut st, cid, &mut out);
        }
        super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
        assert!(
            out.mine.contains(&Concl::Sub(BOTTOM)),
            "disjoint root units must still derive bottom locally"
        );
    }

    #[test]
    fn a_conclusion_with_an_unregistered_constant_stays_local() {
        // Root unit N_v(u) (nominal v != u) derives `v ≈ u`. If the context is NOT
        // registered for v, `v ≈ u` must stay local; if v IS registered, it is suppressed.
        for pre_register_v in [false, true] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let nb = s.ind("b");
            let z = s.class("Z");
            let (ix, _) = probe_ix(s);
            let run = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let RTerm::C(u) = c(na, &ix) else {
                unreachable!()
            };
            let RTerm::C(v) = c(nb, &ix) else {
                unreachable!()
            };
            let mut st = State {
                init: true,
                ..State::default()
            };
            if pre_register_v {
                rst(&mut st, &out).edged.insert(v);
            }
            let nv_u = atom(&ix, &out, nb, u);
            let cid = run.table.push(Clause::new(Vec::new(), vec![nv_u]));
            rpred_c(&ix, &mut st, cid, &mut out);
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            let Made::Lit(eq_uv) = lit(&ix, &out, XLit::Eq(RTerm::C(u), RTerm::C(v))) else {
                unreachable!()
            };
            let has_eq = dl(&st)
                .unwrap()
                .store
                .clauses
                .iter()
                .any(|cl| !cl.dead && cl.c.body().is_empty() && cl.c.head() == [eq_uv]);
            assert_eq!(has_eq, !pre_register_v, "pre_register_v={pre_register_v}");
        }
    }

    #[test]
    fn root_equality_rewrites_no_root_unit_but_rewrites_local_premise() {
        // u ≈ v (u > v) and A(u): both root -> A(v) suppressed (both orders);
        // mixed (one local) -> A(v) derived locally.
        for (eq_root, a_root, eq_first) in [
            (true, true, true),
            (true, true, false),
            (true, false, true),
            (false, true, false),
        ] {
            let mut s = Sig::new();
            let nv = s.ind("v");
            let nu = s.ind("u");
            let [a, z] = ["A", "Z"].map(|n| s.class(n));
            let (ix, _) = probe_ix(s);
            let run = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let RTerm::C(u) = c(nu, &ix) else {
                unreachable!()
            };
            let RTerm::C(v) = c(nv, &ix) else {
                unreachable!()
            };
            assert!(u > v);
            let Made::Lit(eq_uv) = lit(&ix, &out, XLit::Eq(RTerm::C(u), RTerm::C(v))) else {
                unreachable!()
            };
            let au = atom(&ix, &out, a, u);
            let av = atom(&ix, &out, a, v);
            let mut st = State {
                init: true,
                ..State::default()
            };
            // Pre-register v when A(u) arrives first so `routable_unit` sees both u and v.
            rst(&mut st, &out).edged.insert(v);
            let push = |st: &mut State, out: &mut Probe<'_>, l: Lit, as_root: bool| {
                if as_root {
                    let cid = run.table.push(Clause::new(Vec::new(), vec![l]));
                    rpred_c(&ix, st, cid, out);
                } else {
                    ds(st).store.add(Clause::new(Vec::new(), vec![l]));
                }
            };
            if eq_first {
                push(&mut st, &mut out, eq_uv, eq_root);
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
                push(&mut st, &mut out, au, a_root);
            } else {
                push(&mut st, &mut out, au, a_root);
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
                push(&mut st, &mut out, eq_uv, eq_root);
            }
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            let has_av = dl(&st)
                .unwrap()
                .store
                .clauses
                .iter()
                .any(|cl| !cl.dead && cl.c.body().is_empty() && cl.c.head() == [av]);
            assert_eq!(
                has_av,
                !(eq_root && a_root),
                "eq_root={eq_root}, a_root={a_root}, eq_first={eq_first}"
            );
        }
    }

    #[test]
    fn pred_to_non_root_predecessor_preserved_and_to_root_omitted_for_root_unit() {
        // Gap G6: a certified root unit in a successor is STILL sent via PredC to a non-root
        // predecessor (both live and late via dedge), and kept in `store.preds`; only sends
        // back to the root itself (which already holds the unit) are omitted.
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, p, z] = ["A", "P", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, a);
        let (ix, root) = probe_ix(s);
        let run = DlRun::new(false, Default::default()).with_shared_facts(true);
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let RTerm::C(u) = c(na, &ix) else {
            unreachable!()
        };
        let la = atom(&ix, &out, a, u);
        let mut st = State {
            init: true,
            ..State::default()
        };
        // Register both `root` and non-root predecessor `p` before processing the unit.
        assert!(super::super::dl_rules::dedge(
            &ix, z, &mut st, root, e, &mut out
        ));
        assert!(super::super::dl_rules::dedge(
            &ix, z, &mut st, p, e, &mut out
        ));
        out.sent.clear();

        let cid_in = run.table.push(Clause::new(Vec::new(), vec![la]));
        rpred_c(&ix, &mut st, cid_in, &mut out);
        super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);

        let sent_to_p = out
            .sent
            .iter()
            .any(|&(to, c)| to == p && matches!(c, Concl::PredC(from, _) if from == z));
        let sent_to_root = out
            .sent
            .iter()
            .any(|&(to, c)| to == root && matches!(c, Concl::PredC(from, _) if from == z));
        assert!(sent_to_p, "non-root predecessor must receive PredC for G6");
        assert!(
            !sent_to_root,
            "root itself must not be re-sent its own root unit via PredC"
        );
        assert!(
            !dl(&st).unwrap().store.preds.is_empty(),
            "root unit must remain in store.preds for late non-root predecessors"
        );

        // A late non-root predecessor `a` receives it via dedge replay; root does not.
        out.sent.clear();
        assert!(super::super::dl_rules::dedge(
            &ix, z, &mut st, a, e, &mut out
        ));
        assert!(
            out.sent
                .iter()
                .any(|&(to, c)| to == a && matches!(c, Concl::PredC(from, _) if from == z)),
            "late non-root predecessor must receive root unit from store.preds"
        );
    }

    #[test]
    fn constants_only_clauses_emit_once_and_honor_edge_eligibility_and_bottom() {
        // D4: constants-only clauses emit at most once per successor across multiple rooted
        // edges and ordinary/rooted transitions; markers are skipped; cache-before-edge
        // replays on the first eligible edge; empty clause still delivers bottom.
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [a, b, z] = ["A", "B", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let r2 = s.role("r2");
        let e1 = s.some_h(r, a);
        let e2 = s.some_h(r2, a);
        let e_nom = s.some_h(r, na);
        let (ix, _) = probe_ix(s);
        let dx = dix(&ix);
        assert!(
            super::super::dl_rules::into_nominal(&ix, e_nom).is_some(),
            "e_nom must be an into_nominal existential"
        );
        assert!(
            super::super::dl_rules::into_nominal(&ix, e1).is_none()
                && super::super::dl_rules::into_nominal(&ix, e2).is_none()
        );
        let w = dx.hyp[&e1];
        let w_nom = ix.head(e_nom).succ;
        let run = DlRun::new(false, Default::default()).with_shared_facts(true);
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let RTerm::C(ua) = c(na, &ix) else {
            unreachable!()
        };
        let RTerm::C(ub) = c(nb, &ix) else {
            unreachable!()
        };
        let la = atom(&ix, &out, a, ua);
        let lb = atom(&ix, &out, b, ub);

        // 1. Cache-before-edge: clause arrives with NO eligible edge -> cached, 0 work!
        let mut st = State {
            init: true,
            ..State::default()
        };
        let cid_unit = run.table.push(Clause::new(Vec::new(), vec![lb]));
        let cid_marker = run.table.push(Clause::new(vec![la], vec![la]));
        assert!(super::super::dl_rules::pred_c(
            &ix, z, &mut st, w, cid_unit, &mut out
        ));
        assert!(super::super::dl_rules::pred_c(
            &ix, z, &mut st, w, cid_marker, &mut out
        ));
        assert_eq!(
            run.work(),
            0,
            "no eligible edge yet: cached without deriving"
        );

        // 2. First rooted edge `(e1, ua)` to `w`: replays `cid_unit` ONCE, skips marker!
        let mut g = Guard::new(&mut out);
        let mut found = Found::default();
        succ_at(&ix, z, &mut st, e1, ua, &mut found, &mut g);
        drain_admitted(&ix, &mut st, found, &mut g);
        drop(g);
        assert_eq!(
            run.work(),
            1,
            "first eligible edge emits constants-only once and skips marker"
        );

        // 3. Second rooted edge `(e1, ub)` to the same `w`: emits 0 times!
        let mut g = Guard::new(&mut out);
        let mut found = Found::default();
        succ_at(&ix, z, &mut st, e1, ub, &mut found, &mut g);
        drain_admitted(&ix, &mut st, found, &mut g);
        drop(g);
        assert_eq!(
            run.work(),
            1,
            "second rooted edge must not re-emit constants-only clause"
        );

        // 4. New constants-only clause arriving over two edges: emits ONCE!
        let cid_unit2 = run.table.push(Clause::new(Vec::new(), vec![la]));
        assert!(super::super::dl_rules::pred_c(
            &ix, z, &mut st, w, cid_unit2, &mut out
        ));
        assert_eq!(
            run.work(),
            2,
            "pred_c over two edges emits constants-only once"
        );

        // 5. Empty clause `[] → []` still fires and delivers BOTTOM!
        let cid_empty = run.empty();
        assert!(super::super::dl_rules::pred_c(
            &ix, z, &mut st, w, cid_empty, &mut out
        ));
        assert!(
            out.mine.contains(&Concl::Sub(BOTTOM)),
            "empty clause must still deliver bottom under D4"
        );

        // 6. Ordinary -> rooted transition (in default mode 2): once a real ordinary edge
        //    exists to `w`, a subsequent rooted edge `(e1, ua)` to `w` must NOT re-emit.
        super::super::dl_rules::with_late_unit_mode_override(2, || {
            let mut st2 = State {
                init: true,
                ..State::default()
            };
            let run2 = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out2 = Probe {
                run: &run2,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let lb2 = atom(&ix, &out2, b, ub);
            let cid = run2.table.push(Clause::new(Vec::new(), vec![lb2]));
            ds(&mut st2).insert_edge(e1, w);
            assert!(super::super::dl_rules::pred_c(
                &ix, z, &mut st2, w, cid, &mut out2
            ));
            assert_eq!(run2.work(), 1, "ordinary edge emits constants-only once");
            let mut g2 = Guard::new(&mut out2);
            let mut found2 = Found::default();
            succ_at(&ix, z, &mut st2, e1, ua, &mut found2, &mut g2);
            drain_admitted(&ix, &mut st2, found2, &mut g2);
            drop(g2);
            assert_eq!(
                run2.work(),
                1,
                "rooted edge after existing ordinary edge must not re-emit constants-only clause"
            );

            // 7. `into_nominal` existential edge exclusion: an `into_nominal` edge in
            //    `x.edges` or `st.subs` is NOT an eligible ordinary edge and must not emit
            //    in `pred_c` or suppress subsequent replay.
            let mut st_nom = State {
                init: true,
                ..State::default()
            };
            let run_nom = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out_nom = Probe {
                run: &run_nom,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let lb_nom = atom(&ix, &out_nom, b, ub);
            let cid_nom = run_nom.table.push(Clause::new(Vec::new(), vec![lb_nom]));
            ds(&mut st_nom).insert_edge(e_nom, w_nom);
            st_nom.subs.insert(e_nom);
            assert!(
                !super::super::dl_rules::has_eligible_ordinary_edge(&ix, dx, &st_nom, w_nom),
                "into_nominal existential must not count as an eligible ordinary edge"
            );
            assert!(super::super::dl_rules::pred_c(
                &ix,
                z,
                &mut st_nom,
                w_nom,
                cid_nom,
                &mut out_nom
            ));
            assert_eq!(
                run_nom.work(),
                0,
                "pred_c with only an into_nominal edge must not emit constants-only clause"
            );
            // Also verify an into_nominal edge pointing at `w` does not block `succ_at` replay.
            ds(&mut st_nom).insert_edge(e_nom, w);
            assert!(!super::super::dl_rules::has_eligible_ordinary_edge(
                &ix, dx, &st_nom, w
            ));
            assert!(super::super::dl_rules::pred_c(
                &ix,
                z,
                &mut st_nom,
                w,
                cid_nom,
                &mut out_nom
            ));
            assert_eq!(run_nom.work(), 0);
            let mut g_nom = Guard::new(&mut out_nom);
            let mut f_nom = Found::default();
            succ_at(&ix, z, &mut st_nom, e1, ua, &mut f_nom, &mut g_nom);
            drain_admitted(&ix, &mut st_nom, f_nom, &mut g_nom);
            drop(g_nom);
            assert_eq!(
                run_nom.work(),
                1,
                "first real rooted edge after an into_nominal edge must replay cached clause"
            );

            // 8. `late("edge")` cache-before-edge replay in mode 2, plus second ordinary edge
            //    and rooted -> ordinary transition suppression via `pred_pending`.
            let mut st_late = State {
                init: true,
                ..State::default()
            };
            let run_late = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out_late = Probe {
                run: &run_late,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let lb_l = atom(&ix, &out_late, b, ub);
            let la_l = atom(&ix, &out_late, a, ua);
            let cid_l = run_late.table.push(Clause::new(Vec::new(), vec![lb_l]));
            let cid_lm = run_late.table.push(Clause::new(vec![la_l], vec![la_l]));
            assert!(super::super::dl_rules::pred_c(
                &ix,
                z,
                &mut st_late,
                w,
                cid_l,
                &mut out_late
            ));
            assert!(super::super::dl_rules::pred_c(
                &ix,
                z,
                &mut st_late,
                w,
                cid_lm,
                &mut out_late
            ));
            assert_eq!(run_late.work(), 0);
            // First eligible ordinary edge `(e1, w)` triggers `late("edge")`: emits ONCE, skips marker.
            ds(&mut st_late).insert_edge(e1, w);
            let mut gl1 = Guard::new(&mut out_late);
            let mut fl1 = Found::default();
            assert!(super::super::dl_rules::late(
                &ix,
                z,
                &mut st_late,
                e1,
                w,
                "edge",
                &mut fl1,
                &mut gl1
            ));
            drain_admitted(&ix, &mut st_late, fl1, &mut gl1);
            drop(gl1);
            assert_eq!(
                run_late.work(),
                1,
                "late(\"edge\") on first eligible ordinary edge emits once and skips marker"
            );
            // Second eligible ordinary edge `(e2, w)` triggers `late("edge")`: suppressed!
            ds(&mut st_late).insert_edge(e2, w);
            let mut gl2 = Guard::new(&mut out_late);
            let mut fl2 = Found::default();
            assert!(super::super::dl_rules::late(
                &ix,
                z,
                &mut st_late,
                e2,
                w,
                "edge",
                &mut fl2,
                &mut gl2
            ));
            drain_admitted(&ix, &mut st_late, fl2, &mut gl2);
            drop(gl2);
            assert_eq!(
                run_late.work(),
                1,
                "late(\"edge\") on second ordinary edge must not duplicate constants-only clause"
            );

            // 9. `late("unit")` cache-before-edge replay on EL successor `a = head(e1).succ`,
            //    followed by a second unit `e2` sharing the same EL successor `a` (suppressed!).
            let succ_a = ix.head(e1).succ;
            assert_eq!(succ_a, ix.head(e2).succ);
            let mut st_u = State {
                init: true,
                ..State::default()
            };
            let run_u = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out_u = Probe {
                run: &run_u,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let lb_u = atom(&ix, &out_u, b, ub);
            let cid_u = run_u.table.push(Clause::new(Vec::new(), vec![lb_u]));
            assert!(super::super::dl_rules::pred_c(
                &ix, z, &mut st_u, succ_a, cid_u, &mut out_u
            ));
            assert_eq!(run_u.work(), 0);
            st_u.subs.insert(e1);
            let mut gu1 = Guard::new(&mut out_u);
            let mut fu1 = Found::default();
            assert!(super::super::dl_rules::late(
                &ix, z, &mut st_u, e1, succ_a, "unit", &mut fu1, &mut gu1
            ));
            drain_admitted(&ix, &mut st_u, fu1, &mut gu1);
            drop(gu1);
            assert_eq!(
                run_u.work(),
                1,
                "late(\"unit\") on first unit existential replays cached constants-only clause once"
            );
            st_u.subs.insert(e2);
            let mut gu2 = Guard::new(&mut out_u);
            let mut fu2 = Found::default();
            assert!(super::super::dl_rules::late(
                &ix, z, &mut st_u, e2, succ_a, "unit", &mut fu2, &mut gu2
            ));
            drain_admitted(&ix, &mut st_u, fu2, &mut gu2);
            drop(gu2);
            assert_eq!(
                run_u.work(),
                1,
                "late(\"unit\") on second unit existential to same successor must not duplicate"
            );

            // 10. Rooted -> ordinary transition: once rooted edge `(e1, ua)` to `w` already
            //     replayed the cached clause, a subsequent `late("edge")` to `w` must NOT duplicate!
            let mut st_ro = State {
                init: true,
                ..State::default()
            };
            let run_ro = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out_ro = Probe {
                run: &run_ro,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let lb_ro = atom(&ix, &out_ro, b, ub);
            let cid_ro = run_ro.table.push(Clause::new(Vec::new(), vec![lb_ro]));
            assert!(super::super::dl_rules::pred_c(
                &ix,
                z,
                &mut st_ro,
                w,
                cid_ro,
                &mut out_ro
            ));
            assert_eq!(run_ro.work(), 0);
            let mut gro1 = Guard::new(&mut out_ro);
            let mut fro1 = Found::default();
            succ_at(&ix, z, &mut st_ro, e1, ua, &mut fro1, &mut gro1);
            drain_admitted(&ix, &mut st_ro, fro1, &mut gro1);
            drop(gro1);
            assert_eq!(run_ro.work(), 1, "first rooted edge replays once");
            ds(&mut st_ro).insert_edge(e1, w);
            let mut gro2 = Guard::new(&mut out_ro);
            let mut fro2 = Found::default();
            assert!(super::super::dl_rules::late(
                &ix, z, &mut st_ro, e1, w, "edge", &mut fro2, &mut gro2
            ));
            drain_admitted(&ix, &mut st_ro, fro2, &mut gro2);
            drop(gro2);
            assert_eq!(
                run_ro.work(),
                1,
                "ordinary edge via late(\"edge\") after rooted edge must not re-emit constants-only clause"
            );
        });
    }

    #[test]
    fn g6_private_marker_and_existential_unit_with_dl_steps_off_and_repair_on() {
        // Astra §1 + Sol r1 P1 + Neon r1 Note 2 gate witness:
        // - Non-Horn (`Z2 ⊑ W1 ⊑ W2 ⊑ U1 ∨ U2`, `U1, U2 ⊑ ∃p.V`) so classification genuinely
        //   enters rooted Tier D (`Tier::Disjunctive`, `dl_work > 0`).
        // - Early predecessor `:Z` (via EL `Z ⊑ ∃p.V` -> `st.back`) and late predecessor `:Z2`
        //   (via multi-hop DL disjunction -> `x.dpreds`/`dedge`) both hold private conditional
        //   ground fact `G(c)` and neither mentions `:b`.
        // - Child `:V` reaches `:b`, receives root unit `E(b) = (∃r.A)(b)` via `RPred`, and
        //   must preserve `PredC` (live + late replay) to `:Z` and `:Z2`.
        // - Successor of `E = ∃r.A` requires BOTH `A(x)` and `s(y, x)` (via `r ⊑ s`) to derive
        //   `:Step1`, which feeds `:Hit` through anonymous successor `:M` combining with private
        //   `G(c)` (keeping the root free of any empty-body fact linking `b` and `c`), returning
        //   `A(x) ∧ s(y, x) ∧ G(c) → BHit(y)` to `:Z` / `:Z2`, which resolves `BHit(b) → L(c)`
        //   and entails `:Z ⊑ :Ans` and `:Z2 ⊑ :Ans`.
        use crate::ofn_read;
        let build_ofn = |with_r_sub_s: bool| {
            let sub_prop = if with_r_sub_s {
                "  SubObjectPropertyOf(:r :s)\n"
            } else {
                ""
            };
            format!(
                r#"Prefix(:=<http://example.org/g6#>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Ontology(<http://example.org/g6>
  SubClassOf(:Z ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :G)))
  SubClassOf(:Z ObjectSomeValuesFrom(:p :V))
  SubClassOf(:Z2 ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :G)))
  SubClassOf(:Z2 ObjectSomeValuesFrom(:d1 :L1))
  SubClassOf(:L1 ObjectUnionOf(:L1a :L1b))
  SubClassOf(:L1a ObjectSomeValuesFrom(:d2 :L2))
  SubClassOf(:L1b ObjectSomeValuesFrom(:d2 :L2))
  SubClassOf(:L2 ObjectUnionOf(:L2a :L2b))
  SubClassOf(:L2a :L2Done)
  SubClassOf(:L2b :L2Done)
  SubClassOf(ObjectSomeValuesFrom(:d2 :L2Done) :L1Done)
  SubClassOf(ObjectSomeValuesFrom(:d1 :L1Done) :W3)
  SubClassOf(:W3 ObjectSomeValuesFrom(:p :V))
  SubClassOf(:V ObjectUnionOf(ObjectSomeValuesFrom(:k ObjectOneOf(:b)) :NoB))
  ClassAssertion(ObjectSomeValuesFrom(:r :A) :b)
{sub_prop}  SubClassOf(ObjectIntersectionOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:s) owl:Thing)) :Step1)
  SubClassOf(:Step1 ObjectSomeValuesFrom(:m :M))
  SubClassOf(:M ObjectSomeValuesFrom(:n ObjectOneOf(:c)))
  SubClassOf(ObjectSomeValuesFrom(:n :G) :HitM)
  SubClassOf(ObjectSomeValuesFrom(:m :HitM) :Hit)
  SubClassOf(:Hit ObjectOneOf(:c))
  SubClassOf(ObjectSomeValuesFrom(:q :A) :Ans)
)
"#
            )
        };
        let ax = ofn_read::parse(&build_ofn(true)).unwrap().axioms;
        let cls = |n: &str| crate::model::Class::new(format!("http://example.org/g6#{n}"));
        let z = cls("Z");
        let z2 = cls("Z2");
        let ans = cls("Ans");
        let named = [
            "Z", "Z2", "L1", "L1a", "L1b", "L2", "L2a", "L2b", "L2Done", "L1Done", "W3", "V",
            "NoB", "A", "Step1", "M", "G", "HitM", "Hit", "Ans",
        ];
        // Exact non-trivial OWL 2 DL (HermiT) subsumptions among the 22 named classes:
        // - Z ⊑ Ans, Z2 ⊑ Ans (via private G(c) + b ∈ ∃r.A + r ⊑ s -> Step1 -> M -> HitM -> Hit ⊑ {c} -> A(c))
        // - L2a ⊑ L2Done, L2b ⊑ L2Done, L2 ⊑ L2Done (since L2 ⊑ L2a ⊔ L2b)
        // - L1a ⊑ L1Done, L1b ⊑ L1Done, L1 ⊑ L1Done (since L1a,L1b ⊑ ∃d2.L2 ⊑ ∃d2.L2Done ⊑ L1Done)
        // - Z2 ⊑ W3 (since Z2 ⊑ ∃d1.L1 ⊑ ∃d1.L1Done ⊑ W3)
        let expected_pairs: std::collections::BTreeSet<(&str, &str)> = [
            ("Z", "Ans"),
            ("Z2", "Ans"),
            ("Z2", "W3"),
            ("L1", "L1Done"),
            ("L1a", "L1Done"),
            ("L1b", "L1Done"),
            ("L2", "L2Done"),
            ("L2a", "L2Done"),
            ("L2b", "L2Done"),
        ]
        .into_iter()
        .collect();

        for (threads, expansion, dl_steps, repair) in [
            (1, super::super::Expansion::Cautious, true, false),
            (4, super::super::Expansion::Cautious, false, false),
            (1, super::super::Expansion::Eager, true, false),
            (1, super::super::Expansion::Cautious, true, true),
            (4, super::super::Expansion::Eager, false, true),
        ] {
            let limits = super::super::Limits {
                threads,
                expansion,
                rooted: true,
                rooted_decline: false,
                dl_steps,
                rooted_marker_repair: repair,
                ..super::super::Limits::default()
            };
            let (off, off_excl) = with_marker_diag_override(repair, || {
                super::super::dl_rules::with_shared_facts_override(false, || {
                    let mut k = super::super::Classifier::of(&ax);
                    (k.classify(&limits).unwrap(), k.excluded().len())
                })
            });
            let (on, on_excl) = with_marker_diag_override(repair, || {
                super::super::dl_rules::with_shared_facts_override(true, || {
                    let mut k = super::super::Classifier::of(&ax);
                    (k.classify(&limits).unwrap(), k.excluded().len())
                })
            });
            let cfg =
                format!("threads={threads}, exp={expansion:?}, steps={dl_steps}, repair={repair}");
            assert_eq!(
                off.stats.tier,
                super::super::Tier::Disjunctive,
                "{cfg}: OFF tier"
            );
            assert_eq!(
                on.stats.tier,
                super::super::Tier::Disjunctive,
                "{cfg}: ON tier"
            );
            assert!(
                off.stats.dl_work > 0 && on.stats.dl_work > 0,
                "{cfg}: must do Tier D work"
            );
            assert!(
                off.complete && on.complete,
                "{cfg}: both arms must complete"
            );
            assert_eq!(off_excl, 0, "{cfg}: OFF exclusions");
            assert_eq!(on_excl, 0, "{cfg}: ON exclusions");
            assert!(
                !off.stats.dl_over_budget
                    && !on.stats.dl_over_budget
                    && !off.stats.dl_rooted_over
                    && !on.stats.dl_rooted_over
                    && !off.stats.dl_rooted_declined
                    && !on.stats.dl_rooted_declined,
                "{cfg}: no budget/decline fallback permitted"
            );
            assert_eq!(off.consistency, on.consistency, "{cfg}");
            // Full named-class taxonomy equality against sharing OFF and exact OWL/HermiT set.
            let mut got_off = std::collections::BTreeSet::new();
            let mut got_on = std::collections::BTreeSet::new();
            for &a_name in &named {
                let ca = cls(a_name);
                for &b_name in &named {
                    if a_name == b_name {
                        continue;
                    }
                    let cb = cls(b_name);
                    if off.taxonomy.is_subclass(&ca, &cb) {
                        got_off.insert((a_name, b_name));
                    }
                    if on.taxonomy.is_subclass(&ca, &cb) {
                        got_on.insert((a_name, b_name));
                    }
                }
            }
            assert_eq!(
                got_off, expected_pairs,
                "{cfg}: OFF taxonomy must match HermiT/OWL"
            );
            assert_eq!(
                got_on, expected_pairs,
                "{cfg}: ON taxonomy must match HermiT/OWL"
            );
            if repair {
                assert_eq!(
                    on.stats.dl_marker_deficits,
                    [0, 0, 0],
                    "{cfg}: real marker deficits must be zero with marker_diag enabled"
                );
            }
        }

        // Shared completion, exclusions, work and no-fallback invariants applied to every
        // negative control as well as the positive arms (Sol r2 P2).
        let assert_complete_control =
            |res: &super::super::Classification, excl: usize, label: &str| {
                assert_eq!(
                    res.stats.tier,
                    super::super::Tier::Disjunctive,
                    "{label}: tier"
                );
                assert!(res.stats.dl_work > 0, "{label}: must do Tier D work");
                assert!(res.complete, "{label}: must complete");
                assert_eq!(excl, 0, "{label}: must have 0 exclusions");
                assert!(
                    !res.stats.dl_over_budget
                        && !res.stats.dl_rooted_over
                        && !res.stats.dl_rooted_declined,
                    "{label}: no budget/decline fallback permitted"
                );
                // Unrelated non-Horn taxonomy edges must survive intact, proving the run
                // saturated normally rather than stopping early.
                for &(sub_n, sup_n) in &[
                    ("L1", "L1Done"),
                    ("L1a", "L1Done"),
                    ("L1b", "L1Done"),
                    ("L2", "L2Done"),
                    ("L2a", "L2Done"),
                    ("L2b", "L2Done"),
                    ("Z2", "W3"),
                ] {
                    assert!(
                        res.taxonomy.is_subclass(&cls(sub_n), &cls(sup_n)),
                        "{label}: unrelated subsumption {sub_n} ⊑ {sup_n} must survive"
                    );
                }
            };

        // Control 1 (unguarded D2): omitting live + late non-root unit PredC with repair=false
        // MUST break both `:Z ⊑ :Ans` and `:Z2 ⊑ :Ans`, proving the fixture genuinely depends on
        // G6 PredC preservation rather than a root shortcut.
        let ctrl_limits = super::super::Limits {
            threads: 1,
            expansion: super::super::Expansion::Cautious,
            rooted: true,
            rooted_decline: false,
            dl_steps: true,
            rooted_marker_repair: false,
            ..super::super::Limits::default()
        };
        let (ctrl_no_predc, ctrl_no_predc_excl) =
            super::super::dl_rules::with_shared_facts_override(true, || {
                super::super::dl_rules::with_g6_skip_predc_override(3, || {
                    let mut k = super::super::Classifier::of(&ax);
                    (k.classify(&ctrl_limits).unwrap(), k.excluded().len())
                })
            });
        assert_complete_control(&ctrl_no_predc, ctrl_no_predc_excl, "control 1");
        assert!(
            !ctrl_no_predc.taxonomy.is_subclass(&z, &ans),
            "control 1: omitting non-root unit PredC must break Z ⊑ Ans"
        );
        assert!(
            !ctrl_no_predc.taxonomy.is_subclass(&z2, &ans),
            "control 1: omitting non-root unit PredC must break Z2 ⊑ Ans"
        );

        // Control 2 (late replay): omitting ONLY late non-root unit replay (`dedge`/`back`)
        // MUST break late predecessor `:Z2 ⊑ :Ans`, proving late replay is genuinely exercised.
        let (ctrl_no_late, ctrl_no_late_excl) =
            super::super::dl_rules::with_shared_facts_override(true, || {
                super::super::dl_rules::with_g6_skip_predc_override(2, || {
                    let mut k = super::super::Classifier::of(&ax);
                    (k.classify(&ctrl_limits).unwrap(), k.excluded().len())
                })
            });
        assert_complete_control(&ctrl_no_late, ctrl_no_late_excl, "control 2");
        assert!(
            ctrl_no_late.taxonomy.is_subclass(&z, &ans),
            "control 2: early predecessor Z ⊑ Ans must still succeed via live PredC"
        );
        assert!(
            !ctrl_no_late.taxonomy.is_subclass(&z2, &ans),
            "control 2: omitting late non-root unit replay must break late predecessor Z2 ⊑ Ans"
        );

        // Control 3 (`:Step1` / `r ⊑ s` necessity): removing `SubObjectPropertyOf(:r :s)`
        // MUST break `:Z ⊑ :Ans` and `:Z2 ⊑ :Ans`, proving `A(x) ∧ s(y, x) ∧ G(c)` is jointly needed.
        let ax_no_sub = ofn_read::parse(&build_ofn(false)).unwrap().axioms;
        let (ctrl_no_sub, ctrl_no_sub_excl) =
            super::super::dl_rules::with_shared_facts_override(true, || {
                let mut k = super::super::Classifier::of(&ax_no_sub);
                (k.classify(&ctrl_limits).unwrap(), k.excluded().len())
            });
        assert_complete_control(&ctrl_no_sub, ctrl_no_sub_excl, "control 3");
        assert!(
            !ctrl_no_sub.taxonomy.is_subclass(&z, &ans)
                && !ctrl_no_sub.taxonomy.is_subclass(&z2, &ans),
            "control 3: removing r ⊑ s must break Z ⊑ Ans and Z2 ⊑ Ans"
        );

        // Rule-level sender -> destination probe (Sol r2 P2): drive BOTH early live delivery
        // (`st_v.back` during `flush` of `V`) and late replay (`dedge` into `V` after `V`
        // processed `-> E(b)`), starting from `V` receiving `-> E(b)` via `rpred_c` with
        // `Stored::fact = true`, and feeding the emitted `Concl::PredC` messages into `:Z` and
        // `:Z2` via `dl_rules::pred_c` without any direct injection.
        {
            let mut s = Sig::new();
            let nb = s.ind("b");
            let nc = s.ind("c");
            let [a, g, bhit, v_c, z_c, z2_c] =
                ["A", "G", "BHit", "V", "Z", "Z2"].map(|n| s.class(n));
            let p_role = s.role("p");
            let r = s.role("r");
            let s_role = s.role("s");
            s.ax.role_subs.push((r, s_role));
            let e_p = s.some_h(p_role, v_c);
            let e = s.some_h(r, a);
            let (ix, _) = probe_ix(s);
            let dx = dix(&ix);
            let root = dx.root.expect("rooted index");
            let w = dx.hyp[&e];
            let RTerm::C(ub) = c(nb, &ix) else {
                unreachable!()
            };
            let RTerm::C(uc) = c(nc, &ix) else {
                unreachable!()
            };
            let run = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let le = atom(&ix, &out, e, ub);
            let lg = atom(&ix, &out, g, uc);
            let lbhit = atom(&ix, &out, bhit, ub);
            let cid_eb = run.table.push(Clause::new(Vec::new(), vec![le]));
            let ret_clause = Clause::new(vec![Lit::x(a), Lit::ry(s_role), lg], vec![Lit::y(bhit)]);
            let cid_ret = run.table.push(ret_clause);

            // Sender `V`: has `root` in `dpreds` (must be skipped by D2) and early predecessor
            // `Z` pre-registered in `st_v.back` over `p` BEFORE `V` processes `-> E(b)`.
            let mut st_v = State {
                init: true,
                ..State::default()
            };
            let mut back_z = super::super::saturate::Small::default();
            back_z.insert(z_c);
            st_v.back.push((p_role, back_z));
            assert!(super::super::dl_rules::dedge(
                &ix, v_c, &mut st_v, root, e_p, &mut out
            ));
            out.sent.clear();
            // Deliver root unit `-> E(b)` to `V` via `rpred_c` so `Stored::fact == true`.
            assert!(rpred_c(&ix, &mut st_v, cid_eb, &mut out));
            assert!(
                dl(&st_v)
                    .unwrap()
                    .store
                    .clauses
                    .iter()
                    .any(|cl| !cl.dead && cl.fact && cl.c.body().is_empty() && cl.c.head() == [le]),
                "sender V must store -> E(b) with Stored::fact == true"
            );
            super::super::dl_rules::flush(&ix, v_c, &mut st_v, &mut out, 64);
            let cid_early = out
                .sent
                .iter()
                .find_map(|&(to, msg)| match msg {
                    Concl::PredC(from, cid)
                        if to == z_c
                            && from == v_c
                            && run.table.get(cid).body().is_empty()
                            && run.table.get(cid).head() == [le] =>
                    {
                        Some(cid)
                    }
                    _ => None,
                })
                .expect("early Z must receive live PredC(V, -> E(b)) when V flushes");
            assert!(
                !out.sent.iter().any(|&(to, msg)| {
                    to == root && matches!(msg, Concl::PredC(from, _) if from == v_c)
                }),
                "D2 must still skip live unit PredC back to root"
            );
            assert!(
                !out.sent.iter().any(|&(to, _)| to == z2_c),
                "unregistered late Z2 must not receive PredC before dedge"
            );

            // Late predecessor `Z2` registers with `V` AFTER `V` already processed `-> E(b)`.
            out.sent.clear();
            assert!(super::super::dl_rules::dedge(
                &ix, v_c, &mut st_v, z2_c, e_p, &mut out
            ));
            let cid_late = out
                .sent
                .iter()
                .find_map(|&(to, msg)| match msg {
                    Concl::PredC(from, cid)
                        if to == z2_c
                            && from == v_c
                            && run.table.get(cid).body().is_empty()
                            && run.table.get(cid).head() == [le] =>
                    {
                        Some(cid)
                    }
                    _ => None,
                })
                .expect("late Z2 must receive replayed PredC(V, -> E(b)) via dedge");

            // Drive BOTH destinations through actual `dl_rules::pred_c` delivery from `V`.
            for (label, dest, cid_delivered, use_unit_edge) in [
                ("early Z (live PredC)", z_c, cid_early, true),
                ("late Z2 (dedge replay)", z2_c, cid_late, false),
            ] {
                out.sent.clear();
                out.mine.clear();
                let mut st = State {
                    init: true,
                    ..State::default()
                };
                if use_unit_edge {
                    st.subs.insert(e_p);
                } else {
                    ds(&mut st).insert_edge(e_p, v_c);
                }
                // Destination holds private `-> G(c)`, then ingests `PredC(V, -> E(b))`.
                super::super::dl_rules::derive(&ix, &mut st, (Vec::new(), vec![lg]), &mut out);
                assert!(super::super::dl_rules::pred_c(
                    &ix,
                    dest,
                    &mut st,
                    v_c,
                    cid_delivered,
                    &mut out
                ));
                super::super::dl_rules::flush(&ix, dest, &mut st, &mut out, 64);

                let r_st = dl(&st)
                    .and_then(|x| x.root.as_deref())
                    .expect("destination must have RootSt");
                assert!(
                    r_st.succ_set.contains(&(e, ub)),
                    "{label}: destination must open rooted existential edge (E, b)"
                );
                assert!(
                    out.sent.iter().any(|&(to, c)| to == w
                        && matches!(c, Concl::DEdge(from, f) if from == dest && f == e)),
                    "{label}: destination must send DEdge(dest, E) to successor w"
                );
                assert!(
                    r_st.gsent.contains(&(w, lg))
                        && out.sent.iter().any(|&(to, c)| {
                            to == w && matches!(c, Concl::HypL(xid) if xid == lg.xid())
                        }),
                    "{label}: destination must forward private ground marker G(c) to successor w"
                );

                // Successor `w` returns `A(x) ∧ s(y, x) ∧ G(c) -> BHit(y)` to destination.
                assert!(super::super::dl_rules::pred_c(
                    &ix, dest, &mut st, w, cid_ret, &mut out
                ));
                super::super::dl_rules::flush(&ix, dest, &mut st, &mut out, 64);
                let has_bhit = dl(&st)
                    .unwrap()
                    .store
                    .clauses
                    .iter()
                    .any(|s| !s.dead && s.c.body().is_empty() && s.c.head() == [lbhit]);
                assert!(
                    has_bhit,
                    "{label}: destination must derive -> BHit(b) from w's return and private G(c)"
                );
            }
        }
    }

    #[test]
    fn constants_only_replays_on_first_rooted_edge_in_late_unit_mode_0() {
        // Sol r1 P2 + Sol r2 P2:
        // - Order A (mode 0): cache -> eligible ordinary edge -> first rooted edge -> second
        //   rooted edge must replay cached constants-only clauses (both `-> B(b)` and empty
        //   `[] -> []`) once and not duplicate on a second rooted edge.
        // - Order B (inverse, mode 0 AND mode 1): eligible ordinary edge -> `pred_c` (emits
        //   immediately) -> first rooted edge must NOT emit `-> B(b)` or `[] -> []` a second
        //   time when sharing is ON.
        // - Order C (mixed, mode 0): `cid1` cached before ordinary edge -> ordinary edge ->
        //   `cid2` arrives via `pred_c` (emitted immediately) -> first rooted edge replays ONLY
        //   `cid1` and does NOT re-emit `cid2`.
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [a, b, z] = ["A", "B", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e1 = s.some_h(r, a);
        let (ix, _) = probe_ix(s);
        let dx = dix(&ix);
        let w = dx.hyp[&e1];
        let RTerm::C(ua) = c(na, &ix) else {
            unreachable!()
        };
        let RTerm::C(ub) = c(nb, &ix) else {
            unreachable!()
        };

        super::super::dl_rules::with_late_unit_mode_override(0, || {
            for share in [false, true] {
                let run = DlRun::new(false, Default::default()).with_shared_facts(share);
                let mut out = Probe {
                    run: &run,
                    sent: Vec::new(),
                    mine: Vec::new(),
                };
                let mut st = State {
                    init: true,
                    ..State::default()
                };
                let lb = atom(&ix, &out, b, ub);
                let cid_unit = run.table.push(Clause::new(Vec::new(), vec![lb]));
                // Order A (unit): 1. Cache before any edge: 0 derivations.
                assert!(super::super::dl_rules::pred_c(
                    &ix, z, &mut st, w, cid_unit, &mut out
                ));
                assert_eq!(run.work(), 0, "share={share}: cached before edge");

                // 2. Open an eligible ordinary edge in late_unit_mode 0: late() does NOT replay.
                ds(&mut st).insert_edge(e1, w);
                let mut gl = Guard::new(&mut out);
                let mut f_late = Found::default();
                let replayed = super::super::dl_rules::late(
                    &ix,
                    z,
                    &mut st,
                    e1,
                    w,
                    "edge",
                    &mut f_late,
                    &mut gl,
                );
                assert!(!replayed, "share={share}: mode 0 does not replay");
                drain_admitted(&ix, &mut st, f_late, &mut gl);
                drop(gl);
                assert_eq!(run.work(), 0, "share={share}: mode 0 ordinary edge emits 0");

                // 3. First rooted edge `(e1, ua)` to `w`: must emit `-> B(b)`!
                let mut g1 = Guard::new(&mut out);
                let mut f1 = Found::default();
                succ_at(&ix, z, &mut st, e1, ua, &mut f1, &mut g1);
                drain_admitted(&ix, &mut st, f1, &mut g1);
                drop(g1);
                assert_eq!(
                    run.work(),
                    1,
                    "share={share}: first rooted edge in mode 0 must emit cached constants-only unit"
                );

                // 4. Second rooted edge `(e1, ub)` to `w`: must NOT duplicate when sharing is ON.
                let mut g2 = Guard::new(&mut out);
                let mut f2 = Found::default();
                succ_at(&ix, z, &mut st, e1, ub, &mut f2, &mut g2);
                drain_admitted(&ix, &mut st, f2, &mut g2);
                drop(g2);
                if share {
                    assert_eq!(
                        run.work(),
                        1,
                        "share=true: second rooted edge must not re-emit constants-only unit"
                    );
                }

                // Order A (empty clause `[] -> []`, delivers BOTTOM).
                let mut st_e = State {
                    init: true,
                    ..State::default()
                };
                let run_e = DlRun::new(false, Default::default()).with_shared_facts(share);
                let mut out_e = Probe {
                    run: &run_e,
                    sent: Vec::new(),
                    mine: Vec::new(),
                };
                let cid_empty = run_e.empty();
                assert!(super::super::dl_rules::pred_c(
                    &ix, z, &mut st_e, w, cid_empty, &mut out_e
                ));
                assert!(!out_e.mine.contains(&Concl::Sub(BOTTOM)));
                ds(&mut st_e).insert_edge(e1, w);
                let mut gel = Guard::new(&mut out_e);
                let mut fe_late = Found::default();
                assert!(!super::super::dl_rules::late(
                    &ix,
                    z,
                    &mut st_e,
                    e1,
                    w,
                    "edge",
                    &mut fe_late,
                    &mut gel
                ));
                drain_admitted(&ix, &mut st_e, fe_late, &mut gel);
                drop(gel);
                assert!(!out_e.mine.contains(&Concl::Sub(BOTTOM)));
                let mut ge1 = Guard::new(&mut out_e);
                let mut fe1 = Found::default();
                succ_at(&ix, z, &mut st_e, e1, ua, &mut fe1, &mut ge1);
                drain_admitted(&ix, &mut st_e, fe1, &mut ge1);
                drop(ge1);
                assert!(
                    out_e.mine.contains(&Concl::Sub(BOTTOM)),
                    "share={share}: first rooted edge in mode 0 must deliver cached empty clause"
                );
            }

            // Order C (mixed in mode 0, sharing ON): `cid1` cached before ordinary edge,
            // ordinary edge opens (no replay), `cid2` arrives via `pred_c` (emits immediately),
            // then first rooted edge replays ONLY `cid1` and does NOT re-emit `cid2`.
            let run_m = DlRun::new(false, Default::default()).with_shared_facts(true);
            let mut out_m = Probe {
                run: &run_m,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let mut st_m = State {
                init: true,
                ..State::default()
            };
            let la_m = atom(&ix, &out_m, a, ua);
            let lb_m = atom(&ix, &out_m, b, ub);
            let cid1 = run_m.table.push(Clause::new(Vec::new(), vec![lb_m]));
            let cid2 = run_m.table.push(Clause::new(Vec::new(), vec![la_m]));
            assert!(super::super::dl_rules::pred_c(
                &ix, z, &mut st_m, w, cid1, &mut out_m
            ));
            assert_eq!(run_m.work(), 0, "mixed mode 0: cid1 cached before edge");
            ds(&mut st_m).insert_edge(e1, w);
            let mut glm = Guard::new(&mut out_m);
            let mut flm = Found::default();
            assert!(!super::super::dl_rules::late(
                &ix, z, &mut st_m, e1, w, "edge", &mut flm, &mut glm
            ));
            drain_admitted(&ix, &mut st_m, flm, &mut glm);
            drop(glm);
            assert_eq!(run_m.work(), 0);
            assert!(super::super::dl_rules::pred_c(
                &ix, z, &mut st_m, w, cid2, &mut out_m
            ));
            assert_eq!(
                run_m.work(),
                1,
                "mixed mode 0: cid2 emitted immediately over existing ordinary edge"
            );
            let mut gm1 = Guard::new(&mut out_m);
            let mut fm1 = Found::default();
            succ_at(&ix, z, &mut st_m, e1, ua, &mut fm1, &mut gm1);
            drain_admitted(&ix, &mut st_m, fm1, &mut gm1);
            drop(gm1);
            assert_eq!(
                run_m.work(),
                2,
                "mixed mode 0: first rooted edge must emit only pending cid1, not re-emit cid2"
            );
        });

        // Order B (inverse arrival order: ordinary edge -> `pred_c` -> first rooted edge)
        // in BOTH mode 0 and mode 1, for both `-> B(b)` and empty clause `[] -> []`:
        // `pred_c` emits immediately, so the subsequent first rooted edge MUST NOT duplicate.
        for mode in [0u8, 1u8] {
            super::super::dl_rules::with_late_unit_mode_override(mode, || {
                // Unit clause `-> B(b)`
                let run_inv = DlRun::new(false, Default::default()).with_shared_facts(true);
                let mut out_inv = Probe {
                    run: &run_inv,
                    sent: Vec::new(),
                    mine: Vec::new(),
                };
                let mut st_inv = State {
                    init: true,
                    ..State::default()
                };
                let lb_inv = atom(&ix, &out_inv, b, ub);
                let cid_inv = run_inv.table.push(Clause::new(Vec::new(), vec![lb_inv]));
                ds(&mut st_inv).insert_edge(e1, w);
                assert!(super::super::dl_rules::pred_c(
                    &ix,
                    z,
                    &mut st_inv,
                    w,
                    cid_inv,
                    &mut out_inv
                ));
                assert_eq!(
                    run_inv.work(),
                    1,
                    "mode {mode} inverse unit: pred_c over ordinary edge emits once"
                );
                let mut gi = Guard::new(&mut out_inv);
                let mut fi = Found::default();
                succ_at(&ix, z, &mut st_inv, e1, ua, &mut fi, &mut gi);
                drain_admitted(&ix, &mut st_inv, fi, &mut gi);
                drop(gi);
                assert_eq!(
                    run_inv.work(),
                    1,
                    "mode {mode} inverse unit: first rooted edge must NOT replay already-emitted clause"
                );

                // Empty clause `[] -> []`
                let run_ie = DlRun::new(false, Default::default()).with_shared_facts(true);
                let mut out_ie = Probe {
                    run: &run_ie,
                    sent: Vec::new(),
                    mine: Vec::new(),
                };
                let mut st_ie = State {
                    init: true,
                    ..State::default()
                };
                let cid_ie = run_ie.empty();
                ds(&mut st_ie).insert_edge(e1, w);
                assert!(super::super::dl_rules::pred_c(
                    &ix,
                    z,
                    &mut st_ie,
                    w,
                    cid_ie,
                    &mut out_ie
                ));
                assert_eq!(
                    run_ie.work(),
                    1,
                    "mode {mode} inverse empty: pred_c over ordinary edge emits once"
                );
                assert!(
                    out_ie.mine.contains(&Concl::Sub(BOTTOM)),
                    "mode {mode} inverse empty: must deliver BOTTOM on pred_c"
                );
                let mut gie = Guard::new(&mut out_ie);
                let mut fie = Found::default();
                succ_at(&ix, z, &mut st_ie, e1, ua, &mut fie, &mut gie);
                drain_admitted(&ix, &mut st_ie, fie, &mut gie);
                drop(gie);
                assert_eq!(
                    run_ie.work(),
                    1,
                    "mode {mode} inverse empty: first rooted edge must NOT replay already-emitted empty clause"
                );
            });
        }
    }

    #[test]
    fn const_eq_rejects_disjunctive_conditional_inequality_and_function_terms() {
        let mut run = DlRun::new(false, Default::default());
        run.merge_skip = Some(99);
        let mk = |xl: XLit| match run.lits.lit(xl, |_| None) {
            Made::Lit(l) => l,
            other => panic!("expected literal for {xl:?}, got {other:?}"),
        };
        let eq_uv = mk(XLit::Eq(RTerm::C(1), RTerm::C(2)));
        let eq_skip = mk(XLit::Eq(RTerm::C(99), RTerm::C(2)));
        let ne_uv = mk(XLit::Ne(RTerm::C(1), RTerm::C(2)));
        let eq_fn = mk(XLit::Eq(RTerm::C(1), RTerm::Fc(7, 2)));
        let eq_var = mk(XLit::Eq(RTerm::X, RTerm::C(2)));
        let atom_c = mk(XLit::Atom(10, RTerm::C(1)));
        let probe = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let inv = |_: Role| None;

        // Genuine unconditional unit constant equality -> accepted.
        assert_eq!(
            const_eq(&probe, &Clause::new(Vec::new(), vec![eq_uv]), inv),
            Some((1, 2))
        );
        // Disjunctive equality -> rejected.
        assert_eq!(
            const_eq(&probe, &Clause::new(Vec::new(), vec![eq_uv, atom_c]), inv),
            None
        );
        // Conditional equality (non-empty body) -> rejected.
        assert_eq!(
            const_eq(&probe, &Clause::new(vec![atom_c], vec![eq_uv]), inv),
            None
        );
        // Inequality -> rejected.
        assert_eq!(
            const_eq(&probe, &Clause::new(Vec::new(), vec![ne_uv]), inv),
            None
        );
        // Function term equality -> rejected.
        assert_eq!(
            const_eq(&probe, &Clause::new(Vec::new(), vec![eq_fn]), inv),
            None
        );
        // Variable equality -> rejected.
        assert_eq!(
            const_eq(&probe, &Clause::new(Vec::new(), vec![eq_var]), inv),
            None
        );
        // Skipped constant -> rejected.
        assert_eq!(
            const_eq(&probe, &Clause::new(Vec::new(), vec![eq_skip]), inv),
            None
        );
    }

    #[test]
    fn sink_adapters_forward_halt_merge() {
        struct FlagSink {
            calls: std::cell::Cell<usize>,
            win: bool,
        }
        impl Sink for FlagSink {
            fn send(&mut self, _: Ex, _: Concl) {}
            fn me(&mut self, _: Concl) {}
            fn halt_merge(&self) -> bool {
                self.calls.set(self.calls.get() + 1);
                self.win
            }
        }
        let mut inner = FlagSink {
            calls: std::cell::Cell::new(0),
            win: true,
        };
        {
            let g = Guard::new(&mut inner);
            assert!(g.halt_merge());
        }
        assert_eq!(inner.calls.get(), 1);
        {
            let g = DlGuard::new(&mut inner);
            assert!(g.halt_merge());
        }
        assert_eq!(inner.calls.get(), 2);
        {
            let g = CreditGuard::new(&mut inner, 0);
            assert!(g.halt_merge());
        }
        assert_eq!(inner.calls.get(), 3);
        inner.win = false;
        {
            let mut g1 = Guard::new(&mut inner);
            let mut g2 = DlGuard::new(&mut g1);
            let g3 = CreditGuard::new(&mut g2, 0);
            assert!(!g3.halt_merge());
        }
        assert_eq!(inner.calls.get(), 4);
    }

    #[test]
    fn harvest_meters_reads_polls_and_transfers_single_debit() {
        struct HarvestSink<'a> {
            run: &'a DlRun,
            admit_ok: bool,
            halt_ok: bool,
            go_on_calls: std::cell::Cell<usize>,
            admitted_total: usize,
            committed_kept: usize,
            committed_released: usize,
        }
        impl Sink for HarvestSink<'_> {
            fn send(&mut self, _: Ex, _: Concl) {}
            fn me(&mut self, _: Concl) {}
            fn dl(&self) -> &DlRun {
                self.run
            }
            fn go_on(&self) -> bool {
                self.go_on_calls.set(self.go_on_calls.get() + 1);
                true
            }
            fn admit_dl(&mut self, bytes: usize) -> bool {
                if !self.admit_ok {
                    return false;
                }
                self.admitted_total += bytes;
                true
            }
            fn commit_dl(&mut self, admitted: usize, kept: usize) {
                self.committed_kept += kept;
                self.committed_released += admitted.saturating_sub(kept);
            }
            fn halt_merge(&self) -> bool {
                self.halt_ok
            }
        }

        let run = DlRun::new(false, Default::default());
        run.set_merge(true);
        let mk = |xl: XLit| match run.lits.lit(xl, |_| None) {
            Made::Lit(l) => l,
            other => panic!("expected literal for {xl:?}, got {other:?}"),
        };
        let eq01 = mk(XLit::Eq(RTerm::C(0), RTerm::C(1)));
        let eq23 = mk(XLit::Eq(RTerm::C(2), RTerm::C(3)));
        let atom = mk(XLit::Atom(5, RTerm::C(0)));
        let inv = |_: Role| None;

        let mut x = DlSt::default();
        // Add 1100 distinct non-eq clauses to exercise Scan metering past M_SCAN=1024,
        // plus two live unit equalities and one disjunctive equality.
        for i in 0..1100u32 {
            let body_lit = mk(XLit::Atom(100 + i, RTerm::C(0)));
            let _ = x.store.add(Clause::new(vec![body_lit], vec![eq01]));
        }
        // Mark half dead to confirm dead slots are metered too.
        for s in x.store.clauses.iter_mut().step_by(2) {
            s.dead = true;
        }
        let _ = x.store.add(Clause::new(Vec::new(), vec![eq01]));
        let _ = x.store.add(Clause::new(Vec::new(), vec![eq23]));
        let _ = x.store.add(Clause::new(Vec::new(), vec![eq01, atom]));

        // Case 1: admission refusal -> false, zero debit kept, no payload.
        let mut s_refuse = HarvestSink {
            run: &run,
            admit_ok: false,
            halt_ok: true,
            go_on_calls: std::cell::Cell::new(0),
            admitted_total: 0,
            committed_kept: 0,
            committed_released: 0,
        };
        assert!(!harvest_and_halt_merge(&mut x, inv, &mut s_refuse));
        assert_eq!(s_refuse.admitted_total, 0);
        assert_eq!(s_refuse.committed_kept, 0);
        assert!(run.take_merged().is_none());
        assert!(run.work() > 0, "Pass 1 must meter reads via Scan");

        // Case 2: lost stop race (`halt_merge` returns false) -> debit released, no payload.
        let mut s_race = HarvestSink {
            run: &run,
            admit_ok: true,
            halt_ok: false,
            go_on_calls: std::cell::Cell::new(0),
            admitted_total: 0,
            committed_kept: 0,
            committed_released: 0,
        };
        assert!(!harvest_and_halt_merge(&mut x, inv, &mut s_race));
        let want_bytes = 2 * std::mem::size_of::<(u32, u32)>();
        assert_eq!(s_race.admitted_total, want_bytes);
        assert_eq!(s_race.committed_kept, 0);
        assert_eq!(s_race.committed_released, want_bytes);
        assert!(run.take_merged().is_none());

        // Case 3: winning publish -> exact single debit committed, exact 2 pairs, zero realloc.
        let mut s_win = HarvestSink {
            run: &run,
            admit_ok: true,
            halt_ok: true,
            go_on_calls: std::cell::Cell::new(0),
            admitted_total: 0,
            committed_kept: 0,
            committed_released: 0,
        };
        assert!(harvest_and_halt_merge(&mut x, inv, &mut s_win));
        assert!(
            s_win.go_on_calls.get() >= 4,
            "both passes must poll via Scan plus once before publication"
        );
        assert_eq!(s_win.admitted_total, want_bytes);
        assert_eq!(s_win.committed_kept, want_bytes);
        assert_eq!(s_win.committed_released, 0);
        let (pairs, kept) = run.take_merged().expect("winning payload stored");
        assert_eq!(kept, want_bytes);
        assert_eq!(pairs.capacity(), 2);
        assert_eq!(pairs, vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn disj_const_eq_eligibility_tiebreak_reversed_decided_provenance_and_rescan() {
        use super::super::dl_rules::{SplitCand, SplitMode};

        let mut run = DlRun::new(false, Default::default());
        // Rank map: c0 -> rank 1, c1 -> rank 0 (reversed order vs raw u32!),
        // c2 -> rank 2, c3 -> u32::MAX (ineligible: e.g. anonymous or active hub),
        // c4 -> rank 3 (simulates input `urn:sparkles:internal:hub` when not active hub).
        run.configure_split(
            SplitMode::ProbeOnly,
            16_384,
            1,
            vec![1, 0, 2, u32::MAX, 3].into_boxed_slice(),
            vec![(1, 2)].into_boxed_slice(), // epoch-local IDs (1, 2) are ALREADY decided !=
        );
        let mk = |xl: XLit| match run.lits.lit(xl, |_| None) {
            Made::Lit(l) => l,
            other => panic!("expected literal for {xl:?}, got {other:?}"),
        };
        let inv = |_: Role| None;

        // Reversed spelling `c0 ≈ c1` (ranks 1 and 0) normalizes to `(lo=0, hi=1)` (raw c1, c0).
        let eq01 = mk(XLit::Eq(RTerm::C(0), RTerm::C(1)));
        let eq12 = mk(XLit::Eq(RTerm::C(1), RTerm::C(2))); // already decided in branch_neq!
        let eq03 = mk(XLit::Eq(RTerm::C(0), RTerm::C(3))); // c3 ineligible (u32::MAX)!
        let eq24 = mk(XLit::Eq(RTerm::C(2), RTerm::C(4))); // c4 eligible (rank 3)
        let atom = mk(XLit::Atom(7, RTerm::C(0)));

        // 1. Unit clause (len < 2) or conditional clause (non-empty body) -> None.
        let mut scan = Scan(0);
        let polls = std::cell::Cell::new(0usize);
        let mut poll = |_: &mut usize| {
            polls.set(polls.get() + 1);
            true
        };
        assert_eq!(
            disj_const_eq(
                &Clause::new(Vec::new(), vec![eq01]),
                0,
                &run,
                inv,
                &mut scan,
                &mut poll
            ),
            None
        );
        assert_eq!(
            disj_const_eq(
                &Clause::new(vec![atom], vec![eq01, eq24]),
                1,
                &run,
                inv,
                &mut scan,
                &mut poll
            ),
            None
        );

        // 2. All-decided / ineligible disjuncts -> None.
        assert_eq!(
            disj_const_eq(
                &Clause::new(Vec::new(), vec![eq12, eq03]),
                2,
                &run,
                inv,
                &mut scan,
                &mut poll
            ),
            None
        );

        // 3. Pure equality disjunction skipping decided `eq12` -> prio 2, picks (rank 0, rank 1).
        let c_pure = Clause::new(Vec::new(), vec![eq12, eq24, eq01]);
        let got_pure =
            disj_const_eq(&c_pure, 10, &run, inv, &mut scan, &mut poll).expect("pure candidate");
        assert_eq!(
            got_pure,
            SplitCand {
                epoch: 1,
                prio: 2,
                head_len: 3,
                rank_lo: 0,
                rank_hi: 1,
                u_id: 1,
                v_id: 0,
                clause_id: 10,
            }
        );

        // 4. Mixed disjunction (with atom + eq24) -> prio 1 (strictly lower priority than pure).
        let c_mixed = Clause::new(Vec::new(), vec![atom, eq24]);
        let got_mixed =
            disj_const_eq(&c_mixed, 11, &run, inv, &mut scan, &mut poll).expect("mixed candidate");
        assert_eq!(got_mixed.prio, 1);
        assert_eq!((got_mixed.rank_lo, got_mixed.rank_hi), (2, 3));

        // 5. Tie-break: higher prio wins; atomic hints update monotonically.
        run.record_split_cand(got_mixed);
        assert!(
            run.has_split_cand
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(
            run.best_split_prio
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );
        run.record_split_cand(got_pure);
        assert_eq!(
            run.best_split_prio
                .load(std::sync::atomic::Ordering::Acquire),
            2
        );

        // 6. Long head (> HEAD_POLL_EVERY=1024) meters reads and polls cooperatively.
        let mut long_head = Vec::with_capacity(1100);
        for i in 0..1099u32 {
            long_head.push(mk(XLit::Atom(200 + i, RTerm::C(0))));
        }
        long_head.push(eq01);
        let c_long = Clause::new(Vec::new(), long_head);
        let polls_before = polls.get();
        let scan_before = scan.0;
        let got_long = disj_const_eq(&c_long, 20, &run, inv, &mut scan, &mut poll)
            .expect("long head candidate");
        assert_eq!(got_long.prio, 1);
        assert!(
            polls.get() > polls_before,
            "long head must poll every HEAD_POLL_EVERY"
        );
        assert!(
            scan.0 >= scan_before + 1100,
            "long head must meter every literal"
        );

        // 7. Full-width `u32` constant IDs around 2^31 (no i32 sign truncation).
        let big_u = (1u32 << 31) + 5;
        let big_v = (1u32 << 31) + 9;
        let eq_big = mk(XLit::Eq(RTerm::C(big_u), RTerm::C(big_v)));
        assert_eq!(
            disj_const_eq(
                &Clause::new(Vec::new(), vec![eq_big, atom]),
                21,
                &run,
                inv,
                &mut scan,
                &mut poll
            ),
            None
        );

        // 8. Stale-candidate rescan refreshes BOTH atomic hints (`has_split_cand` and
        //    `best_split_prio`) across a long store with dead slots.
        let mut sig = Sig::new();
        sig.ind("a");
        let (ix, _) = probe_ix(sig);
        let mut x = DlSt::default();
        let id_pure = x.store.clauses.len();
        let _ = x.store.add(c_pure);
        let id_mixed = x.store.clauses.len();
        let _ = x.store.add(c_mixed);
        for i in 0..1100u32 {
            let b = mk(XLit::Atom(5000 + i, RTerm::C(0)));
            let _ = x.store.add(Clause::new(vec![b], vec![atom]));
        }
        x.store.clauses[id_pure].dead = true;
        let mut rescan_polls = 0usize;
        let rescanned = rescan_split_cand(&ix, &x, &run, &mut || {
            rescan_polls += 1;
            true
        })
        .expect("mixed clause still live");
        assert_eq!(rescanned.clause_id as usize, id_mixed);
        assert_eq!(rescanned.prio, 1);
        assert!(rescan_polls >= 1, "long store rescan must poll");
        {
            let mut slot = run.split_slot.lock();
            run.refresh_split_best_locked(&mut slot, Some(rescanned));
        }
        assert!(
            run.has_split_cand
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(
            run.best_split_prio
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );

        x.store.clauses[id_mixed].dead = true;
        let empty = rescan_split_cand(&ix, &x, &run, &mut || true);
        assert!(empty.is_none());
        {
            let mut slot = run.split_slot.lock();
            run.refresh_split_best_locked(&mut slot, empty);
        }
        assert!(
            !run.has_split_cand
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(
            run.best_split_prio
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
    }

    /// Barrier interleaving race test (design §4.3.2): Worker A validates candidate X
    /// under `root_st`, commits `winner` + `STOP_SPLIT`, and releases `root_st`; a
    /// concurrent root mutator then acquires `root_st`, kills X, and Worker B rescans and
    /// clears `best`. Immutable `winner` guarantees `take_winning_split` still returns X!
    #[test]
    fn barrier_race_immutable_winner_survives_subsumption_and_rescan_clear() {
        use super::super::dl_rules::{SplitCand, SplitMode};
        use std::sync::{Arc, Barrier};

        let mut sig = Sig::new();
        sig.ind("a");
        let (ix, _) = probe_ix(sig);
        let mut run_raw = DlRun::new(false, Default::default());
        run_raw.configure_split(
            SplitMode::ProbeOnly,
            16_384,
            7,
            vec![0, 1].into_boxed_slice(),
            Vec::new().into_boxed_slice(),
        );
        let run = Arc::new(run_raw);
        let cand = SplitCand {
            epoch: 7,
            prio: 2,
            head_len: 2,
            rank_lo: 0,
            rank_hi: 1,
            u_id: 0,
            v_id: 1,
            clause_id: 0,
        };
        run.record_split_cand(cand);

        let root_st = Arc::new(parking_lot::Mutex::new(DlSt::default()));
        let mk = |xl: XLit| match run.lits.lit(xl, |_| None) {
            Made::Lit(l) => l,
            other => panic!("expected literal for {xl:?}, got {other:?}"),
        };
        let eq01 = mk(XLit::Eq(RTerm::C(0), RTerm::C(1)));
        let atom = mk(XLit::Atom(10, RTerm::C(0)));
        {
            let mut st = root_st.lock();
            let _ = st.store.add(Clause::new(Vec::new(), vec![eq01, atom]));
        }

        let after_publish = Arc::new(Barrier::new(2));
        let run_a = run.clone();
        let st_a = root_st.clone();
        let bar_a = after_publish.clone();

        let worker_a = std::thread::spawn(move || {
            let committed = {
                let st = st_a.lock();
                let mut slot = run_a.split_slot.lock();
                let live = slot.best.filter(|c| {
                    st.store
                        .clauses
                        .get(c.clause_id as usize)
                        .is_some_and(|s| !s.dead)
                });
                if let Some(c) = live {
                    slot.winner = Some(c);
                    true
                } else {
                    false
                }
            };
            bar_a.wait();
            committed
        });

        after_publish.wait();
        {
            let mut st = root_st.lock();
            st.store.clauses[0].dead = true;
            let rescanned = rescan_split_cand(&ix, &st, &run, &mut || true);
            assert!(rescanned.is_none());
            let mut slot = run.split_slot.lock();
            run.refresh_split_best_locked(&mut slot, rescanned);
            assert!(slot.best.is_none());
            assert_eq!(slot.winner, Some(cand));
        }

        assert!(worker_a.join().unwrap());
        assert_eq!(
            run.take_winning_split(),
            Some(cand),
            "immutable winner must never be clobbered by post-publication subsumption/rescan"
        );
    }

    /// Sol r1 #4: `dl_root::flush` must charge every full `M_SCAN` block of split-candidate
    /// head reads during discovery (never losing 1,024-literal blocks via `Scan::end`'s
    /// `% M_SCAN`), and stop early mid-head under a tight work bound.
    #[test]
    fn flush_charges_full_blocks_for_long_split_candidate_heads_and_stops_under_tight_bound() {
        use super::super::dl_rules::SplitMode;
        use super::super::saturate::{Extra, State};
        use std::sync::atomic::Ordering;

        let mut sig = Sig::new();
        sig.ind("a");
        sig.ind("b");
        let (ix, _) = probe_ix(sig);

        struct WorkSink<'a> {
            dl: &'a DlRun,
            stopped: std::cell::Cell<bool>,
        }
        impl Sink for WorkSink<'_> {
            fn send(&mut self, _: Ex, _: Concl) {}
            fn me(&mut self, _: Concl) {}
            fn dl(&self) -> &DlRun {
                self.dl
            }
            fn worked(&mut self, n: usize) {
                self.dl.count(n);
            }
            fn go_on(&self) -> bool {
                if self.dl.over_work() {
                    self.stopped.set(true);
                    return false;
                }
                !self.stopped.get()
            }
            fn stopped(&self) -> bool {
                self.stopped.get() || self.dl.over_work()
            }
            fn admit_dl(&mut self, _: usize) -> bool {
                true
            }
            fn credit_dl(&mut self, _: usize) {}
            fn commit_dl(&mut self, _: usize, _: usize) {}
        }

        // 1. Exact 1,024-literal head: must charge at least `1024 / WORK_READS == 64` work
        //    units (before the fix, `1024 % 1024 == 0` charged 0 work units!).
        {
            let mut run = DlRun::new(false, Default::default());
            run.configure_split(
                SplitMode::ProbeOnly,
                100_000,
                1,
                vec![0, 1].into_boxed_slice(),
                Vec::new().into_boxed_slice(),
            );
            let mk = |xl: XLit| match run.lits.lit(xl, |_| None) {
                Made::Lit(l) => l,
                other => panic!("expected literal for {xl:?}, got {other:?}"),
            };
            let eq01 = mk(XLit::Eq(RTerm::C(0), RTerm::C(1)));
            let mut head_1024 = Vec::with_capacity(1024);
            for i in 0..1023u32 {
                head_1024.push(mk(XLit::Atom(200 + i, RTerm::C(0))));
            }
            head_1024.push(eq01);
            assert_eq!(head_1024.len(), 1024);

            let mut dl_st = DlSt::default();
            let _ = dl_st.store.add(Clause::new(Vec::new(), head_1024));
            let mut st = State {
                extra: Some(Box::new(Extra {
                    dl: Some(Box::new(dl_st)),
                    ..Extra::default()
                })),
                ..State::default()
            };
            let work_before = run.work();
            let mut sink = WorkSink {
                dl: &run,
                stopped: std::cell::Cell::new(false),
            };
            let _ = flush(&ix, &mut st, &mut sink, 1);
            let delta = run.work().saturating_sub(work_before);
            assert!(
                delta >= 1024 / WORK_READS,
                "1,024-literal head must charge at least {} work units in flush, got {delta}",
                1024 / WORK_READS
            );
            assert!(
                run.has_split_cand.load(Ordering::Acquire),
                "candidate must still be recorded"
            );
        }

        // 2. 2,500-literal head under a tight work bound (cap = 80): `flush` must charge
        //    the first full block before polling and stop early with `interrupted == true`.
        {
            let mut run = DlRun::new(false, Default::default());
            run.set_work_limit(80);
            run.configure_split(
                SplitMode::ProbeOnly,
                100_000,
                2,
                vec![0, 1].into_boxed_slice(),
                Vec::new().into_boxed_slice(),
            );
            let mk = |xl: XLit| match run.lits.lit(xl, |_| None) {
                Made::Lit(l) => l,
                other => panic!("expected literal for {xl:?}, got {other:?}"),
            };
            let eq01 = mk(XLit::Eq(RTerm::C(0), RTerm::C(1)));
            let mut head_2500 = Vec::with_capacity(2500);
            for i in 0..2499u32 {
                head_2500.push(mk(XLit::Atom(200 + i, RTerm::C(0))));
            }
            head_2500.push(eq01);

            let mut dl_st = DlSt::default();
            let _ = dl_st.store.add(Clause::new(Vec::new(), head_2500));
            let mut st = State {
                extra: Some(Box::new(Extra {
                    dl: Some(Box::new(dl_st)),
                    ..Extra::default()
                })),
                ..State::default()
            };
            // Pre-charge 20 work units so the first 64-unit block (total 84 > 80) trips
            // `over_work()` during the first periodic poll inside `disj_const_eq`!
            run.count(20);
            let mut sink = WorkSink {
                dl: &run,
                stopped: std::cell::Cell::new(false),
            };
            let _ = flush(&ix, &mut st, &mut sink, 1);
            assert!(
                run.over_work(),
                "first 1,024-literal block must trip work bound (work={})",
                run.work()
            );
            assert!(
                st.extra
                    .as_ref()
                    .and_then(|e| e.dl.as_deref())
                    .is_some_and(|x| x.store.interrupted),
                "early termination mid-head must mark root store interrupted"
            );
        }
    }
}
