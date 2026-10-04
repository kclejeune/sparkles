//! `sparkles compaction`: a dataset's automatic compaction settings, on a local database
//! directory or on a server: print the policy and what it sees, set settings, or go back
//! to the server's.

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value as J;
use sparkles::store::{CompactionPolicy, CompactionSettings, StoreOptions};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct CompactionArgs {
    /// Database directory
    #[arg(long, required_unless_present = "server", conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// Set a setting: enabled, minDeltaQuads, deltaRatio, maxDeltaQuads, maxDeltaMb,
    /// maxWalMb, idleSeconds, maxAgeSeconds, minIntervalSeconds or partial (repeatable)
    #[arg(long, value_name = "KEY=VALUE", conflicts_with = "default")]
    pub set: Vec<String>,
    /// Remove the dataset's own settings, so that the server's apply
    #[arg(long)]
    pub default: bool,
    /// text or json
    #[arg(long, default_value = "text")]
    pub format: String,
    /// A server to ask instead of a local database (with --dataset); changing the
    /// settings there needs admin on the dataset
    #[arg(long, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset on --server
    #[arg(long, requires = "server")]
    pub dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    pub insecure_http: bool,
}

pub fn run(a: CompactionArgs, opts: StoreOptions) -> Result<()> {
    let status = match &a.loc {
        Some(loc) => local(loc, &a, opts)?,
        None => remote(&a)?,
    };
    print(&status, &a.format)
}

/// The settings `--set` makes, on top of `own`.
fn apply_sets(mut own: CompactionSettings, sets: &[String]) -> Result<CompactionSettings> {
    for kv in sets {
        let Some((k, v)) = kv.split_once('=') else {
            bail!("--set expects KEY=VALUE, not {kv:?}");
        };
        own.set(k.trim(), v)?;
    }
    Ok(own)
}

fn local(loc: &std::path::Path, a: &CompactionArgs, opts: StoreOptions) -> Result<J> {
    let compaction = crate::open_dataset(loc, opts)?.settings().compaction();
    if a.default {
        compaction.reset()?;
    } else if !a.set.is_empty() {
        compaction.set(apply_sets(compaction.get(), &a.set)?)?;
    }
    // a local database has no server policy beneath its own settings
    Ok(serde_json::to_value(
        compaction.status(&CompactionPolicy::default()),
    )?)
}

#[cfg(feature = "auth")]
fn remote(a: &CompactionArgs) -> Result<J> {
    use crate::remote::{JsonBody, Remote};
    let Some(ds) = a.dataset.as_deref() else {
        bail!("--dataset NAME is required with --server");
    };
    let r = Remote::open(a.server.as_deref(), a.insecure_http)?;
    let path = format!("/$/compaction/{ds}");
    let req = if a.default {
        r.req(reqwest::Method::DELETE, &path)
    } else if !a.set.is_empty() {
        let cur = r
            .check(r.req(reqwest::Method::GET, &path).send(), Some(ds))?
            .json_value()?;
        let own = CompactionSettings::from_json(&cur["own"])?;
        let own = apply_sets(own, &a.set)?;
        r.req(reqwest::Method::PUT, &path)
            .header("content-type", "application/json")
            .body(serde_json::to_string(&own)?)
    } else {
        r.req(reqwest::Method::GET, &path)
    };
    r.check(req.send(), Some(ds))?.json_value()
}

#[cfg(not(feature = "auth"))]
fn remote(_: &CompactionArgs) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

fn print(status: &J, format: &str) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }
    let m = &status["measures"];
    let p = &status["policy"];
    println!(
        "automatic compaction {}: {}{}",
        if status["enabled"] == true {
            "on"
        } else {
            "off"
        },
        status["state"].as_str().unwrap_or("?"),
        status["trigger"]
            .as_str()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default()
    );
    if let Some(d) = status["deferredDetail"].as_str() {
        println!("waiting: {d}");
    }
    println!(
        "{}: {} base quads, {} delta quads (compacts at {}), write-ahead log {}",
        m["generation"].as_str().unwrap_or("?"),
        m["baseQuads"],
        m["deltaQuads"],
        m["threshold"],
        sparkles::error::human_bytes(m["walBytes"].as_u64().unwrap_or(0))
    );
    let own = status["own"].as_object();
    for k in sparkles::store::SETTING_NAMES {
        let mark = if own.is_some_and(|o| o.contains_key(k)) {
            "  (set on the dataset)"
        } else {
            ""
        };
        println!("  {k} = {}{mark}", p[k]);
    }
    if let Some(last) = status["last"].as_object() {
        println!(
            "last compaction: {} at {}, {:.2} s, writer lock {:.1} ms",
            last.get("outcome").and_then(J::as_str).unwrap_or("?"),
            last.get("finishedAt").and_then(J::as_str).unwrap_or("?"),
            last.get("seconds").and_then(J::as_f64).unwrap_or(0.0),
            last.get("lockMs").and_then(J::as_f64).unwrap_or(0.0)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sparkles::store::Store;

    fn args(loc: &std::path::Path, set: &[&str], default: bool) -> CompactionArgs {
        CompactionArgs {
            loc: Some(loc.to_path_buf()),
            set: set.iter().map(|s| s.to_string()).collect(),
            default,
            format: "json".into(),
            server: None,
            dataset: None,
            insecure_http: false,
        }
    }

    #[test]
    fn sets_and_clears_local_settings() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        drop(Store::open(&db, StoreOptions::default()).unwrap());
        let opts = StoreOptions::default;
        let j = local(&db, &args(&db, &[], false), opts()).unwrap();
        assert_eq!(j["state"], "idle");
        assert_eq!(j["policy"]["deltaRatio"], 0.05);
        print(&j, "text").unwrap();
        let j = local(
            &db,
            &args(&db, &["deltaRatio=0.02", "idleSeconds=60"], false),
            opts(),
        )
        .unwrap();
        assert_eq!(j["own"], json!({"deltaRatio": 0.02, "idleSeconds": 60}));
        assert_eq!(j["policy"]["deltaRatio"], 0.02);
        // kept, and added to
        let j = local(&db, &args(&db, &["enabled=false"], false), opts()).unwrap();
        assert_eq!(
            j["own"],
            json!({"deltaRatio": 0.02, "idleSeconds": 60, "enabled": false})
        );
        assert_eq!(j["state"], "off");
        print(&j, "text").unwrap();
        assert!(local(&db, &args(&db, &["nope=1"], false), opts()).is_err());
        assert!(local(&db, &args(&db, &["deltaRatio"], false), opts()).is_err());
        let j = local(&db, &args(&db, &[], true), opts()).unwrap();
        assert_eq!(j["own"], json!({}));
        assert!(!db.join(sparkles::store::COMPACTION_FILE).exists());
        // not a database: nothing is created
        let none = dir.path().join("none");
        assert!(local(&none, &args(&none, &[], false), opts()).is_err());
        assert!(!none.exists());
    }
}
