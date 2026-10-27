//! Lifecycle records on the existing operator audit target. No request payloads.
use crate::auth::Principal;
use uuid::Uuid;

/// Keep the initiating identity and tracing context when work outlives its request.
#[derive(Clone)]
pub(crate) struct Actor {
    principal: String,
    request_id: Option<String>,
    span: tracing::Span,
    dispatch: tracing::Dispatch,
}

impl Actor {
    pub(crate) fn request(principal: &Principal) -> Self {
        Self::capture(principal.id())
    }

    fn capture(principal: String) -> Self {
        Self {
            principal,
            request_id: None,
            span: tracing::Span::current(),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
        }
    }

    pub(crate) fn local() -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .ok()
            .filter(|user| !user.is_empty())
            .unwrap_or_else(|| "unknown".into());
        Self::capture(format!("local:{user}"))
    }

    #[cfg(feature = "mcp")]
    pub(crate) fn tool(principal: &Principal, request_id: &str) -> Self {
        let mut actor = Self::request(principal);
        actor.request_id = Some(request_id.into());
        actor
    }

    #[cfg(feature = "mcp")]
    pub(crate) fn expiry() -> Self {
        let mut actor = Self::capture("system:scratch-expiry".into());
        actor.span = tracing::Span::none();
        actor
    }

    pub(crate) fn in_scope<T>(&self, work: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, || self.span.in_scope(work))
    }

    pub(crate) fn failed(&self, operation: &'static str, name: Option<&str>, code: &str) {
        self.failure(operation, name, None, code);
    }

    pub(crate) fn failed_branch(
        &self,
        operation: &'static str,
        name: Option<&str>,
        branch: &str,
        code: &str,
    ) {
        self.failure(operation, name, Some(branch), code);
    }

    fn failure(
        &self,
        operation: &'static str,
        name: Option<&str>,
        branch: Option<&str>,
        code: &str,
    ) {
        let code = if code.len() <= 64
            && !code.is_empty()
            && code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            code
        } else {
            "operation-failed"
        };
        dataset(
            "dataset_admin_failed",
            name,
            None,
            self,
            Fields {
                operation: Some(operation),
                code: Some(code),
                branch,
                ..Default::default()
            },
        );
    }
}

#[derive(Default)]
pub(crate) struct Fields<'a> {
    pub(crate) operation: Option<&'static str>,
    pub(crate) code: Option<&'a str>,
    pub(crate) kind: Option<&'a str>,
    pub(crate) declared: Option<bool>,
    pub(crate) source: Option<&'a str>,
    pub(crate) source_id: Option<Uuid>,
    pub(crate) source_commit: Option<u64>,
    pub(crate) branch: Option<&'a str>,
    pub(crate) branch_id: Option<Uuid>,
    pub(crate) reason: Option<&'static str>,
}

/// Only closed lifecycle call sites supply `event` and typed metadata. Counts that
/// cannot be proved at deletion are omitted rather than sampled before mutation.
pub(crate) fn dataset(
    event: &'static str,
    name: Option<&str>,
    id: Option<Uuid>,
    actor: &Actor,
    fields: Fields<'_>,
) {
    actor.in_scope(|| {
        tracing::info!(
            target: "sparkles::audit", event,
            dataset = name.filter(|name| sparkles::catalog::valid_name(name)),
            id = id.map(|id| id.to_string()).as_deref(),
            principal = actor.principal.as_str(), request_id = actor.request_id.as_deref(),
            dataset_type = fields.kind, declared = fields.declared,
            source = fields.source, source_id = fields.source_id.map(|id| id.to_string()).as_deref(),
            source_commit = fields.source_commit,
            branch = fields.branch.filter(|name| sparkles::branch::valid_name(name)),
            branch_id = fields.branch_id.map(|id| id.to_string()).as_deref(), reason = fields.reason,
            operation = fields.operation, code = fields.code,
        );
    });
}

/// A queued clone may be cancelled by dropping its work without calling it. Its
/// audit owner carries no dataset, catalog or Store handle through that boundary.
pub(crate) struct PendingClone {
    actor: Actor,
    target: String,
    settled: bool,
}

impl PendingClone {
    pub(crate) fn new(actor: Actor, target: String) -> Self {
        Self {
            actor,
            target,
            settled: false,
        }
    }

    pub(crate) fn completed(
        &mut self,
        id: Uuid,
        source: &str,
        report: &sparkles::store::CloneReport,
    ) {
        self.settled = true;
        dataset(
            "dataset_cloned",
            Some(&self.target),
            Some(id),
            &self.actor,
            Fields {
                source: Some(source),
                source_id: Some(report.forked_from.id),
                source_commit: Some(report.forked_from.seq),
                ..Default::default()
            },
        );
    }

    pub(crate) fn failed(&mut self, code: &str) {
        self.settled = true;
        self.actor
            .failed("dataset_cloned", Some(&self.target), code);
    }
}

impl Drop for PendingClone {
    fn drop(&mut self) {
        if !self.settled {
            self.actor
                .failed("dataset_cloned", Some(&self.target), "aborted");
        }
    }
}
