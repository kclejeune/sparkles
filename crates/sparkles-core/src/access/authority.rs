//! Revocation generations shared by a host's policy and credential stores.
//!
//! A generation identifies one stable authority state. It grants no permission,
//! credential identity, dataset access or write authority. Capture an even value
//! before a lookup and require the same value afterwards. Every mutation that can
//! change the rights of principals other than one revoked credential must hold the
//! same registry's update guard across its actual visible changes.
//!
//! Issuing a credential adds authority without changing anyone's rights, so it
//! holds the shared issue guard instead and leaves the generation unchanged. The
//! issuer compares the generation its authentication observed with the guarded
//! value and fails only when an exclusive update completed in between.
//!
//! A write authorized by a credential holds the shared guard across its check and
//! its publication ([`AuthorityRegistry::begin_hold`]). Revoking one credential
//! holds the exclusive revocation guard while it removes the credential
//! ([`AuthorityRegistry::begin_revocation`]), which leaves the generation unchanged.
//! A revocation therefore completes either before the write's check, which then
//! finds the credential gone, or after the write published.

use parking_lot::{RawRwLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A common revocation fence for policy and credential changes.
#[derive(Default)]
pub struct AuthorityRegistry {
    // Access adapters may borrow this fence after completing policy callbacks.
    // Keep it unavailable to external hosts so a generation is never a grant.
    pub(super) epoch: Arc<RwLock<u64>>,
    generation: AtomicU64,
}

/// Holds a registry's exclusive fence across one authority mutation.
///
/// Do not acquire a dataset writer or invoke external policy callbacks while
/// holding this guard. Drop it after publishing all the affected rights changes.
pub struct AuthorityUpdate<'a> {
    epoch: RwLockWriteGuard<'a, u64>,
    generation: &'a AtomicU64,
    completed: u64,
}

impl AuthorityUpdate<'_> {
    /// The stable generation immediately before this exclusive update began.
    /// This observation does not acquire a lock or renew an authentication.
    pub fn previous_generation(&self) -> u64 {
        self.completed - 2
    }
}

impl Drop for AuthorityUpdate<'_> {
    fn drop(&mut self) {
        *self.epoch = self.completed;
        self.generation.store(self.completed, Ordering::Release);
    }
}

/// Holds a registry's shared fence across one credential issuance.
///
/// Exclusive updates wait until it drops, and concurrent issuances share it. Do
/// not acquire another guard of the same registry while holding this one.
pub struct AuthorityIssue<'a> {
    epoch: RwLockReadGuard<'a, u64>,
}

impl AuthorityIssue<'_> {
    /// The stable generation that no exclusive update can change while this
    /// guard is held. It is always even.
    pub fn generation(&self) -> u64 {
        *self.epoch
    }
}

/// Holds a registry's shared fence across one authorized publication, such as a
/// typed write's commit. Owned, so it can be kept by a hook that outlives the
/// caller's borrow of the registry.
///
/// Exclusive updates and revocations wait until it drops. Do not acquire another
/// guard of the same registry while holding this one, and do not wait for a
/// dataset writer while holding it: take it under the writer, right before
/// publication.
pub struct AuthorityHold {
    epoch: parking_lot::ArcRwLockReadGuard<RawRwLock, u64>,
}

impl AuthorityHold {
    /// The stable generation that no exclusive update can change while this guard
    /// is held. It is always even.
    pub fn generation(&self) -> u64 {
        *self.epoch
    }
}

/// Holds a registry's exclusive fence across the removal of one credential, without
/// advancing the generation. It waits until the publications that hold the shared
/// fence are published.
///
/// Do not acquire a dataset writer or another guard of the same registry while
/// holding it.
pub struct AuthorityRevocation<'a> {
    _epoch: RwLockWriteGuard<'a, u64>,
}

impl AuthorityRegistry {
    /// Observe without blocking, including while a read fence is already held
    /// and an updater is queued. Even values are stable. Odd values mean rights
    /// are changing and cannot be attached to a lookup result.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Mark a change before making its actual rights mutations visible.
    ///
    /// Concurrent updates serialize. The next stable even generation is
    /// published when the returned guard drops, including on an error return.
    pub fn begin_update(&self) -> AuthorityUpdate<'_> {
        let epoch = self.epoch.write();
        let changing = epoch.checked_add(1).expect("authority epoch exhausted");
        let completed = changing.checked_add(1).expect("authority epoch exhausted");
        self.generation.store(changing, Ordering::Release);
        AuthorityUpdate {
            epoch,
            generation: &self.generation,
            completed,
        }
    }

    /// Serialize one issuance against exclusive updates without advancing the
    /// generation. Waits while an exclusive update is in progress.
    pub fn begin_issue(&self) -> AuthorityIssue<'_> {
        AuthorityIssue {
            epoch: self.epoch.read(),
        }
    }

    /// Hold the shared fence across one authorized publication. Waits while an
    /// exclusive update or a revocation is in progress.
    pub fn begin_hold(&self) -> AuthorityHold {
        AuthorityHold {
            epoch: self.epoch.read_arc(),
        }
    }

    /// Serialize the removal of one credential against authorized publications in
    /// progress without advancing the generation. Waits while a publication holds
    /// the shared fence.
    pub fn begin_revocation(&self) -> AuthorityRevocation<'_> {
        AuthorityRevocation {
            _epoch: self.epoch.write(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn mutations_are_marked_before_visibility_and_finish_at_the_next_even_generation() {
        let registry = AuthorityRegistry::default();
        assert_eq!(registry.generation(), 0);
        let first = registry.begin_update();
        assert_eq!(first.previous_generation(), 0);
        assert_eq!(registry.generation(), 1);
        drop(first);
        assert_eq!(registry.generation(), 2);
        let second = registry.begin_update();
        assert_eq!(second.previous_generation(), 2);
        assert_eq!(registry.generation(), 3);
        drop(second);
        assert_eq!(registry.generation(), 4);
        assert_eq!(*registry.epoch.read(), 4);
    }

    #[test]
    fn observing_a_stable_generation_does_not_invalidate_it() {
        let registry = AuthorityRegistry::default();
        let original = registry.generation();
        let read = registry.epoch.read();
        assert_eq!(*read, original);
        assert_eq!(registry.generation(), original);
        drop(read);
        assert_eq!(registry.generation(), original);
    }

    #[test]
    fn issuance_keeps_the_generation_and_excludes_updates_until_it_ends() {
        let registry = Arc::new(AuthorityRegistry::default());
        drop(registry.begin_update());
        let issue = registry.begin_issue();
        assert_eq!(issue.generation(), 2);
        assert_eq!(registry.generation(), 2);
        let update_registry = registry.clone();
        let (tx, rx) = mpsc::channel();
        let updater = std::thread::spawn(move || {
            let update = update_registry.begin_update();
            tx.send(update.previous_generation()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "an exclusive update must wait for an issuance in progress"
        );
        assert_eq!(registry.generation(), 2);
        drop(issue);
        let previous = rx.recv_timeout(Duration::from_secs(2));
        updater.join().unwrap();
        assert_eq!(previous.unwrap(), 2);
        assert_eq!(registry.generation(), 4);
    }

    #[test]
    fn a_revocation_waits_for_a_held_publication_and_keeps_the_generation() {
        let registry = Arc::new(AuthorityRegistry::default());
        let hold = registry.begin_hold();
        assert_eq!(hold.generation(), 0);
        let revoking = registry.clone();
        let (tx, rx) = mpsc::channel();
        let revoker = std::thread::spawn(move || {
            let _revocation = revoking.begin_revocation();
            tx.send(revoking.generation()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "a revocation must wait for a publication in progress"
        );
        drop(hold);
        let observed = rx.recv_timeout(Duration::from_secs(2));
        revoker.join().unwrap();
        assert_eq!(observed.unwrap(), 0);
        assert_eq!(
            registry.generation(),
            0,
            "a revocation keeps the generation"
        );
        // an update waits for a hold as well
        let hold = registry.begin_hold();
        let updating = registry.clone();
        let updater = std::thread::spawn(move || drop(updating.begin_update()));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(registry.generation(), 0);
        drop(hold);
        updater.join().unwrap();
        assert_eq!(registry.generation(), 2);
    }

    #[test]
    fn concurrent_mutations_capture_the_preceding_completed_generation() {
        let registry = Arc::new(AuthorityRegistry::default());
        let first = registry.begin_update();
        let next_registry = registry.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let next = next_registry.begin_update();
            tx.send(next.previous_generation()).unwrap();
        });
        assert_eq!(registry.generation(), 1);
        drop(first);
        let previous = rx.recv_timeout(Duration::from_secs(2));
        worker.join().unwrap();
        assert_eq!(previous.unwrap(), 2);
        assert_eq!(registry.generation(), 4);
    }

    #[test]
    fn generation_does_not_reenter_a_fair_read_lock_behind_a_queued_updater() {
        let registry = Arc::new(AuthorityRegistry::default());
        let outer = registry.epoch.read();
        let expected = registry.generation();
        let update_registry = registry.clone();
        let updater = std::thread::spawn(move || drop(update_registry.begin_update()));
        let until = Instant::now() + Duration::from_secs(2);
        let queued = loop {
            if registry.epoch.try_read().is_none() {
                break true;
            }
            if Instant::now() >= until {
                break false;
            }
            std::thread::yield_now();
        };
        let read_registry = registry.clone();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || tx.send(read_registry.generation()).unwrap());
        let observed = rx.recv_timeout(Duration::from_secs(1));
        // Release/join before assertions, so a broken lock-based implementation
        // produces an assertion failure instead of hanging the test process.
        drop(outer);
        updater.join().unwrap();
        reader.join().unwrap();
        assert!(
            queued,
            "updater must really be queued behind the outer fence"
        );
        assert_eq!(observed.unwrap(), expected);
    }
}
