//! Matching a neighbourhood against a shape's triple expression: candidates per arc from
//! the typing, per-constraint counting for flat shapes, bag derivatives for
//! deterministic ones, and distributions of ambiguous arcs over their candidates, each
//! tested vector charged to the partition budget. Semantic actions are enumerated
//! within a matching vector under the same budget.

use crate::error::todo;
use crate::ir::{ShapeIr, TcId, Tri};
use crate::neigh::Neigh;
use crate::semact::ActCtx;
use sparkles::id::Id;
use sparkles::{Budget as Exceeded, BudgetKind};

/// The partitions one match may still try ([`crate::ValidateOptions::max_partitions`]).
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// `None`: unlimited
    pub limit: Option<u64>,
    pub used: u64,
}

impl Budget {
    pub fn new(limit: Option<u64>) -> Budget {
        Budget { limit, used: 0 }
    }

    /// Count `n` more partitions; [`sparkles::Error::BudgetExceeded`] with the
    /// `validation-work` budget once past the limit.
    pub fn charge(&mut self, n: u64) -> sparkles::Result<()> {
        self.used = self.used.saturating_add(n);
        match self.limit {
            Some(limit) if self.used > limit => Err(sparkles::Error::BudgetExceeded(Exceeded {
                kind: BudgetKind::ValidationWork,
                limit,
                requested: self.used,
            })),
            _ => Ok(()),
        }
    }
}

/// Does `neigh` match `shape`? `read(value, tc)` tells whether an arc's value satisfies
/// a triple constraint's value expression under the current typing (`Unknown` during
/// discovery). Budget errors are [`sparkles::Error::BudgetExceeded`].
pub fn matches(
    shape: &ShapeIr,
    neigh: &Neigh,
    read: &dyn Fn(Id, TcId) -> Tri,
    budget: &mut Budget,
    acts: &mut ActCtx<'_>,
) -> anyhow::Result<Tri> {
    let _ = (shape, neigh, read, budget, acts);
    Err(todo("shape matching"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_validation_work() {
        let mut b = Budget::new(Some(3));
        b.charge(3).unwrap();
        match b.charge(1) {
            Err(sparkles::Error::BudgetExceeded(e)) => {
                assert_eq!(
                    (e.kind, e.limit, e.requested),
                    (BudgetKind::ValidationWork, 3, 4)
                )
            }
            other => panic!("{other:?}"),
        }
        Budget::new(None).charge(u64::MAX).unwrap();
    }
}
