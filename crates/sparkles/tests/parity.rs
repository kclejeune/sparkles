//! Parity of the HTTP API and the library (spec P06 §7.1): every operation of
//! `docs/openapi.json` has exactly one entry here. `op!` maps it to the library call that
//! performs it, and `server_only!` names the reason of §6 why it has none. The closures are
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

fn entries() -> Vec<(&'static str, Entry)> {
    use Reason::*;
    use sparkles::task::Control;
    vec![
        // ------------------------------------------------ datasets and the catalog
        op!("listDatasets", "catalog.list", |_| {
            any::<sparkles::Catalog>().list();
        }),
        op!("getDataset", "catalog.info", |_| {
            any::<sparkles::Catalog>().info("d");
        }),
        op!("createDataset", "catalog.create", |_| {
            any::<sparkles::Catalog>().create("d", &Default::default());
        }),
        op!("deleteDataset", "catalog.delete", |_| {
            any::<sparkles::Catalog>().delete("d");
        }),
        op!("renameDataset", "catalog.rename", |_| {
            any::<sparkles::Catalog>().rename("d", "renamed");
        }),
        op!("cloneDataset", "catalog.clone_dataset", |_| {
            any::<sparkles::Catalog>().clone_dataset(
                "d",
                "c",
                &Default::default(),
                &Control::none(),
            );
        }),
        op!("listBackupFiles", "catalog.backup_files", |_| {
            any::<sparkles::Catalog>().backup_files();
        }),
        op!("listBackupFilesPost", "catalog.backup_files", |_| {
            any::<sparkles::Catalog>().backup_files();
        }),
        server_only!("setDatasetState", Http),
        op!("getDatasetStats", "dataset.stats", |ds| {
            ds.stats(&Default::default());
        }),
        op!("getDatasetStatsPost", "dataset.stats", |ds| {
            ds.stats(&Default::default());
        }),
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
            ds.dump(Vec::new(), any::<sparkles::RdfSyntax>());
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
        op!("relinkBranch", "branches.relink", |ds| {
            ds.relink_branch("dev", &Default::default());
            ds.relink_branch_with("dev", &Default::default(), &Control::none());
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
        op!("getCommitGraph", "branches.commit_graph", |ds| {
            ds.commit_graph(&Default::default());
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
        op!("textSearch", "indexes.text.search", |ds| {
            ds.indexes().text().search(&Default::default());
        }),
        op!("textSearchPost", "indexes.text.search", |ds| {
            ds.indexes().text().search(&Default::default());
        }),
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
        op!("getSchema", "schema.report", |ds| {
            if let Ok(r) = ds.schema().report(&Default::default()) {
                r.summary("ds");
            }
        }),
        op!("listSchemaClasses", "schema.classes", |ds| {
            if let Ok(r) = ds.schema().report(&Default::default()) {
                r.classes();
            }
        }),
        op!("listSchemaPredicates", "schema.predicates", |ds| {
            if let Ok(r) = ds.schema().report(&Default::default()) {
                r.predicates();
            }
        }),
        op!("getSchemaConstraints", "schema.constraints", |ds| {
            ds.schema().constraints(&Default::default());
        }),
        op!("getSchemaDiff", "schema.diff", |ds| {
            ds.schema()
                .diff(&sparkles::history::At::Commit(1), &Default::default());
        }),
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
        op_if!("graphql", "putGraphqlConfig", "graphql.put", |ds| {
            ds.graphql().put(any(), any());
        }),
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
        op_if!("graphql", "draftGraphqlSchema", "graphql.draft", |ds| {
            ds.graphql().draft("ds", Default::default());
        }),
        op_if!("graphql", "graphqlGet", "graphql.execute", |ds| {
            ds.graphql().execute(&any(), &Default::default());
        }),
        op_if!("graphql", "graphqlPost", "graphql.execute", |ds| {
            ds.graphql().execute(&any(), &Default::default());
        }),
        op_if!("graphql", "graphqlApiSchema", "graphql.sdl", |ds| {
            ds.graphql().sdl();
        }),
        // ------------------------------------------------- reasoning and validation
        op_if!("reasoning", "reason", "reasoning.run_with", |ds| {
            ds.reasoning()
                .run_with(&Default::default(), &Control::none());
        }),
        op!("getReasoning", "reasoning.status", |ds| {
            ds.reasoning().status();
        }),
        op_if!("reasoning", "clearReasoning", "reasoning.clear", |ds| {
            ds.reasoning().clear();
        }),
        op_if!(
            "reasoning",
            "reasoningDiagnostics",
            "reasoning.diagnostics",
            |ds| {
                ds.reasoning().diagnostics(&any());
            }
        ),
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
        op_if!("backup", "createBackup", "backups.create_with", |ds| {
            ds.backups(&any()).create_with(&any(), &Control::none());
        }),
        op_if!("backup", "deleteBackup", "backups.delete", |ds| {
            ds.backups(&any()).delete("b");
        }),
        op_if!("backup", "verifyBackup", "backups.verify_with", |ds| {
            ds.backups(&any())
                .verify_with("b", &any(), &Control::none());
        }),
        op_if!("backup", "restoreBackup", "catalog.restore", |_| {
            any::<sparkles::Catalog>().restore(&any(), "b", &any(), &Control::none());
        }),
        op_if!("backup", "listRepositories", "repositories.list", |_| {
            any::<sparkles::Catalog>().repositories().unwrap().list();
        }),
        op_if!("backup", "getRepository", "repositories.get", |_| {
            any::<sparkles::Catalog>().repositories().unwrap().get("r");
        }),
        op_if!("backup", "createRepository", "repositories.add", |_| {
            any::<sparkles::Catalog>()
                .repositories()
                .unwrap()
                .add(any());
        }),
        op_if!("backup", "updateRepository", "repositories.update", |_| {
            any::<sparkles::Catalog>()
                .repositories()
                .unwrap()
                .update("r", any());
        }),
        op_if!("backup", "deleteRepository", "repositories.remove", |_| {
            any::<sparkles::Catalog>()
                .repositories()
                .unwrap()
                .remove("r");
        }),
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
        op_if!("backup", "runPolicy", "catalog.run_policy", |_| {
            any::<sparkles::Catalog>().run_policy(&any(), &Control::none());
        }),
        op_if!(
            "backup",
            "applyRetention",
            "catalog.apply_retention",
            |_| {
                any::<sparkles::Catalog>().apply_retention(&any(), true);
            }
        ),
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
        op!("validateData", "io.check_data", |_| {
            sparkles::io::check_data(any(), "", None);
        }),
        op!("validateDataPost", "io.check_data", |_| {
            sparkles::io::check_data(any(), "", None);
        }),
        op!("validateIri", "terms.check_iri", |_| {
            sparkles::terms::check_iri("http://ex.org/");
        }),
        op!("validateIriPost", "terms.check_iri", |_| {
            sparkles::terms::check_iri("http://ex.org/");
        }),
        op!("validateLangtag", "terms.check_langtag", |_| {
            sparkles::terms::check_langtag("en");
        }),
        op!("validateLangtagPost", "terms.check_langtag", |_| {
            sparkles::terms::check_langtag("en");
        }),
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
        // C18: the MCP tools over HTTP, and the server's model and assistant settings
        server_only!("checkQuery", Mcp),
        server_only!("recallFacts", Mcp),
        server_only!("diagnoseQuery", Mcp),
        server_only!("assertFacts", Mcp),
        server_only!("registerSource", Mcp),
        server_only!("listSources", Mcp),
        server_only!("memoryBrief", Mcp),
        // C18 Phase 3: the review inbox and its actions run the memory tools; ingest
        // profiles live beside the dataset like the memory settings
        server_only!("getMemoryInbox", Mcp),
        server_only!("getBranchReview", Mcp),
        server_only!("promoteFacts", Mcp),
        server_only!("rejectFacts", Mcp),
        server_only!("relinkEntity", Mcp),
        server_only!("editFact", Mcp),
        server_only!("listIngestProfiles", Process),
        server_only!("putIngestSettings", Process),
        server_only!("getIngestProfile", Process),
        server_only!("putIngestProfile", Process),
        server_only!("deleteIngestProfile", Process),
        // C18 Phase 4: ingestion runs as a task of the server process, with its providers
        server_only!("startIngest", Process),
        server_only!("listIngestTasks", Process),
        server_only!("getIngestTask", Process),
        server_only!("cancelIngestTask", Process),
        server_only!("confirmIngestTask", Process),
        server_only!("approveIngestTask", Process),
        server_only!("listModelProviders", Process),
        server_only!("testModelProvider", Process),
        server_only!("getMemorySettings", Process),
        server_only!("putMemorySettings", Process),
        server_only!("listSuggestions", Process),
        server_only!("suggestExample", Process),
        server_only!("deleteSuggestion", Process),
        server_only!("askQuestion", Mcp),
        server_only!("explainQuery", Mcp),
        server_only!("getAssistantSettings", Process),
        server_only!("putAssistantSettings", Process),
        server_only!("listAsks", Process),
        server_only!("deleteAsks", Process),
        server_only!("askFeedback", Process),
        server_only!("modelUsage", Process),
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
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Library calls with no HTTP operation. Keep their binding decisions explicit too.
const EXTRA_KEYS: &[&str] = &[
    "catalog.open",
    "catalog.memory",
    "catalog.inspect",
    "catalog.get",
    "catalog.get_by_id",
    "catalog.attach",
    "catalog.rename",
    "catalog.reserve",
    "catalog.repositories",
    "dataset.open",
    "dataset.memory",
    "dataset.dataset_id",
    "dataset.head_commit",
    "dataset.select",
    "dataset.select_cursor",
    "dataset.select_cursor_with",
    "dataset.ask",
    "dataset.construct",
    "dataset.transaction",
    "dataset.quads",
    "dataset.insert",
    "dataset.remove",
    "dataset.clone_to_with",
    "dataset.clone_to_memory_with",
    "dataset.branch",
    "history.wait_for_commit",
    "history.prune",
    "history.tick",
    "indexes.vector.embed_until_idle",
    "repositories.open",
    "repositories.with_fixed",
];

#[allow(dead_code)]
fn check_library_only_calls(ds: &Dataset, cat: &sparkles::Catalog) {
    sparkles::Catalog::open("data", Default::default());
    sparkles::Catalog::memory(Default::default());
    sparkles::Catalog::inspect("data");
    cat.get("d");
    cat.get_by_id(ds.dataset_id());
    cat.attach("d", sparkles::catalog::Attach::Memory);
    cat.rename("d", "e");
    cat.reserve("d", sparkles::catalog::ReservationKind::Clone, "holder");
    Dataset::open("db");
    Dataset::memory();
    ds.dataset_id();
    ds.head_commit();
    ds.select("SELECT * {}");
    ds.select_cursor("SELECT * {}");
    ds.select_cursor_with("SELECT * {}", &Default::default(), &Default::default());
    ds.ask("ASK {}");
    ds.construct("CONSTRUCT {} {}");
    ds.transaction(|_| Ok(()));
    ds.quads(None, None, None, None);
    ds.insert(any());
    ds.remove(any());
    ds.branch("dev");
    ds.clone_to_with("clone", &Default::default(), &Default::default());
    ds.clone_to_memory_with("clone", &Default::default(), &Default::default());
    ds.history().wait_for_commit(0, std::time::Duration::ZERO);
    ds.history().prune();
    ds.history().tick();
    ds.indexes()
        .vector()
        .embed_until_idle(std::time::Duration::ZERO);
    #[cfg(feature = "backup")]
    {
        let repos = cat.repositories().unwrap();
        repos.open("r");
        repos.with_fixed(&[]);
    }
}

#[test]
fn every_surface_key_has_a_binding_decision() {
    use std::collections::BTreeSet;
    let mut keys: BTreeSet<&str> = entries()
        .iter()
        .filter_map(|(_, entry)| {
            if let Entry::Op(key) = entry {
                Some(*key)
            } else {
                None
            }
        })
        .collect();
    keys.extend(EXTRA_KEYS.iter().copied());
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings.toml");
    let bindings: toml::Table = std::fs::read_to_string(path)
        .expect("bindings.toml")
        .parse()
        .expect("valid bindings TOML");
    let mut problems = Vec::new();
    for key in &keys {
        match bindings.get(*key).and_then(toml::Value::as_table) {
            None => problems.push(format!("surface key `{key}` has no entry in `crates/sparkles/bindings.toml`. Add its python, jvm and node names, or planned or skip with a reason.")),
            Some(entry) => {
                for column in ["python", "jvm", "node"] {
                    let value = entry.get(column).and_then(toml::Value::as_str).unwrap_or("").trim();
                    if value.is_empty() || value == "planned:" || value == "skip:" {
                        problems.push(format!("surface key `{key}` needs a nonempty `{column}` binding decision"));
                    }
                }
                if entry.keys().any(|k| !["python", "jvm", "node"].contains(&k.as_str())) {
                    problems.push(format!("surface key `{key}` has an unknown binding column"));
                }
            }
        }
    }
    for key in bindings.keys() {
        if !keys.contains(key.as_str()) {
            problems.push(format!(
                "bindings.toml has stale surface key `{key}`; remove it or add its parity entry"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every source file under `dir` whose name ends with `ext`, concatenated.
fn read_sources(dir: &std::path::Path, ext: &str, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            read_sources(&path, ext, out);
        } else if path.to_string_lossy().ends_with(ext) {
            out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            out.push('\n');
        }
    }
}

/// The identifiers a binding name refers to: its type (or top-level function) and each
/// member on the path to the operation. A name can list alternatives with ` / ` or ` + `,
/// and members with `/`. Arguments in parentheses and a trailing note in parentheses are
/// ignored. A name that starts with `Jena ` is Jena's own API, which this repository does
/// not declare, and so is an alternative that starts with `Jena `. They yield nothing to
/// check.
fn binding_identifiers(name: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let name = name.split(" (").next().unwrap_or(name).trim();
    if name.starts_with("Jena ") {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for alternative in name.split(" / ").flat_map(|a| a.split(" + ")) {
        let alternative = alternative.trim();
        if alternative.starts_with("Jena ") {
            continue;
        }
        let mut plain = String::new();
        let mut depth = 0usize;
        for c in alternative.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ if depth == 0 => plain.push(c),
                _ => {}
            }
        }
        let mut segments = plain.split('.');
        let head = segments.next().unwrap_or("").to_string();
        let members: Vec<String> = segments
            .flat_map(|s| s.split('/'))
            .map(str::to_string)
            .collect();
        for ident in std::iter::once(&head).chain(&members) {
            if ident.is_empty() || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!(
                    "cannot read `{alternative}`; name a type and its members, or start with `Jena `"
                ));
            }
        }
        out.push((head, members));
    }
    Ok(out)
}

/// Each concrete JVM and Node name in bindings.toml is declared in the binding's sources,
/// so that the table cannot name an operation that does not exist. The check is textual.
/// It finds a declaration of each identifier anywhere in the binding's sources, not on
/// the named type.
#[test]
fn named_jvm_and_node_bindings_exist_in_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut kotlin = String::new();
    read_sources(
        &root.join("jvm/sparkles-jena/src/main/kotlin"),
        ".kt",
        &mut kotlin,
    );
    let mut typescript = String::new();
    read_sources(&root.join("js/engine/src"), ".ts", &mut typescript);
    if kotlin.is_empty() && typescript.is_empty() {
        eprintln!("the binding sources are not in this checkout, so the check is skipped");
        return;
    }
    assert!(
        !kotlin.is_empty(),
        "no Kotlin sources under jvm/sparkles-jena"
    );
    assert!(
        !typescript.is_empty(),
        "no TypeScript sources under js/engine/src"
    );
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/bindings.toml");
    let bindings: toml::Table = std::fs::read_to_string(path)
        .expect("bindings.toml")
        .parse()
        .expect("valid bindings TOML");

    let declared = |column: &str, ident: &str, is_type: bool| -> bool {
        let ident = regex::escape(ident);
        let pattern = match (column, is_type) {
            ("jvm", true) => format!(r"\b(?:class|object|interface)\s+{ident}\b"),
            ("jvm", false) => {
                format!(r"\b(?:fun|val|var)\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?{ident}\b")
            }
            (_, true) => format!(r"\b(?:class|interface|type)\s+{ident}\b"),
            _ => format!(
                r"(?m)(?:^|[\s{{,;])(?:(?:get|function|readonly|async|static|public)\s+)*{ident}\s*[(<=:?]"
            ),
        };
        let source = if column == "jvm" {
            &kotlin
        } else {
            &typescript
        };
        regex::Regex::new(&pattern).unwrap().is_match(source)
    };

    let mut problems = Vec::new();
    for (key, entry) in &bindings {
        for column in ["jvm", "node"] {
            let Some(value) = entry.get(column).and_then(toml::Value::as_str) else {
                continue;
            };
            if value.starts_with("planned:") || value.starts_with("skip:") {
                continue;
            }
            let names = match binding_identifiers(value) {
                Ok(names) => names,
                Err(e) => {
                    problems.push(format!("`{key}` {column}: {e}"));
                    continue;
                }
            };
            for (head, members) in names {
                let is_type = head.starts_with(|c: char| c.is_ascii_uppercase());
                if !declared(column, &head, is_type) {
                    problems.push(format!("`{key}` {column}: `{head}` is not declared"));
                }
                for member in members {
                    if !declared(column, &member, false) {
                        problems.push(format!(
                            "`{key}` {column}: `{head}` has no member `{member}`"
                        ));
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn binding_identifiers_reads_the_table_forms() {
    assert_eq!(
        binding_identifiers("DatasetGraphSparkles.backups(repository).create").unwrap(),
        vec![(
            "DatasetGraphSparkles".to_string(),
            vec!["backups".to_string(), "create".to_string()]
        )]
    );
    assert!(
        binding_identifiers("Jena UpdateAction.parseExecute / UpdateExec.dataset(ds)")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        binding_identifiers("DatasetGraphSparkles.getGraph(graph).find + Jena RDFDataMgr.write")
            .unwrap(),
        vec![(
            "DatasetGraphSparkles".to_string(),
            vec!["getGraph".to_string(), "find".to_string()]
        )]
    );
    assert_eq!(
        binding_identifiers(
            "Dataset.indexes.vector.embedUntilIdle (bounded waitMs; no AbortSignal)"
        )
        .unwrap()[0]
            .1
            .len(),
        3
    );
    assert_eq!(
        binding_identifiers("Dataset.branches.rename/protect/note").unwrap()[0]
            .1
            .len(),
        4
    );
}
