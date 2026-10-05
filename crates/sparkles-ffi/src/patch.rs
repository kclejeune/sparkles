use crate::{FfiDataset, FfiOperation, FfiResult};
use std::sync::Arc;
#[derive(Clone, Debug, uniffi::Record)]
pub struct PatchReport {
    pub receipt: crate::Receipt,
    pub rows: u64,
    pub inserted: u64,
    pub deleted: u64,
    pub aborted: bool,
    pub prev_checked: bool,
    pub prefixes_set: u64,
    pub prefixes_removed: u64,
}
#[uniffi::export]
impl FfiDataset {
    pub fn apply_patch(
        &self,
        bytes: Vec<u8>,
        binary: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<PatchReport> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let result = self.inner.ds.store().apply_patch(
            bytes.as_slice(),
            &sparkles::store::PatchOptions {
                binary,
                write: sparkles::guard::WriteOptions {
                    cancel: Some(operation.control.cancel.flag()),
                    deadline: operation.control.deadline,
                    ..Default::default()
                },
            },
        )?;
        Ok(PatchReport {
            receipt: (&result.receipt).into(),
            rows: result.rows,
            inserted: result.inserted,
            deleted: result.deleted,
            aborted: result.aborted,
            prev_checked: result.prev_checked,
            prefixes_set: result.prefixes_set,
            prefixes_removed: result.prefixes_removed,
        })
    }
}
