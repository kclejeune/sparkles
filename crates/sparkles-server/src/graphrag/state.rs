//! The persisted state of a dataset's GraphRAG builds (spec C22 §2.2, §3.1, §3.5), kept
//! in `<db>/graphrag/state.json`.
//!
//! It holds the communities of the last build, which the next build matches for stable
//! identities. The daily token ledger and the anomalies live in a small file of their
//! own ([`super::ledger`]), since they change with every model request. The file
//! carries a format version, and a
//! reader refuses a version it does not know. Every collection has a bound, checked when
//! the state is read and before it is written, so a damaged or hostile file cannot make a
//! build allocate without limit. Writes replace the file durably through a temporary
//! file, a flush and a rename.

use super::identity::Previous;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The state format this server writes. Version 3 adds the review policy of a build
/// and the claim and decision pairs each report read under it (spec C25, "Reports on
/// accepted evidence").
pub const STATE_VERSION: u32 = 3;
/// The older format this server still reads, whose reports record no policy.
pub const STATE_VERSION_2: u32 = 2;
/// The most claim and decision pairs the state may hold.
pub const MAX_PAIRS: usize = 1_000_000;
/// The most references to pairs over all communities.
pub const MAX_PAIR_REFERENCES: usize = 32_000_000;
/// The most communities the state may hold.
pub const MAX_COMMUNITIES: usize = 200_000;
/// The most member entries over all communities.
pub const MAX_MEMBER_ENTRIES: usize = 5_000_000;
/// The most fact digests one community may record.
pub const MAX_FACT_DIGESTS: usize = 100_000;
/// The longest member term, in bytes.
pub const MAX_TERM_BYTES: usize = 8_192;
/// The largest state file read, in bytes.
pub const MAX_FILE_BYTES: u64 = 1 << 30;

/// A writer that fails once it would pass its limit.
struct Limited<W> {
    inner: W,
    left: u64,
    over: bool,
}

impl<W: Write> Write for Limited<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.left {
            self.over = true;
            return Err(std::io::Error::other("the state passes its size limit"));
        }
        let n = self.inner.write(buf)?;
        self.left -= n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The file of a database directory.
pub fn path_in(db: &Path) -> PathBuf {
    db.join("graphrag").join("state.json")
}

/// A community as the last build kept it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredCommunity {
    pub depth: u32,
    pub iri: String,
    /// the parent's IRI, `None` at depth 0
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// canonical N-Triples terms, sorted and without repeats
    pub members: Vec<String>,
    /// the report the build left linked, if any
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// digests of the facts a leaf's report was written from, sorted, which a refresh
    /// compares with the facts it reads (§5)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<u64>,
    /// whether the report records a review policy, whichever it is
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub policy: bool,
    /// the positions in [`BuildRecord::pairs`] of the claim and decision pairs the
    /// report's input used, its children's included, when the report was read under the
    /// build's policy and the list is complete; an empty list is complete
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairs: Option<Vec<u32>>,
}

/// The last completed build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BuildRecord {
    pub source_commit: u64,
    pub branch: String,
    /// RFC 3339
    pub finished_at: String,
    pub communities: Vec<StoredCommunity>,
    /// communities the build left without a current report, because its budget or an
    /// accounting anomaly stopped it
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
    /// tokens the build used, reserved or reported
    #[serde(default)]
    pub tokens: u64,
    /// the reports graph the build wrote, and the IRI of its footprint, which decide
    /// who may see this record (§6.4)
    #[serde(default)]
    pub reports_graph: String,
    #[serde(default)]
    pub footprint: String,
    /// the review policy the build read under
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<StoredPolicy>,
    /// the claim and decision pairs the communities name by position
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<[String; 2]>,
}

/// The review policy of a build, as its view recorded it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredPolicy {
    pub name: String,
    pub iri: String,
    pub version: u32,
}

/// `graphrag/state.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BuildState {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_build: Option<BuildRecord>,
}

impl Default for BuildState {
    fn default() -> BuildState {
        BuildState {
            version: STATE_VERSION,
            last_build: None,
        }
    }
}

#[derive(Debug)]
pub enum StateError {
    /// the file names a format this server does not read
    UnknownVersion(u64),
    /// the file is not a state of a known shape
    Corrupt(String),
    /// a collection passes its bound
    Bound {
        what: &'static str,
        limit: usize,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::UnknownVersion(v) => write!(
                f,
                "the GraphRAG state has format version {v}, and this server reads version {STATE_VERSION}"
            ),
            StateError::Corrupt(m) => write!(f, "the GraphRAG state cannot be read: {m}"),
            StateError::Bound { what, limit } => {
                write!(f, "the GraphRAG state holds more than {limit} {what}")
            }
            StateError::Io(e) => write!(f, "the GraphRAG state: {e}"),
        }
    }
}

impl std::error::Error for StateError {}

impl From<std::io::Error> for StateError {
    fn from(e: std::io::Error) -> StateError {
        StateError::Io(e)
    }
}

impl BuildState {
    /// The checks of the bounds and of the members' order.
    pub fn check(&self) -> Result<(), StateError> {
        if self.version != STATE_VERSION {
            return Err(StateError::UnknownVersion(self.version.into()));
        }
        let Some(b) = &self.last_build else {
            return Ok(());
        };
        if b.communities.len() > MAX_COMMUNITIES {
            return Err(StateError::Bound {
                what: "communities",
                limit: MAX_COMMUNITIES,
            });
        }
        if b.pairs.len() > MAX_PAIRS {
            return Err(StateError::Bound {
                what: "claim and decision pairs",
                limit: MAX_PAIRS,
            });
        }
        if b.pairs.iter().flatten().any(|t| t.len() > MAX_TERM_BYTES) {
            return Err(StateError::Bound {
                what: "bytes in a claim or decision IRI",
                limit: MAX_TERM_BYTES,
            });
        }
        if b.policy.is_none() && !b.pairs.is_empty() {
            return Err(StateError::Corrupt(
                "the state records pairs without a review policy".into(),
            ));
        }
        let mut refs = 0usize;
        for c in &b.communities {
            let Some(p) = &c.pairs else { continue };
            refs += p.len();
            if refs > MAX_PAIR_REFERENCES {
                return Err(StateError::Bound {
                    what: "references to pairs",
                    limit: MAX_PAIR_REFERENCES,
                });
            }
            if b.policy.is_none() || p.iter().any(|&i| i as usize >= b.pairs.len()) {
                return Err(StateError::Corrupt(format!(
                    "community {} names a pair the state does not hold",
                    c.iri
                )));
            }
        }
        let mut entries = 0usize;
        let mut iris = std::collections::HashSet::new();
        for c in &b.communities {
            entries += c.members.len();
            if entries > MAX_MEMBER_ENTRIES {
                return Err(StateError::Bound {
                    what: "member entries",
                    limit: MAX_MEMBER_ENTRIES,
                });
            }
            if c.depth > super::settings::MAX_LEVELS {
                return Err(StateError::Corrupt(format!(
                    "community {} has depth {}",
                    c.iri, c.depth
                )));
            }
            if !super::settings::is_guarded(&c.iri) || !iris.insert(c.iri.as_str()) {
                return Err(StateError::Corrupt(format!(
                    "community IRI {} is repeated or outside the GraphRAG namespace",
                    c.iri
                )));
            }
            if c.members.iter().any(|m| m.len() > MAX_TERM_BYTES) {
                return Err(StateError::Bound {
                    what: "bytes in a member term",
                    limit: MAX_TERM_BYTES,
                });
            }
            if c.facts.len() > MAX_FACT_DIGESTS {
                return Err(StateError::Bound {
                    what: "fact digests of one community",
                    limit: MAX_FACT_DIGESTS,
                });
            }
            if c.members.windows(2).any(|w| w[0] >= w[1]) {
                return Err(StateError::Corrupt(format!(
                    "the members of {} are not sorted and distinct",
                    c.iri
                )));
            }
        }
        Ok(())
    }

    /// Parse a state, refusing an unknown version before reading any other member.
    pub fn parse(bytes: &[u8]) -> Result<BuildState, StateError> {
        #[derive(Deserialize)]
        struct Probe {
            version: Option<serde_json::Value>,
        }
        let probe: Probe =
            serde_json::from_slice(bytes).map_err(|e| StateError::Corrupt(e.to_string()))?;
        match probe.version.as_ref().and_then(|v| v.as_u64()) {
            Some(v) if v == u64::from(STATE_VERSION) || v == u64::from(STATE_VERSION_2) => {}
            Some(v) => return Err(StateError::UnknownVersion(v)),
            None => return Err(StateError::Corrupt("the version is missing".into())),
        }
        let mut s: BuildState =
            serde_json::from_slice(bytes).map_err(|e| StateError::Corrupt(e.to_string()))?;
        if s.version == STATE_VERSION_2 {
            // a version 2 state records no policy, and the next write is version 3
            if s.last_build.as_ref().is_some_and(|b| {
                b.policy.is_some()
                    || !b.pairs.is_empty()
                    || b.communities.iter().any(|c| c.policy || c.pairs.is_some())
            }) {
                return Err(StateError::Corrupt(
                    "a version 2 state records a review policy".into(),
                ));
            }
            s.version = STATE_VERSION;
        }
        s.check()?;
        Ok(s)
    }

    /// The state of a database directory: the default when there is none.
    pub fn load(db: &Path) -> Result<BuildState, StateError> {
        let path = path_in(db);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BuildState::default());
            }
            Err(e) => return Err(e.into()),
        };
        if meta.len() > MAX_FILE_BYTES {
            return Err(StateError::Bound {
                what: "bytes",
                limit: MAX_FILE_BYTES as usize,
            });
        }
        BuildState::parse(&std::fs::read(&path)?)
    }

    /// Write the state durably.
    pub fn save(&self, db: &Path) -> Result<(), StateError> {
        self.save_within(db, MAX_FILE_BYTES)
    }

    /// Write the state durably in at most `limit` bytes. The state is serialized
    /// straight into the temporary file through a writer that stops at the limit, so a
    /// state that would not load again is refused before the previous file is replaced.
    fn save_within(&self, db: &Path, limit: u64) -> Result<(), StateError> {
        self.check()?;
        let path = path_in(db);
        let dir = path.parent().expect("the state file has a directory");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("state.json.tmp");
        let written = (|| {
            let f = std::fs::File::create(&tmp)?;
            let mut w = Limited {
                inner: std::io::BufWriter::new(f),
                left: limit,
                over: false,
            };
            let r = serde_json::to_writer(&mut w, self);
            if w.over {
                return Err(StateError::Bound {
                    what: "bytes",
                    limit: limit as usize,
                });
            }
            r.map_err(|e| StateError::Corrupt(e.to_string()))?;
            let f = w.inner.into_inner().map_err(|e| e.into_error())?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::fs::rename(&tmp, &path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    }

    /// The previous build's communities, for identity matching.
    pub fn previous(&self) -> Vec<Previous> {
        self.last_build.as_ref().map_or_else(Vec::new, |b| {
            b.communities
                .iter()
                .map(|c| Previous {
                    depth: c.depth,
                    iri: c.iri.clone(),
                    members: c.members.clone(),
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn community(iri: &str, members: &[&str]) -> StoredCommunity {
        StoredCommunity {
            depth: 0,
            iri: format!("urn:x-sparkles:graphrag:ds:0:{iri}"),
            parent: None,
            members: members.iter().map(|m| m.to_string()).collect(),
            report: Some("urn:uuid:r".into()),
            facts: vec![1, 2],
            policy: false,
            pairs: None,
        }
    }

    fn state() -> BuildState {
        BuildState {
            last_build: Some(BuildRecord {
                source_commit: 12,
                branch: "main".into(),
                finished_at: "2026-10-23T00:00:00Z".into(),
                communities: vec![community("a", &["<urn:a>", "<urn:b>"])],
                pending: Vec::new(),
                tokens: 10,
                reports_graph: "urn:x-sparkles:graphrag:reports".into(),
                footprint: "urn:x-sparkles:graphrag:footprint:00".into(),
                policy: None,
                pairs: Vec::new(),
            }),
            ..BuildState::default()
        }
    }

    #[test]
    fn a_state_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(BuildState::load(dir.path()).unwrap(), BuildState::default());
        let s = state();
        s.save(dir.path()).unwrap();
        assert_eq!(BuildState::load(dir.path()).unwrap(), s);
        assert_eq!(s.previous()[0].members, vec!["<urn:a>", "<urn:b>"]);
    }

    #[test]
    fn unknown_versions_are_refused_before_their_members() {
        let e = BuildState::parse(br#"{"version": 4, "whatever": [1]}"#).unwrap_err();
        assert!(matches!(e, StateError::UnknownVersion(4)), "{e}");
        // version 1 had no record of the reports graph and footprint, and is refused
        let e = BuildState::parse(br#"{"version": 1}"#).unwrap_err();
        assert!(matches!(e, StateError::UnknownVersion(1)), "{e}");
        let e = BuildState::parse(br#"{"lastBuild": null}"#).unwrap_err();
        assert!(matches!(e, StateError::Corrupt(_)), "{e}");
        let e = BuildState::parse(br#"{"version": 3, "extra": 1}"#).unwrap_err();
        assert!(matches!(e, StateError::Corrupt(_)), "{e}");
        assert!(matches!(
            BuildState::parse(b"not json"),
            Err(StateError::Corrupt(_))
        ));
    }

    /// The format registry, which checks a dataset's files when it opens, reads the
    /// version this build writes and refuses the next one.
    #[test]
    fn the_format_registry_reads_the_written_version() {
        let check = |v: u32| {
            sparkles::formats::check_settings(
                "graphrag/state.json",
                &serde_json::json!({ "version": v }),
            )
        };
        assert_eq!(check(STATE_VERSION).unwrap(), STATE_VERSION);
        assert!(check(STATE_VERSION_2).is_ok());
        assert!(check(STATE_VERSION + 1).is_err());
    }

    /// A version 2 state reads as a state without a policy, and is written as version
    /// 3; a version 2 file that records a policy is refused.
    #[test]
    fn version_2_states_read_without_a_policy() {
        let mut v = serde_json::to_value(state()).unwrap();
        v["version"] = 2.into();
        let s = BuildState::parse(v.to_string().as_bytes()).unwrap();
        assert_eq!(s, state());
        assert_eq!(s.version, STATE_VERSION);
        v["lastBuild"]["policy"] =
            serde_json::json!({ "name": "main", "iri": "urn:p", "version": 1 });
        assert!(matches!(
            BuildState::parse(v.to_string().as_bytes()),
            Err(StateError::Corrupt(_))
        ));
    }

    /// Pairs round-trip, an empty list is kept apart from none, and a position past the
    /// table is refused.
    #[test]
    fn pairs_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state();
        let b = s.last_build.as_mut().unwrap();
        b.policy = Some(StoredPolicy {
            name: "main".into(),
            iri: "urn:p".into(),
            version: 1,
        });
        b.pairs = vec![["urn:c".into(), "urn:d".into()]];
        b.communities[0].policy = true;
        b.communities[0].pairs = Some(vec![0]);
        let mut empty = b.communities[0].clone();
        empty.iri = "urn:x-sparkles:graphrag:ds:0:b".into();
        empty.pairs = Some(Vec::new());
        b.communities.push(empty);
        s.save(dir.path()).unwrap();
        assert_eq!(BuildState::load(dir.path()).unwrap(), s);
        let mut bad = s.clone();
        bad.last_build.as_mut().unwrap().communities[0].pairs = Some(vec![1]);
        assert!(matches!(bad.check(), Err(StateError::Corrupt(_))));
        let mut bad = s.clone();
        bad.last_build.as_mut().unwrap().policy = None;
        assert!(matches!(bad.check(), Err(StateError::Corrupt(_))));
    }

    #[test]
    fn bounds_and_order_are_checked() {
        let mut s = state();
        s.last_build.as_mut().unwrap().communities[0].members =
            vec!["<urn:b>".into(), "<urn:a>".into()];
        assert!(matches!(s.check(), Err(StateError::Corrupt(_))));
        let mut s = state();
        let c = s.last_build.as_ref().unwrap().communities[0].clone();
        s.last_build.as_mut().unwrap().communities.push(c);
        assert!(
            matches!(s.check(), Err(StateError::Corrupt(_))),
            "repeated IRI"
        );
        let mut s = state();
        s.last_build.as_mut().unwrap().communities[0].iri = "urn:other".into();
        assert!(matches!(s.check(), Err(StateError::Corrupt(_))));
        let mut s = state();
        s.last_build.as_mut().unwrap().communities[0].members =
            vec!["x".repeat(MAX_TERM_BYTES + 1)];
        assert!(matches!(s.check(), Err(StateError::Bound { .. })));
    }

    /// A state that would pass the file limit is refused and the previous file stays
    /// (review finding 4).
    #[test]
    fn a_state_past_the_size_limit_keeps_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let small = state();
        small.save(dir.path()).unwrap();
        let mut big = state();
        big.last_build.as_mut().unwrap().communities[0].members =
            (0..100).map(|i| format!("<urn:e:{i:05}>")).collect();
        let e = big.save_within(dir.path(), 512).unwrap_err();
        assert!(matches!(e, StateError::Bound { what: "bytes", .. }), "{e}");
        assert_eq!(BuildState::load(dir.path()).unwrap(), small);
        assert!(!dir.path().join("graphrag/state.json.tmp").exists());
    }
}
