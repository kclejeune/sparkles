//! `sparkles describe-settings`: how DESCRIBE describes a resource in a dataset, on a
//! local database directory or on a server: print the setting, change options, or go
//! back to the defaults.

use anyhow::{Result, bail};
use clap::Args;
use serde_json::Value as J;
use sparkles::sparql::describe::DescribeOptions;
use sparkles::store::StoreOptions;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct DescribeArgs {
    /// Database directory
    #[arg(long, required_unless_present = "server", conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// Set an option: mode (cbd, scbd or outgoing), labels, reifiers, maxTriples or
    /// maxDepth (repeatable; a limit of 0 removes it)
    #[arg(long, value_name = "KEY=VALUE", conflicts_with = "default")]
    pub set: Vec<String>,
    /// Remove the dataset's setting, so that the defaults apply
    #[arg(long)]
    pub default: bool,
    /// text or json
    #[arg(long, default_value = "text")]
    pub format: String,
    /// A server to ask instead of a local database (with --dataset); changing the
    /// setting there needs admin on the dataset
    #[arg(long, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset on --server
    #[arg(long, requires = "server")]
    pub dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    pub insecure_http: bool,
}

pub fn run(a: DescribeArgs, opts: StoreOptions) -> Result<()> {
    let status = match &a.loc {
        Some(loc) => local(loc, &a, opts)?,
        None => remote(&a)?,
    };
    print(&status, &a.format)
}

/// The options `--set` makes, on top of `own`.
fn apply_sets(mut own: DescribeOptions, sets: &[String]) -> Result<DescribeOptions> {
    for kv in sets {
        let Some((k, v)) = kv.split_once('=') else {
            bail!("--set expects KEY=VALUE, not {kv:?}");
        };
        own.set(k.trim(), v)?;
    }
    Ok(own)
}

/// The setting as `GET /$/describe/{ds}` reports it
/// ([`DescribeStatus`](sparkles::handles::DescribeStatus)).
pub(crate) fn status(o: &DescribeOptions) -> J {
    serde_json::to_value(sparkles::handles::DescribeStatus::of(o)).unwrap_or_default()
}

fn local(loc: &std::path::Path, a: &DescribeArgs, opts: StoreOptions) -> Result<J> {
    let describe = crate::open_dataset(loc, opts)?.settings().describe();
    if a.default {
        describe.reset()?;
    } else if !a.set.is_empty() {
        describe.set(apply_sets(describe.get(), &a.set)?)?;
    }
    Ok(serde_json::to_value(describe.status())?)
}

#[cfg(feature = "auth")]
fn remote(a: &DescribeArgs) -> Result<J> {
    use crate::remote::{JsonBody, Remote};
    let Some(ds) = a.dataset.as_deref() else {
        bail!("--dataset NAME is required with --server");
    };
    let r = Remote::open(a.server.as_deref(), a.insecure_http)?;
    let path = format!("/$/describe/{ds}");
    let req = if a.default {
        r.req(reqwest::Method::DELETE, &path)
    } else if !a.set.is_empty() {
        let mut cur = r
            .check(r.req(reqwest::Method::GET, &path).send(), Some(ds))?
            .json_value()?;
        if let Some(o) = cur.as_object_mut() {
            o.remove("source");
            o.remove("modes");
        }
        let own = apply_sets(DescribeOptions::from_json(&cur)?, &a.set)?;
        r.req(reqwest::Method::PUT, &path)
            .header("content-type", "application/json")
            .body(serde_json::to_string(&own)?)
    } else {
        r.req(reqwest::Method::GET, &path)
    };
    r.check(req.send(), Some(ds))?.json_value()
}

#[cfg(not(feature = "auth"))]
fn remote(_: &DescribeArgs) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

fn print(j: &J, format: &str) -> Result<()> {
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(j)?),
        "text" => {
            let limit = |v: &J| v.as_u64().map_or("none".to_string(), |n| n.to_string());
            println!("mode:        {}", j["mode"].as_str().unwrap_or("?"));
            println!("labels:      {}", j["labels"]);
            println!("reifiers:    {}", j["reifiers"]);
            println!("max triples: {}", limit(&j["maxTriples"]));
            println!("max depth:   {}", limit(&j["maxDepth"]));
            println!("source:      {}", j["source"].as_str().unwrap_or("?"));
        }
        f => bail!("unknown --format {f}: text or json"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::store::Store;

    #[test]
    fn sets_change_the_local_setting_and_default_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        drop(Store::open(&db, StoreOptions::default()).unwrap());
        let args = |set: &[&str], default: bool| DescribeArgs {
            loc: Some(db.clone()),
            set: set.iter().map(|s| s.to_string()).collect(),
            default,
            format: "json".into(),
            server: None,
            dataset: None,
            insecure_http: false,
        };
        let j = local(
            &db,
            &args(&["mode=scbd", "maxTriples=500"], false),
            Default::default(),
        )
        .unwrap();
        assert_eq!(j["mode"], "scbd");
        assert_eq!(j["maxTriples"], 500);
        assert_eq!(j["source"], "dataset");
        let j = local(&db, &args(&["labels=true"], false), Default::default()).unwrap();
        assert_eq!(
            (j["mode"].as_str(), &j["labels"]),
            (Some("scbd"), &J::Bool(true))
        );
        assert!(db.join(sparkles::store::DESCRIBE_FILE).exists());
        assert!(local(&db, &args(&["mode=all"], false), Default::default()).is_err());
        let j = local(&db, &args(&[], true), Default::default()).unwrap();
        assert_eq!(j["source"], "default");
        assert!(!db.join(sparkles::store::DESCRIBE_FILE).exists());
    }
}
