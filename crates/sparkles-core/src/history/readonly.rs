//! Scalar pin selection without opening, repairing, expiring or modifying a store.
use super::*;
use crate::store::cold::{ColdBudget, Loaded};

/// Select the exact on-disk pin using the ordinary schema and last-name-wins
/// semantics. This returns metadata only, never a retention or access proof.
pub(crate) fn pin_controlled(
    root: &Path,
    dataset: uuid::Uuid,
    name: &str,
    budget: &mut ColdBudget<'_>,
) -> Result<Option<u64>> {
    if !valid_name(name) {
        return Err(Error::invalid("invalid snapshot name"));
    }
    let start = budget.used();
    budget.charge(
        (root.as_os_str().len() as u64)
            .saturating_mul(2)
            .saturating_add(128),
    )?;
    let result = (|| {
        let bytes = match budget.read_file(&root.join("history.json")) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let raw: Loaded<serde_json::Value> = budget.json(&bytes)?;
        // Avoid formatting an unbounded malformed native version value in the
        // shared diagnostic. Known numeric versions keep the authoritative check.
        if raw
            .value
            .get("format")
            .is_some_and(|value| !value.is_null() && !value.is_number())
        {
            return Err(Error::Corrupt(
                "history.json: format is not a version".into(),
            ));
        }
        crate::formats::check_settings("history.json", &raw.value)?;
        let file: HistoryFile = budget.value(raw.value)?;
        if file.dataset_id != dataset {
            return Err(Error::Corrupt(
                "history.json belongs to another dataset".into(),
            ));
        }
        let mut selected = None;
        for pin in file.snapshots {
            budget.check()?;
            if pin.name == name {
                selected = Some(pin.seq);
            }
        }
        budget.check()?;
        Ok(selected)
    })();
    // Owned metadata and its parser scratch drop before their held charge returns
    // to the same request. Existing caller-held bytes remain untouched.
    budget.release(budget.used() - start);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn pin_selection_matches_ordinary_schema_duplicates_uuid_and_version_refusal() {
        let root = tempfile::tempdir().unwrap();
        let dataset = uuid::Uuid::new_v4();
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(8 << 20),
            held: 0,
        })
        .unwrap();
        budget.charge(17).unwrap();
        assert_eq!(
            pin_controlled(root.path(), dataset, "stable", &mut budget).unwrap(),
            None
        );
        assert_eq!(budget.used(), 17);
        assert!(!root.path().join("history.json").exists());
        let valid = serde_json::json!({
            "format": 1, "datasetId": dataset,
            "snapshots": [
                {"name": "stable", "seq": 2, "created": "not-a-time"},
                {"name": "other", "seq": 3, "created": "2020-01-01T00:00:00Z"},
                {"name": "stable", "seq": 7, "created": "2020-01-01T00:00:00Z", "expires": "invalid"}
            ]
        });
        let mut future = valid.clone();
        future["format"] = 99.into();
        future["snapshots"] = false.into();
        let mut foreign = valid.clone();
        foreign["datasetId"] = uuid::Uuid::new_v4().to_string().into();
        let mut malformed = valid.clone();
        malformed["snapshots"] = false.into();
        for value in [valid, future, foreign, malformed] {
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(root.path().join("history.json"), &bytes).unwrap();
            let expected = read_file(root.path(), dataset)
                .map(|config| config.pins.get("stable").map(|p| p.seq));
            let actual = pin_controlled(root.path(), dataset, "stable", &mut budget);
            match expected {
                Ok(expected) => assert_eq!(actual.unwrap(), expected),
                Err(Error::Unsupported(_)) => assert!(matches!(actual, Err(Error::Unsupported(_)))),
                Err(Error::Corrupt(_)) => assert!(matches!(actual, Err(Error::Corrupt(_)))),
                other => panic!("unexpected ordinary pin decode {other:?}"),
            }
            assert_eq!(budget.used(), 17);
            assert_eq!(
                std::fs::read(root.path().join("history.json")).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn wide_pin_metadata_cancels_inside_decode_without_mutation_or_charge_leaks() {
        let root = tempfile::tempdir().unwrap();
        let dataset = uuid::Uuid::new_v4();
        let value = serde_json::json!({"format": 1, "datasetId": dataset, "snapshots":
            (0..2000).map(|n| serde_json::json!({"name": format!("pin{n}"), "seq": n, "created": "not-a-time"})).collect::<Vec<_>>()});
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(root.path().join("history.json"), &bytes).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(32 << 20),
            held: 0,
        })
        .unwrap();
        let observer = |stage: &str, at: usize| {
            if stage == "metadata value decode" && at == 75 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        budget.observe_keys(&observer);
        assert!(matches!(
            pin_controlled(root.path(), dataset, "pin1999", &mut budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(
            std::fs::read(root.path().join("history.json")).unwrap(),
            bytes
        );
    }
}
