//! Borrowed original pin metadata. These documents do not grant a historical
//! RDF view, original physical source or current authorization.
use super::*;
use crate::access::Preparation;
use crate::store::{cold::ColdBudget, managed_history};
use serde::ser::SerializeSeq;

/// Pure receiving-owned history transformation for the closed import producer.
/// Preserve selectors, retention, schedules and every original provenance tag;
/// only native pin bindings become imported. No pin acquires native history.
pub(crate) fn reidentified(
    bytes: &[u8],
    old: uuid::Uuid,
    new: uuid::Uuid,
    preparation: &Preparation,
) -> Result<managed_history::Bytes> {
    preparation.check()?;
    if old.is_nil() || new.is_nil() {
        return Err(Error::invalid(
            "history identity transformation has a nil owner",
        ));
    }
    if bytes.len() > managed_history::MAX_BYTES {
        return Err(preparation.exhausted());
    }
    let _canonical = preparation.reserve(
        (bytes.len() as u64)
            .checked_mul(2)
            .and_then(|n| n.checked_add(4096))
            .ok_or_else(|| preparation.exhausted())?,
    )?;
    let allowance = preparation.room();
    let mut held = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let raw: crate::store::cold::Loaded<serde_json::Value> = budget.json(bytes)?;
    crate::formats::check_settings("history.json", &raw.value)?;
    let mut file: HistoryFile = budget.value(raw.value)?;
    checked_document(&file, bytes, old, &|| budget.check())?;
    for pin in &mut file.snapshots {
        preparation.check()?;
        if let Some(configuration) = &mut pin.configuration
            && let ConfigurationBinding::Known {
                configuration: source,
            } = configuration.binding
        {
            configuration.binding = ConfigurationBinding::ImportedUnbound { source };
        }
    }
    file.dataset_id = new;
    held.release_to(budget.used());
    preparation.check()?;
    managed_history::encode_pretty(&file, preparation)
}

/// Called while the genuine Store's writer and history mutex remain held.
/// Retain the original bytes separately; decoded containers live only in this
/// synchronous loan and never replace the actual in-memory authority.
pub(crate) fn validate_persistent(
    bytes: Option<&[u8]>,
    dataset_id: uuid::Uuid,
    actual: &HistoryState,
    preparation: &Preparation,
) -> Result<()> {
    preparation.check()?;
    let Some(bytes) = bytes else {
        if !actual.pins.is_empty()
            || actual.retention != Retention::default()
            || !actual.schedules.is_empty()
            || actual.catalog != CatalogHorizon::default()
        {
            return Err(managed_history::unavailable(
                "original history is missing live pin metadata",
            ));
        }
        return Ok(());
    };
    // Canonical format2 validation temporarily owns the pretty encoding and
    // its allocator slack. ColdBudget separately admits JSON/typed/map overlap.
    let _canonical = preparation.reserve(
        (bytes.len() as u64)
            .checked_mul(2)
            .and_then(|n| n.checked_add(4096))
            .ok_or_else(|| preparation.exhausted())?,
    )?;
    let allowance = preparation.room();
    let _loan = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let decoded = decode_file(bytes, dataset_id, Some(&mut budget))?;
    if decoded.retention != actual.retention
        || decoded.schedules != actual.schedules
        || decoded.catalog != actual.catalog
        || decoded.pins.len() != actual.pins.len()
    {
        return Err(managed_history::unavailable(
            "original history differs from the live Store",
        ));
    }
    let legacy = PinnedConfiguration::legacy();
    for (name, pin) in &decoded.pins {
        budget.check()?;
        let same = actual.pins.get(name).is_some_and(|live| {
            pin.seq == live.seq
                && pin.created_ms == live.created_ms
                && pin.note == live.note
                && pin.expires_ms == live.expires_ms
                && pin.warm == live.warm
                && pin.configuration.as_ref().unwrap_or(&legacy)
                    == live.configuration.as_ref().unwrap_or(&legacy)
        });
        if !same {
            return Err(managed_history::unavailable(
                "original named snapshot differs from the live Store",
            ));
        }
    }
    preparation.check()
}

/// The surrounding genuine identity document labels this as memory-derived.
/// Only borrow Pin metadata: no Snapshot/Arc/map clone is used for serialization.
pub(crate) fn encode_memory(
    dataset_id: uuid::Uuid,
    actual: &MemHistory,
    preparation: &Preparation,
) -> Result<managed_history::Bytes> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Document<'a> {
        format: u32,
        dataset_id: uuid::Uuid,
        retention: Retention,
        snapshots: Pins<'a>,
        #[serde(skip_serializing_if = "<[Schedule]>::is_empty")]
        schedules: &'a [Schedule],
    }
    struct Pins<'a> {
        actual: &'a BTreeMap<String, (Pin, Arc<Snapshot>)>,
        format: u32,
    }
    #[derive(Serialize)]
    struct BorrowedPin<'a> {
        name: &'a str,
        seq: u64,
        created: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        warm: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        configuration: Option<&'a PinnedConfiguration>,
    }
    impl Serialize for Pins<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            let mut sequence = serializer.serialize_seq(Some(self.actual.len()))?;
            let legacy = PinnedConfiguration::legacy();
            for (name, (pin, _snapshot)) in self.actual {
                sequence.serialize_element(&BorrowedPin {
                    name,
                    seq: pin.seq,
                    created: commit::rfc3339_ms(pin.created_ms),
                    note: pin.note.as_deref(),
                    expires: pin.expires_ms.map(commit::rfc3339_ms),
                    warm: pin.warm,
                    configuration: (self.format == 2)
                        .then(|| pin.configuration.as_ref().unwrap_or(&legacy)),
                })?;
            }
            sequence.end()
        }
    }
    preparation.check()?;
    // Constant serializer/header/date scratch is returned after encoding. The
    // final growing byte buffer has its own actual receiving reservation.
    let _scratch = preparation.reserve(4096)?;
    let format = if actual
        .pins
        .values()
        .any(|(pin, _)| pin.configuration.is_some())
    {
        2
    } else {
        1
    };
    managed_history::encode_pretty(
        &Document {
            format,
            dataset_id,
            retention: actual.retention,
            snapshots: Pins {
                actual: &actual.pins,
                format,
            },
            schedules: &actual.schedules,
        },
        preparation,
    )
}

#[cfg(test)]
pub(crate) fn canonical_test_history(value: serde_json::Value) -> Vec<u8> {
    let document: super::HistoryFile = serde_json::from_value(value).unwrap();
    serde_json::to_vec_pretty(&document).unwrap()
}

#[cfg(test)]
mod reidentify_tests {
    use super::*;
    use crate::task::Control;

    fn identity() -> ConfigurationIdentity {
        ConfigurationIdentity {
            dataset_id: uuid::Uuid::from_u128(1),
            lineage_id: uuid::Uuid::from_u128(2),
            generation: crate::evidence::Sha256Digest::from_digest_bytes([3; 32]),
        }
    }
    fn pin(name: &str, binding: ConfigurationBinding, selector: ConfigurationSelector) -> PinFile {
        PinFile {
            name: name.to_owned(),
            seq: 7,
            created: commit::rfc3339_ms(1000),
            note: Some("retained note".to_owned()),
            expires: Some(commit::rfc3339_ms(5000)),
            warm: true,
            configuration: Some(PinnedConfiguration { binding, selector }),
        }
    }

    #[test]
    fn reidentified_pins_keep_exact_original_identity_selectors_and_policy_across_two_ids() {
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        let third = uuid::Uuid::new_v4();
        let file = HistoryFile {
            format: 2,
            dataset_id: old,
            retention: Retention {
                keep_commits: Some(10),
                keep_age_ms: Some(60000),
                max_bytes: Some(1000000),
            },
            snapshots: vec![
                pin(
                    "head",
                    ConfigurationBinding::Known {
                        configuration: identity(),
                    },
                    ConfigurationSelector::Head,
                ),
                pin(
                    "imported",
                    ConfigurationBinding::ImportedUnbound { source: identity() },
                    ConfigurationSelector::Time { unix_ms: 1000 },
                ),
                pin(
                    "legacy",
                    ConfigurationBinding::LegacyUnbound,
                    ConfigurationSelector::LegacyUnspecified,
                ),
            ],
            schedules: vec![Schedule {
                prefix: "hour-".to_owned(),
                every_ms: 3600000,
                keep_last: 3,
            }],
            catalog: CatalogHorizon {
                keep_commits: Some(100),
                keep_age_ms: Some(86400000),
            },
        };
        let raw = serde_json::to_vec_pretty(&file).unwrap();
        let original = raw.clone();
        let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
        let room = preparation.room();
        let first = reidentified(&raw, old, new, &preparation).unwrap();
        let repeated = reidentified(&first.value, new, third, &preparation).unwrap();
        for (bytes, id) in [(&first.value, new), (&repeated.value, third)] {
            let parsed: HistoryFile = serde_json::from_slice(bytes).unwrap();
            assert_eq!(parsed.dataset_id, id);
            assert_eq!(parsed.retention, file.retention);
            assert_eq!(parsed.catalog, file.catalog);
            assert_eq!(parsed.schedules, file.schedules);
            for (found, before) in parsed.snapshots.iter().zip(&file.snapshots) {
                assert_eq!(found.name, before.name);
                assert_eq!(found.seq, before.seq);
                assert_eq!(found.note, before.note);
                assert_eq!(found.expires, before.expires);
                assert_eq!(found.warm, before.warm);
                assert_eq!(found.created, before.created);
                assert_eq!(
                    found.configuration.as_ref().unwrap().selector,
                    before.configuration.as_ref().unwrap().selector
                );
            }
            let decoded = decode_file(bytes, id, None).unwrap();
            assert_eq!(
                decoded.pins["head"].configuration.as_ref().unwrap().binding,
                ConfigurationBinding::ImportedUnbound { source: identity() }
            );
            assert_eq!(
                decoded.pins["imported"]
                    .configuration
                    .as_ref()
                    .unwrap()
                    .binding,
                ConfigurationBinding::ImportedUnbound { source: identity() }
            );
            assert_eq!(
                decoded.pins["legacy"]
                    .configuration
                    .as_ref()
                    .unwrap()
                    .binding,
                ConfigurationBinding::LegacyUnbound
            );
        }
        assert_eq!(raw, original);
        drop(repeated);
        drop(first);
        assert_eq!(preparation.room(), room);
    }

    #[test]
    fn reidentified_legacy_pins_keep_last_wins_absence_and_refuse_bad_input_or_controls() {
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        let raw=serde_json::to_vec(&serde_json::json!({"format":1,"datasetId":old,"retention":{"keepCommits":2},"snapshots":[{"name":"dup","seq":1,"created":"1970-01-01T00:00:01.000Z"},{"name":"dup","seq":2,"created":"1970-01-01T00:00:02.000Z"}]})).unwrap();
        let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
        let room = preparation.room();
        let output = reidentified(&raw, old, new, &preparation).unwrap();
        let parsed: HistoryFile = serde_json::from_slice(&output.value).unwrap();
        assert_eq!(parsed.format, 1);
        assert!(
            parsed
                .snapshots
                .iter()
                .all(|pin| pin.configuration.is_none())
        );
        assert_eq!(
            decode_file(&output.value, new, None).unwrap().pins["dup"].seq,
            2
        );
        drop(output);
        assert_eq!(preparation.room(), room);
        assert!(reidentified(&raw, uuid::Uuid::new_v4(), new, &preparation).is_err());
        assert!(reidentified(&raw, old, uuid::Uuid::nil(), &preparation).is_err());
        let future =
            serde_json::to_vec(&serde_json::json!({"format":99,"snapshots":false})).unwrap();
        assert!(matches!(
            reidentified(&future, old, new, &preparation),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(preparation.room(), room);
        let control = Control::none();
        let cancelled = Preparation::new(control.clone(), 8 << 20).unwrap();
        let room = cancelled.room();
        control.cancel.cancel();
        assert!(matches!(
            reidentified(&raw, old, new, &cancelled),
            Err(Error::Cancelled)
        ));
        assert_eq!(cancelled.room(), room);
        let small = Preparation::new(Control::none(), 4096).unwrap();
        let room = small.room();
        assert!(matches!(
            reidentified(&raw, old, new, &small),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(small.room(), room);
    }
}
