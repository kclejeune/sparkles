//! Borrowed original pin metadata. These documents do not grant a historical
//! RDF view, original physical source or current authorization.
use super::*;
use crate::access::Preparation;
use crate::store::{cold::ColdBudget, managed_history};
use serde::ser::SerializeSeq;

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
