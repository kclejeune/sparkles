//! The OWL 2 ontology handle on the facade (spec F21 Phase 2). Each operation takes and
//! answers the JSON forms of the HTTP routes (`sparkles::handles::ontology::wire`). The
//! JVM's `jena-ontapi` model reads its graph with `find_all` instead.
use crate::documents::{decode, encode};
use crate::{FfiDataset, FfiOperation, FfiResult};
use std::sync::Arc;

#[uniffi::export]
impl FfiDataset {
    /// Run ontology operation `op` (`summary`, `axioms`, `check`, `edit`, `removeEntity`
    /// or `diff`) with the JSON arguments `args`, under the operation's cancellation and
    /// deadline. Edits and entity removals need a writable dataset and commit on their own.
    pub fn ontology_call(
        &self,
        op: String,
        args: Vec<u8>,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<Vec<u8>> {
        operation.control.check()?;
        let write = matches!(op.as_str(), "edit" | "removeEntity");
        if write {
            self.inner.check_writable()?;
        }
        let args: serde_json::Value = decode(&args)?;
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
