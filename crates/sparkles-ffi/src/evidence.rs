//! UniFFI exports for evidence settings, holds, reference resolution, proposals,
//! reviews, withdrawals, identity resolution and dependent lookups (spec C25, item 509).

use crate::documents::{decode, encode};
use crate::{FfiDataset, FfiOperation, FfiResult};
use serde_json::Value;
use sparkles::evidence::binding;
use std::sync::Arc;

#[uniffi::export]
impl FfiDataset {
    pub fn evidence_settings(&self) -> FfiResult<Vec<u8>> {
        encode(&binding::settings(&self.inner.ds)?)
    }

    pub fn evidence_configure(
        &self,
        bytes: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        encode(&binding::configure(
            &self.inner.ds,
            &bytes,
            &operation.control,
        )?)
    }

    pub fn evidence_detach(&self, graph: String) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        encode(&binding::detach(&self.inner.ds, &graph)?)
    }

    pub fn evidence_holds(
        &self,
        on_branch: Option<String>,
        after: Option<String>,
        limit: Option<u32>,
    ) -> FfiResult<Vec<u8>> {
        encode(&binding::holds(
            &self.inner.ds,
            on_branch.as_deref(),
            after.as_deref(),
            limit.map(|n| n as usize),
        )?)
    }

    pub fn evidence_acquire_hold(&self, request: Vec<u8>) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        let req: Value = decode(&request)?;
        encode(&binding::acquire_hold(&self.inner.ds, &req)?)
    }

    pub fn evidence_release_hold(&self, id: String) -> FfiResult<bool> {
        self.inner.check_writable()?;
        Ok(binding::release_hold(&self.inner.ds, &id)?)
    }

    pub fn evidence_resolve(
        &self,
        branch: Option<String>,
        request: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let req: Value = decode(&request)?;
        encode(&binding::resolve(
            &self.inner.ds,
            branch.as_deref(),
            &req,
            &operation.control,
        )?)
    }

    pub fn evidence_propose(
        &self,
        branch: Option<String>,
        principal: Option<String>,
        author: Option<String>,
        request: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let req: Value = decode(&request)?;
        encode(&binding::propose(
            &self.inner.ds,
            branch.as_deref(),
            principal.as_deref(),
            author.as_deref(),
            &req,
            &operation.control,
        )?)
    }

    pub fn evidence_review(
        &self,
        branch: Option<String>,
        principal: Option<String>,
        author: Option<String>,
        request: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let req: Value = decode(&request)?;
        encode(&binding::review(
            &self.inner.ds,
            branch.as_deref(),
            principal.as_deref(),
            author.as_deref(),
            &req,
            &operation.control,
        )?)
    }

    pub fn evidence_withdraw(
        &self,
        branch: Option<String>,
        principal: Option<String>,
        author: Option<String>,
        request: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let req: Value = decode(&request)?;
        encode(&binding::withdraw(
            &self.inner.ds,
            branch.as_deref(),
            principal.as_deref(),
            author.as_deref(),
            &req,
            &operation.control,
        )?)
    }

    pub fn evidence_resolve_entities(
        &self,
        branch: Option<String>,
        policy: String,
        entities: Vec<String>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        encode(&binding::resolve_entities(
            &self.inner.ds,
            branch.as_deref(),
            &policy,
            &entities,
            &operation.control,
        )?)
    }

    pub fn evidence_find_dependents(
        &self,
        branch: Option<String>,
        changed: Vec<String>,
        transitive: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        encode(&binding::find_dependents(
            &self.inner.ds,
            branch.as_deref(),
            &changed,
            transitive,
            &operation.control,
        )?)
    }
}
