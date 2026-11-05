//! The OWL 2 ontology handle on the facade (spec F21 Phase 2). Each operation takes and
//! answers the JSON forms of the HTTP routes (`sparkles::handles::ontology::wire`). The
//! JVM's `jena-ontapi` model reads its graph with `find_all` instead.
use crate::documents::encode;
use crate::{FfiDataset, FfiOperation, FfiResult};
use std::sync::Arc;

#[uniffi::export]
impl FfiDataset {
    /// Run ontology operation `op` (`summary`, `axioms`, `check`, `edit`, `removeEntity`,
    /// `diff`, `classify` or read-only `keptClassification`) with JSON arguments `args`, under the operation's
    /// cancellation and deadline. Edits, entity removals and classifications that name a
    /// target graph need a writable dataset and commit on their own.
    pub fn ontology_call(
        &self,
        op: String,
        args: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let args = sparkles::handles::ontology::wire::decode_arguments(&op, &args)?;
        let write = matches!(op.as_str(), "edit" | "removeEntity")
            || (op == "classify" && args.get("target").is_some_and(|t| !t.is_null()));
        if write {
            self.inner.check_writable()?;
        }
        let ctl = &operation.control;
        let mut opts = sparkles::sparql::QueryOptions {
            cancel: Some(ctl.cancel.flag()),
            deadline: ctl.deadline,
            ..Default::default()
        };
        opts.write.cancel = Some(ctl.cancel.flag());
        opts.write.deadline = ctl.deadline;
        let answer = sparkles::handles::ontology::wire::call(&self.inner.ds, &op, &args, &opts)?;
        encode(&answer)
    }
}
