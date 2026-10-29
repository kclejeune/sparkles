//! Typed reads of Sparkles history metadata and bounded JSON diffs.
use crate::admin_types::{
    Branch, BranchCreate, BranchList, Diff, NamedSnapshot, SnapshotList, SnapshotRequest,
};
use crate::client::{CallOptions, ReqBody, call_options, read_bytes, read_typed};
use crate::error::{Error, Result};
use crate::{At, Dataset, Graph, routes};
use oxrdf::Term;

/// Per-call controls for snapshot and branch metadata operations.
#[derive(Clone, Debug, Default)]
pub struct HistoryOptions {
    call: CallOptions,
}

impl HistoryOptions {
    pub fn new() -> Self {
        Self::default()
    }
}
call_options!(HistoryOptions);

/// Selectors and controls for the server's bounded JSON diff response.
///
/// Omitted selectors keep the server defaults. This does not request an RDF Patch
/// stream or interpret returned RDF term spellings as local blank-node identities.
#[derive(Clone, Debug, Default)]
pub struct DiffOptions {
    from: Option<At>,
    to: Option<At>,
    graph: Option<Graph>,
    terms: Vec<Term>,
    quads: Option<bool>,
    group_by_class: bool,
    limit: Option<u32>,
    from_branch: Option<String>,
    to_branch: Option<String>,
    call: CallOptions,
}

impl DiffOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn from(mut self, at: impl Into<At>) -> Self {
        self.from = Some(at.into());
        self
    }
    pub fn to(mut self, at: impl Into<At>) -> Self {
        self.to = Some(at.into());
        self
    }
    /// Restrict to a named or default graph. The diff API does not define Union.
    pub fn graph(mut self, graph: impl Into<Graph>) -> Self {
        self.graph = Some(graph.into());
        self
    }
    /// Add one subject/object filter, preserving repeated terms and their order.
    pub fn term(mut self, term: impl Into<Term>) -> Self {
        self.terms.push(term.into());
        self
    }
    pub fn quads(mut self, include: bool) -> Self {
        self.quads = Some(include);
        self
    }
    /// Request class-group counts. Incompatible combinations are refused by the server.
    pub fn group_by_class(mut self) -> Self {
        self.group_by_class = true;
        self
    }
    /// The server bounds the number of listed quads or groups.
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }
    pub fn from_branch(mut self, branch: impl Into<String>) -> Self {
        self.from_branch = Some(branch.into());
        self
    }
    pub fn to_branch(mut self, branch: impl Into<String>) -> Self {
        self.to_branch = Some(branch.into());
        self
    }
}
call_options!(DiffOptions);

impl Dataset {
    /// List retained named snapshots of this dataset.
    pub async fn snapshots(&self) -> Result<SnapshotList> {
        self.snapshots_with(&HistoryOptions::new()).await
    }
    pub async fn snapshots_with(&self, options: &HistoryOptions) -> Result<SnapshotList> {
        let mut request = self
            .client
            .op_req(&routes::LIST_SNAPSHOTS, &[("ds", self.name())])?;
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
    /// Get the metadata of an exact retained snapshot name.
    pub async fn snapshot(&self, name: &str) -> Result<NamedSnapshot> {
        self.snapshot_with(name, &HistoryOptions::new()).await
    }
    pub async fn snapshot_with(
        &self,
        name: &str,
        options: &HistoryOptions,
    ) -> Result<NamedSnapshot> {
        let mut request = self.client.op_req(
            &routes::GET_SNAPSHOT,
            &[("ds", self.name()), ("name", name)],
        )?;
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
    /// List the dataset's branches without fetching each branch separately.
    pub async fn branches(&self) -> Result<BranchList> {
        self.branches_with(&HistoryOptions::new()).await
    }
    pub async fn branches_with(&self, options: &HistoryOptions) -> Result<BranchList> {
        let mut request = self
            .client
            .op_req(&routes::LIST_BRANCHES, &[("ds", self.name())])?;
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
    pub async fn branch(&self, name: &str) -> Result<Branch> {
        self.branch_with(name, &HistoryOptions::new()).await
    }
    pub async fn branch_with(&self, name: &str, options: &HistoryOptions) -> Result<Branch> {
        let mut request = self
            .client
            .op_req(&routes::GET_BRANCH, &[("ds", self.name()), ("name", name)])?;
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
    /// Read bounded JSON changes between selected retained states.
    pub async fn diff(&self, options: &DiffOptions) -> Result<Diff> {
        if options.graph == Some(Graph::Union) {
            return Err(Error::config(
                "a diff graph filter must be a named or default graph",
            ));
        }
        let mut request = self.client.op_req(&routes::DIFF, &[("ds", self.name())])?;
        if let Some(from) = &options.from {
            request.param("from", from.to_string());
        }
        if let Some(to) = &options.to {
            request.param("to", to.to_string());
        }
        if let Some(graph) = &options.graph {
            request.param(
                "graph",
                match graph {
                    Graph::Default => "default".to_owned(),
                    Graph::Named(name) => name.as_str().to_owned(),
                    Graph::Union => unreachable!("refused above"),
                },
            );
        }
        for term in &options.terms {
            request.param("term", term.to_string());
        }
        if let Some(quads) = options.quads {
            request.param("quads", quads.to_string());
        }
        if options.group_by_class {
            request.param("group", "class");
        }
        if let Some(limit) = options.limit {
            request.param("limit", limit.to_string());
        }
        if let Some(branch) = &options.from_branch {
            request.param("fromBranch", branch.clone());
        }
        if let Some(branch) = &options.to_branch {
            request.param("toBranch", branch.clone());
        }
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
}

impl SnapshotRequest {
    /// Construct a JSON request for a named snapshot.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            at: None,
            expires: None,
            note: None,
            warm: None,
            extra: Default::default(),
        }
    }
}

impl BranchCreate {
    /// Construct a branch creation request using the server's defaults.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            at: None,
            from: None,
            note: None,
            protected: None,
            extra: Default::default(),
        }
    }
}

/// Controls for deleting a branch and its dependent history.
#[derive(Clone, Debug, Default)]
pub struct BranchDeleteOptions {
    force: bool,
    reparent: bool,
    call: CallOptions,
}
impl BranchDeleteOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Delete a branch even if it has unmerged commits.
    pub fn force(mut self) -> Self {
        self.force = true;
        self
    }
    /// Reparent branches that depend on the deleted branch.
    pub fn reparent(mut self) -> Self {
        self.reparent = true;
        self
    }
}
call_options!(BranchDeleteOptions);

impl Dataset {
    /// Create or return a named snapshot.
    pub async fn create_snapshot(&self, body: &SnapshotRequest) -> Result<NamedSnapshot> {
        self.create_snapshot_with(body, &HistoryOptions::new())
            .await
    }
    pub async fn create_snapshot_with(
        &self,
        body: &SnapshotRequest,
        options: &HistoryOptions,
    ) -> Result<NamedSnapshot> {
        let mut request = self
            .client
            .op_req(&routes::CREATE_SNAPSHOT, &[("ds", self.name())])?;
        request.body(ReqBody::json(
            &serde_json::to_value(body).map_err(|e| Error::parse("JSON", e))?,
        ));
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
    pub async fn delete_snapshot(&self, name: &str) -> Result<()> {
        self.delete_snapshot_with(name, &HistoryOptions::new())
            .await
    }
    pub async fn delete_snapshot_with(&self, name: &str, options: &HistoryOptions) -> Result<()> {
        let mut request = self.client.op_req(
            &routes::DELETE_SNAPSHOT,
            &[("ds", self.name()), ("name", name)],
        )?;
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_bytes(self.client.send(request).await?).await?;
        Ok(())
    }
    /// Create a branch and return its metadata.
    pub async fn create_branch(&self, body: &BranchCreate) -> Result<Branch> {
        self.create_branch_with(body, &HistoryOptions::new()).await
    }
    pub async fn create_branch_with(
        &self,
        body: &BranchCreate,
        options: &HistoryOptions,
    ) -> Result<Branch> {
        let mut request = self
            .client
            .op_req(&routes::CREATE_BRANCH, &[("ds", self.name())])?;
        request.body(ReqBody::json(
            &serde_json::to_value(body).map_err(|e| Error::parse("JSON", e))?,
        ));
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_typed(self.client.send(request).await?).await
    }
    pub async fn delete_branch(&self, name: &str) -> Result<()> {
        self.delete_branch_with(name, &BranchDeleteOptions::new())
            .await
    }
    pub async fn delete_branch_with(
        &self,
        name: &str,
        options: &BranchDeleteOptions,
    ) -> Result<()> {
        let mut request = self.client.op_req(
            &routes::DELETE_BRANCH,
            &[("ds", self.name()), ("name", name)],
        )?;
        if options.force {
            request.param("force", "true");
        }
        if options.reparent {
            request.param("reparent", "true");
        }
        request.accept = Some("application/json");
        request.call = options.call.clone();
        read_bytes(self.client.send(request).await?).await?;
        Ok(())
    }
}
