//! Work a fresh `serve` process does before it reports ready, so that its first request
//! does not pay for it (spec X07, "First request after a start").
//!
//! A new process has none of its code mapped and none of its one-time state built. On
//! forge at 10.5M triples, the first `path-plus` after a restart spent about 2.4 ms in the
//! server, of which parsing, planning, execution and serialization took 0.8 ms. Most of
//! the rest was page faults on the 100 MB of machine code and the first use of state that
//! is built once, such as compiled regular expressions and the CPU limits read from the
//! cgroup files. Two steps move that work before readiness:
//!
//! * [`spawn_prefault`] maps the executable's code into the process with
//!   `MADV_POPULATE_READ` on a background thread. The pages come from the page cache, so
//!   this takes a few milliseconds and adds the code's size to the resident set as shared
//!   file pages, which the kernel can reclaim. `serve --no-code-prefault` turns it off.
//! * [`warm_up`] runs one query that reads no data through the query path of the HTTP
//!   API, on a scratch in-memory dataset that no request can reach. It is not logged or
//!   counted, and it reads nothing of the served datasets, so their files stay as cold as
//!   they were. `serve --no-warm-up` turns it off.
//!
//! Only `serve` runs either step, so the embedded library and the other commands are
//! unaffected.

use crate::state::{AppState, Dataset, DbType};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The query of [`warm_up`]: a SELECT with an aggregate over a VALUES table, so that it
/// uses the parser, the planner, the executor and the JSON writer without reading data.
pub const WARM_UP_QUERY: &str = "SELECT (COUNT(*) AS ?c) WHERE { VALUES ?x { 1 } }";

/// The name of the scratch dataset of [`warm_up`], which is not a valid dataset name, so
/// that it can never be confused with a served one.
const WARM_UP_DATASET: &str = "$warm-up";

/// Map the code of the running executable into the process on a background thread.
pub fn spawn_prefault() {
    let spawned = std::thread::Builder::new()
        .name("prefault-code".into())
        .spawn(|| {
            let t = Instant::now();
            match prefault_code() {
                Some(bytes) => {
                    tracing::debug!("mapped {bytes} bytes of code in {:?}", t.elapsed())
                }
                None => tracing::debug!("the code was not mapped ahead"),
            }
        });
    if let Err(e) = spawned {
        tracing::debug!("not mapping the code ahead: {e}");
    }
}

/// Populate the page tables of the executable's code mappings, the ones `/proc/self/maps`
/// lists as executable with the path of `/proc/self/exe`. Returns the bytes populated, or
/// `None` where this is not supported: other systems, and Linux before 5.14, which does not
/// know `MADV_POPULATE_READ`.
#[cfg(target_os = "linux")]
pub fn prefault_code() -> Option<usize> {
    let exe = std::fs::read_link("/proc/self/exe").ok()?;
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let mut bytes = 0;
    for (start, len) in code_ranges(&maps, &exe) {
        // SAFETY: the range is a mapping of this process, and populating it reads the
        // file's pages into the page tables without changing any memory
        if unsafe { libc::madvise(start as *mut libc::c_void, len, libc::MADV_POPULATE_READ) } != 0
        {
            return (bytes > 0).then_some(bytes);
        }
        bytes += len;
    }
    (bytes > 0).then_some(bytes)
}

#[cfg(not(target_os = "linux"))]
pub fn prefault_code() -> Option<usize> {
    None
}

/// The start and length of each executable mapping of `exe` in `maps`, the text of
/// `/proc/self/maps`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn code_ranges(maps: &str, exe: &std::path::Path) -> Vec<(usize, usize)> {
    maps.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next()?;
            let perms = fields.next()?;
            // offset, device and inode come before the path, which may contain spaces
            let path = line.splitn(6, char::is_whitespace).nth(5)?.trim_start();
            if perms.as_bytes().get(2) != Some(&b'x') || std::path::Path::new(path) != exe {
                return None;
            }
            let (a, b) = range.split_once('-')?;
            let a = usize::from_str_radix(a, 16).ok()?;
            let b = usize::from_str_radix(b, 16).ok()?;
            (b > a).then_some((a, b - a))
        })
        .collect()
}

/// Run [`WARM_UP_QUERY`] through the query path of `/{ds}/sparql` on a scratch
/// in-memory dataset, on a worker of `rt` as a request would run. Returns how long the
/// query took or why it failed, which only the debug log reports, since the server serves
/// either way. The caller blocks until it ends, so it must not be a runtime thread.
pub fn warm_up(st: &Arc<AppState>, rt: &tokio::runtime::Handle) -> Result<Duration, String> {
    let t = Instant::now();
    let st = st.clone();
    let ran = rt
        .block_on(async move { tokio::spawn(run_warm_up(st)).await })
        .map_err(|e| e.to_string())
        .and_then(|r| r)
        .map(|()| t.elapsed());
    match &ran {
        Ok(elapsed) => tracing::debug!("warmed the query path in {elapsed:?}"),
        Err(e) => tracing::debug!("the warm-up query failed: {e}"),
    }
    ran
}

/// The warm-up's request, without the HTTP layers: the access log, the metrics and the
/// rate limits never see it.
async fn run_warm_up(st: Arc<AppState>) -> Result<(), String> {
    let ds = Arc::new(Dataset {
        name: WARM_UP_DATASET.into(),
        kind: DbType::Memory,
        branch: None,
        branches: Default::default(),
        ephemeral: true,
        validation_metrics: Arc::new(crate::obs::ValidationMetrics::new(WARM_UP_DATASET)),
        offline: AtomicBool::new(false),
        dataset: sparkles::Dataset::memory(),
    });
    let uri: axum::http::Uri = "/warm-up/sparql".parse().map_err(|e| format!("{e}"))?;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::ACCEPT,
        axum::http::HeaderValue::from_static("application/sparql-results+json"),
    );
    let response = crate::http::warm_up_query(st, ds, uri, headers, WARM_UP_QUERY.to_string())
        .await
        .map_err(|e| format!("status {}", e.status()))?;
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map(drop)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_ranges_are_the_executable_mappings_of_the_binary() {
        let maps = "\
55d0c0000000-55d0c1000000 r--p 00000000 fe:01 123 /opt/sparkles/bin/sparkles
55d0c1000000-55d0c7000000 r-xp 01000000 fe:01 123 /opt/sparkles/bin/sparkles
55d0c7000000-55d0c7400000 rw-p 07000000 fe:01 123 /opt/sparkles/bin/sparkles
7f0000000000-7f0000200000 r-xp 00000000 fe:01 456 /usr/lib/libc.so.6
7f0000400000-7f0000500000 r-xp 00000000 fe:01 789 /opt/sparkles/bin/sparkles (deleted)
7f0000600000-7f0000700000 r-xp 00000000 fe:01 790 /opt/my data/sparkles
7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0 [stack]
";
        let exe = std::path::Path::new("/opt/sparkles/bin/sparkles");
        assert_eq!(
            code_ranges(maps, exe),
            vec![(0x55d0c1000000, 0x6000000)],
            "only the executable mapping of the binary itself"
        );
        let spaced = std::path::Path::new("/opt/my data/sparkles");
        assert_eq!(
            code_ranges(maps, spaced),
            vec![(0x7f0000600000, 0x100000)],
            "a path with a space"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_test_binary_maps_its_own_code() {
        let bytes = prefault_code().expect("Linux 5.14 or later maps the code");
        assert!(bytes > 0);
        // a second call finds the pages mapped and succeeds again
        assert_eq!(prefault_code(), Some(bytes));
    }

    #[test]
    fn the_warm_up_answers_without_a_trace_in_the_served_datasets() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(dir.path(), Default::default(), Duration::from_secs(30)).unwrap(),
        );
        st.attach("ds", DbType::Mem, None).unwrap();
        // the request series, which the gauges of memory and time would make differ
        let requests = |st: &AppState| -> Vec<String> {
            crate::obs::render_prometheus(st)
                .lines()
                .filter(|l| l.starts_with("sparkles_requests"))
                .map(str::to_string)
                .collect()
        };
        let before = requests(&st);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let ran = warm_up(&st, rt.handle());
        assert!(ran.is_ok(), "{ran:?}");
        assert_eq!(
            st.datasets().keys().cloned().collect::<Vec<_>>(),
            ["ds"],
            "the scratch dataset is not registered"
        );
        assert!(!crate::obs::render_prometheus(&st).contains(WARM_UP_DATASET));
        assert_eq!(before, requests(&st), "the warm-up is not counted");
    }
}
