//! What the saturated contexts of a run say about one class: its equivalent classes, its
//! superclasses and subclasses, direct and all, and its instances. A DL query asks this
//! of a fresh class defined as equivalent to its class expression.

use super::Classifier;
use super::index::{BOTTOM, Ex, Node, TOP};
use crate::model::*;
use rustc_hash::{FxHashMap, FxHashSet};

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

impl Classifier {
    /// What the last run says about the class `c`, from its saturated contexts and
    /// without building a taxonomy. `None` when no run has saturated the axioms held,
    /// as before the first run, after a run that stopped part way or with changes not
    /// yet classified, and when `c` is not in the signature of the axioms held.
    ///
    /// In an inconsistent ontology every class is unsatisfiable, so the answer is that
    /// of an unsatisfiable class, with every individual as an instance.
    pub fn describe(&self, c: &Class) -> Option<Description> {
        if self.dirty || self.index.is_none() || !self.pending.is_empty() {
            return None;
        }
        let q = self.interner.class_ex(c)?;
        if q != TOP && q != BOTTOM && !self.classes.contains_key(c) {
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
            .classes
            .keys()
            .filter_map(|k| self.interner.class_ex(k).map(|e| (e, k.clone())))
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
        let unsatisfiable = |e: Ex| self.named_subsumers(e).is_some_and(|(b, _)| b);
        if !self.last_consistent {
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
        let (bottom, subsumers) = self
            .named_subsumers(q)
            .unwrap_or((q == BOTTOM, vec![q, TOP]));
        if bottom {
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
        // `a ⊑ b` among saturated contexts
        let below = |a: Ex, b: Ex| -> bool {
            b == TOP
                || a == b
                || self
                    .ctx
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
            if *e == q || eq_set.contains(e) {
                continue;
            }
            if let Some((false, ss)) = self.named_subsumers(*e)
                && (q == TOP || ss.contains(&q))
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
}
