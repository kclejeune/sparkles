//! `sparkles quota`: the storage quota of a persistent dataset, on a local database
//! directory or on a server: print it, set it, or go back to the server's default.

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value as J;
use sparkles::store::StoreOptions;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct QuotaArgs {
    /// Database directory
    #[arg(long, required_unless_present = "server", conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// Give the dataset this quota, in MiB of its directory on disk (0: unlimited)
    #[arg(long, value_name = "MIB", conflicts_with = "default")]
    pub max_mb: Option<u64>,
    /// Remove the dataset's own quota, so that the server's --max-dataset-mb applies
    #[arg(long)]
    pub default: bool,
    /// text or json
    #[arg(long, default_value = "text")]
    pub format: String,
    /// A server to ask instead of a local database (with --dataset); changing a quota
    /// there needs server-admin
    #[arg(long, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset on --server
    #[arg(long, requires = "server")]
    pub dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    pub insecure_http: bool,
}

pub fn run(a: QuotaArgs, opts: StoreOptions) -> Result<()> {
    let status = match &a.loc {
        Some(loc) => local(loc, &a, opts)?,
        None => remote(&a)?,
    };
    print(&status, &a.format)
}

fn local(loc: &std::path::Path, a: &QuotaArgs, opts: StoreOptions) -> Result<J> {
    let quota = crate::open_dataset(loc, opts)?.settings().quota();
    let status = match (a.max_mb, a.default) {
        (Some(mb), _) => quota.set(mb.saturating_mul(1 << 20))?,
        (None, true) => quota.reset()?,
        (None, false) => quota.get(),
    };
    Ok(serde_json::to_value(status)?)
}

#[cfg(feature = "auth")]
fn remote(a: &QuotaArgs) -> Result<J> {
    use crate::remote::{JsonBody, Remote};
    use serde_json::json;
    let Some(ds) = a.dataset.as_deref() else {
        bail!("--dataset NAME is required with --server");
    };
    let r = Remote::open(a.server.as_deref(), a.insecure_http)?;
    let path = format!("/$/quota/{ds}");
    let req = match (a.max_mb, a.default) {
        (Some(mb), _) => r
            .req(reqwest::Method::PUT, &path)
            .header("content-type", "application/json")
            .body(json!({ "maxMb": mb }).to_string()),
        (None, true) => r.req(reqwest::Method::DELETE, &path),
        (None, false) => r.req(reqwest::Method::GET, &path),
    };
    r.check(req.send(), Some(ds))?.json_value()
}

#[cfg(not(feature = "auth"))]
fn remote(_: &QuotaArgs) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

fn print(status: &J, format: &str) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }
    let h = |v: &J| match v.as_u64() {
        Some(b) => sparkles::error::human_bytes(b),
        None => "unlimited".to_string(),
    };
    let source = match status["source"].as_str() {
        Some("dataset") => "set on the dataset".to_string(),
        _ => format!("the default ({})", h(&status["defaultMaxBytes"])),
    };
    println!(
        "quota {}, {source}; {} used",
        h(&status["maxBytes"]),
        h(&status["usedBytes"])
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::store::Store;

    fn args(loc: &std::path::Path, max_mb: Option<u64>, default: bool) -> QuotaArgs {
        QuotaArgs {
            loc: Some(loc.to_path_buf()),
            max_mb,
            default,
            format: "json".into(),
            server: None,
            dataset: None,
            insecure_http: false,
        }
    }

    #[test]
    fn sets_and_clears_a_local_quota() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        drop(Store::open(&db, StoreOptions::default()).unwrap());
        let opts = StoreOptions::default;
        let j = local(&db, &args(&db, Some(5), false), opts()).unwrap();
        assert_eq!(j["maxBytes"], 5 << 20);
        assert_eq!(j["source"], "dataset");
        let j = local(&db, &args(&db, None, false), opts()).unwrap();
        assert_eq!(j["maxBytes"], 5 << 20);
        print(&j, "text").unwrap();
        let j = local(&db, &args(&db, None, true), opts()).unwrap();
        assert!(j["maxBytes"].is_null() && j["source"] == "default", "{j}");
        // not a database: nothing is created
        let none = dir.path().join("none");
        assert!(local(&none, &args(&none, Some(1), false), opts()).is_err());
        assert!(!none.exists());
    }
}
