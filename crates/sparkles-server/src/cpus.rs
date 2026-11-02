//! Where the server's threads run on a processor with performance and efficiency cores.
//!
//! A request runs its sequential parts on the thread that serves it, a runtime worker or
//! a blocking-pool thread, and hands its parallel parts to the query pool. On a hybrid
//! processor whose every logical CPU holds a query thread, a request thread that wakes
//! after a parallel part often finds only an efficiency core or a busy hyperthread free
//! and stays there. On forge, an Intel Core i5-13500 with six performance cores of two
//! threads and eight efficiency cores, that took BSBM BI `q3` at 10M from 22 to 42 ms in
//! some server processes and not in others (roadmap item 449). So on such a processor
//! the request threads run on the performance cores, and the query pool keeps fewer
//! threads than logical CPUs, so that a request thread finds a free CPU. The query pool
//! itself runs on every CPU, since its work steals and shares out across cores of either
//! kind.
//!
//! Linux names the two kinds of core in `/sys/devices/cpu_core/cpus` and
//! `/sys/devices/cpu_atom/cpus`. Elsewhere, on a processor of one kind of core, and when
//! the process may run on cores of one kind only, every thread may run on every CPU and
//! the query pool has one thread per logical CPU, as before.

// Elsewhere than on Linux no `Hybrid` is ever made, so its helpers go unused there.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// The CPUs this process may run on, split by kind of core, when it may run on both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hybrid {
    /// logical CPUs of performance cores
    pub performance: Vec<usize>,
    /// logical CPUs of efficiency cores
    pub efficiency: Vec<usize>,
}

impl Hybrid {
    /// The cores of this machine that the process may run on, or `None` when they are
    /// not of two kinds.
    pub fn detect() -> Option<Hybrid> {
        #[cfg(target_os = "linux")]
        {
            let read = |p: &str| std::fs::read_to_string(p).ok();
            let allowed = linux::allowed()?;
            Hybrid::from_lists(
                &read("/sys/devices/cpu_core/cpus")?,
                &read("/sys/devices/cpu_atom/cpus")?,
                &allowed,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// The split of `allowed` by the CPU lists of the two kinds of core, in the kernel's
    /// list format (`0-11`, `0,2-5`).
    pub fn from_lists(core: &str, atom: &str, allowed: &[usize]) -> Option<Hybrid> {
        let core = parse_list(core)?;
        let atom = parse_list(atom)?;
        let performance: Vec<usize> = allowed
            .iter()
            .copied()
            .filter(|c| core.contains(c))
            .collect();
        let efficiency: Vec<usize> = allowed
            .iter()
            .copied()
            .filter(|c| atom.contains(c))
            .collect();
        (!performance.is_empty() && !efficiency.is_empty()).then_some(Hybrid {
            performance,
            efficiency,
        })
    }

    /// The default size of the query pool: one thread per logical CPU, less one for
    /// every four efficiency cores or part of four, which Intel's hybrid processors
    /// group into clusters of four. On forge's 20 logical CPUs, 18 threads measured as
    /// fast as 16 on the BSBM BI templates and faster on DBpedia's `births-by-decade`,
    /// and both were faster than 12, 14 and 20 (item 449).
    pub fn query_threads(&self) -> usize {
        let all = self.performance.len() + self.efficiency.len();
        all.saturating_sub(self.efficiency.len().div_ceil(4)).max(1)
    }
}

/// The CPUs of a kernel CPU list such as `0-11` or `0,2-5,8`.
pub fn parse_list(s: &str) -> Option<Vec<usize>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let cpu = |s: &str| s.trim().parse::<usize>().ok();
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (cpu(a)?, cpu(b)?),
            None => {
                let a = cpu(part)?;
                (a, a)
            }
        };
        if b < a || b - a > 1 << 16 {
            return None;
        }
        out.extend(a..=b);
    }
    Some(out)
}

/// Run the calling thread only on `cpus`. Where the system has no such control, or it
/// refuses, the thread keeps running where it may.
pub fn pin_current_thread(cpus: &[usize]) {
    #[cfg(target_os = "linux")]
    linux::pin(cpus);
    #[cfg(not(target_os = "linux"))]
    let _ = cpus;
}

#[cfg(target_os = "linux")]
mod linux {
    /// The CPUs the calling thread may run on.
    pub fn allowed() -> Option<Vec<usize>> {
        // SAFETY: a zeroed cpu_set_t is an empty set, and sched_getaffinity writes at
        // most size_of::<cpu_set_t>() bytes into it.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
                return None;
            }
            Some(
                (0..libc::CPU_SETSIZE as usize)
                    .filter(|&c| libc::CPU_ISSET(c, &set))
                    .collect(),
            )
        }
    }

    pub fn pin(cpus: &[usize]) {
        // SAFETY: CPU_SET writes within the zeroed set for CPUs below CPU_SETSIZE, and
        // sched_setaffinity reads size_of::<cpu_set_t>() bytes of it.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            for &c in cpus.iter().filter(|&&c| c < libc::CPU_SETSIZE as usize) {
                libc::CPU_SET(c, &mut set);
            }
            if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
                tracing::debug!(
                    "could not pin a request thread: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_cpu_lists() {
        assert_eq!(parse_list("0-3\n"), Some(vec![0, 1, 2, 3]));
        assert_eq!(parse_list("0,2-4,7"), Some(vec![0, 2, 3, 4, 7]));
        assert_eq!(parse_list("5"), Some(vec![5]));
        assert_eq!(parse_list(""), None);
        assert_eq!(parse_list("3-1"), None);
        assert_eq!(parse_list("a-b"), None);
    }

    #[test]
    fn splits_the_allowed_cpus_by_kind() {
        let all: Vec<usize> = (0..20).collect();
        let h = Hybrid::from_lists("0-11", "12-19", &all).unwrap();
        assert_eq!(h.performance, (0..12).collect::<Vec<_>>());
        assert_eq!(h.efficiency, (12..20).collect::<Vec<_>>());
        // forge: 20 logical CPUs, eight of them efficiency cores
        assert_eq!(h.query_threads(), 18);
        // a process confined to cores of one kind is not hybrid
        assert_eq!(Hybrid::from_lists("0-11", "12-19", &[0, 1, 2]), None);
        assert_eq!(Hybrid::from_lists("0-11", "12-19", &[12, 13]), None);
        // a mixed affinity keeps both kinds, and one efficiency core costs a thread
        let h = Hybrid::from_lists("0-11", "12-19", &[0, 1, 12]).unwrap();
        assert_eq!(
            (h.performance.len(), h.efficiency.len(), h.query_threads()),
            (2, 1, 2)
        );
        // sixteen efficiency cores leave four CPUs free
        let h = Hybrid::from_lists("0-15", "16-31", &(0..32).collect::<Vec<_>>()).unwrap();
        assert_eq!(h.query_threads(), 28);
        // an unreadable list is not hybrid
        assert_eq!(Hybrid::from_lists("", "12-19", &all), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinning_a_thread_limits_it_to_those_cpus() {
        let allowed = linux::allowed().unwrap();
        let first = allowed[0];
        let got = std::thread::spawn(move || {
            pin_current_thread(&[first]);
            linux::allowed().unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(got, vec![first]);
        // the calling thread keeps its own CPUs
        assert_eq!(linux::allowed().unwrap(), allowed);
    }
}
