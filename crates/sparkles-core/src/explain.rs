//! Derivation trees, the answer shape of explanations (spec F18 §2.1, U10 §3.3).
//!
//! The reasoner's search over a materialized closure and the RDFS-on-read explainer
//! ([`crate::sparql::rdfs::RdfsSchema::explain`]) both answer with these types, so the
//! routes, the CLI and the MCP tool render one shape whatever produced it.

use oxrdf::Term;
use std::collections::HashMap;

/// What a node of a derivation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivationKind {
    /// a fact of an input graph, or of the schema RDFS on read was given
    Asserted,
    /// a fact from a rule with an empty body
    Axiom,
    /// a fact a rule instance derived from the node's premises
    Derived,
    /// a fact the tree does not expand because it passed the depth limit; another request
    /// on its triple explains it
    Ref,
}

impl DerivationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DerivationKind::Asserted => "asserted",
            DerivationKind::Axiom => "axiom",
            DerivationKind::Derived => "derived",
            DerivationKind::Ref => "ref",
        }
    }
}

/// The rule of a derived node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleRef {
    /// the rule's name in its rule text, or `rule@line N` for an unnamed rule
    pub name: String,
    /// the profile the rule belongs to, the vocabulary that added it, or `rdfs-on-read`
    pub profile: String,
    /// the rule's id in the OWL 2 RL tables, for the rules of the `owl-rl` profile that
    /// carry one
    pub owl2rl: Option<String>,
}

/// One node of a derivation tree.
#[derive(Clone, Debug)]
pub struct Derivation {
    /// the node's number in the answer; a fact has one number in all the answer's trees,
    /// except that each tree's root has a number of its own
    pub id: u32,
    pub triple: [Term; 3],
    pub kind: DerivationKind,
    /// for an asserted node, the graph that holds the fact: `default` or an IRI. `None`
    /// for a triple of a schema RDFS on read was given as a document.
    pub graph: Option<String>,
    /// for a derived node or an axiom, its rule
    pub rule: Option<RuleRef>,
    /// the premises, in the order of the rule's body
    pub premises: Vec<Derivation>,
    /// whether the node repeats one given earlier in the answer with the same `id`, whose
    /// premises it leaves out
    pub shared: bool,
    /// the depth of the fact's derivation: 0 for an asserted fact, 1 for an axiom
    pub depth: u32,
}

impl Derivation {
    /// An asserted leaf.
    pub fn asserted(triple: [Term; 3], graph: Option<String>) -> Derivation {
        Derivation {
            id: 0,
            triple,
            kind: DerivationKind::Asserted,
            graph,
            rule: None,
            premises: Vec::new(),
            shared: false,
            depth: 0,
        }
    }

    /// A node derived by `rule` from `premises`.
    pub fn derived(triple: [Term; 3], rule: RuleRef, premises: Vec<Derivation>) -> Derivation {
        let depth = 1 + premises.iter().map(|p| p.depth).max().unwrap_or(0);
        Derivation {
            id: 0,
            triple,
            kind: if premises.is_empty() {
                DerivationKind::Axiom
            } else {
                DerivationKind::Derived
            },
            graph: None,
            rule: Some(rule),
            premises,
            shared: false,
            depth,
        }
    }

    /// The number of nodes in the tree, shared repeats included.
    /// The heap the node and its premises hold, with their decoded terms.
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Derivation>()
            + self.triple.iter().map(term_bytes).sum::<usize>()
            + self.graph.as_ref().map_or(0, String::len)
            + self
                .premises
                .iter()
                .map(Derivation::heap_bytes)
                .sum::<usize>()
    }

    pub fn size(&self) -> usize {
        1 + self.premises.iter().map(Derivation::size).sum::<usize>()
    }
}

/// The answer of an explanation.
#[derive(Clone, Debug)]
pub enum Explained {
    /// derivations from the leaves, the first of the smallest depth found, then others
    /// whose top rule differs, and whether derivations by further rules exist
    Found { trees: Vec<Derivation>, more: bool },
    /// no derivation from facts the caller reads exists, or the triple is not entailed
    NotFound,
}

/// Number the nodes of `trees` in the answer's order, give a repeated fact the number it
/// had first with `shared` set and no premises, and cut nodes deeper than `max_depth` as
/// `ref` nodes. Each root keeps a number of its own.
pub fn finish(trees: &mut [Derivation], max_depth: u32) {
    let mut ids: HashMap<[Term; 3], u32> = HashMap::new();
    let mut next = 0u32;
    for t in trees.iter_mut() {
        next += 1;
        t.id = next;
        t.shared = false;
        let mut premises = std::mem::take(&mut t.premises);
        for p in &mut premises {
            number(p, 1, max_depth, &mut ids, &mut next);
        }
        t.premises = premises;
    }
}

fn number(
    d: &mut Derivation,
    level: u32,
    max_depth: u32,
    ids: &mut HashMap<[Term; 3], u32>,
    next: &mut u32,
) {
    if let Some(&id) = ids.get(&d.triple) {
        d.id = id;
        d.shared = true;
        d.premises.clear();
        return;
    }
    *next += 1;
    d.id = *next;
    d.shared = false;
    if d.kind == DerivationKind::Derived && level >= max_depth {
        d.kind = DerivationKind::Ref;
        d.rule = None;
        d.premises.clear();
        return;
    }
    ids.insert(d.triple.clone(), d.id);
    for p in &mut d.premises {
        number(p, level + 1, max_depth, ids, next);
    }
}

/// An explanation that passed its limits: its instance budget, its timeout or the depth
/// its search allows.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct ExplainBudgetExceeded(pub String);

/// The limits of an explanation's answer and search.
#[derive(Clone, Debug)]
pub struct ExplainLimits {
    /// distinct top-level derivations, at least 1
    pub max_trees: usize,
    /// the depth of the trees, deeper nodes are cut as `ref` nodes
    pub max_depth: u32,
    /// when the search gives up
    pub deadline: Option<std::time::Instant>,
    /// set to `true` to stop the search
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// the memory the answer may hold, unbounded when `None`
    pub max_memory: Option<usize>,
}

/// The heap a decoded term holds.
pub fn term_bytes(t: &Term) -> usize {
    std::mem::size_of::<Term>()
        + match t {
            Term::NamedNode(n) => n.as_str().len(),
            Term::BlankNode(b) => b.as_str().len(),
            Term::Literal(l) => {
                l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
            }
            #[allow(unreachable_patterns)]
            _ => 64,
        }
}
