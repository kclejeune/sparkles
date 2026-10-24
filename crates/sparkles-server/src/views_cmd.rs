//! `sparkles views`: the incremental views of a local database directory or of a
//! server (spec F13 §2.4), and `sparkles query --live`, which follows a live query on a
//! server.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value as J, json};
use sparkles::incr::ViewDef;
use sparkles::store::StoreOptions;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Args, Debug)]
pub struct ViewsArgs {
    #[command(subcommand)]
    pub cmd: ViewsCmd,
    /// Database directory
    #[arg(long, global = true, conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// text or json
    #[arg(long, global = true, default_value = "text")]
    pub format: String,
    /// A server to ask instead of a local database (with --dataset); changing views
    /// there needs admin on the dataset
    #[arg(long, global = true, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset on --server
    #[arg(long, global = true)]
    pub dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long, global = true)]
    pub insecure_http: bool,
}

#[derive(Subcommand, Debug)]
pub enum ViewsCmd {
    /// List the views with their state
    List,
    /// Show a view with its state and plan
    Get { name: String },
    /// Define or replace a view from a query, or from a JSON definition with --def
    Put {
        name: String,
        /// The query (or use --query-file or --def)
        query: Option<String>,
        /// A file holding the query
        #[arg(long, value_name = "FILE")]
        query_file: Option<PathBuf>,
        /// A file holding the view's JSON definition
        #[arg(long, value_name = "FILE", conflicts_with_all = ["query", "query_file"])]
        def: Option<PathBuf>,
        /// The user whose view of the dataset the query reads
        #[arg(long)]
        principal: Option<String>,
        /// Wait this many seconds for the first evaluation (local databases)
        #[arg(long, default_value_t = 600.0)]
        wait: f64,
    },
    /// Remove a view and its state
    Delete { name: String },
    /// Evaluate a view again from scratch
    Rebuild { name: String },
    /// Show the plan a query would have as a view, with each operator's mode
    Explain {
        /// The query (or use --query-file)
        query: Option<String>,
        #[arg(long, value_name = "FILE")]
        query_file: Option<PathBuf>,
    },
}

fn query_text(q: &Option<String>, f: &Option<PathBuf>) -> Result<String> {
    match (q, f) {
        (Some(q), _) => Ok(q.clone()),
        (None, Some(f)) => std::fs::read_to_string(f).with_context(|| f.display().to_string()),
        (None, None) => bail!("give the query, or --query-file FILE"),
    }
}

fn definition(cmd: &ViewsCmd) -> Result<Option<ViewDef>> {
    let ViewsCmd::Put {
        query,
        query_file,
        def,
        principal,
        ..
    } = cmd
    else {
        return Ok(None);
    };
    let mut d: ViewDef = match def {
        Some(f) => serde_json::from_slice(&std::fs::read(f)?)
            .with_context(|| format!("{}: not a view definition", f.display()))?,
        None => ViewDef::table(query_text(query, query_file)?),
    };
    if principal.is_some() {
        d.principal = principal.clone();
    }
    Ok(Some(d))
}

pub fn run(a: ViewsArgs, opts: StoreOptions) -> Result<()> {
    let out = match &a.loc {
        Some(loc) => local(loc, &a, opts)?,
        None => remote(&a)?,
    };
    print(&out, &a.format)
}

fn local(loc: &std::path::Path, a: &ViewsArgs, opts: StoreOptions) -> Result<J> {
    let ds = crate::open_dataset(loc, opts)?;
    let views = ds.views();
    // the views attach and recover on the views' thread: report them once they have
    views.settle(Duration::from_secs(60));
    Ok(match &a.cmd {
        ViewsCmd::List => json!({ "views": views.list() }),
        ViewsCmd::Get { name } => {
            serde_json::to_value(views.get(name).with_context(|| format!("no view {name}"))?)?
        }
        ViewsCmd::Put { name, wait, .. } => {
            let def = definition(&a.cmd)?.expect("a put");
            views.put(name, def)?;
            let v = views.wait_built(name, Duration::from_secs_f64(wait.max(0.0)))?;
            serde_json::to_value(v)?
        }
        ViewsCmd::Delete { name } => {
            if !views.delete(name)? {
                bail!("no view {name}");
            }
            json!({ "deleted": name })
        }
        ViewsCmd::Rebuild { name } => {
            views.rebuild(name)?;
            serde_json::to_value(views.wait_built(name, Duration::from_secs(3600))?)?
        }
        ViewsCmd::Explain { query, query_file } => {
            let (kind, plan) = views.explain(&query_text(query, query_file)?)?;
            json!({ "kind": kind, "plan": plan })
        }
    })
}

#[cfg(feature = "auth")]
fn remote(a: &ViewsArgs) -> Result<J> {
    use crate::remote::{JsonBody, Remote};
    use reqwest::Method;
    let Some(ds) = a.dataset.as_deref() else {
        bail!("give --loc (a local database) or --server URL --dataset NAME");
    };
    let r = Remote::open(a.server.as_deref(), a.insecure_http)?;
    let enc = |s: &str| -> String { form_urlencoded::byte_serialize(s.as_bytes()).collect() };
    let base = format!("/$/views/{}", enc(ds));
    let req = match &a.cmd {
        ViewsCmd::List => r.req(Method::GET, &base),
        ViewsCmd::Get { name } => r.req(Method::GET, &format!("{base}/{}", enc(name))),
        ViewsCmd::Put { name, .. } => {
            let def = definition(&a.cmd)?.expect("a put");
            r.req(Method::PUT, &format!("{base}/{}", enc(name)))
                .header("content-type", "application/json")
                .body(serde_json::to_string(&def)?)
        }
        ViewsCmd::Delete { name } => {
            r.check(
                r.req(Method::DELETE, &format!("{base}/{}", enc(name)))
                    .send(),
                Some(ds),
            )?;
            return Ok(json!({ "deleted": name }));
        }
        ViewsCmd::Rebuild { name } => r.req(Method::POST, &format!("{base}/{}/rebuild", enc(name))),
        ViewsCmd::Explain { .. } => {
            bail!(
                "views explain runs on a local database (--loc); a server shows a view's plan in views get"
            )
        }
    };
    r.check(req.send(), Some(ds))?.json_value()
}

#[cfg(not(feature = "auth"))]
fn remote(_: &ViewsArgs) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

fn state_text(v: &J) -> String {
    match v["state"].as_str().unwrap_or("?") {
        "behind" => format!("behind {} commits", v["commits"]),
        "suspended" => format!("suspended: {}", v["reason"].as_str().unwrap_or("")),
        "failed" => format!("failed: {}", v["error"].as_str().unwrap_or("")),
        s => s.to_string(),
    }
}

fn plan_text(p: &J, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    let mode = p["mode"].as_str().unwrap_or("");
    out.push_str(&format!(
        "{pad}{} [{mode}] {}\n",
        p["operator"].as_str().unwrap_or("?"),
        p["detail"].as_str().unwrap_or("")
    ));
    for c in p["children"].as_array().into_iter().flatten() {
        plan_text(c, depth + 1, out);
    }
}

fn print(out: &J, format: &str) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(out)?);
        return Ok(());
    }
    if let Some(views) = out["views"].as_array() {
        if views.is_empty() {
            println!("no views");
        }
        for v in views {
            println!(
                "{}  {}  {} rows  {}",
                v["name"].as_str().unwrap_or("?"),
                state_text(v),
                v["rows"],
                sparkles::error::human_bytes(v["stateBytes"].as_u64().unwrap_or(0))
            );
        }
        return Ok(());
    }
    if let Some(d) = out["deleted"].as_str() {
        println!("deleted view {d}");
        return Ok(());
    }
    if out.get("definition").is_some() {
        println!(
            "{}: {}, {} rows at commit {}, {} passes ({} skipped)",
            out["name"].as_str().unwrap_or("?"),
            state_text(out),
            out["rows"],
            out["commit"],
            out["passes"],
            out["skipped"]
        );
    } else if let Some(t) = out["task"].as_str() {
        println!(
            "rebuilding {} in task {t}",
            out["name"].as_str().unwrap_or("?")
        );
    }
    if out["plan"].is_object() {
        let mut s = String::new();
        plan_text(&out["plan"], 0, &mut s);
        print!("{s}");
    }
    Ok(())
}

// ----------------------------------------------------------- query --live ----

/// A term as text: an IRI in angle brackets, a literal in N-Triples form.
#[cfg(feature = "auth")]
fn term_text(t: &J) -> String {
    match t["type"].as_str() {
        None => "UNDEF".into(),
        Some("uri") => format!("<{}>", t["value"].as_str().unwrap_or("")),
        Some("bnode") => format!("_:{}", t["value"].as_str().unwrap_or("")),
        Some(_) => {
            let v = t["value"].as_str().unwrap_or("");
            let q = format!("{v:?}");
            if let Some(l) = t["xml:lang"].as_str() {
                format!("{q}@{l}")
            } else if let Some(d) = t["datatype"].as_str() {
                format!("{q}^^<{d}>")
            } else {
                q
            }
        }
    }
}

#[cfg(feature = "auth")]
fn row_text(r: &J) -> String {
    let cells: Vec<String> = r["values"]
        .as_array()
        .into_iter()
        .flatten()
        .map(term_text)
        .collect();
    let n = r["count"].as_u64().filter(|&n| n != 1);
    match n {
        Some(n) => format!("{}\t(x{n})", cells.join("\t")),
        None => cells.join("\t"),
    }
}

/// Print one event: JSON lines with `--results json`, else text blocks.
#[cfg(feature = "auth")]
fn print_event(event: &str, data: &J, json_lines: bool) {
    if json_lines {
        let mut d = data.clone();
        d["event"] = event.into();
        println!("{d}");
        return;
    }
    match event {
        "snapshot" | "reset" => {
            if let Some(v) = data.get("value") {
                println!("# {event} at commit {}: {v}", data["commit"]);
                return;
            }
            if data["more"] != true || !data["rows"].as_array().is_some_and(|r| r.is_empty()) {
                println!(
                    "# {event} at commit {}: {}",
                    data["commit"],
                    data["vars"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|v| format!("?{}", v.as_str().unwrap_or("")))
                        .collect::<Vec<_>>()
                        .join("\t")
                );
            }
            for r in data["rows"].as_array().into_iter().flatten() {
                println!("{}", row_text(r));
            }
        }
        "diff" => {
            println!("# commit {} (from {})", data["commit"], data["from"]);
            if let Some(v) = data.get("value") {
                println!("{v}");
                return;
            }
            for r in data["removed"].as_array().into_iter().flatten() {
                println!("- {}", row_text(r));
            }
            for r in data["added"].as_array().into_iter().flatten() {
                println!("+ {}", row_text(r));
            }
        }
        _ => println!("# {event}: {data}"),
    }
}

/// Follow a live query on a server until the stream ends or the process is stopped.
#[cfg(feature = "auth")]
pub fn live(server: Option<&str>, insecure: bool, ds: &str, query: &str, fmt: &str) -> Result<()> {
    use crate::remote::Remote;
    use std::io::BufRead;
    let r = Remote::open(server, insecure)?;
    let enc = |s: &str| -> String { form_urlencoded::byte_serialize(s.as_bytes()).collect() };
    let path = format!("/{}/live?query={}", enc(ds), enc(query));
    let resp = r.check(
        r.req(reqwest::Method::GET, &path)
            .header("accept", "text/event-stream")
            .send(),
        Some(ds),
    )?;
    let json_lines = fmt == "json";
    let reader = std::io::BufReader::new(resp);
    let (mut event, mut data) = (String::new(), String::new());
    for line in reader.lines() {
        let line = line.context("the live query's stream")?;
        if line.is_empty() {
            if !event.is_empty() {
                let d: J = serde_json::from_str(&data).unwrap_or(J::String(data.clone()));
                print_event(&event, &d, json_lines);
                if event == "error" {
                    bail!("{}", d["error"].as_str().unwrap_or("the live query ended"));
                }
            }
            event.clear();
            data.clear();
            continue;
        }
        if let Some(v) = line.strip_prefix("event:") {
            event = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_views_are_managed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        {
            let ds = sparkles::Dataset::open(&db).unwrap();
            ds.update("INSERT DATA { <urn:a> <urn:in> <urn:t> . <urn:b> <urn:in> <urn:t> }")
                .unwrap();
        }
        let args = |cmd: ViewsCmd| ViewsArgs {
            cmd,
            loc: Some(db.clone()),
            format: "json".into(),
            server: None,
            dataset: None,
            insecure_http: false,
        };
        let q = "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p <urn:in> ?t } GROUP BY ?t";
        let j = local(
            &db,
            &args(ViewsCmd::Put {
                name: "sizes".into(),
                query: Some(q.into()),
                query_file: None,
                def: None,
                principal: None,
                wait: 30.0,
            }),
            StoreOptions::default(),
        )
        .unwrap();
        assert_eq!(j["state"], "current", "{j}");
        assert_eq!(j["rows"], 1);
        print(&j, "text").unwrap();
        let j = local(&db, &args(ViewsCmd::List), StoreOptions::default()).unwrap();
        assert_eq!(j["views"][0]["name"], "sizes");
        print(&j, "text").unwrap();
        let j = local(
            &db,
            &args(ViewsCmd::Explain {
                query: Some("SELECT * WHERE { ?s <urn:p>+ ?o }".into()),
                query_file: None,
            }),
            StoreOptions::default(),
        )
        .unwrap();
        assert!(j["plan"].to_string().contains("recompute"), "{j}");
        let j = local(
            &db,
            &args(ViewsCmd::Delete {
                name: "sizes".into(),
            }),
            StoreOptions::default(),
        )
        .unwrap();
        assert_eq!(j["deleted"], "sizes");
        assert!(!db.join("views/sizes").exists());
    }

    #[cfg(feature = "auth")]
    #[test]
    fn live_events_print() {
        let snap = json!({ "commit": 3, "vars": ["p"], "rows": [{ "values": [{ "type": "uri", "value": "urn:a" }] }], "more": false });
        print_event("snapshot", &snap, false);
        let diff = json!({ "commit": 4, "from": 3, "removed": [], "added": [{ "values": [{ "type": "literal", "value": "x", "xml:lang": "en" }], "count": 2 }] });
        print_event("diff", &diff, false);
        print_event("diff", &diff, true);
        assert_eq!(row_text(&diff["added"][0]), "\"x\"@en\t(x2)");
    }
}
