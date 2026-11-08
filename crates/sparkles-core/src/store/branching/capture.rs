//! Original family-table metadata from a genuine resident BranchSet. This does
//! not capture other branches' RDF or authorize a foreign primary header.
use super::*;
use crate::access::Preparation;
use crate::store::{cold::ColdBudget, managed_history};

impl BranchSet {
    /// Call under the actual logical Store's writer, before its history mutex.
    pub(in crate::store) fn original_table(
        &self,
        preparation: &Preparation,
    ) -> Result<Option<managed_history::Bytes>> {
        let table = preparation.lock(&self.table)?;
        if table.dataset_id != self.dataset_id {
            return Err(managed_history::unavailable(
                "original family table identity differs from its BranchSet",
            ));
        }
        if self.memory {
            let _scratch = preparation.reserve(4096)?;
            return managed_history::encode_pretty(&BorrowedTable::new(&table), preparation)
                .map(Some);
        }
        let bytes = super::super::configuration_history::original_branch_table_document(
            &self.root,
            preparation,
        )?;
        let Some(bytes) = bytes else {
            if *table != TableFile::new(self.dataset_id) {
                return Err(managed_history::unavailable(
                    "original branch table is missing live family metadata",
                ));
            }
            return Ok(None);
        };
        inspect(&bytes.value, &table, preparation)?;
        preparation.check()?;
        Ok(Some(bytes))
    }
}

/// Only the normalized version header is derived. Payloads are borrowed from
/// the actual locked table; no Vec/String/Store/Snapshot owner is cloned.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedTable<'a> {
    format: u32,
    dataset_id: uuid::Uuid,
    next_ordinal: u32,
    main: &'a MainEntry,
    branches: &'a [Entry],
    #[serde(skip_serializing_if = "<[Entry]>::is_empty")]
    retired: &'a [Entry],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    exempt: &'a [String],
    #[serde(skip_serializing_if = "<[Tombstone]>::is_empty")]
    deleted: &'a [Tombstone],
}
impl<'a> BorrowedTable<'a> {
    fn new(table: &'a TableFile) -> Self {
        Self {
            format: if table.retired.is_empty() { 1 } else { 2 },
            dataset_id: table.dataset_id,
            next_ordinal: table.next_ordinal,
            main: &table.main,
            branches: &table.branches,
            retired: &table.retired,
            exempt: &table.exempt,
            deleted: &table.deleted,
        }
    }
}

fn inspect(bytes: &[u8], table: &TableFile, preparation: &Preparation) -> Result<()> {
    let allowance = preparation.room();
    let _loan = preparation.reserve(allowance)?;
    let options = crate::history::HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = ColdBudget::new(crate::history::ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let raw: crate::store::cold::Loaded<serde_json::Value> = budget.json(bytes)?;
    crate::formats::check_settings(BRANCHES_FILE, &raw.value)?;
    let mut decoded: TableFile = budget.value(raw.value)?;
    let format = if table.retired.is_empty() { 1 } else { 2 };
    if decoded.format != format {
        return Err(managed_history::unavailable(
            "original branch table version differs from live retirement state",
        ));
    }
    // write_table normalizes a cloned header, so the live stored header can
    // still hold its previous version. Compare all payload fields unchanged.
    decoded.format = table.format;
    budget.check()?;
    if decoded != *table {
        return Err(managed_history::unavailable(
            "original branch table differs from the live family",
        ));
    }
    preparation.check()
}

#[cfg(test)]
pub(in crate::store) fn canonical_test_table(value: serde_json::Value) -> Vec<u8> {
    let table: TableFile = serde_json::from_value(value).unwrap();
    serde_json::to_vec_pretty(&table).unwrap()
}
