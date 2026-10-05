//! Property-handle administration. All engine work stays off the JavaScript thread.
use super::*;
use serde::Serialize;
use sparkles::history::{At, SnapshotOptions};
fn value<T: Serialize>(v: T) -> sparkles::Result<Value> {
    serde_json::to_value(v).map_err(|e| EngineError::invalid(e.to_string()))
}
fn snapshot(s: sparkles::history::NamedSnapshot) -> Value {
    json!({"name":s.name,"seq":s.seq,"commit":s.commit,"createdMs":s.created_ms,"note":s.note,"expiresMs":s.expires_ms,"generation":s.generation,"reconstructable":s.reconstructable,"warm":s.warm})
}
fn history(h: sparkles::history::HistoryStatus) -> Value {
    json!({"head":h.head,"reconstructable":h.reconstructable,"generations":h.generations.iter().map(|g|json!({"name":g.name,"baseSeq":g.base_seq,"endSeq":g.end_seq,"bytes":g.bytes,"current":g.current,"heldBy":g.held_by.iter().map(ToString::to_string).collect::<Vec<_>>()})).collect::<Vec<_>>(),"bytes":h.bytes,"retention":h.retention,"snapshots":h.snapshots,"catalog":h.catalog,"firstCommit":h.first_commit,"cacheEntries":h.cache_entries,"cacheBytes":h.cache_bytes,"hits":h.hits,"misses":h.misses,"materializations":h.materializations,"materializeSeconds":h.materialize_seconds})
}
#[cfg(any(feature = "shacl", feature = "shex"))]
fn guard_outcome(o: sparkles::handles::GuardOutcome) -> sparkles::Result<Value> {
    use sparkles::handles::GuardOutcome::*;
    match o {
        Installed(s) => Ok(json!({"installed":true,"validation":s})),
        NotConforming(s) => Ok(json!({"installed":false,"validation":s})),
        Removed => Ok(json!({"installed":false,"removed":true})),
        _ => Err(EngineError::unsupported("unknown guard outcome")),
    }
}
fn merge_options(
    a: &Value,
    ctl: &sparkles::task::Control,
) -> sparkles::Result<sparkles::branch::MergeOptions> {
    Ok(sparkles::branch::MergeOptions {
        scope: match a["scope"].as_str().unwrap_or("predicate") {
            "quad" => sparkles::branch::ConflictScope::Quad,
            "predicate" | "cell" => sparkles::branch::ConflictScope::Cell,
            "subject" => sparkles::branch::ConflictScope::Subject,
            _ => return Err(EngineError::invalid("unknown conflict scope")),
        },
        squash: a["squash"].as_bool().unwrap_or(false),
        write: sparkles::guard::WriteOptions {
            message: a["message"].as_str().map(Arc::from),
            cancel: Some(ctl.cancel.flag()),
            deadline: ctl.deadline,
            ..Default::default()
        },
        cancel: Some(ctl.cancel.flag()),
        deadline: ctl.deadline,
        ..Default::default()
    })
}
fn merge_result(o: sparkles::branch::MergeOutcome) -> sparkles::Result<Value> {
    match o {
        sparkles::branch::MergeOutcome::Merged(report) => {
            Ok(json!({"merged":true,"receipt":report.commit.as_ref().map(receipt),"report":report}))
        }
        sparkles::branch::MergeOutcome::UpToDate(report) => {
            Ok(json!({"merged":false,"upToDate":true,"report":report}))
        }
        sparkles::branch::MergeOutcome::Conflicts(report) => {
            Ok(json!({"merged":false,"conflicts":report}))
        }
    }
}
fn text<'a>(v: &'a Value, k: &str) -> sparkles::Result<&'a str> {
    v[k].as_str()
        .ok_or_else(|| EngineError::invalid(format!("{k} must be a string")))
}
fn number(v: &Value, k: &str) -> sparkles::Result<u64> {
    if let Some(n) = v[k].as_u64() {
        return Ok(n);
    }
    text(v, k)?
        .parse()
        .map_err(|_| EngineError::invalid(format!("{k} must be an unsigned integer")))
}
fn decode<T: serde::de::DeserializeOwned>(v: &Value) -> sparkles::Result<T> {
    serde_json::from_value(v.clone()).map_err(|e| EngineError::invalid(e.to_string()))
}
// Encode only engine uint64 metadata. Application JSON and typed config
// documents retain their lexical/value types across the transport.
fn meta(v: Value, key: &str) -> Value {
    if matches!(
        key,
        "definition" | "config" | "variables" | "data" | "default"
    ) {
        return v;
    }
    match v {
        Value::Number(ref n) if u64_field(key) => n
            .as_u64()
            .map(|n| Value::String(n.to_string()))
            .unwrap_or(v),
        Value::Array(a) => Value::Array(
            a.into_iter()
                .map(|v| meta(v, if key == "reconstructable" { "seq" } else { key }))
                .collect(),
        ),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| {
                    let v = meta(v, &k);
                    (k, v)
                })
                .collect(),
        ),
        _ => v,
    }
}
fn u64_field(k: &str) -> bool {
    matches!(
        k,
        "seq"
            | "parent"
            | "quads"
            | "inserted"
            | "deleted"
            | "head"
            | "after"
            | "next"
            | "added"
            | "removed"
            | "commit"
            | "firstCommit"
            | "baseSeq"
            | "endSeq"
            | "bytes"
            | "cacheBytes"
            | "hits"
            | "misses"
            | "materializations"
            | "maxBytes"
            | "usedBytes"
            | "inferred"
            | "minDeltaQuads"
            | "maxDeltaQuads"
            | "maxDeltaMb"
            | "maxWalMb"
            | "idleSeconds"
            | "maxAgeSeconds"
            | "minIntervalSeconds"
            | "keepCommits"
            | "keepAgeMs"
            | "segmentBytes"
            | "defaultMaxBytes"
            | "bulkMaxQuads"
            | "version"
            | "ifVersion"
    )
}
fn config_numbers(v: Value, key: &str) -> Value {
    match v {
        Value::String(ref s) if u64_field(key) => s
            .parse::<u64>()
            .map(|n| Value::Number(n.into()))
            .unwrap_or(v),
        Value::Array(a) => Value::Array(a.into_iter().map(|v| config_numbers(v, key)).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| {
                    let v = config_numbers(v, &k);
                    (k, v)
                })
                .collect(),
        ),
        _ => v,
    }
}
fn config<T: serde::de::DeserializeOwned>(v: &Value) -> sparkles::Result<T> {
    serde_json::from_value(config_numbers(v.clone(), ""))
        .map_err(|e| EngineError::invalid(e.to_string()))
}
fn writes(op: &str) -> bool {
    !matches!(
        op,
        "info"
            | "stats"
            | "explain"
            | "schema.report"
            | "schema.classes"
            | "schema.predicates"
            | "schema.diff"
            | "branches.commitGraph"
            | "indexes.geo.features"
            | "indexes.vector.recall"
            | "reasoning.diagnostics"
            | "history.query"
            | "schema.profiles"
            | "schema.draftShapes"
            | "schema.constraints"
            | "graphql.get"
            | "graphql.versions"
            | "graphql.sdl"
            | "graphql.draft"
            | "prefixes.list"
            | "snapshots.list"
            | "snapshots.get"
            | "history.status"
            | "history.diff"
            | "indexes.vector.list"
            | "indexes.vector.get"
            | "indexes.text.search"
            | "reasoning.rdfs.get"
            | "validation.shacl"
            | "validation.shex"
            | "history.commits"
            | "history.commit"
            | "history.changes"
            | "settings.describe.get"
            | "settings.compaction.get"
            | "settings.quota.get"
            | "settings.retention.get"
            | "settings.changeLog.get"
            | "queries.list"
            | "queries.get"
            | "queries.versions"
            | "branches.list"
            | "branches.get"
            | "indexes.text.status"
            | "indexes.geo.status"
            | "reasoning.status"
            | "validation.guard.get"
    )
}
#[napi]
impl NativeDataset {
    #[napi]
    pub fn context_identity(&self) -> napi::Result<String> {
        let s = self.get(false)?;
        Ok(format!(
            "{}:{}",
            s.ds.dataset_id(),
            s.ds.store().branch_id()
        ))
    }
    #[napi]
    pub async fn run_query(
        &self,
        name: String,
        params: String,
        version: Option<String>,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeResult> {
        let shared = self.get(false)?;
        let params = parse(&params)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let _permit = permit(READERS.clone(), &v, &flag).await?;
        let result = blocking(move || {
            shared.ds.queries().run_version(
                &name,
                version
                    .map(|n| {
                        n.parse()
                            .map_err(|_| EngineError::invalid("invalid query version"))
                    })
                    .transpose()?,
                &serde_json::from_value(params).map_err(|e| EngineError::invalid(e.to_string()))?,
                &query_options(&v, flag)?,
            )
        })
        .await?;
        Ok(NativeResult::query(result))
    }
    #[napi]
    pub async fn clone_memory(
        &self,
        name: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeDataset> {
        let shared = self.get(false)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let _permit = permit(shared.writers.clone(), &v, &flag).await?;
        let opts = shared.options.clone();
        let ds = blocking(move || {
            let ctl = sparkles::task::Control {
                deadline: write_options(&v, flag.clone())?.deadline,
                ..sparkles::task::Control::with_cancel(flag)
            };
            let c = shared.ds.clone_to_memory_with(
                &name,
                &sparkles::cloning::Spec {
                    at: v["at"].as_str().map(str::parse).transpose()?,
                    graphs: if v["graphs"].is_null() {
                        None
                    } else {
                        Some(decode(&v["graphs"])?)
                    },
                    ..Default::default()
                },
                &ctl,
            )?;
            let ds = sparkles::Dataset::from_store_with(
                c.store,
                sparkles::DatasetOptions {
                    name: Some(name),
                    origin: Some(c.origin),
                    ..Default::default()
                },
            );
            ds.state().set_reasoning(c.reasoning)?;
            Ok(ds)
        })
        .await?;
        Ok(NativeDataset {
            shared: Mutex::new(Some(Arc::new(Shared {
                ds,
                writers: Arc::new(Semaphore::new(1)),
                options: opts,
            }))),
            read_only: false,
        })
    }
    #[napi]
    pub async fn branch(&self, name: String) -> napi::Result<NativeDataset> {
        let s = self.get(false)?;
        let read_only = self.read_only;
        let branch = blocking(move || {
            let ds = s.ds.branch(&name)?;
            let key = ds.store() as *const _ as usize;
            let mut branches = BRANCHES.lock();
            branches.retain(|_, v| v.strong_count() > 0);
            if let Some(shared) = branches.get(&key).and_then(Weak::upgrade) {
                return Ok(shared);
            }
            let shared = Arc::new(Shared {
                ds,
                writers: Arc::new(Semaphore::new(1)),
                options: s.options.clone(),
            });
            branches.insert(key, Arc::downgrade(&shared));
            Ok(shared)
        })
        .await?;
        Ok(NativeDataset {
            shared: Mutex::new(Some(branch)),
            read_only,
        })
    }
    #[napi]
    pub async fn admin(
        &self,
        op: String,
        args: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        let write = writes(&op);
        let shared = self.get(write)?;
        let a = parse(&args)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let _permit = if write {
            Some(permit(shared.writers.clone(), &v, &flag).await?)
        } else {
            None
        };
        blocking(move||{let ctl=sparkles::task::Control{deadline:write_options(&v,flag.clone())?.deadline,..sparkles::task::Control::with_cancel(flag.clone())};ctl.check()?;let ds=&shared.ds;let result=match op.as_str(){
 "info"=>json!({"id":ds.dataset_id(),"branch":ds.store().branch_name(),"quads":ds.len().to_string(),"head":meta(value(ds.head_commit())?,"")}),
 "prefixes.list"=>value(ds.prefixes())?,"prefixes.set"=>{ds.set_prefix(text(&a,"prefix")?,text(&a,"iri")?)?;Value::Null},"prefixes.delete"=>value(ds.remove_prefix(text(&a,"prefix")?)?)?,
 "snapshots.list"=>Value::Array(ds.snapshots().list().into_iter().map(snapshot).collect()),"snapshots.get"=>ds.snapshots().get(text(&a,"name")?).map(snapshot).unwrap_or(Value::Null),"snapshots.create"=>{let (snapshot,created)=ds.snapshots().create(text(&a,"name")?,&a["at"].as_str().unwrap_or("head").parse::<At>()?,&SnapshotOptions{note:a["note"].as_str().map(String::from),expires_ms:a["expiresMs"].as_i64(),warm:a["warm"].as_bool().unwrap_or(false)})?;json!({"snapshot":self::snapshot(snapshot),"created":created})},"snapshots.delete"=>value(ds.snapshots().delete(text(&a,"name")?)?)?,
 "history.status"=>history(ds.history().status()),"history.commit"=>value(ds.history().commit(&text(&a,"reference")?.parse()?)?)?,"history.commits"=>{let range=if !a["before"].is_null(){sparkles::commit::CommitRange::Before(number(&a,"before")?)}else if !a["after"].is_null(){sparkles::commit::CommitRange::After(number(&a,"after")?)}else{sparkles::commit::CommitRange::Latest};value(ds.history().commits(range,a["limit"].as_u64().unwrap_or(100).min(10000) as usize))?},
 "history.changes"=>{let page=ds.history().changes(number(&a,"after")?,&sparkles::store::ChangesOptions{max_commits:a["maxCommits"].as_u64().unwrap_or(100).min(10000) as usize,max_quads:a["maxQuads"].as_u64().unwrap_or(10000),cancel:Some(flag),deadline:ctl.deadline,..Default::default()})?;json!({"after":page.after.to_string(),"next":page.next().to_string(),"head":value(page.head)?,"commits":page.commits.iter().map(|c|Ok(json!({"commit":value(c.commit)?,"added":c.added.to_string(),"removed":c.removed.to_string(),"complete":c.complete(),"changes":c.iter().map(|(op,q)|json!({"op":if op==sparkles::store::DiffOp::Add{"add"}else{"remove"},"quad":terms::encode_quad(&q)})).collect::<Vec<_>>()}))).collect::<sparkles::Result<Vec<_>>>()?})},
 "settings.describe.get"=>value(ds.settings().describe().get())?,"settings.describe.set"=>{ds.settings().describe().set(decode(&a)?)?;Value::Null},"settings.describe.reset"=>{ds.settings().describe().reset()?;Value::Null},
 "settings.compaction.get"=>value(ds.settings().compaction().get())?,"settings.compaction.set"=>{ds.settings().compaction().set(config(&a)?)?;Value::Null},"settings.compaction.reset"=>{ds.settings().compaction().reset()?;Value::Null},
 "settings.quota.get"=>value(ds.settings().quota().get())?,"settings.quota.set"=>value(ds.settings().quota().set(number(&a,"maxBytes")?)?)?,"settings.quota.reset"=>value(ds.settings().quota().reset()?)?,
 "settings.retention.get"=>value(ds.settings().retention().get())?,"settings.retention.reset"=>history(ds.settings().retention().reset()?),
 "settings.changeLog.get"=>value(ds.settings().change_log().get())?,"settings.changeLog.set"=>value(ds.settings().change_log().set(config(&a)?)?)?,"settings.changeLog.reset"=>value(ds.settings().change_log().reset()?)?,
 "queries.list"=>value(ds.queries().list())?,"queries.get"=>value(ds.queries().get(text(&a,"name")?,if a["version"].is_null(){None}else{Some(number(&a,"version")?)}))?,"queries.versions"=>value(ds.queries().versions(text(&a,"name")?))?,"queries.put"=>{let change=sparkles::stored::Change{author:a["author"].as_str().map(String::from),message:a["message"].as_str().map(String::from),if_version:if a["ifVersion"].is_null(){None}else{Some(number(&a,"ifVersion")?)},..Default::default()};let saved=ds.queries().put(text(&a,"name")?,decode(&a["definition"])?,change)?;json!({"stored":value(saved.stored)?,"created":saved.created,"changed":saved.changed})},"queries.delete"=>value(ds.queries().delete(text(&a,"name")?,if a["ifVersion"].is_null(){None}else{Some(number(&a,"ifVersion")?)} )?)?,
 "branches.list"=>value(ds.branches()?)?,"branches.get"=>value(ds.branch_info(text(&a,"name")?)?)?,"branches.create"=>value(ds.create_branch(text(&a,"name")?,&sparkles::branch::BranchOptions{from:a["from"].as_str().unwrap_or("main").into(),at:a["at"].as_str().unwrap_or("head").parse()?,protected:a["protected"].as_bool().unwrap_or(false),note:a["note"].as_str().map(String::from)})?)?,"branches.delete"=>{ds.delete_branch_with(text(&a,"name")?,&sparkles::branch::DeleteOptions{force:a["force"].as_bool().unwrap_or(false),reparent:a["reparent"].as_bool().unwrap_or(false)})?;Value::Null},"branches.rename"=>value(ds.rename_branch(text(&a,"name")?,text(&a,"to")?)?)?,"branches.protect"=>value(ds.set_branch_protected(text(&a,"name")?,a["protected"].as_bool().unwrap_or(true))?)?,
 #[cfg(feature="text")]
 "indexes.text.status"=>value(ds.indexes().text().status())?,#[cfg(feature="text")]
 "indexes.text.enable"=>value(ds.indexes().text().enable(decode(&a)?)?)?,#[cfg(feature="text")]
 "indexes.text.disable"=>{ds.indexes().text().disable()?;Value::Null},#[cfg(feature="text")]
 "indexes.text.rebuild"=>value(ds.indexes().text().rebuild()?)?,
 #[cfg(feature="geo")]
 "indexes.geo.status"=>value(ds.indexes().geo().status())?,#[cfg(feature="geo")]
 "indexes.geo.enable"=>value(ds.indexes().geo().enable(decode(&a)?)?)?,#[cfg(feature="geo")]
 "indexes.geo.disable"=>{ds.indexes().geo().disable()?;Value::Null},#[cfg(feature="geo")]
 "indexes.geo.rebuild"=>value(ds.indexes().geo().rebuild()?)?,
 "reasoning.status"=>ds.reasoning().status().map(|s|json!({"record":s.record,"head":s.head,"freshness":s.freshness})).unwrap_or(Value::Null),
 "validation.guard.get"=>ds.validation().guard().get().map(|g|g.json()).unwrap_or(Value::Null),"validation.guard.reset"=>{ds.validation().guard().reset()?;Value::Null},
 "stats"=>value(ds.stats(&sparkles::stats::StatsOptions{at:a["at"].as_str().map(str::parse).transpose()?,cancel:Some(flag.clone()),deadline:ctl.deadline})?)?,
 "explain"=>{let (text,plan)=ds.explain(text(&a,"query")?,&query_options(&v,flag.clone())?)?;json!({"text":text,"plan":plan})},
 "clearCache"=>{ds.clear_cache();Value::Null},
 "applyPatch"=>{let result=ds.store().apply_patch(text(&a,"patch")?.as_bytes(),&sparkles::store::PatchOptions{binary:false,write:write_options(&v,flag.clone())?})?;json!({"receipt":receipt(&result.receipt),"inserted":result.inserted.to_string(),"deleted":result.deleted.to_string(),"rows":result.rows,"aborted":result.aborted,"prevChecked":result.prev_checked})},
 "history.tick"=>{let r=ds.history().tick()?;json!({"created":r.created,"expired":r.expired,"rotated":r.rotated,"warmed":r.warmed,"pruned":r.pruned})},
 "history.query"=>{let r=ds.history().query(&sparkles::store::HistoryQuery{from:a["from"].as_str().map(|s|s.parse().map(sparkles::store::HistoryBound::At)).transpose()?,to:a["to"].as_str().map(|s|s.parse().map(sparkles::store::HistoryBound::At)).transpose()?,limit:a["limit"].as_u64().unwrap_or(10000) as usize,cancel:Some(flag.clone()),deadline:ctl.deadline,..Default::default()})?;json!({"from":r.from,"to":r.to,"head":r.head,"truncated":r.truncated,"changes":r.changes.iter().map(|c|json!({"commit":{"seq":c.commit.seq,"timestampMs":c.commit.timestamp_ms,"kind":c.commit.kind.name(),"bulk":c.commit.bulk,"inserted":c.commit.inserted,"deleted":c.commit.deleted,"message":c.commit.message.as_deref()},"op":if c.op==sparkles::store::DiffOp::Add{"add"}else{"remove"},"quad":terms::encode_quad(&c.quad)})).collect::<Vec<_>>()})},
 "schema.report"=>{let r=ds.schema().report(&sparkles::handles::ReportRequest{options:sparkles::schema::SchemaOptions{cancel:Some(flag.clone()),deadline:ctl.deadline,..Default::default()},limit:a["limit"].as_u64().unwrap_or(100) as usize,..Default::default()})?;value(r.report.as_ref())?},
 "schema.profiles"=>value(ds.schema().profiles(&sparkles::schema::profile::ProfileOptions{schema:sparkles::schema::SchemaOptions{cancel:Some(flag.clone()),deadline:ctl.deadline,..Default::default()},classes:serde_json::from_value(a["classes"].clone()).unwrap_or_default()})?)?,
 "schema.draftShapes"=>value(ds.schema().draft_shapes(&sparkles::schema::draft::DraftOptions{schema:sparkles::schema::SchemaOptions{cancel:Some(flag.clone()),deadline:ctl.deadline,..Default::default()},support:a["support"].as_f64().unwrap_or(1.0),..Default::default()})?)?,
 "schema.classes" | "schema.predicates" | "schema.diff"=>{let req=sparkles::handles::ReportRequest{options:sparkles::schema::SchemaOptions{cancel:Some(flag.clone()),deadline:ctl.deadline,..Default::default()},at:a["at"].as_str().map(str::parse).transpose()?,limit:a["limit"].as_u64().unwrap_or(1000) as usize,cursor:a["cursor"].as_str().map(String::from),..Default::default()};if op=="schema.diff"{let (diff,computed)=ds.schema().diff(&text(&a,"from")?.parse()?,&req)?;json!({"diff":diff,"computed":computed.to_string()})}else{let r=ds.schema().report(&req)?;if op=="schema.classes"{value(r.classes())?}else{value(r.predicates())?}}},
 "branches.commitGraph"=>{let g=ds.commit_graph(&sparkles::store::CommitGraphOptions{branches:a["branches"].as_array().map(|v|v.iter().filter_map(Value::as_str).map(String::from).collect()),before:a["before"].as_str().map(str::parse).transpose()?,limit:a["limit"].as_u64().unwrap_or(100) as usize})?;json!({"branches":g.branches.iter().map(|b|json!({"name":b.name,"id":b.id,"ordinal":b.ordinal,"head":b.head,"from":b.from,"upstream":b.upstream,"createdMs":b.created_ms})).collect::<Vec<_>>(),"commits":g.commits.iter().map(|c|json!({"commit":c.commit,"branch":c.branch,"branchId":c.branch_id,"parents":c.parents,"mergedFrom":c.merged_from,"replayedFrom":c.replayed_from,"annotation":c.annotation.as_ref().map(|a|json!({"message":a.message.as_deref(),"digest":a.digest}))})).collect::<Vec<_>>(),"next":g.next.map(|c|c.to_string())})},
 #[cfg(feature="geo")]
 "indexes.geo.features"=>ds.indexes().geo().features(&sparkles::geo::map::BoxQuery{bbox:decode(&a["bbox"])?,graph:a["graph"].as_str().map(String::from),predicate:a["predicate"].as_str().map(String::from),limit:a["limit"].as_u64().unwrap_or(1000) as usize,tolerance:a["tolerance"].as_f64()},None)?,
 "indexes.vector.recall"=>value(ds.indexes().vector().recall(text(&a,"name")?,&sparkles::handles::RecallOptions{samples:a["samples"].as_u64().unwrap_or(100) as usize,k:a["k"].as_u64().unwrap_or(10) as usize,ef:a["ef"].as_u64().map(|n|n as usize)})?)?,
 "indexes.vector.reembed"=>{ds.indexes().vector().reembed(text(&a,"name")?)?;Value::Null},
 "indexes.vector.embedUntilIdle"=>{ds.indexes().vector().embed_until_idle(Duration::from_millis(a["waitMs"].as_u64().unwrap_or(30000)))?;Value::Null},
 #[cfg(feature="reasoning")]
 "reasoning.diagnostics"=>{let d=ds.reasoning().diagnostics(&sparkles_reasoner::diagnostics::DiagnoseOptions{checks:decode(&a["checks"]).unwrap_or_default(),graphs:decode(&a["graphs"]).unwrap_or_default(),limit:a["limit"].as_u64().unwrap_or(100) as usize,inferences:a["inferences"].as_bool().unwrap_or(false),timeout:ctl.deadline.map(|d|d.saturating_duration_since(Instant::now())),..Default::default()})?;json!({"report":d.report.to_json(),"commit":d.commit,"inferences":d.inferences})},
 "schema.constraints"=>value(ds.schema().constraints(&Default::default())?)?,
 #[cfg(feature="graphql")]
 "graphql.get"=>value(ds.graphql().get(if a["version"].is_null(){None}else{Some(number(&a,"version")?)}))?,
 #[cfg(feature="graphql")]
 "graphql.versions"=>value(ds.graphql().versions())?,
 #[cfg(feature="graphql")]
 "graphql.sdl"=>value(ds.graphql().sdl()?)?,
 #[cfg(feature="graphql")]
 "graphql.reset"=>value(ds.graphql().reset(if a["ifVersion"].is_null(){None}else{Some(number(&a,"ifVersion")?)})?)?,
 #[cfg(feature="graphql")]
 "graphql.put"=>{let (s,w)=ds.graphql().put(decode(&a["config"])?,sparkles::handles::graphql::Change::default())?;json!({"stored":s.stored,"created":s.created,"changed":s.changed,"warnings":w})},
 #[cfg(feature="graphql")]
 "graphql.execute"=>{let result=ds.graphql().execute(&sparkles::handles::graphql::Request{query:text(&a,"query")?.into(),operation_name:a["operationName"].as_str().map(String::from),variables:a["variables"].as_object().cloned().unwrap_or_default()},&sparkles::handles::graphql::Options{query:query_options(&v,flag.clone())?,..Default::default()})?;result.body},
 #[cfg(feature="graphql")]
 "graphql.draft"=>{let (draft,commit)=ds.graphql().draft(a["name"].as_str().unwrap_or("dataset"),Default::default())?;json!({"draft":draft,"commit":commit})},
 "settings.retention.set"=>history(ds.settings().retention().set(sparkles::handles::HistoryUpdate{retention:if a["retention"].is_null(){None}else{Some(config(&a["retention"])?)},schedules:if a["schedules"].is_null(){None}else{Some(decode(&a["schedules"])?)},catalog:if a["catalog"].is_null(){None}else{Some(config(&a["catalog"])?)}})?),
 "history.prune"=>value(ds.history().prune()?)?,
 "history.diff"=>{let diff=ds.history().diff(&text(&a,"from")?.parse()?,&text(&a,"to")?.parse()?,&sparkles::store::DiffOptions{cancel:Some(flag.clone()),deadline:ctl.deadline,..Default::default()})?;json!({"changes":diff.iter().map(|(op,q)|json!({"op":if op==sparkles::store::DiffOp::Add{"add"}else{"remove"},"quad":terms::encode_quad(&q)})).collect::<Vec<_>>()})},
 "branches.note"=>value(ds.set_branch_note(text(&a,"name")?,a["note"].as_str().map(String::from))?)?,
 "branches.previewMerge"=>value(ds.preview_merge(text(&a,"source")?,text(&a,"target")?,&merge_options(&a,&ctl)?)?)?,
 "branches.merge"=>merge_result(ds.merge_with(text(&a,"source")?,text(&a,"target")?,&merge_options(&a,&ctl)?,&ctl)?)?,
 "branches.revert"=>merge_result(ds.revert(text(&a,"branch")?,number(&a,"commit")?,&merge_options(&a,&ctl)?)?)?,
 "branches.previewRevert"=>value(ds.preview_revert(text(&a,"branch")?,number(&a,"commit")?,&merge_options(&a,&ctl)?)?)?,
 "branches.cherryPick"=>merge_result(ds.cherry_pick(text(&a,"source")?,number(&a,"commit")?,text(&a,"target")?,&merge_options(&a,&ctl)?)?)?,
 "branches.previewCherryPick"=>value(ds.preview_cherry_pick(text(&a,"source")?,number(&a,"commit")?,text(&a,"target")?,&merge_options(&a,&ctl)?)?)?,
 "branches.exempt.get"=>value(ds.merge_exempt()?.iter().map(|n|n.as_str().to_string()).collect::<Vec<_>>())?,
 "branches.exempt.set"=>value(ds.set_merge_exempt(&decode::<Vec<String>>(&a["predicates"] )?.into_iter().map(oxrdf::NamedNode::new).collect::<std::result::Result<Vec<_>,_>>().map_err(|e|EngineError::invalid(e.to_string()))?)?.iter().map(|n|n.as_str().to_string()).collect::<Vec<_>>())?,
 "indexes.vector.list"=>value(ds.indexes().vector().list())?,"indexes.vector.get"=>value(ds.indexes().vector().get(text(&a,"name")?))?,
 "indexes.vector.put"=>value(ds.indexes().vector().put(text(&a,"name")?,decode(&a["config"])?)?)?,"indexes.vector.drop"=>{ds.indexes().vector().drop(text(&a,"name")?)?;Value::Null},"indexes.vector.rebuild"=>{ds.indexes().vector().rebuild(text(&a,"name")?)?;Value::Null},"indexes.vector.wait"=>{let name=text(&a,"name")?;loop{ctl.check()?;let status=ds.indexes().vector().get(name);if status.as_ref().is_none_or(|s|s.state!="building"){break value(status)?;}std::thread::sleep(Duration::from_millis(10));}},
 #[cfg(feature="text")]
 "indexes.text.search"=>value(ds.indexes().text().search(&sparkles::handles::TextSearch{query:text(&a,"query")?.into(),limit:a["limit"].as_u64().unwrap_or(20) as usize,highlight:a["highlight"].as_bool().unwrap_or(true),lang:a["lang"].as_str().map(String::from),..Default::default()})?)?,
 "reasoning.rdfs.get"=>json!({"enabled":ds.reasoning().rdfs().get().is_some()}),"reasoning.rdfs.set"=>{ds.reasoning().rdfs().set(sparkles::reasoning::rdfs::NewSchema::Graph(text(&a,"graph")?.into()))?;Value::Null},"reasoning.rdfs.reset"=>{ds.reasoning().rdfs().reset()?;Value::Null},
 #[cfg(feature="reasoning")]
 "reasoning.run"=>{let profile=text(&a,"profile")?.parse().map_err(|e|EngineError::invalid(format!("{e}")))?;let r=ds.reasoning().run_with(&sparkles::reasoning::ReasonRequest{profile,incremental:a["incremental"].as_bool().unwrap_or(true),..Default::default()},&ctl)?;json!({"report":{"profile":r.report.profile,"inferred":r.report.inferred,"iterations":r.report.iterations,"millis":r.report.millis,"warnings":r.report.warnings},"record":r.record})},
 #[cfg(feature="reasoning")]
 "reasoning.clear"=>value(ds.reasoning().clear()?)?,
 #[cfg(feature="shacl")]
 "validation.guard.setShacl"=>guard_outcome(ds.validation().guard().set_shacl(decode(&a)?)?)?,
 #[cfg(feature="shex")]
 "validation.guard.setShex"=>guard_outcome(ds.validation().guard().set_shex(decode(&a)?,&sparkles_shex::NoImports)?)?,
 #[cfg(feature="shacl")]
 "validation.shacl"=>{let syntax=if a["format"].as_str()==Some("shaclc"){sparkles_shacl::ShapesSyntax::Compact}else{streams::format(&a)?.into()};let shapes=sparkles_shacl::Shapes::parse(text(&a,"shapes")?,syntax,a["baseIri"].as_str()).map_err(|e|EngineError::invalid(e.to_string()))?;let report=ds.validation().shacl(&shapes,&sparkles_shacl::ValidateOptions{cancel:Some(flag.clone()),timeout:ctl.deadline.map(|d|d.saturating_duration_since(Instant::now())),..Default::default()})?;json!({"conforms":report.conforms,"turtle":report.to_turtle(),"results":report.results.iter().map(|r|json!({"focusNode":r.focus_node.to_string(),"path":r.result_path.as_ref().map(ToString::to_string),"value":r.value.as_ref().map(ToString::to_string),"severity":r.severity.as_str(),"messages":r.messages.iter().map(|m|m.value()).collect::<Vec<_>>()})).collect::<Vec<_>>()})},
 #[cfg(feature="shex")]
 "validation.shex"=>{let parsed=sparkles_shex::parse_schema(text(&a,"schema")?,None,None).map_err(|e|EngineError::invalid(e.to_string()))?;let compiled=sparkles_shex::compile(&parsed,&sparkles_shex::NoImports).map_err(|e|EngineError::invalid(e.to_string()))?;let m=text(&a,"shapeMap")?;let map=if m.trim_start().starts_with('['){sparkles_shex::ShapeMap::from_json(m)}else{sparkles_shex::ShapeMap::parse(m,compiled.prefixes(),compiled.base())}.map_err(|e|EngineError::invalid(e.to_string()))?;let report=ds.validation().shex(&compiled,&map,&sparkles_shex::ValidateOptions{cancel:Some(flag.clone()),timeout:ctl.deadline.map(|d|d.saturating_duration_since(Instant::now())),..Default::default()})?;json!({"conforms":report.conforms,"warnings":report.warnings,"millis":report.millis,"results":report.results.iter().map(|r|json!({"node":r.node.to_string(),"shape":format!("{:?}",r.shape),"conformant":r.status==sparkles_shex::Status::Conformant,"reason":r.reason})).collect::<Vec<_>>()})},
 "compact"=>{ds.compact_with(&Default::default(),&ctl)?;Value::Null},"cloneTo"=>{ds.clone_to_with(text(&a,"path")?,&Default::default(),&ctl)?;Value::Null},"backup"=>value(ds.backup(text(&a,"path")?)?.display().to_string())?,
 _=>return Err(EngineError::unsupported(format!("unknown or disabled administration operation {op}"))),};Ok(if op=="graphql.execute"{result.to_string()}else if matches!(op.as_str(),"history.prune"|"reasoning.clear"){Value::String(result.to_string()).to_string()}else{meta(result,"").to_string()})}).await
    }
}
pub(crate) static BRANCHES: LazyLock<Mutex<HashMap<usize, Weak<Shared>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
