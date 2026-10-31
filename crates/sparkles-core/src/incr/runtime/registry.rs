//! The views a runtime knows, as they load from `view.json` and as backups capture
//! them.

use super::*;

pub(super) struct ViewEntry {
    pub(super) def: ViewDef,
    pub(super) cell: Option<Arc<Cell>>,
    pub(super) access: Option<Arc<GraphAccess>>,
    /// suspended by its budget until a rebuild
    pub(super) suspended: Option<String>,
    /// why the view could not be attached (its query, its principal)
    pub(super) error: Option<String>,
    pub(super) checkpointed: u64,
    /// restored or cloned and not resumed: its `view.json` is the paused format
    pub(super) paused: bool,
}

#[derive(Default)]
pub(super) struct Registry {
    pub(super) cells: FxHashMap<u64, Arc<Cell>>,
    /// the dataset's inference that every circuit is compiled for
    pub(super) inference: Option<Arc<Inference>>,
    pub(super) views: BTreeMap<String, ViewEntry>,
    /// why the definitions could not be read; views cannot be changed until it is fixed
    pub(super) broken: Option<String>,
}

/// The definitions a capture records ([`DefinitionSource`]).
#[derive(Default)]
pub(super) struct Defs {
    /// why the definitions on disk could not be read: a capture is refused
    pub(super) broken: Option<String>,
    pub(super) files: Arc<[DefinitionFile]>,
}

/// Why a restored or cloned view waits ([`definitions::Paused`]), as its suspension.
fn paused_reason(p: &definitions::Paused) -> String {
    p.reason.clone()
}

/// Why definition `def` does not run in this release, as `{code}: {message}`: an
/// option a later phase adds, such as a graph view or maintenance at commit
/// (`unsupported-option`), or a definition that is not valid (`invalid-view`). `put`
/// refuses such a definition, and a `view.json` on disk that holds one, written by
/// hand or by another release at the same format, is marked failed instead of running
/// as a table view.
pub(super) fn unsupported(def: &ViewDef) -> Option<String> {
    def.check().err().map(|e| format!("{}: {e}", e.code()))
}

impl ViewEntry {
    /// The entry of definition `def`, paused when it was restored or cloned and not
    /// resumed. Every entry is checked here, so a definition this release does not run
    /// is failed from the start, whether it comes from `put` or from disk.
    pub(super) fn new(name: &str, def: ViewDef, paused: Option<&definitions::Paused>) -> Self {
        let error = unsupported(&def);
        if let Some(e) = &error {
            tracing::warn!(
                target: "sparkles::incr::runtime",
                "view {name}: {e}; it does not run until it is defined again"
            );
        }
        ViewEntry {
            def,
            cell: None,
            access: None,
            // a restored or cloned view waits until it is resumed
            suspended: paused.map(paused_reason),
            error,
            checkpointed: 0,
            paused: paused.is_some(),
        }
    }
}

impl Registry {
    /// The views defined under `root` (`None`: in memory, none). Definitions that
    /// cannot be read mark the registry broken, and the views cannot be changed until
    /// they are fixed.
    pub(super) fn load(root: Option<&std::path::Path>) -> Registry {
        let mut reg = Registry::default();
        let Some(r) = root else {
            return reg;
        };
        match views::read_all(r) {
            Ok(defs) => {
                for (name, def, paused) in defs {
                    let entry = ViewEntry::new(&name, def, paused.as_ref());
                    reg.views.insert(name, entry);
                }
            }
            Err(e) => {
                tracing::error!(
                    target: "sparkles::incr::runtime",
                    "views of {}: {e}; they cannot be changed until it is fixed",
                    r.display()
                );
                reg.broken = Some(e.to_string());
            }
        }
        reg
    }

    /// View `name`, or `view-not-found`.
    pub(super) fn view(&self, name: &str) -> Result<&ViewEntry> {
        self.views.get(name).ok_or_else(|| not_found(name))
    }

    /// Refuse a change while the definitions on disk cannot be read.
    pub(super) fn writable(&self) -> Result<()> {
        match &self.broken {
            Some(e) => Err(Error::Unsupported(format!(
                "the views cannot be changed until their definitions are fixed: {e}"
            ))),
            None => Ok(()),
        }
    }

    /// The definition files of the views, as a capture records them.
    pub(super) fn defs(&self) -> Defs {
        let mut files = Vec::with_capacity(self.views.len());
        for (name, e) in &self.views {
            match views::to_bytes(&e.def, None) {
                Ok(bytes) => files.push(DefinitionFile {
                    kind: "view",
                    path: views_path(name),
                    format: views::FORMAT,
                    bytes: bytes.into(),
                }),
                Err(err) => {
                    return Defs {
                        broken: Some(format!("view {name:?}: {err}")),
                        files: Arc::from([]),
                    };
                }
            }
        }
        Defs {
            broken: self.broken.clone(),
            files: files.into(),
        }
    }
}

/// The error of a view that is not defined.
pub(super) fn not_found(name: &str) -> Error {
    crate::incr::error("view-not-found", format!("no view {name:?}"))
}

/// `views/NAME/view.json`
fn views_path(name: &str) -> String {
    format!("{}/{name}/{}", views::DIR, views::FILE)
}

impl DefinitionSource for Runtime {
    fn kind(&self) -> &'static str {
        "view"
    }

    fn capture_definitions(&self) -> Result<Vec<DefinitionFile>> {
        let d = self.defs.read();
        if let Some(why) = &d.broken {
            return Err(Error::Unsupported(format!(
                "the view definitions cannot be captured until they are fixed: {why}"
            )));
        }
        Ok(d.files.to_vec())
    }
}
