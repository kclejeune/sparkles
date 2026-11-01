//! The query options of one caller on one dataset: the dataset's defaults, the server's
//! budgets and settings, the request's parameters and the caller's permissions, built
//! with the library's [`QueryRequest`].

use super::*;
use sparkles::{Budgets, QueryRequest};

/// A caller on a dataset of this server.
pub(crate) struct CallerContext<'a> {
    st: &'a AppState,
    ds: &'a Dataset,
    principal: &'a Principal,
}

impl<'a> CallerContext<'a> {
    pub(crate) fn for_caller(
        st: &'a AppState,
        ds: &'a Dataset,
        principal: &'a Principal,
    ) -> CallerContext<'a> {
        CallerContext { st, ds, principal }
    }

    /// The options of a query request, as [`query_options`] gives them, restricted to
    /// what the caller may read through `endpoint`.
    pub(crate) fn options(&self, params: &Params, endpoint: crate::auth::Endpoint) -> QueryOptions {
        self.options_on(params, &self.ds.name, endpoint)
    }

    /// [`options`](Self::options) under the grants of `name`, such as a branch of the
    /// dataset.
    pub(crate) fn options_on(
        &self,
        params: &Params,
        name: &str,
        endpoint: crate::auth::Endpoint,
    ) -> QueryOptions {
        let mut opts = request_options(self.st, self.ds, params);
        crate::auth::restrict(&mut opts, self.principal, name, endpoint);
        opts
    }

    /// The options of the queries of a background task the caller started: the server's
    /// budgets with the update timeout, which no task request parameter changes, and no
    /// grant of the memory pool, which the task takes when it starts.
    // Only the GraphRAG builder starts such tasks, and it runs only behind the MCP tools.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub(crate) fn task_options(&self, cancel: Arc<AtomicBool>) -> QueryOptions {
        let mut opts = request_options(self.st, self.ds, &Params(Vec::new()));
        opts.timeout = self.st.limits.update_timeout;
        opts.grant = None;
        opts.cancel = Some(cancel);
        self.restrict_reads(&mut opts);
        opts
    }

    /// The options of the reads of a route without query parameters of its own: the
    /// server's query timeout and budgets, restricted to what the caller may query.
    pub(crate) fn read_options(&self) -> QueryOptions {
        let mut opts = request_options(self.st, self.ds, &Params(Vec::new()));
        self.restrict_reads(&mut opts);
        opts
    }

    /// Restrict `opts` to what the caller may read through the query endpoint.
    fn restrict_reads(&self, opts: &mut QueryOptions) {
        #[cfg(feature = "auth")]
        crate::auth::restrict(
            opts,
            self.principal,
            &self.ds.name,
            crate::auth::Endpoint::Query,
        );
        #[cfg(not(feature = "auth"))]
        {
            opts.graphs = self
                .principal
                .view(&self.ds.name, crate::auth::Endpoint::Query);
        }
    }
}

/// The server's budgets of a query that may run for `timeout`.
pub(crate) fn server_budgets(st: &AppState, timeout: Option<Duration>) -> Budgets {
    Budgets {
        timeout,
        max_rows: Some(st.limits.max_rows),
        max_memory_bytes: st.limits.query_memory_bytes,
        max_rows_produced: st.limits.max_rows_produced,
        max_spill_bytes: st.limits.query_spill_bytes,
    }
}

/// The options of a query request: the dataset's query defaults (RDFS on read, the
/// inferred overlay and DESCRIBE, see [`sparkles::Dataset::query_options`]) with the
/// server's budgets and settings and the request's parameters. `reasoning=false` leaves
/// the inferred overlay and RDFS on read out, so the request reads the asserted data
/// alone.
pub(crate) fn request_options(st: &AppState, ds: &Dataset, params: &Params) -> QueryOptions {
    let view = ds
        .dataset
        .stored_inference_view(reasoning_mode(params).reads_inferences());
    let mut request = QueryRequest::new(&ds.dataset)
        .with_reasoning(&view)
        .budgets(&server_budgets(st, Some(timeout_param(st, params))));
    let opts = request.options_mut();
    // the grant the request was admitted with, if the server has a memory pool
    opts.grant = crate::server_memory::current_grant();
    // cursor sorts write sorted runs here when they outgrow the memory budget
    opts.spill = st.spill.clone();
    opts.no_cache = params.get("nocache").is_some_and(truthy);
    opts.default_graph_uris = params.all("default-graph-uri");
    opts.named_graph_uris = params.all("named-graph-uri");
    opts.allow_service = st.allow_service;
    opts.outbound = st.outbound.clone();
    request.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::store::StoreOptions;

    fn state(dir: &std::path::Path) -> AppState {
        let mut st = AppState::new(dir, StoreOptions::default(), Duration::from_secs(7)).unwrap();
        st.limits.max_rows = 1000;
        st.limits.query_memory_bytes = Some(64 << 20);
        st.limits.max_rows_produced = Some(5000);
        st.limits.query_spill_bytes = Some(128 << 20);
        st.limits.update_timeout = Some(Duration::from_secs(90));
        st
    }

    fn params(query: &str) -> Params {
        Params(
            url::form_urlencoded::parse(query.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        )
    }

    /// The fields of the options that the recipe sets.
    fn fields(o: &QueryOptions) -> String {
        format!(
            "timeout={:?} rows={:?} memory={:?} produced={:?} spill={:?} cache={} \
             default={:?} named={:?} service={} extra={:?} rdfs={} forbid={}{}{} \
             graphs={} grant={} cancel={}",
            o.timeout,
            o.max_rows,
            o.max_memory_bytes,
            o.max_rows_produced,
            o.max_spill_bytes,
            o.no_cache,
            o.default_graph_uris,
            o.named_graph_uris,
            o.allow_service,
            o.default_graph_extra,
            o.rdfs.is_some(),
            o.forbid_service,
            o.forbid_remote_load,
            o.forbid_file_load,
            o.graphs.is_some(),
            o.grant.is_some(),
            o.cancel.is_some(),
        )
    }

    /// The options of a request: the server's budgets and settings, the request's
    /// timeout, graphs and cache parameters, and the dataset's inferred data unless the
    /// request turns reasoning off.
    #[test]
    fn a_request_has_the_server_budgets_and_its_parameters() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(dir.path());
        let ds = st.attach("ds", DbType::Mem, None).unwrap();
        ds.dataset
            .state()
            .set_reasoning(Some(sparkles::reasoning::ReasoningRecord {
                profile: "rdfs".into(),
                ..Default::default()
            }))
            .unwrap();
        let p = Principal::local();
        let ctx = CallerContext::for_caller(&st, &ds, &p);
        let o = ctx.options(
            &params("timeout=3&nocache=true&default-graph-uri=urn:g&named-graph-uri=urn:n"),
            crate::auth::Endpoint::Query,
        );
        assert_eq!(
            fields(&o),
            format!(
                "timeout=Some(3s) rows=Some(1000) memory=Some(67108864) produced=Some(5000) \
                 spill=Some(134217728) cache=true default=[\"urn:g\"] named=[\"urn:n\"] \
                 service={} extra=[\"urn:x-sparkles:inferred\"] rdfs=false \
                 forbid=falsefalsefalse graphs=false grant=false cancel=false",
                st.allow_service
            )
        );
        let off = ctx.options(&params("reasoning=false"), crate::auth::Endpoint::Query);
        assert_eq!(off.timeout, Some(Duration::from_secs(7)));
        assert!(off.default_graph_extra.is_empty() && !off.no_cache);
        // a task runs under the update timeout without a grant, and a route without
        // parameters under the query timeout
        let flag = Arc::new(AtomicBool::new(false));
        let task = ctx.task_options(flag);
        assert_eq!(task.timeout, Some(Duration::from_secs(90)));
        assert_eq!(
            (task.max_rows, task.max_memory_bytes, task.max_rows_produced),
            (Some(1000), Some(64 << 20), Some(5000))
        );
        assert!(task.cancel.is_some() && task.grant.is_none());
        let read = ctx.read_options();
        assert_eq!(read.timeout, Some(Duration::from_secs(7)));
        assert_eq!(read.max_rows, Some(1000));
        assert_eq!(read.default_graph_extra, ["urn:x-sparkles:inferred"]);
    }
}
