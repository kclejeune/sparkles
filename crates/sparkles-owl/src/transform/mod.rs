//! Ontology transformations with the meaning of ROBOT 1.9.11's commands (spec F24):
//! [`reason`] asserts the inferred axioms of a classification in the ontology,
//! [`reduce`] removes the subclass axioms that other subclass axioms entail,
//! [`relax`] turns equivalences with class expressions into subclass axioms, and
//! [`materialize`] asserts the direct superclasses of the form `ObjectSomeValuesFrom(p B)`
//! for chosen properties. Each works
//! on a [`Document`] in memory, so that commands chain without writing a file.
//!
//! The commands follow ROBOT's operations as they are written, not only as they are
//! documented, so that a release pipeline gets the same axioms. ROBOT runs ELK, and the
//! classification here is the caller's: a closure that classifies a list of axioms, such
//! as the OWL 2 EL classifier of [`crate::classify`]. A query that ELK does not answer,
//! such as the disjoint classes or the property hierarchy, adds no axioms here, as it adds
//! none in ROBOT, which logs a warning. Where ROBOT asks ELK a question of a class
//! expression, the expression is named by a fresh class, as ROBOT itself does in
//! `reduce` and `materialize`, so the classifier answers it from its taxonomy of named
//! classes.

pub(crate) mod materialize;
mod module;
mod reason;
mod reduce;
mod relax;
mod terms;

pub use materialize::{MaterializeOptions, materialize};
pub use module::bottom_module;
pub use reason::{
    EquivalentClasses, Generator, NewOntology, ReasonOptions, Tautologies, is_tautology, reason,
};
pub use reduce::{ReduceOptions, reduce};
pub use relax::{RelaxOptions, relax};
pub use terms::parse_terms;

use crate::classify::{Classification, Excluded, Taxonomy, TaxonomyNode};
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashSet;
use std::collections::BTreeSet;
use std::fmt;

/// The classification an operation runs: the taxonomy of a list of logical axioms and
/// the axioms the classifier left out or used in part.
pub type Classify<'a, E> =
    dyn FnMut(&[&AxiomKind]) -> Result<(Classification, Vec<Excluded>), E> + 'a;

/// An ontology held in memory: its header and its axioms, each once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    pub header: Header,
    pub axioms: BTreeSet<Axiom>,
}

impl Document {
    pub fn new(header: Header, axioms: impl IntoIterator<Item = Axiom>) -> Document {
        Document {
            header,
            axioms: axioms.into_iter().collect(),
        }
    }

    /// The logical axioms without annotations, each once, in order.
    pub fn logical_kinds(&self) -> Vec<&AxiomKind> {
        let mut out: Vec<&AxiomKind> = Vec::with_capacity(self.axioms.len());
        for a in &self.axioms {
            if crate::classify::logical(&a.kind) && out.last() != Some(&&a.kind) {
                out.push(&a.kind);
            }
        }
        out
    }

    /// The classes, object properties and named individuals of the axioms' signature,
    /// as OWL API's `getClassesInSignature` and its siblings give them: the entities of
    /// each axiom, declarations included, but not of its annotations.
    pub fn signature(&self) -> Signature {
        let mut s = crate::model::Signature::default();
        for a in &self.axioms {
            s.axiom(&a.kind);
        }
        let mut out = Signature::default();
        for e in s.0 {
            match e {
                Entity::Class(c) => {
                    out.classes.insert(c);
                }
                Entity::ObjectProperty(p) => {
                    out.object_properties.insert(p);
                }
                Entity::NamedIndividual(i) => {
                    out.individuals.insert(i);
                }
                _ => {}
            }
        }
        out
    }
}

/// The classes, object properties and named individuals of a document.
#[derive(Clone, Debug, Default)]
pub struct Signature {
    pub classes: BTreeSet<Class>,
    pub object_properties: BTreeSet<ObjectProperty>,
    pub individuals: BTreeSet<NamedIndividual>,
}

/// What an operation did, for the command's log.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// axioms added and removed
    pub added: usize,
    pub removed: usize,
    /// the axioms the classifier left out or used only in part, in the classifications
    /// the operation ran
    pub excluded: Vec<Excluded>,
    /// the warnings ROBOT logs for the same input
    pub warnings: Vec<String>,
}

/// Why an operation failed. The messages are ROBOT's where it has one.
#[derive(Debug)]
pub enum Error<E> {
    /// the classification failed, such as by its deadline or memory budget
    Classifier(E),
    /// the ontology has no model
    Inconsistent,
    /// unsatisfiable classes, and the bottom-locality module of them when the options
    /// asked for it (`--dump-unsatisfiable`)
    UnsatisfiableClasses {
        classes: Vec<Class>,
        module: Option<Vec<Axiom>>,
    },
    /// object properties that no pair of individuals can be related by
    UnsatisfiableProperties(Vec<ObjectProperty>),
    /// equivalences between named classes that `--equivalent-classes-allowed` forbids
    EquivalentClasses {
        mode: EquivalentClasses,
        pairs: Vec<(Class, Class)>,
    },
    /// an option ROBOT refuses for this input, with ROBOT's message
    Refused(String),
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Classifier(e) => write!(f, "{e}"),
            Error::Inconsistent => f.write_str(
                "INCONSISTENT ONTOLOGY ERROR the ontology is inconsistent; it has no model",
            ),
            Error::UnsatisfiableClasses { classes, .. } => {
                write!(
                    f,
                    "INCOHERENT TBOX ERROR there are {} unsatisfiable classes in the ontology:",
                    classes.len()
                )?;
                for c in classes {
                    write!(f, "\n    unsatisfiable: {}", c.as_str())?;
                }
                Ok(())
            }
            Error::UnsatisfiableProperties(ps) => {
                write!(
                    f,
                    "INCOHERENT RBOX ERROR there are {} unsatisfiable properties in the ontology:",
                    ps.len()
                )?;
                for p in ps {
                    write!(f, "\n    unsatisfiable property: {}", p.as_str())?;
                }
                Ok(())
            }
            Error::EquivalentClasses { mode, pairs } => {
                write!(
                    f,
                    "EQUIVALENT CLASS AXIOM ERROR one or more equivalent named classes were found. {}",
                    mode.explanation()
                )?;
                for (a, b) in pairs {
                    write!(f, "\n    Equivalence: {} == {}", a.as_str(), b.as_str())?;
                }
                Ok(())
            }
            Error::Refused(m) => f.write_str(m),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for Error<E> {}

/// The node of owl:Thing and of the classes equivalent to it, and the node of
/// owl:Nothing and the unsatisfiable classes, as [`Taxonomy`] numbers them.
pub(crate) const TOP: usize = 0;
pub(crate) const BOTTOM: usize = 1;

/// The classes a taxonomy answers with, the fresh classes an operation added left out.
pub(crate) struct View<'t> {
    pub tax: &'t Taxonomy,
    pub fresh: &'t FxHashSet<Class>,
}

impl<'t> View<'t> {
    pub fn new(c: &'t Classification, fresh: &'t FxHashSet<Class>) -> View<'t> {
        View {
            tax: &c.taxonomy,
            fresh,
        }
    }

    /// The node of a class, or `None` for a class the taxonomy does not hold, which ELK
    /// answers as a direct subclass of owl:Thing.
    pub fn node_of(&self, c: &Class) -> Option<usize> {
        self.tax.node_of(c)
    }

    /// The classes of a node, fresh ones left out.
    pub fn members(&self, n: usize) -> impl Iterator<Item = &'t Class> + '_ {
        let node: &'t TaxonomyNode = self.tax.node(n);
        node.classes.iter().filter(|c| !self.fresh.contains(*c))
    }

    /// The nodes of the direct superclasses of a class, as ELK's
    /// `getSuperClasses(c, true)`: the parents of its node, the top node for a class the
    /// taxonomy does not hold, and none for a class of the top node.
    pub fn direct_super_nodes(&self, c: &Class) -> Vec<usize> {
        match self.node_of(c) {
            None => vec![TOP],
            Some(n) => self.tax.node(n).parents.clone(),
        }
    }

    /// The nodes of all superclasses of a class, its own node left out, as ELK's
    /// `getSuperClasses(c, false)`. An unsatisfiable class has every other node.
    pub fn strict_super_nodes(&self, c: &Class) -> Vec<usize> {
        match self.node_of(c) {
            None => vec![TOP],
            Some(BOTTOM) => (0..self.tax.nodes().len())
                .filter(|&n| n != BOTTOM)
                .collect(),
            Some(n) => self.tax.ancestors(n),
        }
    }

    /// The nodes of the direct subclasses of a node, as ELK's `getSubClasses(c, true)`:
    /// its children, or the bottom node for a node without them.
    pub fn direct_sub_nodes(&self, n: usize) -> Vec<usize> {
        let node = self.tax.node(n);
        if node.children.is_empty() && n != BOTTOM {
            vec![BOTTOM]
        } else {
            node.children.clone()
        }
    }
}

/// A class IRI that names nothing in `taken`: `base` with a number appended.
pub(crate) fn fresh_class(base: &str, n: usize, taken: &BTreeSet<Class>) -> Class {
    let mut i = 0usize;
    loop {
        let c = if i == 0 {
            Class::new(format!("{base}{n}"))
        } else {
            Class::new(format!("{base}{n}-{i}"))
        };
        if !taken.contains(&c) {
            return c;
        }
        i += 1;
    }
}

pub(crate) fn thing() -> Class {
    Class::new(vocab::OWL_THING)
}

pub(crate) fn nothing() -> Class {
    Class::new(vocab::OWL_NOTHING)
}

#[cfg(test)]
pub(crate) mod tests;
