//! Fuseki's Prometheus metric names (`serve --metrics-fuseki-names`), rendered next to
//! the Sparkles ones so that dashboards built for Fuseki keep working.
//!
//! Fuseki (`FusekiRequestsMetrics`) registers three Micrometer gauges for every endpoint
//! of every dataset: `fuseki_requests`, `fuseki_requests_good` and `fuseki_requests_bad`.
//! Their labels are `dataset` (the dataset path, `/ds`), `endpoint` (the service name,
//! empty for the dataset URL itself), `operation` and `description` (Fuseki's operation
//! name and description), plus the registry's common tag `application="fuseki"`. Fuseki
//! counts a request as good when its action finishes without an error and as bad
//! otherwise, so here `good` is the `ok` outcome, `bad` every other outcome, and
//! `requests` their sum. Micrometer's uptime binder adds `process_uptime_seconds` and
//! `process_start_time_seconds`, and its processor binder `system_cpu_count`; those three
//! are rendered too. The JVM, garbage collector, class loader, file descriptor, disk and
//! CPU load gauges have no counterpart.

use super::{AppState, DsMetrics, Metrics, NONE, Op, escape_label};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One Fuseki endpoint: its service name and the operation it runs.
struct Endpoint {
    name: &'static str,
    operation: &'static str,
    description: &'static str,
}

const fn ep(name: &'static str, op: (&'static str, &'static str)) -> Endpoint {
    Endpoint {
        name,
        operation: op.0,
        description: op.1,
    }
}

// Fuseki's operation names and descriptions (`org.apache.jena.fuseki.server.Operation`)
const QUERY: (&str, &str) = ("query", "SPARQL Query");
const UPDATE: (&str, &str) = ("update", "SPARQL Update");
const GSP_R: (&str, &str) = ("gsp-r", "Graph Store Protocol (Read)");
const GSP_RW: (&str, &str) = ("gsp-rw", "Graph Store Protocol");
const UPLOAD: (&str, &str) = ("upload", "File Upload");
const SHACL: (&str, &str) = ("SHACL", "SHACL Validation");
const PATCH: (&str, &str) = ("patch", "RDF Patch");

/// Every endpoint a Sparkles dataset has, in Fuseki's terms.
const ENDPOINTS: [Endpoint; 14] = [
    ep("", QUERY),
    ep("", UPDATE),
    ep("", GSP_RW),
    ep("", GSP_R),
    ep("sparql", QUERY),
    ep("query", QUERY),
    ep("update", UPDATE),
    ep("data", GSP_RW),
    ep("data", GSP_R),
    ep("get", GSP_R),
    ep("upload", UPLOAD),
    ep("shacl", SHACL),
    ep("", PATCH),
    ep("patch", PATCH),
];

/// The dataset route a request matched, as far as Fuseki has an endpoint for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// `/{ds}`, whose operation depends on the request
    Dataset,
    Sparql,
    Query,
    Update,
    Data,
    Get,
    Upload,
    Shacl,
    Patch,
}

/// The Fuseki route of a matched route (`None`: no Fuseki endpoint, such as `/$/…`,
/// `/{ds}/shex` or `/{ds}/explain`).
pub(super) fn route(r: Option<&str>) -> Option<Route> {
    Some(match r? {
        "/{ds}" => Route::Dataset,
        "/{ds}/sparql" => Route::Sparql,
        "/{ds}/query" => Route::Query,
        "/{ds}/update" => Route::Update,
        "/{ds}/data" => Route::Data,
        "/{ds}/get" => Route::Get,
        "/{ds}/upload" => Route::Upload,
        "/{ds}/shacl" => Route::Shacl,
        "/{ds}/patch" => Route::Patch,
        _ => return None,
    })
}

/// The index in [`ENDPOINTS`] of a request: its route, refined by its operation. Graph
/// Store requests on a read-only server count under Fuseki's read-only operation.
fn endpoint(route: Route, op: Op, read_only: bool) -> Option<usize> {
    let gsp = |rw: usize| if read_only { rw + 1 } else { rw };
    Some(match route {
        Route::Dataset => match op {
            Op::Query => 0,
            Op::Update => 1,
            Op::Gsp => gsp(2),
            Op::Patch => 12,
            _ => return None,
        },
        Route::Sparql => 4,
        Route::Query => 5,
        Route::Update => 6,
        Route::Data => gsp(7),
        Route::Get => 9,
        Route::Upload => 10,
        Route::Shacl => 11,
        Route::Patch => 13,
    })
}

/// The good and bad requests of each endpoint of one dataset label.
#[derive(Default)]
pub(super) struct Counters {
    good: [AtomicU64; ENDPOINTS.len()],
    bad: [AtomicU64; ENDPOINTS.len()],
}

impl Metrics {
    /// Count a finished request under its Fuseki endpoint (requests naming no dataset
    /// have none).
    pub(super) fn record_fuseki(
        &self,
        dataset: Option<&str>,
        route: Route,
        op: Op,
        ok: bool,
        read_only: bool,
    ) {
        // counted whether or not the Fuseki names are rendered: `/$/stats` reports them
        if !self.enabled || dataset.is_none() {
            return;
        }
        let Some(i) = endpoint(route, op, read_only) else {
            return;
        };
        let (_, ds) = self.series(dataset);
        let c = if ok { &ds.fuseki.good } else { &ds.fuseki.bad };
        c[i].fetch_add(1, Ordering::Relaxed);
    }

    /// The requests of each Fuseki endpoint of a dataset that has had any, for Fuseki's
    /// `/$/stats`: (endpoint name, operation, description, good, bad). Empty for a
    /// dataset without its own series (metrics off, or past `--metrics-max-datasets`).
    pub fn fuseki_endpoint_counts(
        &self,
        dataset: &str,
    ) -> Vec<(&'static str, &'static str, &'static str, u64, u64)> {
        let Some(m) = self.datasets.read().get(dataset).cloned() else {
            return Vec::new();
        };
        ENDPOINTS
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let good = m.fuseki.good[i].load(Ordering::Relaxed);
                let bad = m.fuseki.bad[i].load(Ordering::Relaxed);
                (e.name, e.operation, e.description, good, bad)
            })
            .filter(|(.., g, b)| g + b > 0)
            .collect()
    }
}

/// Fuseki's families. Like Fuseki's, they are gauges without a `_total` suffix; an
/// endpoint's series appear after its first request.
pub(super) fn render(o: &mut String, st: &AppState, series: &[(String, Arc<DsMetrics>)]) {
    type Value = fn(u64, u64) -> u64;
    let families: [(&str, &str, Value); 3] = [
        (
            "fuseki_requests",
            "Requests to a Fuseki endpoint (Fuseki-compatible name).",
            |g, b| g + b,
        ),
        (
            "fuseki_requests_good",
            "Requests to a Fuseki endpoint that succeeded (Fuseki-compatible name).",
            |g, _| g,
        ),
        (
            "fuseki_requests_bad",
            "Requests to a Fuseki endpoint that failed (Fuseki-compatible name).",
            |_, b| b,
        ),
    ];
    for (name, help, value) in families {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} gauge");
        for (ds, m) in series {
            if ds == NONE {
                continue;
            }
            // a path, as Fuseki names datasets (the capped ones share `/$other`)
            let path = escape_label(&format!("/{ds}"));
            for (i, e) in ENDPOINTS.iter().enumerate() {
                let good = m.fuseki.good[i].load(Ordering::Relaxed);
                let bad = m.fuseki.bad[i].load(Ordering::Relaxed);
                if good + bad == 0 {
                    continue;
                }
                let _ = writeln!(
                    o,
                    "{name}{{application=\"fuseki\",dataset=\"{path}\",description=\"{}\",endpoint=\"{}\",operation=\"{}\"}} {}",
                    e.description,
                    e.name,
                    e.operation,
                    value(good, bad)
                );
            }
        }
    }
    let uptime = st.started.elapsed().as_secs_f64();
    let process: [(&str, &str, String); 3] = [
        (
            "process_uptime_seconds",
            "The uptime of the process (Fuseki-compatible name).",
            format!("{uptime:.3}"),
        ),
        (
            "process_start_time_seconds",
            "Start time of the process since the Unix epoch (Fuseki-compatible name).",
            format!("{:.3}", super::start_time_seconds(st)),
        ),
        (
            "system_cpu_count",
            "The number of processors available to the process (Fuseki-compatible name).",
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .to_string(),
        ),
    ];
    for (name, help, v) in process {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} gauge");
        let _ = writeln!(o, "{name}{{application=\"fuseki\"}} {v}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_map_to_fuseki_endpoints() {
        let name =
            |r, op, ro| endpoint(r, op, ro).map(|i| (ENDPOINTS[i].name, ENDPOINTS[i].operation));
        assert_eq!(name(Route::Dataset, Op::Query, false), Some(("", "query")));
        assert_eq!(
            name(Route::Dataset, Op::Update, false),
            Some(("", "update"))
        );
        assert_eq!(name(Route::Dataset, Op::Gsp, false), Some(("", "gsp-rw")));
        assert_eq!(name(Route::Dataset, Op::Gsp, true), Some(("", "gsp-r")));
        assert_eq!(name(Route::Dataset, Op::Admin, false), None);
        assert_eq!(
            name(Route::Sparql, Op::Query, false),
            Some(("sparql", "query"))
        );
        assert_eq!(name(Route::Data, Op::Gsp, false), Some(("data", "gsp-rw")));
        assert_eq!(name(Route::Data, Op::Gsp, true), Some(("data", "gsp-r")));
        assert_eq!(name(Route::Get, Op::Gsp, false), Some(("get", "gsp-r")));
        assert_eq!(
            name(Route::Shacl, Op::Shacl, false),
            Some(("shacl", "SHACL"))
        );
        assert_eq!(route(Some("/{ds}/shex")), None);
        assert_eq!(route(Some("/$/metrics")), None);
        assert_eq!(route(Some("/{ds}/upload")), Some(Route::Upload));
    }
}
