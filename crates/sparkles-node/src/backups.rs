use super::*;
use sparkles::backup::{self, Repository};
#[napi]
pub struct NativeRepository {
    pub(crate) repo: Mutex<Option<Arc<Repository>>>,
}
impl NativeRepository {
    pub(crate) fn get(&self) -> napi::Result<Arc<Repository>> {
        self.repo
            .lock()
            .clone()
            .ok_or_else(|| invalid("repository is closed"))
    }
}
#[napi]
impl NativeRepository {
    #[napi(factory)]
    pub async fn open(config: String, init: bool) -> napi::Result<Self> {
        let cfg = serde_json::from_str(&config).map_err(|e| invalid(e.to_string()))?;
        let repo = off_runtime(move || {
            backup::open(
                &cfg,
                &backup::OpenEnv {
                    init,
                    ..Default::default()
                },
            )
        })
        .await?;
        Ok(Self {
            repo: Mutex::new(Some(Arc::new(repo))),
        })
    }
    #[napi]
    pub async fn admin(
        &self,
        op: String,
        args: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        let repo = self.get()?;
        let a = parse(&args)?;
        let flag = cancel.flag.clone();
        off_runtime(move||{let ctl=sparkles::task::Control::with_cancel(flag);ctl.check()?;let b=backup::blocking(&repo);let v=match op.as_str(){"list"=>serde_json::to_value(b.list(&backup::ListFilter{dataset:a["dataset"].as_str().map(String::from),limit:a["limit"].as_u64().map(|n|n as usize),before:a["before"].as_str().map(String::from),..Default::default()})?),"stats"=>serde_json::to_value(b.stats()?),"test"=>serde_json::to_value(b.test()?),"locks"=>serde_json::to_value(b.locks()?),"breakLock"=>serde_json::to_value(b.break_lock(a["id"].as_str().ok_or_else(||EngineError::invalid("id must be a string"))?)?),"gc"=>serde_json::to_value(b.gc(&backup::GcOptions{dry_run:a["dryRun"].as_bool().unwrap_or(true),ctl:(&ctl).into(),..Default::default()})?),"verify"=>{let names:Vec<String>=serde_json::from_value(a["names"].clone()).map_err(|e|EngineError::invalid(e.to_string()))?;serde_json::to_value(b.verify(&names,&backup::VerifyOptions{ctl:(&ctl).into(),..Default::default()})?)},"restoreToDir"=>{let r=b.restore_to_dir(a["name"].as_str().ok_or_else(||EngineError::invalid("name must be a string"))?,Path::new(a["path"].as_str().ok_or_else(||EngineError::invalid("path must be a string"))?),&backup::RestoreOptions{ctl:(&ctl).into(),..Default::default()})?;Ok(json!({"backup":r.backup,"datasetId":r.dataset_id,"identity":r.identity,"forkedFrom":r.forked_from,"check":r.check,"millis":r.millis}))},_=>return Err(EngineError::unsupported("unknown repository operation"))}.map_err(|e|EngineError::invalid(e.to_string()))?;Ok(lossless(v).to_string())}).await
    }
    #[napi]
    pub fn close(&self) {
        self.repo.lock().take();
    }
}
#[napi]
impl NativeDataset {
    #[napi]
    pub async fn backups(
        &self,
        repo: &NativeRepository,
        op: String,
        args: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        let write = !matches!(op.as_str(), "list" | "get" | "verify");
        let shared = self.get(write)?;
        let repo = repo.get()?;
        let a = parse(&args)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let _permit = if write {
            Some(permit(shared.writers.clone(), &v, &flag).await?)
        } else {
            None
        };
        off_runtime(move || {
            let ctl = sparkles::task::Control {
                deadline: write_options(&v, flag.clone())?.deadline,
                ..sparkles::task::Control::with_cancel(flag)
            };
            ctl.check()?;
            let b = shared.ds.backups(&repo);
            let name = || {
                a["name"]
                    .as_str()
                    .ok_or_else(|| EngineError::invalid("name must be a string"))
            };
            let value = match op.as_str() {
                "create" => serde_json::to_value(b.create_with(
                    &backup::CreateOptions {
                        name: name()?.into(),
                        note: a["note"].as_str().map(String::from),
                        dataset_name: a["datasetName"].as_str().unwrap_or("dataset").into(),
                        ..Default::default()
                    },
                    &ctl,
                )?),
                "list" => serde_json::to_value(b.list(&Default::default())?),
                "get" => serde_json::to_value(b.get(name()?)?),
                "delete" => serde_json::to_value(b.delete(name()?)?),
                "verify" => {
                    serde_json::to_value(b.verify_with(name()?, &Default::default(), &ctl)?)
                }
                _ => return Err(EngineError::unsupported("unknown backup operation")),
            }
            .map_err(|e| EngineError::invalid(e.to_string()))?;
            Ok(lossless(value).to_string())
        })
        .await
    }
}
