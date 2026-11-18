//! A dataset's reading of errors in aggregates: the
//! [`AggregateErrors`] its queries use unless a request asks for another. A persistent
//! store keeps a setting in `aggregate-errors.json`, and an in-memory store keeps it in
//! memory. Without one, the store's option applies, which is the server's default.

use super::{Store, sync_dir, write_atomic};
use crate::error::{Error, Result};
use crate::sparql::AggregateErrors;
use std::path::Path;

/// The file in the dataset directory that holds the dataset's reading of errors in
/// aggregates.
pub const AGGREGATE_ERRORS_FILE: &str = "aggregate-errors.json";

impl Store {
    /// The dataset's own reading of errors in aggregates, `None` without one.
    pub fn aggregate_errors_setting(&self) -> Option<AggregateErrors> {
        *self.aggregate_errors.read()
    }

    /// The reading the dataset's queries use unless they choose one: its own setting,
    /// else the store's option.
    pub fn aggregate_errors(&self) -> AggregateErrors {
        self.aggregate_errors_setting()
            .unwrap_or(self.opts.aggregate_errors)
    }

    /// Replace the dataset's own reading (`None`: remove it, so that the store's option
    /// applies). A persistent store writes `aggregate-errors.json` before the setting
    /// applies.
    pub fn set_aggregate_errors_setting(&self, s: Option<AggregateErrors>) -> Result<()> {
        let mut cur = self.aggregate_errors.write();
        if let Some(root) = &self.root {
            let path = root.join(AGGREGATE_ERRORS_FILE);
            match s {
                None => match std::fs::remove_file(&path) {
                    Ok(()) => sync_dir(root)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                },
                Some(m) => write_atomic(&path, &setting_bytes(m))?,
            }
        }
        *cur = s;
        Ok(())
    }
}

/// The contents of `aggregate-errors.json` for reading `m`.
pub(crate) fn setting_bytes(m: AggregateErrors) -> Vec<u8> {
    let v = serde_json::json!({ "format": 1, "aggregateErrors": m.as_str() });
    let mut bytes = serde_json::to_vec_pretty(&v).expect("json serializes");
    bytes.push(b'\n');
    bytes
}

/// Read a dataset's `aggregate-errors.json` (`None` without one).
pub(crate) fn read_setting(root: &Path) -> Result<Option<AggregateErrors>> {
    let corrupt = |e: String| Error::Corrupt(format!("{AGGREGATE_ERRORS_FILE}: {e}"));
    match std::fs::read(root.join(AGGREGATE_ERRORS_FILE)) {
        Ok(b) => {
            let v: serde_json::Value =
                serde_json::from_slice(&b).map_err(|e| corrupt(e.to_string()))?;
            crate::formats::check_settings(AGGREGATE_ERRORS_FILE, &v)?;
            let m = v
                .get("aggregateErrors")
                .and_then(|m| m.as_str())
                .ok_or_else(|| corrupt("no aggregateErrors".into()))?;
            Ok(Some(m.parse().map_err(|e: Error| corrupt(e.to_string()))?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreOptions;

    #[test]
    fn a_newer_file_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(AGGREGATE_ERRORS_FILE),
            br#"{"format": 2, "aggregateErrors": "spec"}"#,
        )
        .unwrap();
        let e = read_setting(dir.path()).expect_err("refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    }

    #[test]
    fn an_unknown_reading_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(AGGREGATE_ERRORS_FILE),
            br#"{"format": 1, "aggregateErrors": "oxigraph"}"#,
        )
        .unwrap();
        let e = read_setting(dir.path()).expect_err("refused");
        assert!(matches!(e, Error::Corrupt(_)), "{e:?}");
    }

    #[test]
    fn the_setting_survives_a_reopen_and_falls_back_to_the_option() {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOptions {
            aggregate_errors: AggregateErrors::Sparkles,
            ..Default::default()
        };
        {
            let s = Store::open(dir.path(), opts.clone()).unwrap();
            assert_eq!(s.aggregate_errors(), AggregateErrors::Sparkles);
            s.set_aggregate_errors_setting(Some(AggregateErrors::Spec))
                .unwrap();
            assert_eq!(s.aggregate_errors(), AggregateErrors::Spec);
        }
        let s = Store::open(dir.path(), opts).unwrap();
        assert_eq!(s.aggregate_errors_setting(), Some(AggregateErrors::Spec));
        s.set_aggregate_errors_setting(None).unwrap();
        assert!(!dir.path().join(AGGREGATE_ERRORS_FILE).exists());
        assert_eq!(s.aggregate_errors(), AggregateErrors::Sparkles);
    }
}
