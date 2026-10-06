//! Catalog handles own only Rust state; JavaScript wrappers remain environment-local.
use super::*;
use napi::{Env, bindgen_prelude::PromiseRaw};
use sparkles::catalog::{CreateDataset, DatasetInfo, DatasetKind};
struct CatalogState {
    catalog: sparkles::Catalog,
    options: Value,
}
static CATALOGS: LazyLock<Mutex<HashMap<PathBuf, Weak<CatalogState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
fn info(i: DatasetInfo) -> Value {
    json!({"name":i.name,"id":i.id,"kind":i.kind,"path":i.path,"attached":i.attached,"reservedBy":i.reserved_by})
}
pub(crate) fn wrap(ds: Dataset, options: Value, read_only: bool) -> NativeDataset {
    let key = ds.store() as *const _ as usize;
    let mut map = admin::BRANCHES.lock();
    map.retain(|_, v| v.strong_count() > 0);
    let shared = map.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
        let s = Arc::new(Shared {
            ds,
            writers: Arc::new(Semaphore::new(1)),
            options,
        });
        map.insert(key, Arc::downgrade(&s));
        s
    });
    drop(map);
    if let Some(root) = shared.ds.store().root()
        && let Ok(path) = std::fs::canonicalize(root)
    {
        STORES.lock().insert(path, Arc::downgrade(&shared));
    }
    NativeDataset {
        shared: Mutex::new(Some(shared)),
        read_only,
    }
}
pub(crate) fn shared_for_path(
    path: &Path,
    options: &Value,
) -> sparkles::Result<Option<Arc<Shared>>> {
    let catalogs = CATALOGS.lock();
    for state in catalogs.values().filter_map(Weak::upgrade) {
        for info in state.catalog.list() {
            if info
                .path
                .as_ref()
                .is_some_and(|p| std::fs::canonicalize(p).is_ok_and(|p| p == path))
            {
                if state.options["unionDefaultGraph"] != options["unionDefaultGraph"]
                    || state.options["cacheBytes"] != options["cacheBytes"]
                {
                    return Err(EngineError::invalid(
                        "dataset open options differ from existing catalog",
                    ));
                }
                if let Some(ds) = state.catalog.get(&info.name) {
                    let key = ds.store() as *const _ as usize;
                    let mut branches = admin::BRANCHES.lock();
                    if let Some(s) = branches.get(&key).and_then(Weak::upgrade) {
                        return Ok(Some(s));
                    }
                    let s = Arc::new(Shared {
                        ds,
                        writers: Arc::new(Semaphore::new(1)),
                        options: state.options.clone(),
                    });
                    branches.insert(key, Arc::downgrade(&s));
                    return Ok(Some(s));
                }
            }
        }
    }
    Ok(None)
}
#[napi]
pub struct NativeCatalog {
    state: Mutex<Option<Arc<CatalogState>>>,
    read_only: bool,
}
#[napi]
pub struct NativeReservation {
    reservation: Mutex<Option<sparkles::catalog::Reservation>>,
}
#[napi]
impl NativeReservation {
    #[napi]
    pub fn close(&self) {
        self.reservation.lock().take();
    }
}
impl NativeCatalog {
    fn get(&self, write: bool) -> napi::Result<Arc<CatalogState>> {
        if write && self.read_only {
            return Err(err(EngineError::NotPermitted(
                "catalog is read-only".into(),
            )));
        }
        self.state
            .lock()
            .clone()
            .ok_or_else(|| invalid("catalog is closed"))
    }
}
#[napi]
impl NativeCatalog {
    #[napi]
    pub fn reserve(
        &self,
        name: String,
        kind: String,
        holder: String,
    ) -> napi::Result<NativeReservation> {
        let state = self.get(true)?;
        let kind = match kind.as_str() {
            "clone" => sparkles::catalog::ReservationKind::Clone,
            "restore" => sparkles::catalog::ReservationKind::Restore,
            _ => return Err(invalid("reservation kind must be clone or restore")),
        };
        Ok(NativeReservation {
            reservation: Mutex::new(Some(
                state.catalog.reserve(&name, kind, &holder).map_err(err)?,
            )),
        })
    }
    #[napi]
    pub async fn inspect(path: String) -> napi::Result<String> {
        blocking(move || {
            Ok(Value::Array(
                sparkles::Catalog::inspect(path)?
                    .into_iter()
                    .map(info)
                    .collect(),
            )
            .to_string())
        })
        .await
    }
    #[napi(factory)]
    pub fn memory(options: String) -> napi::Result<Self> {
        let v = normalize_store_options(parse(&options)?);
        Ok(Self {
            read_only: v["readOnly"].as_bool().unwrap_or(false),
            state: Mutex::new(Some(Arc::new(CatalogState {
                catalog: sparkles::Catalog::memory({
                    let mut o = sparkles::store::StoreOptions::default();
                    o.union_default_graph = v["unionDefaultGraph"].as_bool().unwrap_or(false);
                    o.cache_bytes = v["cacheBytes"].as_u64().unwrap_or(o.cache_bytes);
                    o.into()
                }),
                options: v,
            }))),
        })
    }
    #[napi(factory)]
    pub async fn open(path: String, options: String) -> napi::Result<Self> {
        let v = normalize_store_options(parse(&options)?);
        let read_only = v["readOnly"].as_bool().unwrap_or(false);
        let state = blocking(move || {
            std::fs::create_dir_all(&path)?;
            let path = std::fs::canonicalize(path)?;
            let mut map = CATALOGS.lock();
            map.retain(|_, v| v.strong_count() > 0);
            if let Some(s) = map.get(&path).and_then(Weak::upgrade) {
                if s.options["unionDefaultGraph"] != v["unionDefaultGraph"]
                    || s.options["cacheBytes"] != v["cacheBytes"]
                {
                    return Err(EngineError::invalid(
                        "catalog open options differ from existing handle",
                    ));
                }
                return Ok(s);
            }
            let mut opts = sparkles::store::StoreOptions::default();
            opts.union_default_graph = v["unionDefaultGraph"].as_bool().unwrap_or(false);
            opts.cache_bytes = v["cacheBytes"].as_u64().unwrap_or(opts.cache_bytes);
            let s = Arc::new(CatalogState {
                catalog: sparkles::Catalog::open(&path, opts.into())?,
                options: v,
            });
            map.insert(path, Arc::downgrade(&s));
            Ok(s)
        })
        .await?;
        Ok(Self {
            state: Mutex::new(Some(state)),
            read_only,
        })
    }
    #[napi]
    pub fn clone_dataset<'env>(
        &self,
        env: &'env Env,
        source: String,
        name: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<PromiseRaw<'env, NativeDataset>> {
        let state = self.get(true)?;
        let ds = state
            .catalog
            .get(&source)
            .ok_or_else(|| invalid("source dataset not found"))?;
        let shared = wrap(ds, state.options.clone(), self.read_only).get(false)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let read_only = self.read_only;
        let opts = state.options.clone();
        env.spawn_future(async move {
            let _permit = permit(shared.writers.clone(), &v, &flag).await?;
            let ds = blocking(move || {
                let ctl = sparkles::task::Control {
                    deadline: write_options(&v, flag.clone())?.deadline,
                    ..sparkles::task::Control::with_cancel(flag)
                };
                ctl.check()?;
                state.catalog.clone_dataset(
                    &source,
                    &name,
                    &sparkles::catalog::CloneRequest::default(),
                    &ctl,
                )
            })
            .await?;
            Ok(wrap(ds, opts, read_only))
        })
    }
    #[napi]
    pub async fn dataset(&self, op: String, args: String) -> napi::Result<Option<NativeDataset>> {
        let write = !matches!(op.as_str(), "get" | "getById");
        let state = self.get(write)?;
        let a = parse(&args)?;
        let name = a["name"]
            .as_str()
            .ok_or_else(|| invalid("name must be a string"))?
            .to_owned();
        let read_only = self.read_only;
        let options = state.options.clone();
        let ds = blocking(move || {
            Ok(match op.as_str() {
                "get" => state.catalog.get(&name),
                "getById" => state.catalog.get_by_id(
                    name.parse()
                        .map_err(|e| EngineError::invalid(format!("{e}")))?,
                ),
                "create" => Some(state.catalog.create(
                    &name,
                    &CreateDataset {
                        kind: if a["kind"].as_str() == Some("mem") {
                            DatasetKind::Memory
                        } else {
                            DatasetKind::Persistent
                        },
                        geo: None,
                    },
                )?),
                "rename" => Some(
                    state.catalog.rename(
                        &name,
                        a["to"]
                            .as_str()
                            .ok_or_else(|| EngineError::invalid("to must be a string"))?,
                    )?,
                ),
                "attach" => Some(state.catalog.attach(
                    &name,
                    if let Some(path) = a["path"].as_str() {
                        sparkles::catalog::Attach::Directory(path.into())
                    } else {
                        sparkles::catalog::Attach::Memory
                    },
                )?),
                _ => return Err(EngineError::unsupported("unknown catalog operation")),
            })
        })
        .await?;
        Ok(ds.map(|ds| wrap(ds, options, read_only)))
    }
    #[napi]
    pub async fn admin(&self, op: String, args: String) -> napi::Result<String> {
        let state = self.get(!matches!(
            op.as_str(),
            "list" | "info" | "backupFiles" | "repositories.list" | "repositories.get"
        ))?;
        let a = parse(&args)?;
        blocking(move || {
            let name = || {
                a["name"]
                    .as_str()
                    .ok_or_else(|| EngineError::invalid("name must be a string"))
            };
            let v = match op.as_str() {
                "list" => Value::Array(state.catalog.list().into_iter().map(info).collect()),
                "info" => state.catalog.info(name()?).map(info).unwrap_or(Value::Null),
                "delete" => json!(state.catalog.delete(name()?)?),
                "backupFiles" => json!(
                    state
                        .catalog
                        .backup_files()?
                        .into_iter()
                        .map(|f| json!({"name":f.name,"path":f.path}))
                        .collect::<Vec<_>>()
                ),
                #[cfg(feature = "backup")]
                "repositories.list" => serde_json::to_value(state.catalog.repositories()?.list())
                    .map_err(|e| EngineError::invalid(e.to_string()))?,
                #[cfg(feature = "backup")]
                "repositories.get" => {
                    serde_json::to_value(state.catalog.repositories()?.get(name()?)?)
                        .map_err(|e| EngineError::invalid(e.to_string()))?
                }
                #[cfg(feature = "backup")]
                "repositories.add" => serde_json::to_value(
                    state.catalog.repositories()?.add(
                        serde_json::from_value(a["config"].clone())
                            .map_err(|e| EngineError::invalid(e.to_string()))?,
                    )?,
                )
                .map_err(|e| EngineError::invalid(e.to_string()))?,
                #[cfg(feature = "backup")]
                "repositories.update" => serde_json::to_value(
                    state.catalog.repositories()?.update(
                        name()?,
                        serde_json::from_value(a["config"].clone())
                            .map_err(|e| EngineError::invalid(e.to_string()))?,
                    )?,
                )
                .map_err(|e| EngineError::invalid(e.to_string()))?,
                #[cfg(feature = "backup")]
                "repositories.remove" => json!(state.catalog.repositories()?.remove(name()?)?),
                #[cfg(feature = "backup")]
                "repositories.withFixed" => {
                    let entries: Vec<sparkles::backup::RepoConfig> =
                        serde_json::from_value(a["entries"].clone())
                            .map_err(|e| EngineError::invalid(e.to_string()))?;
                    serde_json::to_value(state.catalog.repositories()?.with_fixed(&entries)?.list())
                        .map_err(|e| EngineError::invalid(e.to_string()))?
                }
                _ => return Err(EngineError::unsupported("unknown catalog operation")),
            };
            Ok(v.to_string())
        })
        .await
    }
    #[napi]
    pub fn close(&self) {
        self.state.lock().take();
    }
}

#[cfg(feature = "backup")]
#[napi]
impl NativeCatalog {
    #[napi]
    pub fn run_policy<'env>(
        &self,
        env: &'env Env,
        policy: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<PromiseRaw<'env, String>> {
        let state = self.get(true)?;
        let policy: sparkles::backup::PolicyConfig =
            serde_json::from_str(&policy).map_err(|e| invalid(e.to_string()))?;
        let v = parse(&options)?;
        if !v["dryRun"].is_null() || !v["ifHead"].is_null() {
            return Err(err(EngineError::unsupported(
                "policy runs do not support dryRun or ifHead",
            )));
        }
        let flag = cancel.flag.clone();
        // A capture takes core writer locks. Share Node's admission order with all
        // dataset wrappers, and use one stable order across concurrent policy runs.
        let mut shared = std::collections::BTreeMap::new();
        for info in state.catalog.list() {
            if !policy
                .datasets
                .iter()
                .any(|p| sparkles::backup::policy::matches_dataset(p, &info.name))
            {
                continue;
            }
            if let Some(ds) = state.catalog.get(&info.name) {
                let key = ds.dataset_id().to_string();
                shared
                    .entry(key)
                    .or_insert(wrap(ds, state.options.clone(), self.read_only).get(false)?);
            }
        }
        env.spawn_future(async move {
            let shared = shared.into_values().collect::<Vec<_>>();
            let mut permits = Vec::new();
            for item in &shared {
                permits.push(permit(item.writers.clone(), &v, &flag).await?);
            }
            let result = off_runtime(move || {
                let ctl = sparkles::task::Control {
                    deadline: write_options(&v, flag.clone())?.deadline,
                    ..sparkles::task::Control::with_cancel(flag)
                };
                Ok(lossless(
                    serde_json::to_value(state.catalog.run_policy(&policy, &ctl)?)
                        .map_err(|e| EngineError::invalid(e.to_string()))?,
                )
                .to_string())
            })
            .await;
            drop(permits);
            drop(shared);
            result
        })
    }
    #[napi]
    pub async fn apply_retention(&self, policy: String, dry_run: bool) -> napi::Result<String> {
        let state = self.get(!dry_run)?;
        let policy: sparkles::backup::PolicyConfig =
            serde_json::from_str(&policy).map_err(|e| invalid(e.to_string()))?;
        off_runtime(move || {
            Ok(lossless(
                serde_json::to_value(state.catalog.apply_retention(&policy, dry_run)?)
                    .map_err(|e| EngineError::invalid(e.to_string()))?,
            )
            .to_string())
        })
        .await
    }
    #[napi]
    pub async fn repository(&self, name: String) -> napi::Result<crate::backups::NativeRepository> {
        let state = self.get(false)?;
        let repo = off_runtime(move || state.catalog.repositories()?.open(&name)).await?;
        Ok(crate::backups::NativeRepository {
            repo: Mutex::new(Some(repo)),
        })
    }
    #[napi]
    pub async fn restore(
        &self,
        repo: &crate::backups::NativeRepository,
        name: String,
        target: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeDataset> {
        let state = self.get(true)?;
        let repo = repo.get()?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let options = state.options.clone();
        let read_only = self.read_only;
        let ds = off_runtime(move || {
            state.catalog.restore(
                &repo,
                &name,
                &sparkles::backup::RestoreRequest {
                    target: Some(target),
                    ..Default::default()
                },
                &sparkles::task::Control {
                    deadline: write_options(&v, flag.clone())?.deadline,
                    ..sparkles::task::Control::with_cancel(flag)
                },
            )
        })
        .await?;
        Ok(wrap(ds, options, read_only))
    }
}
