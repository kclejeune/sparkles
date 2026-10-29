//! Graph algorithm handles and named projections for the JVM facade.
use crate::documents::{decode, encode};
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiReadTxn, FfiResult, FfiWriteTxn};
use sparkles::sparql::{QueryOptions, algorun::AlgoRequest};
use sparkles::store::Snapshot;
use std::sync::Arc;
#[cfg(all(feature = "algorithm-test-probe", unix))]
mod probe;

#[derive(Clone, Debug, uniffi::Record)]
pub struct AlgorithmOpts {
    pub union_default_graph: Option<bool>,
    pub default_graph: Vec<String>,
    pub named_graphs: Vec<String>,
    pub include_inferred: bool,
    pub max_rows: Option<u64>,
    pub max_memory_bytes: Option<u64>,
    pub max_rows_produced: Option<u64>,
    pub no_cache: bool,
}
fn options(ds: &sparkles::Dataset, opts: AlgorithmOpts, operation: &FfiOperation) -> QueryOptions {
    let mut q = QueryOptions {
        union_default_graph: opts.union_default_graph,
        default_graph_uris: opts.default_graph,
        named_graph_uris: opts.named_graphs,
        max_rows: opts.max_rows.map(|v| v as usize),
        max_memory_bytes: opts.max_memory_bytes,
        max_rows_produced: opts.max_rows_produced,
        no_cache: opts.no_cache,
        cancel: Some(operation.control.cancel.flag()),
        deadline: operation.control.deadline,
        ..Default::default()
    };
    if opts.include_inferred {
        q.default_graph_extra = vec![crate::query::INFERRED_GRAPH.to_string()];
    }
    let mut q = ds.with_query_defaults(&q).into_owned();
    if !opts.include_inferred {
        q.default_graph_extra.clear();
    }
    q
}
fn request(bytes: &[u8]) -> FfiResult<AlgoRequest> {
    Ok(AlgoRequest::from_json(decode(bytes)?)?)
}
fn stats(
    ds: &sparkles::Dataset,
    snap: Arc<Snapshot>,
    bytes: &[u8],
    opts: AlgorithmOpts,
    op: &FfiOperation,
) -> FfiResult<Vec<u8>> {
    op.control.check()?;
    let req = request(bytes)?;
    let mut prefixes = sparkles::io::standard_prefixes();
    prefixes.extend(ds.store().prefixes());
    let prefixes: Vec<_> = prefixes.into_iter().collect();
    let (query, _) = sparkles::sparql::algorun::query_text(&req, &prefixes, true)?;
    let result = sparkles::sparql::query(snap, &query, &options(ds, opts, op))?;
    let text = result
        .rows()
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next().flatten())
        .map(|term| match term {
            oxrdf::Term::Literal(l) => l.value().to_string(),
            t => t.to_string(),
        })
        .ok_or_else(|| FfiError::new(ErrorKind::Invalid, "the run has no nodes to report on"))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string()))?;
    encode(&value)
}
#[uniffi::export]
impl FfiDataset {
    pub fn algorithm_projections_list(&self) -> FfiResult<Vec<u8>> {
        encode(
            &self
                .inner
                .ds
                .algorithms()
                .projections()
                .list()
                .into_iter()
                .map(|(name, stored)| serde_json::json!({"name": name, "stored": stored}))
                .collect::<Vec<_>>(),
        )
    }
    pub fn algorithm_projections_get(&self, name: String) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .algorithms()
            .projections()
            .get(&name)
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn algorithm_projections_put(
        &self,
        name: String,
        definition: Vec<u8>,
        author: Option<String>,
        if_revision: Option<u64>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        let def = sparkles::algo::catalog::ProjectionDef::from_json(decode(&definition)?)?;
        let (stored, created) =
            self.inner
                .ds
                .algorithms()
                .projections()
                .put(&name, def, author, if_revision)?;
        encode(&serde_json::json!({"stored":stored,"created":created}))
    }
    pub fn algorithm_projections_delete(
        &self,
        name: String,
        if_revision: Option<u64>,
    ) -> FfiResult<bool> {
        self.inner.check_writable()?;
        Ok(self
            .inner
            .ds
            .algorithms()
            .projections()
            .delete(&name, if_revision)?)
    }
    pub fn algorithm_query_text(&self, request_bytes: Vec<u8>, summary: bool) -> FfiResult<String> {
        Ok(self
            .inner
            .ds
            .algorithms()
            .query_text(&request(&request_bytes)?, summary)?)
    }
    pub fn algorithm_stats(
        &self,
        request: Vec<u8>,
        opts: AlgorithmOpts,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        stats(&self.inner.ds, self.head(), &request, opts, &operation)
    }
    pub fn algorithm_run_write(
        &self,
        request_bytes: Vec<u8>,
        opts: AlgorithmOpts,
        author: Option<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let req = request(&request_bytes)?;
        let q = options(&self.inner.ds, opts, &operation);
        let control = operation.control.clone();
        #[cfg(all(feature = "algorithm-test-probe", unix))]
        let (probe, control) =
            match probe::Probe::install(&control, &decode(&request_bytes)?, author.as_deref())
                .map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string()))?
            {
                Some((probe, control)) => (Some(probe), control),
                None => (None, control),
            };
        let result =
            self.inner
                .ds
                .algorithms()
                .run_write(&req, &q, author.map(Into::into), &control);
        #[cfg(all(feature = "algorithm-test-probe", unix))]
        if let Some(probe) = probe {
            probe
                .finish(&result)
                .map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string()))?;
        }
        encode(&result?)
    }
}
#[uniffi::export]
impl FfiReadTxn {
    pub fn algorithm_stats(
        &self,
        request: Vec<u8>,
        opts: AlgorithmOpts,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        stats(&self.ds.ds, self.snap.clone(), &request, opts, &operation)
    }
}
#[uniffi::export]
impl FfiWriteTxn {
    pub fn algorithm_stats(
        &self,
        request: Vec<u8>,
        opts: AlgorithmOpts,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        stats(&self.ds.ds, self.view()?, &request, opts, &operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn dataset() -> Arc<FfiDataset> {
        let ds = FfiDataset::memory(crate::DatasetOptions {
            blank_node_labels: crate::BlankNodeMode::Dataset,
            read_only: false,
            term_cache_size: 1000,
        });
        ds.inner.ds.update("INSERT DATA { <urn:a> <urn:p> <urn:b>. <urn:b> <urn:p> <urn:c>. <urn:c> <urn:p> <urn:a>. <urn:d> <urn:p> <urn:e> }").unwrap();
        ds
    }
    fn opts() -> AlgorithmOpts {
        AlgorithmOpts {
            union_default_graph: None,
            default_graph: vec![],
            named_graphs: vec![],
            include_inferred: false,
            max_rows: None,
            max_memory_bytes: None,
            max_rows_produced: None,
            no_cache: false,
        }
    }
    fn write() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"algorithm":"pageRank","projection":{"predicates":["urn:p"]},"write":{"graph":"urn:out","predicates":{"score":"urn:rank"},"batchSize":2}})).unwrap()
    }
    fn cancelling_on(prefix: &'static str) -> Arc<FfiOperation> {
        let mut op = FfiOperation::new(None);
        let control = &mut Arc::get_mut(&mut op).unwrap().control;
        let flag = control.cancel.flag();
        let original = control.progress.clone();
        control.progress = sparkles::task::Progress::new(move |fraction, message| {
            original.report(fraction, message);
            if message.starts_with(prefix) {
                flag.store(true, Ordering::SeqCst);
            }
        });
        op
    }
    fn output_count(ds: &FfiDataset) -> String {
        let result = ds
            .inner
            .ds
            .query("SELECT (COUNT(*) AS ?n) { GRAPH <urn:out> { ?s <urn:rank> ?o } }")
            .unwrap();
        result.rows()[0][0].as_ref().unwrap().to_string()
    }
    #[test]
    fn stats_honours_dataset_override_of_standard_prefix() {
        let ds = dataset();
        let body =
            serde_json::json!({"algorithm":"pageRank", "projection":{"predicates":["rdf:p"]}});
        let bytes = serde_json::to_vec(&body).unwrap();
        let run = || {
            let value = ds
                .algorithm_stats(bytes.clone(), opts(), FfiOperation::new(None))
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&value).unwrap()
        };
        assert_eq!(run()["nodes"], 0);
        ds.set_prefix("rdf".into(), "urn:".into()).unwrap();
        let req = AlgoRequest::from_json(body).unwrap();
        assert!(
            ds.inner
                .ds
                .algorithms()
                .query_text(&req, false)
                .unwrap()
                .contains("<urn:p>")
        );
        let expected: serde_json::Value = serde_json::from_str(
            &ds.inner
                .ds
                .algorithms()
                .stats(&req, &QueryOptions::default())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(expected["nodes"], 5);
        assert_eq!(run()["nodes"], 5);
    }
    #[test]
    fn cancellation_after_first_batch_keeps_only_committed_rows() {
        let ds = dataset();
        let error = ds
            .algorithm_run_write(write(), opts(), None, cancelling_on("writing batch 2 of "))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Cancelled);
        assert!(output_count(&ds).starts_with("\"2\""));
    }
    #[test]
    fn cancellation_at_done_preserves_successful_report() {
        let ds = dataset();
        let report = ds
            .algorithm_run_write(write(), opts(), None, cancelling_on("done"))
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&report).unwrap();
        assert_eq!(report["status"], "complete");
        assert_eq!(report["triples"], 5);
        assert!(output_count(&ds).starts_with("\"5\""));
    }
}
