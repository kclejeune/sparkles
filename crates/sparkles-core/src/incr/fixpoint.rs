//! The fixed-point operator (spec F13 §4.3): one interface for recursive programs, which
//! property paths with `+` and `*` and the reasoner's rules plug into.
//!
//! A [`Program`] names its derived facts and the rule instances that derive them. The
//! operator, [`FixedPoint`], keeps every derived fact of the program at the latest
//! snapshot, each with a *record*: the one rule instance that derived it first. Records
//! are well founded. A fact's recorded instance uses only base facts and derived facts
//! that held before it was recorded, so following records from any fact ends at
//! instances over base facts alone, and no fact supports itself through a cycle.
//!
//! **Maintenance.** Counts cannot weight recursive maintenance, because a fact in a cycle
//! can count its own derivations, so a change is applied by backward and forward
//! rederivation, in the manner of the reasoner's backward/forward algorithm (Motik,
//! Nenov, Piro and Horrocks, AAAI 2015).
//!
//! 1. The facts whose recorded instance uses a base fact the changes removed are broken
//!    ([`Program::broken`]). They and every fact whose record depends on them, followed
//!    through the records at the snapshot before the changes, are the candidates. A
//!    fact that is not a candidate keeps a record whose every premise still holds, so
//!    it holds after the changes.
//! 2. Each candidate is checked backward ([`Program::prove`]): an instance at the new
//!    snapshot whose derived premises are facts that are not candidates, or candidates
//!    proved before it, keeps the candidate with that instance as its new record.
//! 3. The instances that use a base fact the changes added ([`Program::added`]) derive
//!    new facts, and candidates too.
//! 4. Forward chaining from every fact kept or derived in 2 and 3
//!    ([`Program::consequences`]) proves the candidates that follow from them and
//!    derives the rest of the new facts, semi-naively.
//! 5. The candidates that no step proved no longer follow and are deleted.
//!
//! Unlike DRed, a candidate that has another derivation is never deleted and derived
//! again, so the operator's output changes only by the facts that come and go. Unlike
//! the reasoner's backward search, the candidates are the facts whose records depend
//! on the removed facts, not every fact with a derivation that uses them, so a fact
//! with an intact record is never checked.
//!
//! **Annotations.** With a provenance setting (spec F13 §5), each fact's annotation is
//! that of its recorded derivation: one derivation, whose support is the union of the
//! supports of the recorded instances it follows ([`Program::annotate`]). The witness
//! it reports is therefore real, and a fact whose record changes reports a new one. A
//! fact counts one derivation, since a recursive fact has unboundedly many.

use super::pass::Which;
use super::persist::{Reader, Writer};
use super::prov::{Ann, Atom, Provenance};
use crate::error::{Error, Result};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::fmt::Debug;
use std::hash::Hash;

/// A recursive program that the fixed-point operator maintains: its derived facts, the
/// rule instances that derive them and the base facts those instances read.
///
/// Every method that reads base facts reads them through the context `Cx` at the
/// snapshot it names, `Which::New` unless a method says otherwise. A `held` callback
/// tells whether a derived fact holds at that moment of the maintenance: an instance
/// whose derived premises do not all hold must not be returned.
pub trait Program {
    /// A derived fact.
    type Fact: Copy + Eq + Hash + Debug;
    /// The rule instance that derived a fact, enough to name its derived premises.
    type Record: Copy + Eq + Debug;
    /// What the program reads base facts through.
    type Cx<'c>;

    /// The derived premises of instance `r` of fact `f`.
    fn premises(&self, f: &Self::Fact, r: &Self::Record, out: &mut Vec<Self::Fact>);

    /// The instances whose premises are all base facts, at the new snapshot.
    fn axioms(
        &mut self,
        cx: &mut Self::Cx<'_>,
        out: &mut Vec<(Self::Fact, Self::Record)>,
    ) -> Result<()>;

    /// The instances at snapshot `w` that use derived fact `f` as a premise, with their
    /// other derived premises held.
    fn consequences(
        &mut self,
        cx: &mut Self::Cx<'_>,
        w: Which,
        f: &Self::Fact,
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Vec<(Self::Fact, Self::Record)>,
    ) -> Result<()>;

    /// One instance at the new snapshot that derives `f` from base facts and held
    /// derived facts, if there is one.
    fn prove(
        &mut self,
        cx: &mut Self::Cx<'_>,
        f: &Self::Fact,
        held: &dyn Fn(&Self::Fact) -> bool,
    ) -> Result<Option<Self::Record>>;

    /// The facts whose recorded instance, as `recorded` gives it, uses a base fact the
    /// changes removed.
    fn broken(
        &mut self,
        cx: &mut Self::Cx<'_>,
        recorded: &dyn Fn(&Self::Fact) -> Option<Self::Record>,
        out: &mut Vec<Self::Fact>,
    ) -> Result<()>;

    /// The instances that use a base fact the changes added, with their derived
    /// premises held.
    fn added(
        &mut self,
        cx: &mut Self::Cx<'_>,
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Vec<(Self::Fact, Self::Record)>,
    ) -> Result<()>;

    /// The held facts whose recorded instance's base facts hold before and after the
    /// changes with another annotation. Only annotated maintenance asks.
    fn moved(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _recorded: &dyn Fn(&Self::Fact) -> Option<Self::Record>,
        _out: &mut Vec<Self::Fact>,
    ) -> Result<()> {
        Ok(())
    }

    /// The annotation of fact `f` derived by instance `r`, given the annotations of its
    /// derived premises in the order of [`premises`](Self::premises).
    fn annotate(
        &mut self,
        cx: &mut Self::Cx<'_>,
        mode: Provenance,
        f: &Self::Fact,
        r: &Self::Record,
        premises: &[&Ann],
    ) -> Result<Ann>;

    /// Check the deadline and the cancel flag.
    fn tick(&mut self, cx: &mut Self::Cx<'_>) -> Result<()>;

    /// Fact `f` is now in the state, as a held fact or, during maintenance, as a
    /// candidate. A program that matches derived premises through an index of its own
    /// keeps it here; `held` callbacks still decide which facts hold.
    fn inserted(&mut self, _f: &Self::Fact) {}

    /// Fact `f` left the state.
    fn removed(&mut self, _f: &Self::Fact) {}

    /// The state was emptied.
    fn cleared(&mut self) {}

    /// Write one fact with its record.
    fn save(&self, w: &mut Writer, f: &Self::Fact, r: &Self::Record) -> Result<()>;

    /// Read what [`save`](Self::save) wrote.
    fn load(&self, r: &mut Reader) -> Result<(Self::Fact, Self::Record)>;

    /// The approximate memory of one held fact with its record.
    fn fact_bytes(&self) -> u64;
}

/// One change of the operator's facts: the fact, its weight's change (`+1` when it
/// starts to hold, `−1` when it stops, `0` when only its annotation moved), and the
/// change of its annotation in annotated maintenance.
#[derive(Clone, Debug)]
pub struct Change<F> {
    pub fact: F,
    pub weight: i64,
    pub ann: Option<Ann>,
}

/// The state of a fixed-point operator: the facts that hold at the latest snapshot
/// with their records, and their annotations when it keeps them.
#[derive(Clone, Debug)]
pub struct FixedPoint<P: Program> {
    pub program: P,
    facts: FxHashMap<P::Fact, P::Record>,
    ann: FxHashMap<P::Fact, Ann>,
    ann_bytes: u64,
    mode: Provenance,
}

/// The atoms of an annotation of one derivation: its support's atoms for `why`, its
/// atoms for `lineage`, none for `count`.
pub fn atoms_of(a: &Ann) -> Vec<Atom> {
    match a {
        Ann::Count(_) => Vec::new(),
        Ann::Lineage(l) => {
            let mut v: Vec<Atom> = l.atoms.keys().copied().collect();
            v.sort_unstable();
            v
        }
        Ann::Why(w) => w
            .supports
            .iter()
            .flat_map(|(x, _)| x.atoms().iter().copied())
            .collect(),
    }
}

impl<P: Program> FixedPoint<P> {
    pub fn new(program: P) -> FixedPoint<P> {
        FixedPoint {
            program,
            facts: FxHashMap::default(),
            ann: FxHashMap::default(),
            ann_bytes: 0,
            mode: Provenance::None,
        }
    }

    /// Keep annotation `mode` from the next build on.
    pub fn set_provenance(&mut self, mode: Provenance) {
        self.mode = mode;
        self.ann.clear();
        self.ann_bytes = 0;
    }

    pub fn provenance(&self) -> Provenance {
        self.mode
    }

    /// The facts that hold, with their records.
    pub fn facts(&self) -> impl Iterator<Item = (&P::Fact, &P::Record)> + '_ {
        self.facts.iter()
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    pub fn holds(&self, f: &P::Fact) -> bool {
        self.facts.contains_key(f)
    }

    pub fn record(&self, f: &P::Fact) -> Option<&P::Record> {
        self.facts.get(f)
    }

    /// A held fact's annotation, in annotated maintenance.
    pub fn annotation(&self, f: &P::Fact) -> Option<&Ann> {
        self.ann.get(f)
    }

    /// The approximate memory of the state.
    pub fn bytes(&self) -> u64 {
        let n = self.facts.capacity() as u64;
        n * self.program.fact_bytes() + self.ann_bytes + self.ann.len() as u64 * 16
    }

    fn over(&self, max: Option<u64>) -> Result<()> {
        if let Some(m) = max
            && self.facts.len().is_multiple_of(4096)
            && self.bytes() > m
        {
            return Err(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                limit: m,
                requested: self.bytes(),
            }));
        }
        Ok(())
    }

    /// Derive every fact at the new snapshot from nothing, and return them all as
    /// additions.
    pub fn build(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
    ) -> Result<Vec<Change<P::Fact>>> {
        self.facts.clear();
        self.ann.clear();
        self.ann_bytes = 0;
        self.program.cleared();
        let mut queue: VecDeque<P::Fact> = VecDeque::new();
        let mut found = Vec::new();
        self.program.axioms(cx, &mut found)?;
        let mut order = Vec::new();
        for (f, r) in found.drain(..) {
            if let std::collections::hash_map::Entry::Vacant(e) = self.facts.entry(f) {
                e.insert(r);
                self.program.inserted(&f);
                queue.push_back(f);
                order.push(f);
                self.over(max_bytes)?;
            }
        }
        while let Some(f) = queue.pop_front() {
            self.program.tick(cx)?;
            let facts = &self.facts;
            self.program.consequences(
                cx,
                Which::New,
                &f,
                &|x| facts.contains_key(x),
                &mut found,
            )?;
            for (g, r) in found.drain(..) {
                if let std::collections::hash_map::Entry::Vacant(e) = self.facts.entry(g) {
                    e.insert(r);
                    self.program.inserted(&g);
                    queue.push_back(g);
                    order.push(g);
                    self.over(max_bytes)?;
                }
            }
        }
        let dirty: FxHashSet<P::Fact> = order.iter().copied().collect();
        let anns = self.annotate(cx, &dirty)?;
        Ok(order
            .into_iter()
            .map(|f| {
                let ann = anns.as_ref().map(|m| {
                    let a = m.get(&f).cloned().expect("annotated above");
                    self.store_ann(f, a.clone());
                    a
                });
                Change {
                    fact: f,
                    weight: 1,
                    ann,
                }
            })
            .collect())
    }

    fn store_ann(&mut self, f: P::Fact, a: Ann) {
        self.ann_bytes += a.bytes();
        if let Some(old) = self.ann.insert(f, a) {
            self.ann_bytes -= old.bytes();
        }
    }

    fn drop_ann(&mut self, f: &P::Fact) -> Option<Ann> {
        let old = self.ann.remove(f);
        if let Some(o) = &old {
            self.ann_bytes -= o.bytes();
        }
        old
    }

    /// The annotations of the facts of `dirty` from their records, premises first; the
    /// other facts keep the annotations held. `None` without annotations.
    fn annotate(
        &mut self,
        cx: &mut P::Cx<'_>,
        dirty: &FxHashSet<P::Fact>,
    ) -> Result<Option<FxHashMap<P::Fact, Ann>>> {
        if self.mode.is_none() {
            return Ok(None);
        }
        let mut done: FxHashMap<P::Fact, Ann> = FxHashMap::default();
        let mut stack: Vec<P::Fact> = Vec::new();
        let mut prem = Vec::new();
        for &f in dirty {
            if done.contains_key(&f) {
                continue;
            }
            stack.push(f);
            while let Some(&top) = stack.last() {
                if stack.len() > self.facts.len() + 1 {
                    return Err(Error::Corrupt(
                        "a fixed point's records form a cycle".into(),
                    ));
                }
                if done.contains_key(&top) {
                    stack.pop();
                    continue;
                }
                let r = *self.facts.get(&top).ok_or_else(|| {
                    Error::Corrupt("a fixed point annotates a fact it does not hold".into())
                })?;
                prem.clear();
                self.program.premises(&top, &r, &mut prem);
                let missing: Vec<P::Fact> = prem
                    .iter()
                    .filter(|p| dirty.contains(*p) && !done.contains_key(*p))
                    .copied()
                    .collect();
                if !missing.is_empty() {
                    stack.extend(missing);
                    continue;
                }
                let mut anns: Vec<&Ann> = Vec::with_capacity(prem.len());
                for p in &prem {
                    let a = done.get(p).or_else(|| self.ann.get(p)).ok_or_else(|| {
                        Error::Corrupt(
                            "a fixed point's record names a fact without annotation".into(),
                        )
                    })?;
                    anns.push(a);
                }
                let a = self.program.annotate(cx, self.mode, &top, &r, &anns)?;
                done.insert(top, a);
                stack.pop();
            }
        }
        Ok(Some(done))
    }

    /// Apply the changes that the context lists, and return the change of the facts.
    pub fn advance(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
    ) -> Result<Vec<Change<P::Fact>>> {
        // 1. the broken facts, and every fact whose record depends on them, at the
        //    snapshot before the changes
        let mut broken = Vec::new();
        {
            let facts = &self.facts;
            self.program
                .broken(cx, &|f| facts.get(f).copied(), &mut broken)?;
        }
        let mut pending: FxHashSet<P::Fact> = FxHashSet::default();
        let mut candidates: Vec<P::Fact> = Vec::new();
        let mut found = Vec::new();
        while let Some(f) = broken.pop() {
            if !self.facts.contains_key(&f) || !pending.insert(f) {
                continue;
            }
            candidates.push(f);
            self.program.tick(cx)?;
            let facts = &self.facts;
            self.program.consequences(
                cx,
                Which::Old,
                &f,
                &|x| facts.contains_key(x),
                &mut found,
            )?;
            for (g, r) in found.drain(..) {
                if self.facts.get(&g) == Some(&r) && !pending.contains(&g) {
                    broken.push(g);
                }
            }
        }
        let mut queue: VecDeque<P::Fact> = VecDeque::new();
        let mut proved: Vec<P::Fact> = Vec::new();
        let mut added: Vec<P::Fact> = Vec::new();
        // 2. a backward check of each candidate against the facts that hold
        for &f in &candidates {
            self.program.tick(cx)?;
            let r = {
                let (facts, pend) = (&self.facts, &pending);
                self.program
                    .prove(cx, &f, &|x| facts.contains_key(x) && !pend.contains(x))?
            };
            if let Some(r) = r {
                pending.remove(&f);
                self.facts.insert(f, r);
                proved.push(f);
                queue.push_back(f);
            }
        }
        // 3. the instances of the added base facts
        {
            let (facts, pend) = (&self.facts, &pending);
            self.program.added(
                cx,
                &|x| facts.contains_key(x) && !pend.contains(x),
                &mut found,
            )?;
        }
        let mut take = |s: &mut Self,
                        g: P::Fact,
                        r: P::Record,
                        queue: &mut VecDeque<P::Fact>,
                        pending: &mut FxHashSet<P::Fact>|
         -> Result<()> {
            if pending.remove(&g) {
                s.facts.insert(g, r);
                proved.push(g);
                queue.push_back(g);
            } else if let std::collections::hash_map::Entry::Vacant(e) = s.facts.entry(g) {
                e.insert(r);
                s.program.inserted(&g);
                added.push(g);
                queue.push_back(g);
                s.over(max_bytes)?;
            }
            Ok(())
        };
        for (g, r) in std::mem::take(&mut found) {
            take(self, g, r, &mut queue, &mut pending)?;
        }
        // 4. forward chaining from what was kept or derived
        while let Some(f) = queue.pop_front() {
            self.program.tick(cx)?;
            {
                let (facts, pend) = (&self.facts, &pending);
                self.program.consequences(
                    cx,
                    Which::New,
                    &f,
                    &|x| facts.contains_key(x) && !pend.contains(x),
                    &mut found,
                )?;
            }
            for (g, r) in std::mem::take(&mut found) {
                take(self, g, r, &mut queue, &mut pending)?;
            }
        }
        // 5. the candidates without a derivation
        let mut out: Vec<Change<P::Fact>> = Vec::new();
        for f in &candidates {
            if pending.contains(f) {
                self.facts.remove(f);
                self.program.removed(f);
                let ann = self.drop_ann(f).map(|a| a.scaled(-1));
                out.push(Change {
                    fact: *f,
                    weight: -1,
                    ann,
                });
            }
        }
        if self.mode.is_none() {
            out.extend(added.into_iter().map(|f| Change {
                fact: f,
                weight: 1,
                ann: None,
            }));
            return Ok(out);
        }
        // the annotations of the facts whose records changed, of the facts whose
        // recorded base facts moved, and of every fact whose record depends on them
        let mut dirty: FxHashSet<P::Fact> = proved.iter().chain(added.iter()).copied().collect();
        let mut moved = Vec::new();
        {
            let facts = &self.facts;
            self.program
                .moved(cx, &|f| facts.get(f).copied(), &mut moved)?;
        }
        let mut stack: Vec<P::Fact> = moved
            .into_iter()
            .filter(|f| self.facts.contains_key(f))
            .chain(proved.iter().copied())
            .collect();
        let mut seen: FxHashSet<P::Fact> = FxHashSet::default();
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            dirty.insert(f);
            self.program.tick(cx)?;
            let facts = &self.facts;
            self.program.consequences(
                cx,
                Which::New,
                &f,
                &|x| facts.contains_key(x),
                &mut found,
            )?;
            for (g, r) in found.drain(..) {
                if self.facts.get(&g) == Some(&r) {
                    stack.push(g);
                }
            }
        }
        let anns = self.annotate(cx, &dirty)?.unwrap_or_default();
        let fresh: FxHashSet<P::Fact> = added.iter().copied().collect();
        for f in dirty {
            let Some(a) = anns.get(&f).cloned() else {
                continue;
            };
            let old = self.ann.get(&f).cloned();
            let d = match &old {
                Some(o) => a.sub(o),
                None => a.clone(),
            };
            let weight = i64::from(fresh.contains(&f));
            if weight == 0 && d.is_zero() {
                continue;
            }
            self.store_ann(f, a);
            out.push(Change {
                fact: f,
                weight,
                ann: Some(d),
            });
        }
        Ok(out)
    }

    /// Write the facts with their records, then their annotations.
    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.u64(self.facts.len() as u64);
        for (f, r) in &self.facts {
            self.program.save(w, f, r)?;
        }
        if !self.mode.is_none() {
            for f in self.facts.keys() {
                match self.ann.get(f) {
                    Some(a) => a.save(w),
                    None => Ann::zero(self.mode).save(w),
                }
            }
        }
        Ok(())
    }

    /// Read what [`save`](Self::save) wrote.
    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.facts.clear();
        self.ann.clear();
        self.ann_bytes = 0;
        self.program.cleared();
        let n = r.u64()?;
        let mut order = Vec::with_capacity(n.min(1 << 20) as usize);
        for _ in 0..n {
            let (f, rec) = self.program.load(r)?;
            self.facts.insert(f, rec);
            self.program.inserted(&f);
            order.push(f);
        }
        if self.facts.len() != order.len() {
            return Err(Error::Corrupt(
                "a fixed point's state holds a fact twice".into(),
            ));
        }
        if !self.mode.is_none() {
            for f in order {
                let a = Ann::load(r, self.mode)?;
                self.store_ann(f, a);
            }
        }
        Ok(())
    }
}
