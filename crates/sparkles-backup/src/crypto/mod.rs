//! Client-encrypted repository engine. Provider lookup and public configuration
//! wiring are separate; this module accepts explicit local secret inputs.

mod manage;
mod memory;
pub(crate) mod objects;
mod primitive;
pub(crate) mod repository;
pub(crate) mod slots;
pub(crate) use repository::{Snapshot, open_snapshot};
pub use slots::{EncryptionOptions, KeySlotSummary, LocalKey, LocalKeySource, Passphrase};

#[cfg(test)]
mod tests;

// Keep bounded Argon2 work off async runtime workers. A dropped caller leaves an
// independently owned, zeroizing task whose bounded derivation can finish safely.
static KEY_WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> crate::Result<T> + Send + 'static,
) -> crate::Result<T> {
    let permit = KEY_WORK.acquire().await.map_err(|_| {
        crate::BackupError::new(
            crate::Code::Internal,
            "repository key worker admission failed",
        )
    })?;
    tokio::task::spawn_blocking(move || {
        // The worker owns its admission until completion, even if the caller is
        // cancelled. At most two 64MiB Argon2 derivations run in this process.
        let _permit = permit;
        f()
    })
    .await
    .map_err(|_| {
        crate::BackupError::new(
            crate::Code::Internal,
            "repository cryptography worker failed",
        )
    })?
}

#[cfg(test)]
mod worker_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[tokio::test]
    async fn concurrent_key_jobs_bound_work_and_release_admission_after_errors() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut jobs = Vec::new();
        for i in 0..20 {
            let (active, peak) = (active.clone(), peak.clone());
            jobs.push(tokio::spawn(super::blocking(move || {
                let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(running, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
                active.fetch_sub(1, Ordering::SeqCst);
                if i % 2 == 0 {
                    Ok(i)
                } else {
                    Err(crate::BackupError::new(
                        crate::Code::Internal,
                        "bounded test worker error",
                    ))
                }
            })));
        }
        for (i, job) in jobs.into_iter().enumerate() {
            assert_eq!(job.await.unwrap().is_ok(), i % 2 == 0);
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!((1..=2).contains(&peak.load(Ordering::SeqCst)));
    }
}
