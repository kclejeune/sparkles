//! Strict, source-pinned browser reads of recorded materialized consequences.
use super::*;
use crate::history::At;
use crate::reasoning::ReasoningRecord;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A recorded producer whose complete input view is readable at this exact revision.
/// This is a materialized profile's source, never a classifier proof or a claim that
/// the profile covers arbitrary OWL entailments.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializationSource {
    pub kind: &'static str,
    pub source_id: String,
    pub profile: String,
    pub producer_commit: u64,
    pub commit: u64,
    pub bound_store_id: Uuid,
    pub family_id: Uuid,
    pub input_graphs: Vec<String>,
    pub input_graph_count: usize,
    pub input_graphs_truncated: bool,
    pub warning_count: usize,
    pub warnings_truncated: bool,
    pub warnings: Vec<String>,
    pub complete: Option<bool>,
    pub consistency: &'static str,
    pub justifications: &'static str,
    #[serde(skip)]
    record_fingerprint: String,
}

struct HashWriter<'a> {
    hash: Sha256,
    opts: &'a QueryOptions,
    unpolled: usize,
}
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.unpolled += bytes.len();
        if self.unpolled >= 65536 {
            check(self.opts).map_err(std::io::Error::other)?;
            self.unpolled = 0;
        }
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn fingerprint(value: &impl Serialize, opts: &QueryOptions) -> Result<String> {
    check(opts)?;
    let mut writer = HashWriter {
        hash: Sha256::new(),
        opts,
        unpolled: 0,
    };
    writer.hash.update(b"sparkles-materialized-browse-v1\n");
    let result = serde_json::to_writer(&mut writer, value);
    check(opts)?;
    result.map_err(|e| Error::invalid(e.to_string()))?;
    Ok(format!(
        "sha256:{}",
        writer
            .hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn changed() -> Error {
    Error::Conflict("materialized inference source changed or cannot be verified".into())
}

impl Ontologies {
    pub(super) fn include_materialization(
        &self,
        scope: &mut Scope,
        req: &OntologyRequest,
        source_id: Option<&str>,
        continuation: bool,
    ) -> Result<()> {
        if !matches!(req.at.as_ref(), Some(At::Commit(_))) {
            return Err(Error::invalid(
                "an inferred browser read requires at=commit:N",
            ));
        }
        if source_id.is_some_and(|id| {
            id.strip_prefix("sha256:").is_none_or(|hex| {
                hex.len() != 64
                    || !hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        }) {
            return Err(Error::invalid(
                "sourceId must be a canonical materialization identity",
            ));
        }
        if continuation && source_id.is_none() {
            return Err(Error::invalid("an inferred continuation requires sourceId"));
        }
        // Do not reveal even producer metadata through a partial view. In particular,
        // an explicit grant on the inferred graph does not prove its inputs visible.
        if scope.opts.graphs.is_some() || !scope.snap.cacheable {
            return Err(Error::NotPermitted(
                "materialized browsing requires a complete input view".into(),
            ));
        }
        let record = self.ds.reasoning_record().ok_or_else(|| {
            Error::NotFound("no recorded materialization for this browser view".into())
        })?;
        let Some(commit) = record.commit else {
            return Err(changed());
        };
        if record.reasoning_format != 2
            || record.position_source.as_deref() != Some("commit")
            || record.dataset_id.as_deref() != Some(scope.snap.dataset_id.to_string().as_str())
            || record.inherited_stale
            || commit > scope.snap.commit
        {
            return Err(changed());
        }
        if !matches!(record.profile.as_str(), "rdfs" | "owl-rl") {
            return Err(Error::unsupported(
                "this materialization profile has no browser relation contract",
            ));
        }
        let mut input_graphs = record
            .input_graphs
            .clone()
            .unwrap_or_else(|| vec!["default".into()]);
        let selected: BTreeSet<_> = scope.refs.iter().map(|g| g.as_str()).collect();
        let produced: BTreeSet<_> = input_graphs.iter().map(String::as_str).collect();
        if selected != produced {
            return Err(Error::NotPermitted(
                "materialized browsing requires the exact producer input scope".into(),
            ));
        }
        let record_fingerprint = fingerprint(&record, &scope.opts)?;
        let input_graph_count = input_graphs.len();
        input_graphs.truncate(64);
        let warning_count = record.warnings.len();
        let source = MaterializationSource {
            kind: "materialization",
            source_id: fingerprint(
                &(
                    self.ds.store().branch_id(),
                    scope.snap.dataset_id,
                    scope.snap.commit,
                    req.graphs(),
                    req.imports,
                    req.mode.as_str(),
                    &record_fingerprint,
                ),
                &scope.opts,
            )?,
            profile: record.profile.clone(),
            producer_commit: commit,
            commit: scope.snap.commit,
            bound_store_id: self.ds.store().branch_id(),
            family_id: scope.snap.dataset_id,
            input_graphs,
            input_graph_count,
            input_graphs_truncated: input_graph_count > 64,
            warning_count,
            warnings_truncated: warning_count > 100,
            warnings: record.warnings.iter().take(100).cloned().collect(),
            complete: None,
            consistency: "unknown",
            justifications: "unavailable",
            record_fingerprint,
        };
        if source_id.is_some_and(|id| id != source.source_id) {
            return Err(changed());
        }
        self.verify_materialized_graphs(&record, scope)?;
        let graph = GraphRef::Named(super::super::INFERRED_GRAPH.to_string());
        if !scope.refs.contains(&graph) {
            if let Some(id) = graph.id(&scope.snap) {
                scope.graphs.insert(id, scope.refs.len() as u32);
            }
            scope.refs.push(graph);
        }
        scope.inference_source = Some(source);
        self.verify_browse_source(scope)
    }

    fn verify_materialized_graphs(&self, record: &ReasoningRecord, scope: &Scope) -> Result<()> {
        let commit = record.commit.ok_or_else(changed)?;
        if commit == scope.snap.commit {
            return Ok(());
        }
        let mut watched: BTreeSet<_> = crate::reasoning::watched_graphs(record)
            .into_iter()
            .collect();
        watched.insert(super::super::INFERRED_GRAPH.to_string());
        let mut graphs = Vec::with_capacity(watched.len());
        for graph in watched {
            check(&scope.opts)?;
            graphs.push(if graph == "default" {
                oxrdf::GraphName::DefaultGraph
            } else {
                oxrdf::GraphName::NamedNode(oxrdf::NamedNode::new(graph).map_err(|_| changed())?)
            });
        }
        let first = commit + 1; // commit < selectedCommit was checked above.
        let result = self
            .ds
            .store()
            .history_changes(&crate::store::HistoryQuery {
                graphs,
                from: Some(crate::store::HistoryBound::Commit(first)),
                to: Some(crate::store::HistoryBound::Commit(scope.snap.commit)),
                limit: 1,
                cancel: scope.opts.cancel.clone(),
                cancellation: scope.opts.cancellation.clone(),
                deadline: scope.opts.deadline,
                ..Default::default()
            });
        check(&scope.opts)?;
        match result {
            Ok(history)
                if history.from == first
                    && history.to == scope.snap.commit
                    && history.head >= scope.snap.commit
                    && history.changes.is_empty()
                    && history.unrecorded.is_empty()
                    && !history.truncated => {}
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::Timeout) => return Err(Error::Timeout),
            _ => return Err(changed()),
        }

        Ok(())
    }

    pub(super) fn verify_browse_source(&self, scope: &Scope) -> Result<()> {
        check(&scope.opts)?;
        if let Some(source) = &scope.inference_source {
            let record = self.ds.reasoning_record().ok_or_else(changed)?;
            if fingerprint(&record, &scope.opts)? != source.record_fingerprint {
                return Err(changed());
            }
        }
        Ok(())
    }
}
