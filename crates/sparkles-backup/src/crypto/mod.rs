//! Client-encrypted repository engine. Provider lookup and public configuration
//! wiring are separate; this module accepts explicit local secret inputs.

mod floor;
mod manage;
pub(crate) use crate::seal::{memory, primitive};
pub(crate) mod objects;
pub(crate) mod repository;
pub(crate) mod slots;
pub use crate::seal::{KeyMemory, SEALED_SECRET_PREFIX, SecretSealer};
pub(crate) use repository::{KeyState, Snapshot};
pub use slots::{EncryptionOptions, KeySlotSummary, LocalKey, LocalKeySource, Passphrase};

/// Prepare this process to hold encrypted-repository keys.
///
/// On macOS this permanently disables process core dumps by lowering both the soft
/// and hard `RLIMIT_CORE` limits to zero. Child processes inherit the limits. Call
/// this explicitly before reading key inputs, or configure those limits in the host.
/// The generic key constructors never change process-wide policy themselves.
/// Other supported platforms protect each key mapping and need no preparation.
pub fn prepare_key_memory_for_process() -> crate::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: setrlimit reads a valid limit structure. This explicit host API
        // deliberately lowers both limits and never attempts to raise them.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limits) } != 0 {
            return Err(crate::BackupError::new(
                crate::Code::InvalidConfig,
                "cannot disable macOS process core dumps for repository keys",
            ));
        }
        if !memory::core_dumps_disabled() {
            return Err(crate::BackupError::new(
                crate::Code::InvalidConfig,
                "macOS process core-dump limits were not disabled",
            ));
        }
    }
    Ok(())
}

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

    #[test]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn process_preparation_preserves_per_mapping_platform_core_policy() {
        fn limits() -> libc::rlimit {
            let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            // SAFETY: getrlimit initializes the structure on success.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_CORE, limits.as_mut_ptr()) },
                0
            );
            unsafe { limits.assume_init() }
        }
        let before = limits();
        super::prepare_key_memory_for_process().unwrap();
        let after = limits();
        assert_eq!(before.rlim_cur, after.rlim_cur);
        assert_eq!(before.rlim_max, after.rlim_max);
    }

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
