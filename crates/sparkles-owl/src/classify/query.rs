//! What the saturated contexts of a run say about one class: its equivalent classes, its
//! superclasses and subclasses, direct and all, and its instances. A DL query asks this
//! of a fresh class defined as equivalent to its class expression.

use super::Saturation;
use super::index::{BOTTOM, Ex, Node, TOP};
use crate::model::*;
use rustc_hash::{FxHashMap, FxHashSet};

/// Cached top and inferred-bottom closures for [`Saturation::describe`], matching the
/// taxonomy's equivalence-inferred bottom propagation and upward top lifting.
#[derive(Debug)]
pub(super) struct QueryClosure {
    top: FxHashSet<Ex>,
    bottom: FxHashSet<Ex>,
}

/// What a run says about one class. Classes are sorted by IRI, and the class itself is
/// left out of every list. The superclasses include `owl:Thing`, and the subclasses
/// leave out `owl:Nothing` and the unsatisfiable classes, as the taxonomy's bottom node
/// leaves them out of every other node's descendants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Description {
    /// false when the class is unsatisfiable; its equivalent classes are then
    /// `owl:Nothing` and the other unsatisfiable classes, and every other list is empty
    pub satisfiable: bool,
    pub equivalents: Vec<Class>,
    pub superclasses: Vec<Class>,
    /// the superclasses with no other superclass below them
    pub direct_superclasses: Vec<Class>,
    pub subclasses: Vec<Class>,
    /// the subclasses with no other subclass above them
    pub direct_subclasses: Vec<Class>,
    /// the named individuals of the axioms whose type the class is
    pub instances: Vec<NamedIndividual>,
}

impl Saturation {
    /// What the last run says about the class `c`, from its saturated contexts and
    /// without building a taxonomy. `None` when no run has saturated its axioms, as
    /// before the first run and after a run that stopped part way, and when `c` is not
    /// in their signature. The answer is that of the axioms the last run read, which
    /// the caller keeps unchanged until it asks.
    ///
    /// In an inconsistent ontology every class is unsatisfiable, so the answer is that
    /// of an unsatisfiable class, with every individual as an instance.
    pub fn describe(&self, c: &Class) -> Option<Description> {
        let mut d = self.describe_all(c)?;
        // an anonymous individual is a fresh individual of the run, which no answer names
        // (roadmap item 1281)
        d.instances.retain(|i| !super::index::is_anonymous(i));
        if self.reduced {
            // the names that item 805's reductions made up, such as the hub individual and
            // the classes of the keys, stay out of the answer
            let ours = |s: &str| !s.starts_with(super::dl_reduce::PREFIX);
            for v in [
                &mut d.equivalents,
                &mut d.superclasses,
                &mut d.direct_superclasses,
                &mut d.subclasses,
                &mut d.direct_subclasses,
            ] {
                v.retain(|k| ours(k.as_str()));
            }
            d.instances.retain(|i| ours(i.as_str()));
        }
        Some(d)
    }

    /// [`Saturation::describe`] with every name of the run's axioms.
    fn describe_all(&self, c: &Class) -> Option<Description> {
        if self.dirty || self.index.is_none() {
            return None;
        }
        let q = self.interner.class_ex(c)?;
        if q != TOP && q != BOTTOM && !self.interner.in_signature(c) {
            return None;
        }
        let class_of = |e: Ex| -> Option<Class> {
            match self.interner.nodes[e as usize] {
                Node::Class(id) => Some(self.interner.classes[id as usize].clone()),
                Node::Top => Some(Class::thing()),
                Node::Bottom => Some(Class::nothing()),
                _ => None,
            }
        };
        let sorted = |mut v: Vec<Class>| {
            v.sort();
            v.dedup();
            v
        };
        // the named classes of the signature with their expressions
        let named: Vec<(Ex, Class)> = self
            .interner
            .met_classes()
            .filter_map(|e| class_of(e).map(|k| (e, k)))
            .collect();
        let individuals = || -> Vec<(Ex, NamedIndividual)> {
            self.interner
                .individuals
                .iter()
                .enumerate()
                .filter_map(|(i, n)| {
                    self.interner
                        .get(&Node::Nominal(i as u32))
                        .map(|e| (e, n.clone()))
                })
                .filter(|(e, _)| self.named_subsumers(*e).is_some())
                .collect()
        };
        if !self.consistent {
            let mut eq: Vec<Class> = named.iter().map(|(_, k)| k.clone()).collect();
            eq.push(Class::thing());
            eq.push(Class::nothing());
            eq.retain(|k| k != c);
            let mut is: Vec<NamedIndividual> = individuals().into_iter().map(|(_, i)| i).collect();
            is.sort();
            return Some(Description {
                satisfiable: false,
                equivalents: sorted(eq),
                instances: is,
                ..Default::default()
            });
        }
        let closure = self.query_closure();
        let bottom = &closure.bottom;
        let top = &closure.top;
        let unsatisfiable = |e: Ex| e == BOTTOM || bottom.contains(&e) || self.is_bottom(e);
        let (raw_bottom, mut subsumers) = self
            .named_subsumers(q)
            .unwrap_or((q == BOTTOM, vec![q, TOP]));
        if raw_bottom || unsatisfiable(q) {
            let mut eq: Vec<Class> = named
                .iter()
                .filter(|(e, _)| unsatisfiable(*e))
                .map(|(_, k)| k.clone())
                .collect();
            eq.push(Class::nothing());
            eq.retain(|k| k != c);
            return Some(Description {
                satisfiable: false,
                equivalents: sorted(eq),
                ..Default::default()
            });
        }
        // Every satisfiable class reachable upward from owl:Thing is equivalent to
        // owl:Thing, including through hidden internal classes (item 1086).
        for &e in top {
            let visible = match self.interner.nodes[e as usize] {
                Node::Top => true,
                Node::Class(id) => self.interner.met.get(id as usize).copied().unwrap_or(false),
                _ => false,
            };
            if visible {
                subsumers.push(e);
            }
        }
        // Unsatisfiable classes stay at bottom even if an incomplete row named one.
        subsumers.retain(|&s| !unsatisfiable(s));
        if self.reduced {
            // the made-up classes of item 805's reductions take no part in the answer,
            // whose direct superclasses are those among the visible ones
            subsumers.retain(|&e| {
                class_of(e).is_none_or(|k| !k.as_str().starts_with(super::dl_reduce::PREFIX))
            });
        }
        subsumers.sort_unstable();
        subsumers.dedup();
        // `a ⊑ b` among saturated contexts, where a class classified in a world of its
        // own reads its subsumers from that world
        let below = |a: Ex, b: Ex| -> bool {
            if b == TOP || a == b || top.contains(&b) || unsatisfiable(a) {
                return true;
            }
            if let Some((bottom, subs)) = self.worlds.get(&a) {
                return *bottom || subs.contains(&b);
            }
            self.ctx
                .get(a as usize)
                .and_then(|x| x.get())
                .is_some_and(|x| {
                    let st = x.st.lock();
                    st.bottom || st.subs.contains(&b)
                })
        };
        let mut equivalents = Vec::new();
        let mut supers: Vec<Ex> = Vec::new();
        for s in subsumers {
            if s == q {
                continue;
            }
            if below(s, q) {
                equivalents.push(s);
            } else {
                supers.push(s);
            }
        }
        let direct_supers: Vec<Ex> = supers
            .iter()
            .copied()
            .filter(|&s| {
                !supers
                    .iter()
                    .any(|&t| t != s && below(t, s) && !below(s, t))
            })
            .collect();
        // the subclasses: the satisfiable named classes whose context has `q`, with
        // their named subsumers for the direct ones
        let mut subs: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
        let eq_set: FxHashSet<Ex> = equivalents.iter().copied().collect();
        for (e, _) in &named {
            if *e == q || eq_set.contains(e) || unsatisfiable(*e) {
                continue;
            }
            if let Some((false, ss)) = self.named_subsumers(*e)
                && (top.contains(&q) || ss.contains(&q))
            {
                subs.insert(*e, ss);
            }
        }
        let direct_subs: Vec<Ex> = subs
            .iter()
            .filter(|(x, ss)| {
                !ss.iter()
                    .any(|y| y != *x && subs.get(y).is_some_and(|ys| !ys.contains(x)))
            })
            .map(|(x, _)| *x)
            .collect();
        let instances: Vec<NamedIndividual> = individuals()
            .into_iter()
            .filter(|(e, _)| below(*e, q))
            .map(|(_, i)| i)
            .collect();
        let classes = |es: &[Ex]| sorted(es.iter().filter_map(|e| class_of(*e)).collect());
        let mut instances = instances;
        instances.sort();
        Some(Description {
            satisfiable: true,
            equivalents: classes(&equivalents),
            superclasses: classes(&supers),
            direct_superclasses: classes(&direct_supers),
            subclasses: classes(&subs.keys().copied().collect::<Vec<_>>()),
            direct_subclasses: classes(&direct_subs),
            instances,
        })
    }

    /// Cached top and inferred-bottom closures across descriptions of the same saturated
    /// state.
    fn query_closure(&self) -> &QueryClosure {
        self.query_cache
            .get_or_init(|| self.compute_query_closure())
    }

    fn compute_query_closure(&self) -> QueryClosure {
        let bottom = self.compute_bottom_closure();
        let top = self.compute_top_closure(&bottom);
        QueryClosure { top, bottom }
    }

    /// Visible classes marked unsatisfiable or mutually subsuming with one, matching
    /// [`super::taxonomy::build`]'s inferred-bottom rule.
    fn compute_bottom_closure(&self) -> FxHashSet<Ex> {
        let mut bottom = FxHashSet::default();
        let mut stack: Vec<Ex> = Vec::new();
        for e in self.interner.met_classes() {
            if self.is_bottom(e) && bottom.insert(e) {
                stack.push(e);
            }
        }
        while let Some(u) = stack.pop() {
            let Some((_, subs)) = self.named_subsumers(u) else {
                continue;
            };
            for b in subs {
                if self.is_visible_class(b)
                    && !bottom.contains(&b)
                    && self.raw_has_subsumer(b, u)
                    && bottom.insert(b)
                {
                    stack.push(b);
                }
            }
        }
        bottom
    }

    /// `TOP` and every satisfiable `Node::Class` reachable upward from `TOP` through
    /// named subsumers, including through hidden internal classes (item 1086), stopping
    /// before both raw and equivalence-inferred unsatisfiable classes.
    fn compute_top_closure(&self, bottom: &FxHashSet<Ex>) -> FxHashSet<Ex> {
        let mut top = FxHashSet::default();
        top.insert(TOP);
        if self.top_row_closed() {
            #[cfg(test)]
            top_closure_count_visit();
            let _ = self.visit_satisfiable_named_subsumers(TOP, |e| {
                if e == TOP || !bottom.contains(&e) {
                    top.insert(e);
                }
                Ok::<(), std::convert::Infallible>(())
            });
            top.retain(|&e| e == TOP || (!bottom.contains(&e) && !self.is_bottom(e)));
            return top;
        }
        let mut seen = FxHashSet::default();
        seen.insert(TOP);
        let mut stack: Vec<Ex> = Vec::new();
        #[cfg(test)]
        top_closure_count_visit();
        let seed_ok = self.visit_satisfiable_named_subsumers(TOP, |e| {
            if !bottom.contains(&e) && seen.insert(e) {
                stack.push(e);
            }
            Ok::<(), std::convert::Infallible>(())
        });
        if !matches!(seed_ok, Ok(Some(false))) {
            return top;
        }
        while let Some(e) = stack.pop() {
            #[cfg(test)]
            top_closure_count_visit();
            let visited = self.visit_satisfiable_named_subsumers(e, |x| {
                if !bottom.contains(&x) && seen.insert(x) {
                    stack.push(x);
                }
                Ok::<(), std::convert::Infallible>(())
            });
            match visited {
                Ok(Some(true)) => continue,
                Ok(Some(false)) | Ok(None) => {
                    top.insert(e);
                }
                Err(infallible) => match infallible {},
            }
        }
        top
    }
}

#[cfg(test)]
thread_local! {
    static TOP_CLOSURE_ROW_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn top_closure_count_visit() {
    TOP_CLOSURE_ROW_VISITS.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
fn take_top_closure_row_visits() -> usize {
    TOP_CLOSURE_ROW_VISITS.with(|c| c.replace(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Classifier, Limits};

    fn c(s: &str) -> Class {
        Class::new(format!("http://ex.org/{s}"))
    }

    #[test]
    fn describe_thing_on_dense_el_top_node_does_not_rescan_quadratically() {
        let n = 200usize;
        let mut axioms: Vec<Axiom> = (0..n)
            .map(|i| Axiom::sub_class_of(Class::thing(), c(&format!("C{i:04}"))))
            .collect();
        axioms.push(Axiom::sub_class_of(c("D"), Class::thing()));
        let kinds: Vec<&AxiomKind> = axioms.iter().map(|a| &a.kind).collect();
        let mut k = Saturation::new();
        k.classify(&kinds, None, &Limits::default()).unwrap();

        let _ = take_top_closure_row_visits();
        let first = k.describe(&Class::thing()).unwrap();
        let first_visits = take_top_closure_row_visits();
        assert_eq!(first.equivalents.len(), n);
        assert_eq!(first.subclasses, vec![c("D")]);
        assert!(
            first_visits <= 1,
            "dense EL top node should read only TOP's row, visited {first_visits} rows"
        );

        for _ in 0..4 {
            let repeated = k.describe(&Class::thing()).unwrap();
            assert_eq!(repeated, first);
        }
        let repeat_visits = take_top_closure_row_visits();
        assert_eq!(
            repeat_visits, 0,
            "repeated describe(owl:Thing) should reuse cached top closure, visited {repeat_visits} rows"
        );
    }

    #[test]
    fn describe_caches_top_closure_and_invalidates_on_reclassify() {
        let a_ind = NamedIndividual::new("http://ex.org/a");
        let r = ObjectProperty::new("http://ex.org/r");
        let axioms = vec![
            Axiom::sub_class_of(
                Class::thing(),
                ClassExpression::ObjectOneOf([Individual::Named(a_ind.clone())].into()),
            ),
            Axiom::sub_class_of(Class::thing(), c("A")),
            Axiom::new(AxiomKind::ObjectPropertyAssertion(
                r.clone().into(),
                Individual::Named(a_ind.clone()),
                Individual::Named(a_ind),
            )),
            Axiom::sub_class_of(c("A"), ClassExpression::all(r, c("B"))),
            Axiom::sub_class_of(c("B"), c("C")),
        ];
        let limits = Limits {
            rooted: true,
            ..Limits::default()
        };
        let mut clf = Classifier::of(&axioms);
        clf.classify(&limits).unwrap();

        let _ = take_top_closure_row_visits();
        let d1 = clf.describe(&Class::thing()).unwrap();
        let first_visits = take_top_closure_row_visits();
        assert!(first_visits >= 1);
        assert_eq!(d1.equivalents, vec![c("A"), c("B"), c("C")]);

        for _ in 0..3 {
            assert_eq!(clf.describe(&Class::thing()).unwrap(), d1);
            assert!(
                clf.describe(&c("B"))
                    .unwrap()
                    .equivalents
                    .contains(&Class::thing())
            );
        }
        assert_eq!(
            take_top_closure_row_visits(),
            0,
            "repeated describe calls should reuse cached top closure"
        );

        // Mutating axioms and reclassifying must invalidate the cached top closure.
        let extra = Axiom::sub_class_of(c("C"), c("D"));
        clf.add(std::iter::once(&extra));
        clf.classify(&limits).unwrap();
        let d2 = clf.describe(&Class::thing()).unwrap();
        assert_eq!(d2.equivalents, vec![c("A"), c("B"), c("C"), c("D")]);
    }
}
