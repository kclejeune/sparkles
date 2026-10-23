//! The daily token ledger and the accounting anomalies of a dataset (spec C22 §3.5),
//! kept in `<db>/graphrag/ledger.json` so that a restart does not reset them.
//!
//! Every build and refresh of a dataset in this process shares one [`DatasetLedger`],
//! found by the dataset's directory, so their reservations come out of the same day. A
//! reservation is written durably before its request can be sent, and a reservation
//! that cannot be written is refused. The file carries a format version, and a reader
//! refuses a version it does not know rather than starting from an empty ledger.

use super::budget::{Anomaly, DayLedger, Persist};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

/// The ledger format this server reads and writes.
pub const LEDGER_VERSION: u32 = 1;
/// The anomalies kept, the latest last.
pub const MAX_ANOMALIES: usize = 32;
/// The largest ledger file read, in bytes.
pub const MAX_LEDGER_BYTES: u64 = 1 << 20;

/// The file of a database directory.
pub fn path_in(db: &Path) -> PathBuf {
    db.join("graphrag").join("ledger.json")
}

/// `graphrag/ledger.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LedgerFile {
    pub version: u32,
    #[serde(default)]
    pub day: DayLedger,
    #[serde(default)]
    pub anomalies: Vec<Anomaly>,
}

impl Default for LedgerFile {
    fn default() -> LedgerFile {
        LedgerFile {
            version: LEDGER_VERSION,
            day: DayLedger::default(),
            anomalies: Vec::new(),
        }
    }
}

impl LedgerFile {
    /// Parse a ledger, refusing an unknown version before any other member.
    pub fn parse(bytes: &[u8]) -> Result<LedgerFile, String> {
        #[derive(Deserialize)]
        struct Probe {
            version: Option<serde_json::Value>,
        }
        let p: Probe = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        match p.version.as_ref().and_then(|v| v.as_u64()) {
            Some(v) if v == u64::from(LEDGER_VERSION) => {}
            Some(v) => {
                return Err(format!(
                    "the token ledger has format version {v}, and this server reads version {LEDGER_VERSION}"
                ));
            }
            None => return Err("the token ledger has no version".into()),
        }
        let f: LedgerFile = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if f.anomalies.len() > MAX_ANOMALIES {
            return Err(format!(
                "the token ledger holds more than {MAX_ANOMALIES} anomalies"
            ));
        }
        Ok(f)
    }

    /// The ledger of a database directory, empty when there is none.
    pub fn load(db: &Path) -> Result<LedgerFile, String> {
        let path = path_in(db);
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_LEDGER_BYTES => Err("the token ledger is too large".into()),
            Ok(_) => LedgerFile::parse(&std::fs::read(&path).map_err(|e| e.to_string())?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LedgerFile::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Write the ledger durably: a new file, a flush and a rename.
    pub fn save(&self, db: &Path) -> std::io::Result<()> {
        let path = path_in(db);
        let dir = path.parent().expect("the ledger has a directory");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("ledger.json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(self).expect("serializable"))?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    }

    /// Record an anomaly, keeping the latest [`MAX_ANOMALIES`].
    pub fn record(&mut self, a: Anomaly) {
        self.anomalies.push(a);
        let over = self.anomalies.len().saturating_sub(MAX_ANOMALIES);
        self.anomalies.drain(..over);
    }
}

/// Called with each new anomaly, for its inbox notice and metric.
pub type OnAnomaly = Box<dyn Fn(&Anomaly) + Send + Sync>;

/// The ledger a dataset's builds share in this process.
pub struct DatasetLedger {
    db: PathBuf,
    /// the daily ledger the builds' [`super::budget::Ledger`]s reserve from
    pub day: Arc<Mutex<DayLedger>>,
    file: Mutex<LedgerFile>,
    on_anomaly: Mutex<Option<OnAnomaly>>,
}

static OPEN: LazyLock<Mutex<HashMap<PathBuf, Weak<DatasetLedger>>>> =
    LazyLock::new(Default::default);

impl DatasetLedger {
    /// The ledger of a database directory, shared with every other build of it in this
    /// process. A ledger file of an unknown version or that cannot be read refuses.
    pub fn open(db: &Path) -> Result<Arc<DatasetLedger>, String> {
        let key = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
        let mut open = OPEN.lock();
        if let Some(l) = open.get(&key).and_then(Weak::upgrade) {
            return Ok(l);
        }
        let file = LedgerFile::load(db)?;
        let l = Arc::new(DatasetLedger {
            db: db.to_path_buf(),
            day: Arc::new(Mutex::new(file.day)),
            file: Mutex::new(file),
            on_anomaly: Mutex::new(None),
        });
        open.retain(|_, w| w.strong_count() > 0);
        open.insert(key, Arc::downgrade(&l));
        Ok(l)
    }

    /// Set what an anomaly raises besides its record.
    pub fn on_anomaly(&self, f: OnAnomaly) {
        *self.on_anomaly.lock() = Some(f);
    }

    /// The anomalies recorded, the latest last.
    pub fn anomalies(&self) -> Vec<Anomaly> {
        self.file.lock().anomalies.clone()
    }
}

impl Persist for DatasetLedger {
    fn day(&self, d: &DayLedger) -> Result<(), String> {
        let mut f = self.file.lock();
        let before = f.day;
        f.day = *d;
        f.save(&self.db).map_err(|e| {
            f.day = before;
            e.to_string()
        })
    }

    fn anomaly(&self, a: &Anomaly) {
        {
            let mut f = self.file.lock();
            f.record(a.clone());
            if let Err(e) = f.save(&self.db) {
                tracing::warn!(target: "sparkles::graphrag", "the token ledger cannot record an anomaly: {e}");
            }
        }
        if let Some(cb) = &*self.on_anomaly.lock() {
            cb(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphrag::budget::{Ledger, Source};

    fn anomaly(i: u64) -> Anomaly {
        Anomaly {
            provider: "p".into(),
            model: "m".into(),
            source: Source::ByteBound,
            reserved: i,
            reported: i + 1,
        }
    }

    #[test]
    fn reservations_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let dl = DatasetLedger::open(dir.path()).unwrap();
            let l = Ledger::new(1_000, 100, dl.day.clone(), || 9).persisted(dl.clone());
            l.reserve_for_test(60).unwrap();
        }
        // a new process reads the same day
        OPEN.lock().clear();
        let dl = DatasetLedger::open(dir.path()).unwrap();
        assert_eq!(*dl.day.lock(), DayLedger { day: 9, used: 60 });
        let l = Ledger::new(1_000, 100, dl.day.clone(), || 9).persisted(dl.clone());
        assert!(
            l.reserve_for_test(60).is_err(),
            "the day's budget carried over"
        );
        assert!(l.reserve_for_test(40).is_ok());
    }

    #[test]
    fn builds_of_one_dataset_share_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let a = DatasetLedger::open(dir.path()).unwrap();
        let b = DatasetLedger::open(dir.path()).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn an_unwritable_ledger_refuses_the_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let dl = DatasetLedger::open(dir.path()).unwrap();
        // a file where the directory should be makes every write fail
        std::fs::write(dir.path().join("graphrag"), b"x").unwrap();
        let l = Ledger::new(1_000, 1_000, dl.day.clone(), || 3).persisted(dl.clone());
        let e = l.reserve_for_test(10).unwrap_err();
        assert!(e.contains("ledger"), "{e}");
        assert_eq!(dl.day.lock().used, 0, "nothing stays reserved");
        assert_eq!(l.build_used(), 0);
    }

    #[test]
    fn unknown_versions_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("graphrag")).unwrap();
        std::fs::write(path_in(dir.path()), br#"{"version": 2}"#).unwrap();
        let e = DatasetLedger::open(dir.path()).err().unwrap();
        assert!(e.contains("version 2"), "{e}");
    }

    #[test]
    fn anomalies_are_recorded_and_raised() {
        let dir = tempfile::tempdir().unwrap();
        let dl = DatasetLedger::open(dir.path()).unwrap();
        let raised = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = raised.clone();
        dl.on_anomaly(Box::new(move |_| {
            r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        for i in 0..(MAX_ANOMALIES as u64 + 3) {
            dl.anomaly(&anomaly(i));
        }
        assert_eq!(
            raised.load(std::sync::atomic::Ordering::SeqCst),
            MAX_ANOMALIES + 3
        );
        assert_eq!(dl.anomalies().len(), MAX_ANOMALIES);
        let back = LedgerFile::load(dir.path()).unwrap();
        assert_eq!(back.anomalies.len(), MAX_ANOMALIES);
        assert_eq!(
            back.anomalies.last().unwrap().reserved,
            MAX_ANOMALIES as u64 + 2
        );
    }
}
