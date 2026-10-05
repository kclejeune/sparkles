use crate::documents::encode;
use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiResult};
use std::sync::Arc;
#[uniffi::export]
impl FfiDataset {
    pub fn history_commit(&self, reference: String) -> FfiResult<Option<Vec<u8>>> {
        self.inner
            .ds
            .history()
            .commit(&reference.parse()?)?
            .as_ref()
            .map(encode)
            .transpose()
    }
    pub fn history_status(&self) -> FfiResult<Vec<u8>> {
        let h = self.inner.ds.history().status();
        encode(
            &serde_json::json!({"head":h.head,"reconstructable":h.reconstructable,"bytes":h.bytes,"retention":h.retention,"snapshots":h.snapshots,"catalog":h.catalog,"firstCommit":h.first_commit,"cacheEntries":h.cache_entries,"cacheBytes":h.cache_bytes,"hits":h.hits}),
        )
    }
    pub fn history_diff(
        &self,
        from: String,
        to: String,
        max_quads: u64,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let diff = self.inner.ds.history().diff(
            &from.parse()?,
            &to.parse()?,
            &sparkles::store::DiffOptions {
                max_quads,
                cancel: Some(operation.control.cancel.flag()),
                deadline: operation.control.deadline,
                ..Default::default()
            },
        )?;
        let changes:Vec<_>=diff.iter().map(|(op,q)|serde_json::json!({"op":format!("{:?}",op).to_lowercase(),"quad":q.to_string()})).collect();
        encode(
            &serde_json::json!({"from":diff.from.commit.seq,"to":diff.to.commit.seq,"added":diff.added,"removed":diff.removed,"method":diff.method.as_str(),"changes":changes}),
        )
    }
    pub fn history_changes(
        &self,
        after: u64,
        max_commits: u32,
        max_quads: u64,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let page = self.inner.ds.history().changes(
            after,
            &sparkles::store::ChangesOptions {
                max_commits: max_commits as usize,
                max_quads,
                cancel: Some(operation.control.cancel.flag()),
                deadline: operation.control.deadline,
                ..Default::default()
            },
        )?;
        let commits:Vec<_>=page.commits.iter().map(|c| {let changes:Vec<_>=c.iter().map(|(op,q)|serde_json::json!({"op":format!("{:?}",op).to_lowercase(),"quad":q.to_string()})).collect();serde_json::json!({"commit":c.commit,"added":c.added,"removed":c.removed,"changes":changes,"recorded":c.complete()})}).collect();
        encode(
            &serde_json::json!({"after":page.after,"head":page.head,"next":page.next(),"commits":commits}),
        )
    }
    pub fn history_query(
        &self,
        pattern: Vec<u8>,
        from: Option<String>,
        to: Option<String>,
        limit: u32,
        descending: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let p = self
            .inner
            .labels()
            .with(|resolve, _| crate::read::decode_pattern(&pattern, resolve))?
            .ok_or_else(|| FfiError::new(ErrorKind::Invalid, "invalid history pattern"))?;
        let graph = match p.graph {
            sparkles::embed::GraphMatch::Any => vec![],
            sparkles::embed::GraphMatch::Default => vec![oxrdf::GraphName::DefaultGraph],
            sparkles::embed::GraphMatch::Union => {
                return Err(FfiError::new(
                    ErrorKind::Invalid,
                    "union graph is not a recorded graph",
                ));
            }
            sparkles::embed::GraphMatch::Named(n) => vec![n.into()],
        };
        let q = sparkles::store::HistoryQuery {
            subjects: p.subject.into_iter().map(Into::into).collect(),
            predicates: p.predicate.into_iter().collect(),
            objects: p.object.into_iter().collect(),
            graphs: graph,
            from: from
                .map(|s| s.parse().map(sparkles::store::HistoryBound::At))
                .transpose()?,
            to: to
                .map(|s| s.parse().map(sparkles::store::HistoryBound::At))
                .transpose()?,
            limit: limit as usize,
            descending,
            cancel: Some(operation.control.cancel.flag()),
            deadline: operation.control.deadline,
            ..Default::default()
        };
        let r = self.inner.ds.history().query(&q)?;
        let changes:Vec<_>=r.changes.iter().map(|c|serde_json::json!({"commit":{"seq":c.commit.seq,"timestampMs":c.commit.timestamp_ms,"kind":format!("{:?}",c.commit.kind).to_lowercase(),"author":c.commit.author.as_deref(),"message":c.commit.message.as_deref()},"op":format!("{:?}",c.op).to_lowercase(),"quad":c.quad.to_string()})).collect();
        encode(
            &serde_json::json!({"from":r.from,"to":r.to,"head":r.head,"changes":changes,"unrecorded":r.unrecorded,"truncated":r.truncated}),
        )
    }
    pub fn history_prune(&self) -> FfiResult<u64> {
        self.inner.check_writable()?;
        Ok(self.inner.ds.history().prune()?)
    }
    pub fn history_tick(&self) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        let t = self.inner.ds.history().tick()?;
        encode(
            &serde_json::json!({"created":t.created,"expired":t.expired,"rotated":t.rotated,"warmed":t.warmed,"pruned":t.pruned}),
        )
    }
    pub fn history_wait(&self, after: u64, timeout_ms: u64) -> Option<u64> {
        self.inner
            .ds
            .history()
            .wait_for_commit(after, std::time::Duration::from_millis(timeout_ms))
    }
}
