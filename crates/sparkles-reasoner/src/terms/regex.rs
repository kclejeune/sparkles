//! Shared compiled regex ownership. The inherited16KiB charge is an opaque
//! estimate, not an exact accounting of regex's private heap (roadmap757).
use crate::budget::{Budget, BudgetFailure, Held};
use sparkles_budget::ParentRefused;
use std::sync::Arc;

const REGEX_BYTES: usize = 16 << 10;

pub(crate) struct BudgetRegex {
    regex: regex::Regex,
    // The last shared payload is destroyed before its credit returns.
    _held: Held,
}

impl BudgetRegex {
    pub fn compile(
        pattern: &str,
        budget: &Arc<Budget>,
    ) -> Result<Option<Arc<Self>>, BudgetFailure> {
        let bytes = pattern
            .len()
            .checked_add("^(?:)$".len())
            .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?;
        let temporary = Held::try_new_exact(budget, bytes)?;
        let owned = REGEX_BYTES
            .checked_add(bytes)
            .and_then(|n| n.checked_add(size_of::<Self>()))
            .and_then(|n| n.checked_add(2 * size_of::<usize>()))
            .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?;
        let held = Held::try_new_exact(budget, owned)?;
        let mut anchored = String::with_capacity(bytes);
        anchored.push_str("^(?:");
        anchored.push_str(pattern);
        anchored.push_str(")$");
        // The default builder and whole-string matching semantics are unchanged.
        let regex = regex::Regex::new(&anchored).ok();
        drop(anchored);
        drop(temporary);
        Ok(regex.map(|regex| Arc::new(Self { regex, _held: held })))
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }

    pub fn captures<'h>(&self, text: &'h str) -> Option<regex::Captures<'h>> {
        self.regex.captures(text)
    }
}

impl std::fmt::Debug for BudgetRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.regex.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terms::Terms;
    use sparkles_core::store::{Store, StoreOptions};

    #[test]
    fn cache_clones_share_credit_and_last_regex_outlives_eviction() {
        let budget = Budget::unlimited();
        let terms = Terms::new_in(
            Store::in_memory(StoreOptions::default()).snapshot(),
            &budget,
        )
        .unwrap();
        let first = terms.try_regex("x+").unwrap().unwrap();
        let used = budget.used();
        let same = terms.try_regex("x+").unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(budget.used(), used, "Arc clone has no new cache pool");
        for i in 0..256 {
            terms.try_regex(&format!("p{i}")).unwrap();
        }
        drop(terms);
        assert!(first.is_match("xxx"));
        assert!(budget.used() > 0);
        drop(first);
        assert!(budget.used() > 0, "second shared owner is still alive");
        drop(same);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn refused_migration_preserves_cache_success_retires_it_at_commit() {
        let budget = Budget::unlimited();
        let terms = Terms::new_in(
            Store::in_memory(StoreOptions::default()).snapshot(),
            &budget,
        )
        .unwrap();
        let first = terms.try_regex("x+").unwrap().unwrap();
        let before = budget.used();
        let refused = Budget::new(Some(1));
        assert!(matches!(
            terms.set_budget(&refused),
            Err(BudgetFailure::Local(_))
        ));
        assert_eq!(refused.used(), 0);
        assert_eq!(budget.used(), before);
        assert!(Arc::ptr_eq(
            &first,
            &terms.try_regex("x+").unwrap().unwrap()
        ));
        let receiving = Budget::unlimited();
        terms.set_budget(&receiving).unwrap();
        assert_eq!(receiving.used(), terms.heap_bytes());
        let fresh = terms.try_regex("x+").unwrap().unwrap();
        assert!(!Arc::ptr_eq(&first, &fresh));
        drop(terms);
        assert!(first.is_match("xxx") && fresh.is_match("xxx"));
        drop(first);
        assert_eq!(budget.used(), 0);
        assert!(receiving.used() > 0);
        drop(fresh);
        assert_eq!(receiving.used(), 0);
    }

    #[test]
    fn eviction_publication_rechecks_key_after_same_shape_interleaving() {
        let budget = Budget::unlimited();
        let terms = Arc::new(
            Terms::new_in(
                Store::in_memory(StoreOptions::default()).snapshot(),
                &budget,
            )
            .unwrap(),
        );
        terms.try_regex("original").unwrap();
        let published = Arc::new(std::sync::Mutex::new(None));
        let save = published.clone();
        let interleaved = terms.clone();
        crate::terms::REGEX_PREPARED.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                // Full eviction and refill restores the original one-entry shape.
                for i in 0..255 {
                    interleaved.try_regex(&format!("entry{i}")).unwrap();
                }
                let same = interleaved.try_regex("same").unwrap().unwrap();
                *save.lock().unwrap() = Some(same);
            }))
        });
        let result = terms.try_regex("same").unwrap().unwrap();
        let earlier = published.lock().unwrap().take().unwrap();
        assert!(
            Arc::ptr_eq(&result, &earlier),
            "unlocked preparation replaced a concurrently published cache handle"
        );
        drop(terms);
        assert!(result.is_match("same"));
        drop(result);
        drop(earlier);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn invalid_and_whole_string_capture_semantics_are_unchanged() {
        let budget = Budget::unlimited();
        assert!(BudgetRegex::compile("[", &budget).unwrap().is_none());
        assert_eq!(budget.used(), 0);
        let regex = BudgetRegex::compile("(x+)(y)", &budget).unwrap().unwrap();
        assert!(!regex.is_match("zxxxy"));
        assert_eq!(
            regex.captures("xxxy").unwrap().get(1).unwrap().as_str(),
            "xxx"
        );
        drop(regex);
        assert_eq!(budget.used(), 0);
    }
}
