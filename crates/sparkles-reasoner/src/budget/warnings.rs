//! Private warning storage. Public report transfer remains an explicit675 output
//! boundary; compiler scratch never stands in for this actual Vec/String owner.
use super::{Budget, BudgetFailure, Held};
use sparkles_budget::ParentRefused;
use std::fmt::{self, Write};
use std::sync::Arc;

pub(crate) struct WorkingWarnings {
    values: Vec<String>,
    held: Held,
}
impl std::fmt::Debug for WorkingWarnings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.values.fmt(f)
    }
}

pub(crate) fn format_len(args: fmt::Arguments<'_>) -> Result<usize, BudgetFailure> {
    struct Count(usize);
    impl Write for Count {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            self.0 = self.0.checked_add(s.len()).ok_or(fmt::Error)?;
            Ok(())
        }
    }
    let mut count = Count(0);
    fmt::write(&mut count, args).map_err(|_| BudgetFailure::parent(ParentRefused::Overflow))?;
    Ok(count.0)
}

impl WorkingWarnings {
    pub fn new(budget: &Arc<Budget>) -> Self {
        Self {
            values: Vec::new(),
            held: Held::new(budget),
        }
    }
    pub fn try_clone_from(values: &[String], budget: &Arc<Budget>) -> Result<Self, BudgetFailure> {
        let mut owned = Self::new(budget);
        for value in values {
            owned.push_fmt(format_args!("{value}"))?;
        }
        Ok(owned)
    }
    pub fn as_slice(&self) -> &[String] {
        &self.values
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.values.iter()
    }

    pub fn push_fmt(&mut self, args: fmt::Arguments<'_>) -> Result<(), BudgetFailure> {
        let bytes = format_len(args)?;
        // Admission precedes formatting and both sides of a Vec reallocation.
        let cap = self.values.capacity();
        let next = if self.values.len() == cap {
            Some(
                cap.checked_mul(2)
                    .map(|n| n.max(4))
                    .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?,
            )
        } else {
            None
        };
        let vector = next
            .map_or(Some(0), |n| n.checked_mul(size_of::<String>()))
            .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?;
        let need = bytes
            .checked_add(vector)
            .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?;
        let mut prepared = Held::try_new_exact(self.held.budget(), need)?;
        let mut replacement = next.map(Vec::<String>::with_capacity);
        if let Some(replacement) = &replacement {
            let actual = replacement
                .capacity()
                .checked_mul(size_of::<String>())
                .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?;
            if actual > vector {
                prepared.try_grow(actual - vector)?;
            } else {
                prepared.shrink(vector - actual);
            }
        }
        let mut value = String::with_capacity(bytes);
        fmt::write(&mut value, args).expect("private formatting into String is infallible");
        if value.capacity() > bytes {
            prepared.try_grow(value.capacity() - bytes)?;
        }
        self.held.try_merge(&mut prepared)?;
        if let Some(mut replacement) = replacement.take() {
            replacement.append(&mut self.values);
            let retired = std::mem::replace(&mut self.values, replacement);
            drop(retired);
            self.held.shrink(cap * size_of::<String>());
        }
        self.values.push(value);
        Ok(())
    }
    pub fn extend_borrowed<'a>(
        &mut self,
        values: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), BudgetFailure> {
        for value in values {
            self.push_fmt(format_args!("{value}"))?;
        }
        Ok(())
    }

    /// The existing public report surface is migrated separately in675 phase3.
    /// This explicit boundary is never evidence of retained-output enforcement.
    pub fn into_legacy_report(self) -> Vec<String> {
        let Self { values, held } = self;
        drop(held);
        values
    }
}
impl std::ops::Deref for WorkingWarnings {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_append_keeps_existing_warning_and_full_drop_returns_credit() {
        let budget = Budget::new(Some(1024));
        let mut warnings = WorkingWarnings::new(&budget);
        warnings.push_fmt(format_args!("first {}", 17)).unwrap();
        let before = budget.used();
        assert!(matches!(
            warnings.push_fmt(format_args!("{:width$}", "", width = 2048)),
            Err(BudgetFailure::Local(_))
        ));
        assert_eq!(warnings.as_slice(), ["first 17"]);
        assert_eq!(budget.used(), before);
        drop(warnings);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn parent_refusal_precedes_formatting_and_keeps_scope() {
        let pool = sparkles_core::sparql::pool::QueryPool::new(1);
        let budget = Budget::with_parent(None, Some(pool.work_parent()));
        let mut warnings = WorkingWarnings::new(&budget);
        assert!(matches!(
            warnings.push_fmt(format_args!("warning")),
            Err(BudgetFailure::Parent {
                refusal: ParentRefused::Limit {
                    scope: sparkles_budget::Scope::Work,
                    ..
                },
                ..
            })
        ));
        assert!(warnings.is_empty());
        assert_eq!(budget.used(), 0);
        drop(warnings);
        drop(budget);
        assert_eq!(pool.granted(), 0);
    }
}
