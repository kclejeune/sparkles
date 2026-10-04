//! `sparkles branch` and `sparkles merge`: the branches of a database, on a local
//! directory or on a server, and the global `--branch` option of the commands that
//! open a database.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value as J, json};
use sparkles::branch::{
    BranchInfo, BranchOptions, ConflictReport, ConflictScope, MAIN, MergeOptions, MergeOutcome,
    MergeReport, NamedCommitRef, Take,
};
use sparkles::store::{Store, StoreOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The branch the global `--branch` chose (`None`: main).
static BRANCH: OnceLock<Option<String>> = OnceLock::new();

/// Record the global `--branch` (once, at start).
pub fn set_branch(b: Option<String>) {
    let _ = BRANCH.set(b.filter(|b| b != MAIN));
}

/// The global `--branch`, unless it is `main`.
pub fn branch() -> Option<&'static str> {
    BRANCH.get().and_then(|b| b.as_deref())
}

/// A database opened for the global `--branch`: the branch's store, which the
/// dataset's own store keeps open.
pub struct Db {
    main: Store,
    branch: Option<Arc<Store>>,
}

impl std::ops::Deref for Db {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.branch.as_deref().unwrap_or(&self.main)
    }
}

impl From<Store> for Db {
    fn from(main: Store) -> Db {
        Db { main, branch: None }
    }
}

/// Open the database `loc` and choose the global `--branch` in it.
pub fn open_db(loc: &Path, opts: StoreOptions) -> Result<Db> {
    let main = Store::open(loc, opts)?;
    let branch = match branch() {
        Some(b) => Some(
            main.branch(b)?
                .shared()
                .expect("a branch other than main has its own store"),
        ),
        None => None,
    };
    Ok(Db { main, branch })
}

/// The directory of branch `name` of database `loc`, from its branch table (for the
/// commands that read files without opening the database).
pub fn branch_dir(loc: &Path, name: &str) -> Result<PathBuf> {
    let t = sparkles::store::read_branch_table(loc)?
        .with_context(|| format!("{} has no branches", loc.display()))?;
    let id = t["branches"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["name"] == name))
        .and_then(|b| b["id"].as_str())
        .with_context(|| format!("no such branch: {name}"))?;
    Ok(loc.join(sparkles::store::BRANCHES_DIR).join(id))
}

/// A dataset name with the global `--branch`, in the path form (`ds@branch`) that
/// every dataset endpoint of a server takes.
pub fn remote_name(ds: &str) -> String {
    match branch() {
        Some(b) => format!("{ds}@{b}"),
        None => ds.to_string(),
    }
}

// ------------------------------------------------------------------ branch ------

#[derive(Args, Debug)]
pub struct Target {
    /// Database directory
    #[arg(long, required_unless_present = "server", conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// A server to ask instead of a local database (with --dataset)
    #[arg(long, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset on --server
    #[arg(long, requires = "server")]
    pub dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    pub insecure_http: bool,
}

#[derive(Subcommand, Debug)]
pub enum BranchCmd {
    /// List the branches (reads the files when a server holds the database)
    List {
        #[command(flatten)]
        target: Target,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Create a branch from a commit of another branch
    Create {
        #[command(flatten)]
        target: Target,
        name: String,
        /// The branch to start from (default: main)
        #[arg(long)]
        from: Option<String>,
        /// The commit of --from to start at: N, commit:N, time:<RFC 3339>,
        /// snapshot:NAME (default: its head)
        #[arg(long)]
        at: Option<String>,
        /// Refuse writes other than merges
        #[arg(long)]
        protected: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// Show one branch
    Show {
        #[command(flatten)]
        target: Target,
        name: String,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Delete a branch: its commits, snapshots and storage
    Delete {
        #[command(flatten)]
        target: Target,
        name: String,
        /// Delete it even with commits its upstream does not have
        #[arg(long)]
        force: bool,
    },
    /// Protect a branch (it takes changes through merges only), or stop (--off)
    Protect {
        #[command(flatten)]
        target: Target,
        name: String,
        #[arg(long)]
        off: bool,
    },
}

pub fn run_branch(cmd: BranchCmd, opts: StoreOptions) -> Result<()> {
    match cmd {
        BranchCmd::List { target, format } => {
            let list = match &target.loc {
                Some(loc) => match Store::open(loc, opts) {
                    Ok(s) => s.branches()?.iter().map(branch_json).collect(),
                    // held by a server: read the files
                    Err(e) if e.to_string().contains("in use") => offline_list(loc)?,
                    Err(e) => return Err(e.into()),
                },
                None => {
                    let j = remote_get(&target, "")?;
                    j["branches"].as_array().cloned().unwrap_or_default()
                }
            };
            print_list(&list, &format)
        }
        BranchCmd::Create {
            target,
            name,
            from,
            at,
            protected,
            note,
        } => {
            let b = match &target.loc {
                Some(loc) => {
                    let s = Store::open(loc, opts)?;
                    let o = BranchOptions {
                        from: from.unwrap_or_else(|| MAIN.to_string()),
                        at: at.as_deref().unwrap_or("head").parse()?,
                        protected,
                        note,
                    };
                    branch_json(&s.create_branch(&name, &o)?)
                }
                None => {
                    let mut body = json!({ "name": name, "protected": protected });
                    if let Some(f) = from {
                        body["from"] = f.into();
                    }
                    if let Some(a) = at {
                        body["at"] = a.into();
                    }
                    if let Some(n) = note {
                        body["note"] = n.into();
                    }
                    remote_send(&target, "POST", "", Some(body))?
                }
            };
            eprintln!(
                "created branch {} from {}@{}{}",
                b["name"].as_str().unwrap_or_default(),
                b["from"]["branch"].as_str().unwrap_or("?"),
                b["from"]["seq"],
                if b["storage"]["linked"] == true {
                    " (linked: it shares its upstream's index)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        BranchCmd::Show {
            target,
            name,
            format,
        } => {
            let b = match &target.loc {
                Some(loc) => {
                    let s = Store::open(loc, opts)?;
                    branch_json(&s.branch_info(&name)?)
                }
                None => remote_get(&target, &format!("/{name}"))?,
            };
            if format == "json" {
                println!("{}", serde_json::to_string_pretty(&b)?);
            } else {
                print_list(&[b], "text")?;
            }
            Ok(())
        }
        BranchCmd::Delete {
            target,
            name,
            force,
        } => {
            match &target.loc {
                Some(loc) => Store::open(loc, opts)?.delete_branch(&name, force)?,
                None => {
                    let q = if force { "?force=true" } else { "" };
                    remote_send(&target, "DELETE", &format!("/{name}{q}"), None)?;
                }
            }
            eprintln!("deleted branch {name}");
            Ok(())
        }
        BranchCmd::Protect { target, name, off } => {
            match &target.loc {
                Some(loc) => {
                    Store::open(loc, opts)?.set_branch_protected(&name, !off)?;
                }
                None => {
                    remote_send(
                        &target,
                        "PATCH",
                        &format!("/{name}"),
                        Some(json!({ "protected": !off })),
                    )?;
                }
            }
            eprintln!(
                "branch {name} is {}",
                if off { "not protected" } else { "protected" }
            );
            Ok(())
        }
    }
}

/// A branch as the server's API gives it.
fn branch_json(b: &BranchInfo) -> J {
    json!({
        "name": b.name,
        "id": b.id,
        "ordinal": b.ordinal,
        "head": b.head.map(|h| h.seq),
        "modified": b.head.map(|h| h.timestamp()),
        "from": b.from.as_ref().map(|f| json!({ "branch": f.branch, "branchId": f.branch_id, "seq": f.seq })),
        "upstream": b.upstream,
        "mergeBase": b.merge_base.as_ref().map(|m| json!({ "branch": m.branch, "seq": m.seq })),
        "ahead": b.ahead,
        "behind": b.behind,
        "protected": b.protected,
        "note": b.note,
        "created": sparkles::commit::rfc3339_ms(b.created_ms),
        "storage": {
            "linked": b.storage.linked,
            "ownBytes": b.storage.own_bytes,
            "heldBytes": b.storage.held_bytes,
            "generation": b.storage.generation,
        },
    })
}

/// The branches of a database held by another process, from its files: names, heads
/// and starting points (ahead and behind need the database open).
fn offline_list(loc: &Path) -> Result<Vec<J>> {
    let head_of = |dir: &Path| -> Option<u64> {
        sparkles::commit::read_catalog(&dir.join("commits.bin"))
            .ok()
            .flatten()
            .and_then(|(_, r)| r.last().map(|c| c.seq))
    };
    let current = |dir: &Path| {
        std::fs::read_to_string(dir.join("CURRENT"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let t = sparkles::store::read_branch_table(loc)?.unwrap_or(json!({}));
    let mut out = vec![json!({
        "name": MAIN,
        "head": head_of(loc),
        "from": null,
        "protected": t["main"]["protected"].as_bool().unwrap_or(false),
        "note": t["main"]["note"],
        "storage": { "linked": false, "generation": current(loc) },
    })];
    let ids: std::collections::HashMap<String, String> = t["branches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            Some((
                b["id"].as_str()?.to_string(),
                b["name"].as_str()?.to_string(),
            ))
        })
        .collect();
    let main_id = t["datasetId"].as_str().unwrap_or_default().to_string();
    let mut bs: Vec<J> = t["branches"].as_array().cloned().unwrap_or_default();
    bs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    for b in bs {
        let id = b["id"].as_str().unwrap_or_default();
        let dir = loc.join(sparkles::store::BRANCHES_DIR).join(id);
        let from_id = b["from"]["branchId"].as_str().unwrap_or_default();
        let from_name = if from_id == main_id {
            Some(MAIN.to_string())
        } else {
            ids.get(from_id).cloned()
        };
        let own = |d: &Path| -> u64 {
            fn size(p: &Path) -> u64 {
                std::fs::read_dir(p)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| match e.metadata() {
                                Ok(m) if m.is_dir() => size(&e.path()),
                                Ok(m) => m.len(),
                                Err(_) => 0,
                            })
                            .sum()
                    })
                    .unwrap_or(0)
            }
            size(d)
        };
        out.push(json!({
            "name": b["name"],
            "id": id,
            "head": head_of(&dir),
            "from": { "branch": from_name, "branchId": from_id, "seq": b["from"]["seq"] },
            "upstream": from_name,
            "protected": b["protected"],
            "note": b["note"],
            "created": b["created"],
            "storage": {
                "linked": b["holds"].as_array().is_some_and(|h| !h.is_empty()),
                "ownBytes": own(&dir),
                "generation": current(&dir),
            },
        }));
    }
    Ok(out)
}

fn human(b: &J) -> String {
    b.as_u64()
        .map(sparkles::error::human_bytes)
        .unwrap_or_default()
}

fn print_list(list: &[J], format: &str) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(list)?);
        return Ok(());
    }
    if format != "text" {
        bail!("--format: text or json, not {format}");
    }
    let rows: Vec<[String; 7]> = list
        .iter()
        .map(|b| {
            let num = |v: &J| v.as_u64().map_or("-".to_string(), |n| n.to_string());
            let from = match b["from"]["branch"].as_str() {
                Some(f) => format!("{f}@{}", b["from"]["seq"]),
                None if b["from"].is_object() => format!("(deleted)@{}", b["from"]["seq"]),
                None => "-".into(),
            };
            let storage = if b["storage"]["linked"] == true {
                format!("linked ({})", human(&b["storage"]["ownBytes"]))
            } else {
                b["storage"]["generation"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            };
            let main = b["name"] == MAIN;
            let mut note = b["note"].as_str().unwrap_or_default().to_string();
            if b["protected"] == true {
                note = format!("[protected] {note}").trim_end().to_string();
            }
            [
                b["name"].as_str().unwrap_or_default().to_string(),
                num(&b["head"]),
                from,
                if main { "-".into() } else { num(&b["ahead"]) },
                if main { "-".into() } else { num(&b["behind"]) },
                storage,
                note,
            ]
        })
        .collect();
    let head = [
        "branch", "head", "from", "ahead", "behind", "storage", "note",
    ];
    let mut w = [0usize; 7];
    for r in rows
        .iter()
        .map(|r| r.each_ref().map(|c| c.chars().count()))
        .chain([head.map(str::len)])
    {
        for (i, n) in r.iter().enumerate() {
            w[i] = w[i].max(*n);
        }
    }
    let line = |c: [&str; 7]| {
        format!(
            "{:<w0$}  {:>w1$}  {:<w2$}  {:>w3$}  {:>w4$}  {:<w5$}  {}",
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[5],
            c[6],
            w0 = w[0],
            w1 = w[1],
            w2 = w[2],
            w3 = w[3],
            w4 = w[4],
            w5 = w[5],
        )
        .trim_end()
        .to_string()
    };
    println!("{}", line(head));
    for r in &rows {
        println!(
            "{}",
            line([&r[0], &r[1], &r[2], &r[3], &r[4], &r[5], &r[6]])
        );
    }
    Ok(())
}

#[cfg(feature = "auth")]
fn remote_get(t: &Target, rest: &str) -> Result<J> {
    remote_send(t, "GET", rest, None)
}

#[cfg(feature = "auth")]
fn remote_send(t: &Target, method: &str, rest: &str, body: Option<J>) -> Result<J> {
    use crate::remote::{JsonBody, Remote};
    let method = reqwest::Method::from_bytes(method.as_bytes())?;
    let ds = t
        .dataset
        .as_deref()
        .context("--dataset NAME is required with --server")?;
    let r = Remote::open(t.server.as_deref(), t.insecure_http)?;
    let mut req = r.req(method.clone(), &format!("/$/branches/{ds}{rest}"));
    if let Some(b) = body {
        req = req
            .header("content-type", "application/json")
            .body(b.to_string());
    }
    let resp = r.check(req.send(), Some(ds))?;
    if method == reqwest::Method::DELETE {
        return Ok(J::Null);
    }
    resp.json_value()
}

#[cfg(not(feature = "auth"))]
fn remote_get(_: &Target, _: &str) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

#[cfg(not(feature = "auth"))]
fn remote_send(_: &Target, _: &str, _: &str, _: Option<J>) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

// ------------------------------------------------------------------- merge ------

#[derive(Args, Debug)]
pub struct MergeArgs {
    #[command(flatten)]
    pub target: Target,
    /// The branch to merge
    pub source: String,
    /// The branch to merge into (default: main)
    #[arg(long, default_value = "main")]
    pub into: String,
    /// Refuse anything but a fast-forward
    #[arg(long)]
    pub ff_only: bool,
    /// What counts as one value when both sides changed it: cell, subject or quad
    #[arg(long, default_value = "cell")]
    pub conflicts: String,
    /// The rule for conflicts --resolve does not cover: fail, ours, theirs or union
    #[arg(long, default_value = "fail")]
    pub on_conflict: String,
    /// A JSON array of resolutions ({graph, subject?, predicate?, take, objects?})
    #[arg(long, value_name = "FILE.json")]
    pub resolve: Option<PathBuf>,
    /// The source head the conflicts were read at
    #[arg(long)]
    pub expect_source: Option<u64>,
    /// The target head the conflicts were read at
    #[arg(long)]
    pub expect_target: Option<u64>,
    /// Merge the inferred graph too
    #[arg(long)]
    pub include_inferences: bool,
    /// The merge commit's message
    #[arg(long)]
    pub message: Option<String>,
    /// Show what the merge would do, and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// text or json
    #[arg(long, default_value = "text")]
    pub format: String,
}

/// The exit status of a merge stopped by conflicts.
pub const CONFLICT_EXIT: i32 = 2;

/// `sparkles merge`: 0 after a merge or when up to date, 2 when conflicts stopped it.
pub fn run_merge(a: MergeArgs, opts: StoreOptions) -> Result<i32> {
    if !matches!(a.format.as_str(), "text" | "json") {
        bail!("--format: text or json, not {}", a.format);
    }
    let resolutions: Option<J> = match &a.resolve {
        Some(f) => Some(
            serde_json::from_slice(
                &std::fs::read(f).with_context(|| format!("reading {}", f.display()))?,
            )
            .with_context(|| format!("{}: not JSON", f.display()))?,
        ),
        None => None,
    };
    let out = match &a.target.loc {
        Some(loc) => local_merge(loc, &a, resolutions.as_ref(), opts)?,
        None => remote_merge(&a, resolutions)?,
    };
    let code = match &out {
        Out::Conflicts(_) => CONFLICT_EXIT,
        _ => 0,
    };
    print_merge(&out, &a)?;
    Ok(code)
}

enum Out {
    Done(J),
    Conflicts(J),
}

fn report_json(r: &MergeReport) -> J {
    let side = |c: &NamedCommitRef| json!({ "branch": c.branch, "seq": c.seq });
    json!({
        "merged": r.merged,
        "upToDate": r.up_to_date,
        "fastForward": r.fast_forward,
        "source": side(&r.source),
        "target": side(&r.target),
        "base": r.base.as_ref().map(side),
        "changes": { "inserted": r.inserted, "deleted": r.deleted },
        "conflicts": { "found": r.conflicts_found, "resolved": r.conflicts_resolved },
        "commit": r.commit.as_ref().filter(|c| c.committed).map(|c| c.commit.seq),
        "inferences": r.inferences_excluded.map(|n| json!({ "excluded": n })),
    })
}

fn conflicts_json(c: &ConflictReport) -> J {
    serde_json::to_value(c).unwrap_or_default()
}

fn local_merge(loc: &Path, a: &MergeArgs, res: Option<&J>, opts: StoreOptions) -> Result<Out> {
    let s = Store::open(loc, opts)?;
    // the target's write-time validation, as for any write
    if a.into == MAIN {
        crate::write_validation::install(&s)?;
    } else {
        let b = s.branch(&a.into)?;
        crate::write_validation::install(&b)?;
    }
    let mut o = MergeOptions {
        ff_only: a.ff_only,
        scope: ConflictScope::parse(&a.conflicts)
            .with_context(|| format!("--conflicts: cell, subject or quad, not {}", a.conflicts))?,
        on_conflict: match a.on_conflict.as_str() {
            "fail" => None,
            t => Some(
                Take::parse(t)
                    .filter(|t| *t != Take::Base)
                    .with_context(|| {
                        format!("--on-conflict: fail, ours, theirs or union, not {t}")
                    })?,
            ),
        },
        expect_source: a.expect_source,
        expect_target: a.expect_target,
        include_inferences: a.include_inferences,
        ..Default::default()
    };
    if let Some(m) = &a.message {
        o.write.message = sparkles::annotations::validate_message(m)?;
    }
    if let Some(rs) = res {
        for r in rs
            .as_array()
            .context("--resolve: a JSON array of resolutions")?
        {
            o.resolutions.push(parse_resolution(r)?);
        }
    }
    if a.dry_run {
        let r = s.preview_merge(&a.source, &a.into, &o)?;
        return Ok(match &r.conflicts {
            Some(c) => Out::Conflicts(conflicts_json(c)),
            None => Out::Done(report_json(&r)),
        });
    }
    Ok(match s.merge(&a.source, &a.into, &o)? {
        MergeOutcome::UpToDate(r) | MergeOutcome::Merged(r) => Out::Done(report_json(&r)),
        MergeOutcome::Conflicts(c) => Out::Conflicts(conflicts_json(&c)),
    })
}

/// A resolution of `--resolve`, in the API's form.
fn parse_resolution(v: &J) -> Result<sparkles::branch::Resolution> {
    let term = |s: &str| -> Result<oxrdf::Term> {
        let doc = format!("<urn:x-s> <urn:x-p> {s} .");
        let q = oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
            .for_slice(doc.as_bytes())
            .next()
            .with_context(|| format!("not a term: {s}"))?
            .with_context(|| format!("not a term: {s}"))?;
        Ok(q.object)
    };
    let graph = match v.get("graph") {
        None | Some(J::Null) => oxrdf::GraphName::DefaultGraph,
        Some(J::String(s)) => match term(s)? {
            oxrdf::Term::NamedNode(n) => oxrdf::GraphName::NamedNode(n),
            oxrdf::Term::BlankNode(b) => oxrdf::GraphName::BlankNode(b),
            _ => bail!("graph: an IRI or blank node"),
        },
        _ => bail!("graph: a string or null"),
    };
    let subject = v.get("subject").and_then(J::as_str).map(term).transpose()?;
    let predicate = match v
        .get("predicate")
        .and_then(J::as_str)
        .map(term)
        .transpose()?
    {
        Some(oxrdf::Term::NamedNode(n)) => Some(n),
        Some(_) => bail!("predicate: an IRI"),
        None => None,
    };
    let take = match v.get("take").and_then(J::as_str) {
        Some("objects") => Take::Objects(
            v.get("objects")
                .and_then(J::as_array)
                .context("take objects needs objects")?
                .iter()
                .map(|o| term(o.as_str().context("objects: strings")?))
                .collect::<Result<_>>()?,
        ),
        Some(t) => Take::parse(t)
            .with_context(|| format!("take: ours, theirs, base, union or objects, not {t}"))?,
        None => bail!("a resolution needs take"),
    };
    Ok(sparkles::branch::Resolution {
        graph,
        subject,
        predicate,
        take,
    })
}

#[cfg(feature = "auth")]
fn remote_merge(a: &MergeArgs, res: Option<J>) -> Result<Out> {
    use crate::remote::{JsonBody, Remote};
    let ds = a
        .target
        .dataset
        .as_deref()
        .context("--dataset NAME is required with --server")?;
    let r = Remote::open(a.target.server.as_deref(), a.target.insecure_http)?;
    let mut body = json!({
        "source": a.source,
        "target": a.into,
        "ff": if a.ff_only { "only" } else { "auto" },
        "conflicts": a.conflicts,
        "onConflict": a.on_conflict,
        "inferences": if a.include_inferences { "include" } else { "exclude" },
    });
    if let Some(res) = res {
        body["resolutions"] = res;
    }
    if a.expect_source.is_some() || a.expect_target.is_some() {
        body["expect"] = json!({ "source": a.expect_source, "target": a.expect_target });
    }
    if let Some(m) = &a.message {
        body["message"] = m.clone().into();
    }
    let resp = if a.dry_run {
        let q: String = form_urlencoded::Serializer::new(String::new())
            .append_pair("source", &a.source)
            .append_pair("target", &a.into)
            .append_pair("conflicts", &a.conflicts)
            .append_pair("onConflict", &a.on_conflict)
            .finish();
        r.req(reqwest::Method::GET, &format!("/$/merge/{ds}?{q}"))
            .send()
    } else {
        r.req(reqwest::Method::POST, &format!("/$/merge/{ds}"))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
    }
    .with_context(|| format!("cannot reach {}", r.base))?;
    if resp.status() == reqwest::StatusCode::CONFLICT {
        let j = resp.json_value()?;
        if j["code"] == "merge-conflict" {
            return Ok(Out::Conflicts(j));
        }
        bail!("409: {}", j["error"].as_str().unwrap_or_default());
    }
    let j = r.check(Ok(resp), Some(ds))?.json_value()?;
    if a.dry_run && j["conflictCount"].as_u64().unwrap_or(0) > 0 {
        return Ok(Out::Conflicts(j));
    }
    Ok(Out::Done(j))
}

#[cfg(not(feature = "auth"))]
fn remote_merge(_: &MergeArgs, _: Option<J>) -> Result<Out> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

/// A term as text: a number or string literal's lexical form, else N-Triples.
fn short(t: &J) -> String {
    let s = t.as_str().unwrap_or_default();
    if let Some(rest) = s.strip_prefix('"')
        && let Some((lex, dt)) = rest.split_once("\"^^<")
        && dt.starts_with("http://www.w3.org/2001/XMLSchema#")
    {
        return lex.to_string();
    }
    s.to_string()
}

fn print_merge(out: &Out, a: &MergeArgs) -> Result<()> {
    let j = match out {
        Out::Done(j) | Out::Conflicts(j) => j,
    };
    if a.format == "json" {
        println!("{}", serde_json::to_string_pretty(j)?);
        return Ok(());
    }
    let base = match j["base"]["branch"].as_str() {
        Some(b) => format!(", base {b}@{}", j["base"]["seq"]),
        None => String::new(),
    };
    println!(
        "merge {} (commit {}) into {} (commit {}){base}",
        a.source, j["source"]["seq"], a.into, j["target"]["seq"]
    );
    match out {
        Out::Done(j) if j["upToDate"] == true => println!("already up to date, nothing merged"),
        Out::Done(j) => {
            let what = if j["fastForward"] == true {
                "fast-forward"
            } else {
                "three-way merge"
            };
            let verb = if a.dry_run { "would merge" } else { "merged" };
            let commit = match j["commit"].as_u64().or(j["commit"]["seq"].as_u64()) {
                Some(c) => format!(" as commit {c}"),
                None => String::new(),
            };
            println!(
                "{verb} ({what}): +{} -{}{commit}; {} conflicts, {} resolved",
                j["changes"]["inserted"],
                j["changes"]["deleted"],
                j["conflicts"]["found"].as_u64().unwrap_or(0),
                j["conflicts"]["resolved"].as_u64().unwrap_or(0)
            );
        }
        Out::Conflicts(j) => {
            let ours = j["target"]["branch"]
                .as_str()
                .unwrap_or(&a.into)
                .to_string();
            let theirs = j["source"]["branch"]
                .as_str()
                .unwrap_or(&a.source)
                .to_string();
            for c in j["cells"].as_array().into_iter().flatten() {
                let graph = c["graph"].as_str().unwrap_or("(default graph)");
                println!(
                    "CONFLICT  {} {}  {graph}",
                    c["subject"].as_str().unwrap_or_default(),
                    c["predicate"].as_str().unwrap_or("")
                );
                let vals = |v: &J| {
                    v.as_array()
                        .into_iter()
                        .flatten()
                        .map(short)
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                println!("  base    {}", vals(&c["base"]));
                println!("  ours    {}   ({ours})", vals(&c["ours"]));
                println!("  theirs  {}   ({theirs})", vals(&c["theirs"]));
            }
            let n = j["conflicts"]
                .as_u64()
                .or(j["conflictCount"].as_u64())
                .unwrap_or(0);
            println!(
                "{n} conflict{}, nothing merged. Resolve with --on-conflict or --resolve FILE.",
                if n == 1 { "" } else { "s" }
            );
        }
    }
    Ok(())
}
