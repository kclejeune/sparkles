//! Parity of the HTTP API and the library (spec P06 §7.1): every operation of
//! `docs/openapi.json` has exactly one entry here. `op!` maps it to the library call that
//! performs it, `server_only!` names the reason of §6 why it has none, and `pending!`
//! marks an operation whose library call a later step of Phase 1 adds. The closures are
//! type-checked and never called, so an entry compiles only when the library call exists
//! with arguments of that shape. A call behind a Cargo feature is checked in builds with
//! that feature (`--features full` checks them all).

#![allow(clippy::unit_arg, unused_must_use)]

use sparkles::Dataset;
use std::collections::BTreeMap;

/// The subsections of §6: why an operation has no library call.
#[derive(Clone, Copy, Debug)]
enum Reason {
    /// §6.1 identities and credentials
    Credentials,
    /// §6.3 HTTP routing state
    Http,
    /// §6.4 the task queue
    Tasks,
    /// §6.5 schedulers
    Schedulers,
    /// §6.6 process and HTTP state
    Process,
    /// §6.7 the MCP transport
    Mcp,
}

#[derive(Debug)]
enum Entry {
    /// the surface key of the library call
    Op(#[allow(dead_code)] &'static str),
    ServerOnly(#[allow(dead_code)] Reason),
    /// the step of Phase 1 that adds the call
    Pending(&'static str),
}

/// A value of any type, for arguments whose value does not matter: the closures that
/// use it are never called.
fn any<T>() -> T {
    unreachable!("parity closures are type-checked, never called")
}

macro_rules! op {
    ($id:literal, $key:literal, $f:expr) => {{
        let _check: fn(&Dataset) = $f;
        ($id, Entry::Op($key))
    }};
}

/// An entry whose call needs a Cargo feature: it is checked in builds with the feature.
macro_rules! op_if {
    ($feature:literal, $id:literal, $key:literal, $f:expr) => {{
        #[cfg(feature = $feature)]
        {
            let _check: fn(&Dataset) = $f;
        }
        ($id, Entry::Op($key))
    }};
}

macro_rules! server_only {
    ($id:literal, $reason:expr) => {
        ($id, Entry::ServerOnly($reason))
    };
}

macro_rules! pending {
    ($id:literal, $step:literal) => {
        ($id, Entry::Pending($step))
    };
}

fn entries() -> Vec<(&'static str, Entry)> {
    use Reason::*;
    use sparkles::task::Control;
    vec![
        // ------------------------------------------------ datasets and the catalog
        pending!("listDatasets", "phase 1 step 6"),
        pending!("getDataset", "phase 1 step 6"),
        pending!("createDataset", "phase 1 step 6"),
        pending!("deleteDataset", "phase 1 step 6"),
        pending!("cloneDataset", "phase 1 step 6"),
        pending!("listBackupFiles", "phase 1 step 6"),
        pending!("listBackupFilesPost", "phase 1 step 6"),
        server_only!("setDatasetState", Http),
        pending!("getDatasetStats", "phase 1 step 5"),
        pending!("getDatasetStatsPost", "phase 1 step 5"),
        op!("clearCache", "dataset.clear_cache", |ds| ds.clear_cache()),
        op!("compactDataset", "dataset.compact_with", |ds| {
            ds.compact_with(&Default::default(), &Control::none());
        }),
        op!("getCompaction", "settings.compaction.get", |ds| {
            ds.settings().compaction().get();
        }),
        op!("setCompaction", "settings.compaction.set", |ds| {
            ds.settings().compaction().set(Default::default());
        }),
        op!("deleteCompaction", "settings.compaction.reset", |ds| {
            ds.settings().compaction().reset();
        }),
        op!("getQuota", "settings.quota.get", |ds| {
            ds.settings().quota().get();
        }),
        op!("setQuota", "settings.quota.set", |ds| {
            ds.settings().quota().set(1 << 20);
        }),
        op!("deleteQuota", "settings.quota.reset", |ds| {
            ds.settings().quota().reset();
        }),
        op!("getAllPrefixes", "dataset.prefixes", |ds| {
            ds.prefixes();
        }),
        op!("getPrefixes", "dataset.prefixes", |ds| {
            ds.prefixes();
        }),
        op!("addPrefix", "dataset.set_prefix", |ds| {
            ds.set_prefix("ex", "http://ex.org/");
        }),
        op!("putPrefix", "dataset.set_prefix", |ds| {
            ds.set_prefix("ex", "http://ex.org/");
        }),
        op!("deletePrefix", "dataset.remove_prefix", |ds| {
            ds.remove_prefix("ex");
        }),
        // ------------------------------------------- SPARQL and the Graph Store Protocol
        op!("queryGet", "dataset.query_with", |ds| {
            ds.query_with("ASK {}", &Default::default());
        }),
        op!("queryPost", "dataset.query_with", |ds| {
            ds.query_with("ASK {}", &Default::default());
        }),
        op!("sparqlQueryGet", "dataset.query_with", |ds| {
            ds.query_with("ASK {}", &Default::default());
        }),
        op!("sparqlQueryPost", "dataset.query_with", |ds| {
            ds.query_with("ASK {}", &Default::default());
        }),
        op!("datasetGet", "dataset.query_with", |ds| {
            ds.query_with("ASK {}", &Default::default());
        }),
        op!("datasetPost", "dataset.query_with", |ds| {
            ds.query_with("ASK {}", &Default::default());
        }),
        op!("update", "dataset.update_with", |ds| {
            ds.update_with("CLEAR DEFAULT", &Default::default());
        }),
        op!("explainGet", "dataset.explain", |ds| {
            ds.explain("ASK {}", &Default::default());
        }),
        op!("explainPost", "dataset.explain", |ds| {
            ds.explain("ASK {}", &Default::default());
        }),
        op!("getDescribe", "settings.describe.get", |ds| {
            ds.settings().describe().get();
        }),
        op!("setDescribe", "settings.describe.set", |ds| {
            ds.settings().describe().set(Default::default());
        }),
        op!("deleteDescribe", "settings.describe.reset", |ds| {
            ds.settings().describe().reset();
        }),
        op!("runStoredQuery", "queries.run", |ds| {
            ds.queries().run("q", &BTreeMap::new(), &Default::default());
        }),
        op!("runStoredQueryPost", "queries.run", |ds| {
            ds.queries().run("q", &BTreeMap::new(), &Default::default());
        }),
        op!("gspGet", "dataset.dump_graph", |ds| {
            ds.dump_graph(oxrdf::GraphNameRef::DefaultGraph, Vec::new(), any());
        }),
        op!("gspReadGet", "dataset.dump_graph", |ds| {
            ds.dump_graph(oxrdf::GraphNameRef::DefaultGraph, Vec::new(), any());
        }),
        op!("directGraphGet", "dataset.dump_graph", |ds| {
            ds.dump_graph(oxrdf::GraphNameRef::DefaultGraph, Vec::new(), any());
        }),
        op!("gspHead", "dataset.dump_graph", |ds| {
            ds.dump_graph(oxrdf::GraphNameRef::DefaultGraph, Vec::new(), any());
        }),
        op!("gspReadHead", "dataset.dump_graph", |ds| {
            ds.dump_graph(oxrdf::GraphNameRef::DefaultGraph, Vec::new(), any());
        }),
        op!("directGraphHead", "dataset.dump_graph", |ds| {
            ds.dump_graph(oxrdf::GraphNameRef::DefaultGraph, Vec::new(), any());
        }),
        op!("datasetHead", "dataset.dump", |ds| {
            ds.dump(Vec::new(), any());
        }),
        op!("gspPost", "dataset.load_sources_receipt", |ds| {
            ds.load_sources_receipt(Vec::new());
        }),
        op!("directGraphPost", "dataset.load_sources_receipt", |ds| {
            ds.load_sources_receipt(Vec::new());
        }),
        op!("upload", "dataset.load_sources_receipt", |ds| {
            ds.load_sources_receipt(Vec::new());
        }),
        op!("gspPut", "dataset.replace", |ds| {
            ds.replace(sparkles::store::ReplaceTarget::Default, &[]);
        }),
        op!("directGraphPut", "dataset.replace", |ds| {
            ds.replace(sparkles::store::ReplaceTarget::Default, &[]);
        }),
        op!("datasetPut", "dataset.replace", |ds| {
            ds.replace(sparkles::store::ReplaceTarget::All, &[]);
        }),
        op!("gspDelete", "graph.clear", |ds| {
            ds.default_graph().clear();
        }),
        op!("directGraphDelete", "graph.clear", |ds| {
            ds.default_graph().clear();
        }),
        op!("datasetDelete", "dataset.clear", |ds| {
            ds.clear();
        }),
        op!("patch", "dataset.apply_patch", |ds| {
            ds.apply_patch(&b""[..], false);
        }),
        op!("patchPost", "dataset.apply_patch", |ds| {
            ds.apply_patch(&b""[..], false);
        }),
        // -------------------------------------------------- history and snapshots
        op!("listCommits", "history.commits", |ds| {
            ds.history()
                .commits(sparkles::commit::CommitRange::Latest, 10);
        }),
        op!("getCommit", "history.commit", |ds| {
            ds.history().commit(&sparkles::handles::CommitRef::Head);
        }),
        op!("getHistory", "history.status", |ds| {
            ds.history().status();
        }),
        op!("setHistory", "settings.retention.set", |ds| {
            ds.settings().retention().set(Default::default());
            ds.settings().change_log().set(Default::default());
        }),
        op!("listSnapshots", "snapshots.list", |ds| {
            ds.snapshots().list();
        }),
        op!("getSnapshot", "snapshots.get", |ds| {
            ds.snapshots().get("s");
        }),
        op!("createSnapshot", "snapshots.create", |ds| {
            ds.snapshots()
                .create("s", &sparkles::history::At::Head, &Default::default());
        }),
        op!("deleteSnapshot", "snapshots.delete", |ds| {
            ds.snapshots().delete("s");
        }),
        op!("diff", "history.diff", |ds| {
            ds.history().diff(
                &sparkles::history::At::Commit(1),
                &sparkles::history::At::Head,
                &Default::default(),
            );
        }),
        op!("changes", "history.changes", |ds| {
            ds.history().changes(0, &Default::default());
        }),
        op!("historyChanges", "history.query", |ds| {
            ds.history().query(&any());
        }),
        // --------------------------------------------------- branches and merges
        op!("listBranches", "branches.list", |ds| {
            ds.branches();
        }),
        op!("updateBranchSettings", "branches.settings", |ds| {
            ds.merge_exempt();
            ds.set_merge_exempt(&[]);
        }),
        op!("getBranch", "branches.get", |ds| {
            ds.branch_info("dev");
        }),
        op!("createBranch", "branches.create", |ds| {
            ds.create_branch("dev", &Default::default());
        }),
        op!("updateBranch", "branches.update", |ds| {
            ds.set_branch_protected("dev", true);
            ds.set_branch_note("dev", None);
            ds.rename_branch("dev", "work");
        }),
        op!("deleteBranch", "branches.delete", |ds| {
            ds.delete_branch("dev", false);
            ds.delete_branch_with("dev", &Default::default());
        }),
        op!("previewMerge", "branches.preview_merge", |ds| {
            ds.preview_merge("dev", "main", &Default::default());
        }),
        op!("merge", "branches.merge", |ds| {
            ds.merge("dev", "main", &Default::default());
            ds.merge_with("dev", "main", &Default::default(), &Control::none());
        }),
        op!("previewRevert", "branches.preview_revert", |ds| {
            ds.preview_revert("main", 3, &Default::default());
        }),
        op!("revert", "branches.revert", |ds| {
            ds.revert("main", 3, &Default::default());
        }),
        op!("previewCherryPick", "branches.preview_cherry_pick", |ds| {
            ds.preview_cherry_pick("dev", 3, "main", &Default::default());
        }),
        op!("cherryPick", "branches.cherry_pick", |ds| {
            ds.cherry_pick("dev", 3, "main", &Default::default());
        }),
        // ----------------------------------------------------------- search indexes
        op!("getTextIndex", "indexes.text.status", |ds| {
            ds.indexes().text().status();
        }),
        op!("setTextIndex", "indexes.text.enable", |ds| {
            ds.indexes().text().enable(any());
        }),
        op!("deleteTextIndex", "indexes.text.disable", |ds| {
            ds.indexes().text().disable();
        }),
        op!("rebuildTextIndex", "indexes.text.rebuild", |ds| {
            ds.indexes().text().rebuild();
        }),
        pending!("textSearch", "phase 1 step 5"),
        pending!("textSearchPost", "phase 1 step 5"),
        op!("getVectorIndexes", "indexes.vector.list", |ds| {
            ds.indexes().vector().list();
        }),
        op!("getVectorIndex", "indexes.vector.get", |ds| {
            ds.indexes().vector().get("v");
        }),
        op!("putVectorIndex", "indexes.vector.put", |ds| {
            ds.indexes().vector().put("v", any());
        }),
        op!("deleteVectorIndex", "indexes.vector.drop", |ds| {
            ds.indexes().vector().drop("v");
        }),
        op!("rebuildVectorIndex", "indexes.vector.rebuild", |ds| {
            ds.indexes().vector().rebuild("v");
        }),
        op!("reembedVectorIndex", "indexes.vector.reembed", |ds| {
            ds.indexes().vector().reembed("v");
        }),
        op!("measureVectorRecall", "indexes.vector.recall", |ds| {
            ds.indexes().vector().recall("v", &Default::default());
        }),
        op!("getGeoIndex", "indexes.geo.status", |ds| {
            ds.indexes().geo().status();
        }),
        op!("setGeoIndex", "indexes.geo.enable", |ds| {
            ds.indexes().geo().enable(any());
        }),
        op!("deleteGeoIndex", "indexes.geo.disable", |ds| {
            ds.indexes().geo().disable();
        }),
        op!("rebuildGeoIndex", "indexes.geo.rebuild", |ds| {
            ds.indexes().geo().rebuild();
        }),
        op_if!("geo", "geoFeatures", "indexes.geo.features", |ds| {
            ds.indexes().geo().features(&any(), None);
        }),
        op_if!("geo", "convertGeometries", "geo.convert", |_| {
            sparkles::geo::convert::convert(&[]);
        }),
        // ------------------------------------------------------------------- schema
        pending!("getSchema", "phase 1 step 5"),
        pending!("listSchemaClasses", "phase 1 step 5"),
        pending!("listSchemaPredicates", "phase 1 step 5"),
        pending!("getSchemaConstraints", "phase 1 step 5"),
        pending!("getSchemaDiff", "phase 1 step 5"),
        op!("getClassProfiles", "schema.profiles", |ds| {
            ds.schema().profiles(&Default::default());
        }),
        op!("draftShapes", "schema.draft_shapes", |ds| {
            ds.schema().draft_shapes(&Default::default());
        }),
        // ------------------------------------------------ stored queries and GraphQL
        op!("listStoredQueries", "queries.list", |ds| {
            ds.queries().list();
        }),
        op!("getStoredQuery", "queries.get", |ds| {
            ds.queries().get("q", None);
        }),
        op!("putStoredQuery", "queries.put", |ds| {
            ds.queries().put("q", any(), any());
        }),
        op!("deleteStoredQuery", "queries.delete", |ds| {
            ds.queries().delete("q", None);
        }),
        op!("listStoredQueryVersions", "queries.versions", |ds| {
            ds.queries().versions("q");
        }),
        op_if!("graphql", "getGraphqlConfig", "graphql.config.get", |ds| {
            ds.graphql().get(None);
        }),
        pending!("putGraphqlConfig", "phase 1 step 5"),
        op_if!(
            "graphql",
            "deleteGraphqlConfig",
            "graphql.config.reset",
            |ds| {
                ds.graphql().reset(None);
            }
        ),
        op_if!("graphql", "listGraphqlVersions", "graphql.versions", |ds| {
            ds.graphql().versions();
        }),
        pending!("draftGraphqlSchema", "phase 1 step 5"),
        pending!("graphqlGet", "phase 1 step 5"),
        pending!("graphqlPost", "phase 1 step 5"),
        pending!("graphqlApiSchema", "phase 1 step 5"),
        // ------------------------------------------------- reasoning and validation
        pending!("reason", "phase 1 step 5"),
        pending!("getReasoning", "phase 1 step 5"),
        op_if!("reasoning", "clearReasoning", "reasoning.clear", |ds| {
            ds.reasoning().clear();
        }),
        pending!("reasoningDiagnostics", "phase 1 step 5"),
        op!("getRdfs", "reasoning.rdfs.get", |ds| {
            ds.reasoning().rdfs().get();
        }),
        op!("setRdfs", "reasoning.rdfs.set", |ds| {
            ds.reasoning()
                .rdfs()
                .set(sparkles::reasoning::rdfs::NewSchema::Graph(
                    "default".into(),
                ));
        }),
        op!("deleteRdfs", "reasoning.rdfs.reset", |ds| {
            ds.reasoning().rdfs().reset();
        }),
        server_only!("setAutoReasoning", Schedulers),
        server_only!("clearAutoReasoning", Schedulers),
        op!("getWriteValidation", "validation.guard.get", |ds| {
            ds.validation().guard().get();
        }),
        op_if!(
            "shacl",
            "setWriteValidation",
            "validation.guard.set",
            |ds| {
                ds.validation().guard().set_shacl(any());
            }
        ),
        op!("deleteWriteValidation", "validation.guard.reset", |ds| {
            ds.validation().guard().reset();
        }),
        op_if!("shacl", "shacl", "validation.shacl", |ds| {
            ds.validation().shacl(&any(), &any());
        }),
        op_if!("shex", "shex", "validation.shex", |ds| {
            ds.validation().shex(&any(), &any(), &any());
        }),
        // ------------------------------------------------------------------ backups
        op!("backupNquads", "dataset.backup", |ds| {
            ds.backup("backups");
        }),
        op_if!("backup", "listDatasetBackups", "backups.list", |ds| {
            ds.backups(&any()).list(&Default::default());
        }),
        op_if!("backup", "getBackup", "backups.get", |ds| {
            ds.backups(&any()).get("b");
        }),
        pending!("createBackup", "phase 1 step 5"),
        op_if!("backup", "deleteBackup", "backups.delete", |ds| {
            ds.backups(&any()).delete("b");
        }),
        op_if!("backup", "verifyBackup", "backups.verify_with", |ds| {
            ds.backups(&any())
                .verify_with("b", &any(), &Control::none());
        }),
        pending!("restoreBackup", "phase 1 step 6"),
        pending!("listRepositories", "phase 1 step 6"),
        pending!("getRepository", "phase 1 step 6"),
        pending!("createRepository", "phase 1 step 6"),
        pending!("updateRepository", "phase 1 step 6"),
        pending!("deleteRepository", "phase 1 step 6"),
        op_if!(
            "backup",
            "listRepositoryBackups",
            "backup.repository.list",
            |_| {
                sparkles::backup::blocking(&any()).list(&Default::default());
            }
        ),
        op_if!(
            "backup",
            "collectRepository",
            "backup.repository.gc",
            |_| {
                sparkles::backup::blocking(&any()).gc(&any());
            }
        ),
        op_if!(
            "backup",
            "listRepositoryLocks",
            "backup.repository.locks",
            |_| {
                sparkles::backup::blocking(&any()).locks();
            }
        ),
        op_if!(
            "backup",
            "breakRepositoryLock",
            "backup.repository.break_lock",
            |_| {
                sparkles::backup::blocking(&any()).break_lock("l");
            }
        ),
        op_if!("backup", "testRepository", "backup.repository.test", |_| {
            sparkles::backup::blocking(&any()).test();
        }),
        op_if!(
            "backup",
            "verifyRepository",
            "backup.repository.verify",
            |_| {
                sparkles::backup::blocking(&any()).verify(&[], &any());
            }
        ),
        op_if!(
            "backup",
            "previewSchedule",
            "backup.policy.next_runs",
            |_| {
                sparkles::backup::policy::next_runs(&any(), any(), any(), 5);
            }
        ),
        pending!("runPolicy", "phase 1 step 6"),
        pending!("applyRetention", "phase 1 step 6"),
        server_only!("listPolicies", Schedulers),
        server_only!("getPolicy", Schedulers),
        server_only!("createPolicy", Schedulers),
        server_only!("updatePolicy", Schedulers),
        server_only!("deletePolicy", Schedulers),
        server_only!("listPolicyRuns", Schedulers),
        // -------------------------------------------------------- stateless utilities
        op!("validateQuery", "sparql.parse_query", |_| {
            sparkles::sparql::parse_query("ASK {}", None, &[]);
        }),
        op!("validateQueryPost", "sparql.parse_query", |_| {
            sparkles::sparql::parse_query("ASK {}", None, &[]);
        }),
        op!("validateUpdate", "sparql.parse_update", |_| {
            sparkles::sparql::update::parse_update("CLEAR DEFAULT", &Default::default());
        }),
        op!("validateUpdatePost", "sparql.parse_update", |_| {
            sparkles::sparql::update::parse_update("CLEAR DEFAULT", &Default::default());
        }),
        pending!("validateData", "phase 1 step 5"),
        pending!("validateDataPost", "phase 1 step 5"),
        pending!("validateIri", "phase 1 step 5"),
        pending!("validateIriPost", "phase 1 step 5"),
        pending!("validateLangtag", "phase 1 step 5"),
        pending!("validateLangtagPost", "phase 1 step 5"),
        op_if!("fmt", "format", "fmt.format", |_| {
            sparkles::fmt::format("", any(), &any());
        }),
        op_if!("fmt", "lint", "fmt.lint", |_| {
            sparkles::fmt::lint("", any(), &any());
        }),
        // ---------------------------------------------------------------- server only
        server_only!("login", Credentials),
        server_only!("logout", Credentials),
        server_only!("authConfig", Credentials),
        server_only!("whoami", Credentials),
        server_only!("tokenGrant", Credentials),
        server_only!("authorizeCli", Credentials),
        server_only!("deviceAuthorization", Credentials),
        server_only!("getDeviceLogin", Credentials),
        server_only!("approveDeviceLogin", Credentials),
        server_only!("denyDeviceLogin", Credentials),
        server_only!("oidcLogin", Credentials),
        server_only!("oidcCallback", Credentials),
        server_only!("oidcBackchannelLogout", Credentials),
        server_only!("listTokens", Credentials),
        server_only!("createToken", Credentials),
        server_only!("revokeToken", Credentials),
        server_only!("revokeOwnerTokens", Credentials),
        server_only!("listTasks", Tasks),
        server_only!("getTask", Tasks),
        server_only!("cancelTask", Tasks),
        server_only!("fusekiStats", Process),
        server_only!("fusekiStatsPost", Process),
        server_only!("mcpPost", Mcp),
        server_only!("mcpStream", Mcp),
        server_only!("mcpEndSession", Mcp),
        server_only!("ping", Process),
        server_only!("pingPost", Process),
        server_only!("getServer", Process),
        server_only!("getServerPost", Process),
        server_only!("metrics", Process),
        server_only!("ready", Process),
        server_only!("readyDataset", Process),
        server_only!("openapiJson", Process),
        server_only!("openapiYaml", Process),
    ]
}

/// `(operationId, method, path)` of every operation in `docs/openapi.json`.
fn operations() -> Vec<(String, String, String)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/openapi.json");
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("docs/openapi.json")).unwrap();
    let mut out = Vec::new();
    for (p, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if let Some(id) = op.get("operationId").and_then(|v| v.as_str()) {
                out.push((id.to_string(), method.to_uppercase(), p.clone()));
            }
        }
    }
    out
}

#[test]
fn every_operation_maps_to_a_library_call_or_a_reason() {
    let entries = entries();
    let mut problems = Vec::new();
    let mut seen = BTreeMap::new();
    for (id, _) in &entries {
        if seen.insert(*id, ()).is_some() {
            problems.push(format!(
                "`parity.rs` has two entries for `{id}`. Keep one of them."
            ));
        }
    }
    let ops = operations();
    for (id, method, path) in &ops {
        if !seen.contains_key(id.as_str()) {
            problems.push(format!(
                "operation `{id}` (`{method} {path}`) has no entry in `crates/sparkles/tests/parity.rs`. Map it with `op!` to the library call that performs it, or mark it `server_only!` with a reason from §6 of spec P06."
            ));
        }
    }
    for (id, _) in &entries {
        if !ops.iter().any(|(o, _, _)| o == id) {
            problems.push(format!(
                "`parity.rs` maps `{id}`, which `docs/openapi.json` does not describe. Remove the entry or rename it."
            ));
        }
    }
    let pending: Vec<_> = entries
        .iter()
        .filter_map(|(id, e)| match e {
            Entry::Pending(step) => Some(format!("{id} ({step})")),
            _ => None,
        })
        .collect();
    if !pending.is_empty() {
        println!(
            "{} of {} operations are pending: {}",
            pending.len(),
            entries.len(),
            pending.join(", ")
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
