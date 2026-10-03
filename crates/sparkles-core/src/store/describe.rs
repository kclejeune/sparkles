//! A dataset's DESCRIBE setting: the [`DescribeOptions`] its queries use unless a request
//! asks for others. A persistent store keeps a setting other than the defaults in
//! `describe.json`, and an in-memory store keeps it in memory.

use super::{Store, sync_dir, write_atomic};
use crate::error::{Error, Result};
use crate::sparql::describe::DescribeOptions;
use std::path::Path;

/// The file in the dataset directory that holds the dataset's DESCRIBE setting.
pub const DESCRIBE_FILE: &str = "describe.json";

impl Store {
    /// The dataset's DESCRIBE setting (the defaults without one).
    pub fn describe_settings(&self) -> DescribeOptions {
        self.describe.read().clone()
    }

    /// Replace the dataset's DESCRIBE setting (`None` or the defaults: remove it). A
    /// persistent store writes `describe.json` before the setting applies.
    pub fn set_describe_settings(&self, s: Option<DescribeOptions>) -> Result<()> {
        let s = s.unwrap_or_default();
        let mut cur = self.describe.write();
        if let Some(root) = &self.root {
            let path = root.join(DESCRIBE_FILE);
            if s.is_default() {
                match std::fs::remove_file(&path) {
                    Ok(()) => sync_dir(root)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                let mut v = serde_json::to_value(&s).expect("settings serialize");
                v.as_object_mut()
                    .expect("an object")
                    .insert("format".into(), 1.into());
                let mut bytes = serde_json::to_vec_pretty(&v).expect("json serializes");
                bytes.push(b'\n');
                write_atomic(&path, &bytes)?;
            }
        }
        *cur = s;
        Ok(())
    }
}

/// Read a dataset's `describe.json` (the defaults without one).
pub(crate) fn read_settings(root: &Path) -> Result<DescribeOptions> {
    match std::fs::read(root.join(DESCRIBE_FILE)) {
        Ok(b) => {
            let v: serde_json::Value = serde_json::from_slice(&b)
                .map_err(|e| Error::Corrupt(format!("{DESCRIBE_FILE}: {e}")))?;
            if let Some(f) = v.get("format").and_then(|f| f.as_u64())
                && f != 1
            {
                return Err(Error::Corrupt(format!(
                    "{DESCRIBE_FILE} has format {f}, this build reads 1"
                )));
            }
            DescribeOptions::from_json(&v)
                .map_err(|e| Error::Corrupt(format!("{DESCRIBE_FILE}: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DescribeOptions::default()),
        Err(e) => Err(e.into()),
    }
}
