//! Closed imported-origin metadata. Decoding establishes syntax and ownership,
//! never permission to replay a foreign seal. Physical/source proofs are separate.

mod documents;
mod identities;
mod physical;

pub(in crate::store) use identities::Identities;

use super::*;
use crate::evidence::Sha256Digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

const FORMAT: u32 = 1;
const MAX_MEMBERS: usize = 4096;
const ROOT_FILE: &str = "evidence-history/origin.json";

/// Physical storage is independent of the family's mutable identity. Main stays
/// at the root across identity changes; a branch directory has its own UUID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Location {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<Uuid>,
    generation: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BaseImport {
    local: Location,
    source: Sha256Digest,
    original: Location,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RangeImport {
    local: Location,
    source: Sha256Digest,
    interval: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Origin {
    format: u32,
    dataset_id: Uuid,
    sources: Vec<Sha256Digest>,
    bases: Vec<BaseImport>,
    ranges: Vec<RangeImport>,
}

/// A source root may be a standalone branch. Its logical UUID and the publishing
/// family's UUID are checked separately against their original small documents.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceRoot {
    logical_id: Uuid,
    head: u64,
    generation: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceFile {
    bytes: u64,
    sha256: Sha256Digest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceInterval {
    root: String,
    generation: String,
    start: u64,
    end: u64,
    sha256: Sha256Digest,
    first_seq: u64,
    last_seq: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Descriptor {
    format: u32,
    family_id: Uuid,
    roots: BTreeMap<String, SourceRoot>,
    files: BTreeMap<String, SourceFile>,
    sources: Vec<Sha256Digest>,
    intervals: Vec<SourceInterval>,
}

fn version(format: u32) -> Result<()> {
    if format != FORMAT {
        return Err(Error::Unsupported(format!(
            "configuration import format {format}; this reader supports {FORMAT}"
        )));
    }
    Ok(())
}

fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

fn generation(value: &str) -> bool {
    value.strip_prefix("gen-").is_some_and(|digits| {
        (4..=8).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn digest_name(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn lineage_file(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("evidence-history/") else {
        return false;
    };
    if rest == "origin.json" {
        return true;
    }
    let Some((lineage, member)) = rest.split_once('/') else {
        return false;
    };
    canonical_uuid(lineage)
        && (member == "coverage.json"
            || member
                .strip_prefix("generations/")
                .and_then(|file| file.strip_suffix(".json"))
                .is_some_and(digest_name))
}

fn primary_metadata(path: &str) -> bool {
    let Some((directory, file)) = path.split_once('/') else {
        return false;
    };
    generation(directory) && matches!(file, "commit.json" | "meta.json")
}

/// Original small members only. In particular, no WAL, RDF index, arbitrary
/// nested path or recursively copied import directory is a source member.
fn source_member(path: &str) -> bool {
    if let Some(rest) = path.strip_prefix("branches/") {
        let Some((id, member)) = rest.split_once('/') else {
            return false;
        };
        return canonical_uuid(id)
            && (matches!(
                member,
                "CURRENT" | "dataset.json" | "branch.json" | "history.json" | "origin.json"
            ) || primary_metadata(member));
    }
    matches!(
        path,
        "CURRENT"
            | "dataset.json"
            | "family-dataset.json"
            | "managed.json"
            | "branches.json"
            | "branch.json"
            | "history.json"
            | "origin.json"
            | "prefixes.json"
    ) || crate::managed::OwnerKind::ALL
        .iter()
        .any(|owner| path == owner.settings_file())
        || primary_metadata(path)
        || lineage_file(path)
}

fn imported_path(path: &str) -> bool {
    if path == ROOT_FILE {
        return true;
    }
    let Some(rest) = path.strip_prefix("evidence-history/imports/") else {
        return false;
    };
    let Some((digest, member)) = rest.split_once('/') else {
        return false;
    };
    digest_name(digest)
        && (member == "origin.json" || member.strip_prefix("source/").is_some_and(source_member))
}

fn sorted_sources(sources: &[Sha256Digest]) -> bool {
    sources.len() <= MAX_MEMBERS
        && sources
            .windows(2)
            .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
}

impl Location {
    fn validate(&self) -> Result<()> {
        if self.branch.is_some_and(|id| id.is_nil()) || !generation(&self.generation) {
            return Err(managed_history::unavailable(
                "invalid imported physical generation",
            ));
        }
        Ok(())
    }
}

impl Origin {
    fn validate(&self, preparation: &Preparation) -> Result<()> {
        preparation.check()?;
        version(self.format)?;
        if self.dataset_id.is_nil()
            || self.sources.is_empty()
            || !sorted_sources(&self.sources)
            || self.bases.len() > MAX_MEMBERS
            || self.ranges.len() > MAX_MEMBERS
        {
            return Err(managed_history::unavailable("invalid imported origin root"));
        }
        let contains = |source: &Sha256Digest| {
            self.sources
                .binary_search_by(|value| value.as_bytes().cmp(source.as_bytes()))
                .is_ok()
        };
        let mut previous = None;
        for base in &self.bases {
            preparation.check()?;
            base.local.validate()?;
            base.original.validate()?;
            if !contains(&base.source) || previous.is_some_and(|value| value >= &base.local) {
                return Err(managed_history::unavailable(
                    "ambiguous imported base assignment",
                ));
            }
            previous = Some(&base.local);
        }
        let mut previous: Option<&RangeImport> = None;
        for range in &self.ranges {
            preparation.check()?;
            range.local.validate()?;
            if !contains(&range.source)
                || range.interval as usize >= MAX_MEMBERS
                || previous.is_some_and(|value| {
                    (&value.local, value.source.as_bytes(), value.interval)
                        >= (&range.local, range.source.as_bytes(), range.interval)
                })
            {
                return Err(managed_history::unavailable(
                    "ambiguous imported interval assignment",
                ));
            }
            previous = Some(range);
        }
        preparation.check()
    }

    fn decode(bytes: &[u8], preparation: &Preparation) -> Result<managed_history::Owned<Self>> {
        let owned: managed_history::Owned<Self> = managed_history::decode(bytes, preparation)?;
        owned.value.validate(preparation)?;
        Ok(owned)
    }
}

impl Descriptor {
    fn validate(&self, preparation: &Preparation) -> Result<()> {
        preparation.check()?;
        version(self.format)?;
        if self.family_id.is_nil()
            || self.roots.is_empty()
            || self.roots.len() > MAX_MEMBERS
            || !self.roots.contains_key("main")
            || self.files.len() > MAX_MEMBERS
            || !self.files.contains_key("dataset.json")
            || !self.files.contains_key("family-dataset.json")
            || !self.files.contains_key("managed.json")
            || !sorted_sources(&self.sources)
            || self.intervals.len() > MAX_MEMBERS
        {
            return Err(managed_history::unavailable(
                "invalid imported source descriptor",
            ));
        }
        let _scratch = preparation.reserve((self.roots.len() as u64).saturating_mul(128))?;
        let mut identities = std::collections::BTreeSet::new();
        for (name, root) in &self.roots {
            preparation.check()?;
            if root.logical_id.is_nil()
                || !generation(&root.generation)
                || !identities.insert(root.logical_id)
                || (name != "main"
                    && (!canonical_uuid(name)
                        || root.logical_id.to_string() != *name
                        || root.logical_id == self.family_id))
            {
                return Err(managed_history::unavailable(
                    "invalid imported logical root",
                ));
            }
            let prefix = if name == "main" {
                String::new()
            } else {
                format!("branches/{name}/")
            };
            if !self.files.contains_key(&format!("{prefix}dataset.json"))
                || (root.logical_id != self.family_id
                    && !self.files.contains_key(&format!("{prefix}branch.json")))
                || (name == "main" && root.logical_id != self.family_id && self.roots.len() != 1)
            {
                return Err(managed_history::unavailable(
                    "imported root identity documents missing",
                ));
            }
        }
        let mut bytes = 0u64;
        for (path, file) in &self.files {
            preparation.check()?;
            if !source_member(path) {
                return Err(managed_history::unavailable(
                    "unknown imported source member",
                ));
            }
            bytes = bytes.checked_add(file.bytes).ok_or_else(|| {
                managed_history::unavailable("imported metadata length overflows")
            })?;
        }
        let mut previous: Option<&SourceInterval> = None;
        for interval in &self.intervals {
            preparation.check()?;
            let Some(root) = self.roots.get(&interval.root) else {
                return Err(managed_history::unavailable(
                    "imported interval has no logical root",
                ));
            };
            if !generation(&interval.generation)
                || interval.start >= interval.end
                || !interval.start.is_multiple_of(WAL_REC as u64)
                || !interval.end.is_multiple_of(WAL_REC as u64)
                || interval.first_seq > interval.last_seq
                || interval.last_seq > root.head
                || previous.is_some_and(|value| {
                    (&value.root, &value.generation, value.start)
                        >= (&interval.root, &interval.generation, interval.start)
                        || (value.root == interval.root
                            && value.generation == interval.generation
                            && value.end > interval.start)
                })
            {
                return Err(managed_history::unavailable(
                    "invalid imported source interval",
                ));
            }
            previous = Some(interval);
        }
        preparation.check()
    }

    fn decode(bytes: &[u8], preparation: &Preparation) -> Result<managed_history::Owned<Self>> {
        let owned: managed_history::Owned<Self> = managed_history::decode(bytes, preparation)?;
        owned.value.validate(preparation)?;
        Ok(owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preparation(bytes: u64) -> Preparation {
        Preparation::new(crate::task::Control::none(), bytes).unwrap()
    }

    fn digest(value: u8) -> Sha256Digest {
        Sha256Digest::from_digest_bytes([value; 32])
    }

    fn origin() -> Origin {
        Origin {
            format: FORMAT,
            dataset_id: Uuid::from_u128(3),
            sources: vec![digest(1)],
            bases: Vec::new(),
            ranges: Vec::new(),
        }
    }

    fn descriptor() -> Descriptor {
        Descriptor {
            format: FORMAT,
            family_id: Uuid::from_u128(1),
            roots: BTreeMap::from([(
                "main".into(),
                SourceRoot {
                    logical_id: Uuid::from_u128(2),
                    head: 7,
                    generation: "gen-0001".into(),
                },
            )]),
            files: [
                "dataset.json",
                "family-dataset.json",
                "managed.json",
                "branch.json",
                "gen-0001/commit.json",
            ]
            .into_iter()
            .map(|path| {
                (
                    path.into(),
                    SourceFile {
                        bytes: 1,
                        sha256: digest(4),
                    },
                )
            })
            .collect(),
            sources: Vec::new(),
            intervals: vec![SourceInterval {
                root: "main".into(),
                generation: "gen-0001".into(),
                start: 0,
                end: WAL_REC as u64 * 4,
                sha256: digest(5),
                first_seq: 7,
                last_seq: 7,
            }],
        }
    }

    #[test]
    fn imported_metadata_paths_are_closed_and_do_not_copy_wals() {
        let id = Uuid::from_u128(1);
        let hash = "ab".repeat(32);
        let prefix = format!("evidence-history/imports/{hash}");
        for path in [
            ROOT_FILE.to_string(),
            format!("{prefix}/origin.json"),
            format!("{prefix}/source/family-dataset.json"),
            format!("{prefix}/source/branches/{id}/gen-0001/commit.json"),
            format!("{prefix}/source/evidence-history/{id}/coverage.json"),
            format!("{prefix}/source/evidence-history/{id}/generations/{hash}.json"),
        ] {
            assert!(imported_path(&path), "{path}");
        }
        for path in [
            format!("/{prefix}/origin.json"),
            format!("{prefix}/source/../dataset.json"),
            format!("{prefix}/source/gen-0001/wal.log"),
            format!("{prefix}/source/gen-0001/spo.dat"),
            format!("{prefix}/source/evidence-history/imports/{hash}/origin.json"),
            format!("{prefix}/source/branches/{}/dataset.json", Uuid::nil()),
            format!("{prefix}/source/branches/{id}/evidence.json"),
            format!("{prefix}/origin.json/extra"),
            format!(
                "evidence-history/imports/{}/origin.json",
                hash.to_uppercase()
            ),
        ] {
            assert!(!imported_path(&path), "{path}");
        }
    }

    #[test]
    fn imported_decoded_payload_holds_actual_caller_credit() {
        let preparation = preparation(1 << 20);
        let encoded = managed_history::encode(&origin(), &preparation).unwrap();
        let before = preparation.room();
        let decoded = Origin::decode(&encoded.value, &preparation).unwrap();
        assert!(preparation.room() < before);
        assert_eq!(decoded.value.dataset_id, Uuid::from_u128(3));
        drop(decoded);
        assert_eq!(preparation.room(), before);
        let pressure = preparation
            .reserve(preparation.room().checked_sub(64).unwrap())
            .unwrap();
        assert_eq!(preparation.room(), 64);
        assert!(matches!(
            Origin::decode(&encoded.value, &preparation),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(preparation.room(), 64);
        drop(pressure);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn imported_codec_refuses_unknown_duplicate_and_unsorted_metadata() {
        let preparation = preparation(1 << 20);
        let encoded = managed_history::encode(&origin(), &preparation).unwrap();
        let text = std::str::from_utf8(&encoded.value).unwrap();
        for invalid in [
            text.replacen("\"format\":1", "\"format\":2", 1),
            text.replacen("\"format\":1", "\"format\":1,\"format\":1", 1),
            text.replacen("\"format\":1", "\"format\":1,\"unknown\":0", 1),
        ] {
            assert!(Origin::decode(invalid.as_bytes(), &preparation).is_err());
        }
        let mut bad = origin();
        bad.sources = vec![digest(2), digest(1)];
        assert!(bad.validate(&preparation).is_err());
        bad.sources = vec![digest(1), digest(1)];
        assert!(bad.validate(&preparation).is_err());
        bad.sources.clear();
        assert!(bad.validate(&preparation).is_err());
    }

    #[test]
    fn imported_source_family_and_standalone_logical_identity_remain_distinct() {
        let preparation = preparation(1 << 20);
        let mut source = descriptor();
        source.validate(&preparation).unwrap();
        assert_ne!(source.family_id, source.roots["main"].logical_id);
        let encoded = managed_history::encode(&source, &preparation).unwrap();
        let decoded = Descriptor::decode(&encoded.value, &preparation).unwrap();
        assert_eq!(decoded.value.family_id, source.family_id);
        source.files.remove("branch.json");
        assert!(source.validate(&preparation).is_err());
    }

    #[test]
    fn imported_interval_schema_rejects_overlap_alignment_and_overflow() {
        let preparation = preparation(1 << 20);
        let mut source = descriptor();
        source.intervals.push(SourceInterval {
            root: "main".into(),
            generation: "gen-0001".into(),
            start: WAL_REC as u64 * 3,
            end: WAL_REC as u64 * 5,
            sha256: digest(6),
            first_seq: 7,
            last_seq: 7,
        });
        assert!(source.validate(&preparation).is_err());
        source.intervals.pop();
        source.intervals[0].start = 1;
        assert!(source.validate(&preparation).is_err());
        source.intervals[0].start = 0;
        source.intervals[0].last_seq = 8;
        assert!(source.validate(&preparation).is_err());
        source.intervals[0].last_seq = 7;
        source.files.get_mut("dataset.json").unwrap().bytes = u64::MAX;
        assert!(source.validate(&preparation).is_err());
    }
}
