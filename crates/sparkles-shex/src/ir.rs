//! The compiled form of a schema: shape expressions in an arena, references resolved,
//! `&include`s expanded, triple expressions flattened per shape, and the pair kinds the
//! typing assigns to (node, shape expression) pairs. Independent of any snapshot:
//! predicates and value-set terms are strings here, resolved to store ids per snapshot
//! ([`crate::neigh::SnapPlan`], [`crate::nc::NcPlan`]).

use crate::ast::{Label, NodeConstraint, SemAct};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            #[inline]
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(
    /// A shape expression: an index into [`Ir::ses`].
    SeId
);
id_type!(
    /// A node constraint: an index into [`Ir::ncs`].
    NcId
);
id_type!(
    /// A shape: an index into [`Ir::shapes`].
    ShapeId
);
id_type!(
    /// A triple constraint of one shape: an index into [`ShapeIr::tcs`].
    TcId
);
id_type!(
    /// A triple expression of one shape: an index into [`ShapeIr::te`].
    TeId
);
id_type!(
    /// What the typing keys pairs by, besides the node: a declared label, START, or the
    /// value expression of a triple constraint. An index into [`Ir::pairs`].
    PairKind
);

/// A compiled shape expression.
#[derive(Clone, Debug)]
pub enum Se {
    And(Vec<SeId>),
    Or(Vec<SeId>),
    Not(SeId),
    /// a reference: reads the typing of the pair (node, kind) at the same node
    Ref(PairKind),
    Nc(NcId),
    Shape(ShapeId),
    /// an EXTERNAL shape that nothing references (a referenced one without a definition
    /// is a schema error)
    External,
}

/// A compiled node constraint: the constraint and its pattern, compiled.
#[derive(Clone, Debug)]
pub struct NcIr {
    pub nc: NodeConstraint,
    pub regex: Option<regex::Regex>,
}

/// The direction of the arcs a triple constraint matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    /// `(node, p, value)`
    Out,
    /// `(value, p, node)`: an inverse constraint (`^p`)
    In,
}

/// How a shape is matched (see [`crate::matcher`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeClass {
    /// one triple constraint, or an EachOf of triple constraints without group
    /// cardinality, with one triple constraint per (predicate, direction): per-constraint
    /// counting
    Flat,
    /// one triple constraint per (predicate, direction): bag derivatives over counts
    Deterministic,
    /// some (predicate, direction) has several triple constraints: distributions of
    /// arcs over candidates
    Ambiguous,
}

/// A compiled triple expression of a shape.
#[derive(Clone, Debug)]
pub enum Te {
    Tc(TcId),
    EachOf {
        kids: Vec<TeId>,
        min: u32,
        /// `None`: unbounded
        max: Option<u32>,
        acts: Vec<SemAct>,
    },
    OneOf {
        kids: Vec<TeId>,
        min: u32,
        max: Option<u32>,
        acts: Vec<SemAct>,
    },
}

/// A compiled triple constraint (one instance per occurrence after `&include`
/// expansion).
#[derive(Clone, Debug)]
pub struct TcIr {
    /// predicate IRI
    pub pred: String,
    pub dir: Dir,
    /// the value expression (`None`: any value)
    pub value: Option<SeId>,
    /// the pair kind of the value expression, when it has one
    pub pair: Option<PairKind>,
    pub min: u32,
    /// `None`: unbounded
    pub max: Option<u32>,
    pub sem_acts: Vec<SemAct>,
}

/// A compiled shape.
#[derive(Clone, Debug)]
pub struct ShapeIr {
    pub closed: bool,
    /// `EXTRA` predicate IRIs
    pub extra: SmallVec<[String; 2]>,
    /// the shape's triple constraints
    pub tcs: Vec<TcIr>,
    /// the shape's triple expressions (an arena; [`Self::root`] is the expression)
    pub te: Vec<Te>,
    /// `None`: the shape has no triple expression (`{}`)
    pub root: Option<TeId>,
    /// the triple constraints per (predicate, direction), in first-seen order
    pub preds: Vec<(String, Dir, SmallVec<[TcId; 2]>)>,
    /// per triple constraint: the product of the maximum cardinalities from it up to
    /// the root (`None`: unbounded)
    pub max_occ: Vec<Option<u64>>,
    pub class: ShapeClass,
    pub sem_acts: Vec<SemAct>,
}

/// A pair kind: the shape expression evaluated for its pairs.
#[derive(Clone, Debug)]
pub struct PairKindInfo {
    pub se: SeId,
    /// the declared label (`None` for START and for value expressions)
    pub label: Option<Label>,
    /// the stratum the pairs are decided in (lower strata first)
    pub stratum: u32,
}

/// A compiled schema.
#[derive(Clone, Debug, Default)]
pub struct Ir {
    pub ses: Vec<Se>,
    pub ncs: Vec<NcIr>,
    pub shapes: Vec<ShapeIr>,
    pub pairs: Vec<PairKindInfo>,
    /// declared labels, by their ShExJ form (the IRI, or `_:label`)
    pub labels: FxHashMap<String, PairKind>,
    /// the pair kind of START, if the schema has one
    pub start: Option<PairKind>,
    /// semantic actions run once per validation
    pub start_acts: Vec<SemAct>,
    /// the number of strata (strata are `0..strata`)
    pub strata: u32,
}

/// A three-valued (Kleene) truth value: a typing read during discovery is `Unknown`
/// until the pair is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tri {
    False,
    Unknown,
    True,
}

impl std::ops::Not for Tri {
    type Output = Tri;
    fn not(self) -> Tri {
        match self {
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
            Tri::True => Tri::False,
        }
    }
}

impl Tri {
    pub fn and(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Unknown,
        }
    }

    pub fn or(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Unknown,
        }
    }
}

impl From<bool> for Tri {
    fn from(b: bool) -> Tri {
        if b { Tri::True } else { Tri::False }
    }
}

#[cfg(test)]
mod tests {
    use super::Tri::{self, *};

    #[test]
    fn kleene_logic() {
        let all = [False, Unknown, True];
        for a in all {
            assert_eq!(!!a, a);
            for b in all {
                assert_eq!(a.and(b), b.and(a));
                assert_eq!(a.or(b), b.or(a));
                // De Morgan
                assert_eq!(!a.and(b), (!a).or(!b));
            }
        }
        assert_eq!(Unknown.and(False), False);
        assert_eq!(Unknown.or(True), True);
        assert_eq!(Unknown.and(True), Unknown);
        assert_eq!(Tri::from(true), True);
    }
}
