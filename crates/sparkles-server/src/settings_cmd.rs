//! `sparkles settings`: the layered settings of spec C19 §8. `check` validates a
//! settings file of `serve --settings` without a server. The other subcommands talk to a
//! server through `/$/settings/{ds}/{kind}`, or with `--global` through the server-wide
//! kinds of `/$/server/settings/{kind}` (§11.4), with the server and token of the other
//! remote commands.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct SettingsArgs {
    #[command(flatten)]
    conn: ConnArgs,
    #[command(subcommand)]
    cmd: SettingsCmd,
}

/// How the remote subcommands reach the server.
#[derive(Args, Debug)]
pub(crate) struct ConnArgs {
    /// The server (else SPARKLES_SERVER, or the saved default of `sparkles auth login`)
    #[arg(long, global = true, env = "SPARKLES_SERVER")]
    pub(crate) server: Option<String>,
    /// Allow plain http to a server other than localhost
    #[arg(long, global = true)]
    pub(crate) insecure_http: bool,
    /// Print JSON instead of text
    #[arg(long, global = true)]
    pub(crate) json: bool,
}

#[derive(Subcommand, Debug)]
enum SettingsCmd {
    /// Print a dataset's settings, every kind or one, with the source of each field
    /// (default, declared, runtime or locked). A runtime value that replaces a value of
    /// the server's configuration says which. With --global, print the server-wide
    /// settings (models)
    Get {
        /// The dataset; with --global, the server-wide kind (default: models)
        #[arg(value_name = "DS", required_unless_present = "global")]
        dataset: Option<String>,
        /// assistant, memory or ingest (default: every kind)
        kind: Option<String>,
        /// Print one layer: the effective object, the declared layers of the settings
        /// file, or the runtime layer. With --json and a layer, print only that object
        #[arg(long, value_parser = ["effective", "declared", "runtime"])]
        layer: Option<String>,
        /// The server-wide settings, as in `get --global models` (needs server-admin)
        #[arg(long)]
        global: bool,
    },
    /// Change fields of the runtime layer: one PATCH per kind. A value is read as JSON,
    /// else as a string, and null removes the runtime value. With --global --preset NAME
    /// PROVIDER, add the model provider PROVIDER from a preset, with FIELD=VALUE
    /// assignments of the provider's own members in place of the preset's
    Set {
        /// The dataset; with --global, the first assignment; with --preset, the name of
        /// the new provider
        #[arg(value_name = "DS", required_unless_present = "global")]
        dataset: Option<String>,
        /// Such as assistant.send=documents or assistant.budget.perRequest=80000, or
        /// with --global models.providers.claude.budget.tokensPerDay=500000, or with
        /// --preset endpoint=https://proxy.example or apiKey.secret=team-key
        #[arg(value_name = "KIND.FIELD=VALUE", required_unless_present = "global")]
        assignments: Vec<String>,
        /// Change the server-wide settings (needs server-admin)
        #[arg(long)]
        global: bool,
        /// Add a model provider from a preset: anthropic, openai or ollama (needs
        /// --global). An OpenAI-compatible gateway has no preset; set its kind and
        /// endpoint as fields instead
        #[arg(long, value_name = "NAME", requires = "global")]
        preset: Option<String>,
    },
    /// Edit the runtime layer of a kind in $VISUAL or $EDITOR and send the changes with
    /// If-Match
    Edit {
        /// The dataset; with --global, the server-wide kind (default: models)
        #[arg(value_name = "DS", required_unless_present = "global")]
        dataset: Option<String>,
        /// assistant, memory or ingest
        kind: Option<String>,
        /// Edit a server-wide kind, as in `edit --global models` (needs server-admin)
        #[arg(long)]
        global: bool,
    },
    /// Remove the runtime value of a field, or the whole runtime layer of a kind, so
    /// that the declared value or the default applies, and print which one applies now
    Reset {
        /// The dataset; with --global, the kind or field
        #[arg(value_name = "DS", required = true)]
        dataset: Option<String>,
        /// Such as assistant or assistant.historyDays, or with --global
        /// models.providers.claude.budget in place of the dataset
        #[arg(value_name = "KIND[.FIELD]", required_unless_present = "global")]
        target: Option<String>,
        /// Reset a server-wide field or kind (needs server-admin)
        #[arg(long)]
        global: bool,
    },
    /// List the runtime values that differ from the declared values and defaults, for
    /// one dataset or every dataset, or with --global for the server-wide settings
    Diff {
        /// The dataset (default: every dataset the caller can see)
        #[arg(value_name = "DS", conflicts_with = "global")]
        dataset: Option<String>,
        /// The server-wide settings (needs server-admin)
        #[arg(long)]
        global: bool,
    },
    /// Patch the runtime layers of a server's datasets from a settings file: `defaults`
    /// for every dataset and each entry for its dataset. Locks, those of `server`
    /// included, are reported and not applied, since only the server's own settings file
    /// can lock (exit status 1 when a patch fails)
    Apply {
        /// A settings file in the format of serve --settings
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Validate a settings file offline: its form, the kinds and fields it names, and
    /// every effective object it declares (exit status 1 when it is not valid). With
    /// --model-config, roles and sendByProvider must name configured providers and
    /// models they allow, and the locks of server.locked are checked against it. The
    /// prefixes it declares for one dataset count against --max-prefixes, and a prefix
    /// that shadows a well-known one is a warning
    Check {
        /// The settings file
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// The model configuration of serve --model-config
        #[arg(long, value_name = "FILE")]
        model_config: Option<PathBuf>,
    },
}

pub fn run(args: SettingsArgs) -> Result<()> {
    match args.cmd {
        SettingsCmd::Check { file, model_config } => {
            crate::settings::check_file(&file, model_config.as_deref())
        }
        _ if crate::branch_cmd::branch().is_some() => {
            bail!("settings belong to a dataset's main branch; --branch is not supported")
        }
        cmd => remote::run(cmd, &args.conn),
    }
}

/// What a command's settings belong to: a dataset, or with `--global` the server
/// (spec C19 §11.4).
#[cfg_attr(not(feature = "auth"), allow(dead_code))]
enum Owner {
    Dataset(String),
    Server,
}

#[cfg_attr(not(feature = "auth"), allow(dead_code))]
impl Owner {
    fn dataset(name: &str) -> Result<Owner> {
        if !sparkles::catalog::valid_name(name) {
            bail!("invalid dataset name {name:?}");
        }
        Ok(Owner::Dataset(name.to_string()))
    }

    /// The route of every kind of a dataset, or of one kind.
    fn path(&self, kind: Option<&str>) -> String {
        match (self, kind) {
            (Owner::Dataset(ds), None) => format!("/$/settings/{ds}"),
            (Owner::Dataset(ds), Some(k)) => format!("/$/settings/{ds}/{}", enc(k)),
            (Owner::Server, k) => {
                format!("/$/server/settings/{}", enc(k.unwrap_or(SERVER_KIND)))
            }
        }
    }

    /// The owner in messages, such as `/org` or `the server`.
    fn label(&self) -> String {
        match self {
            Owner::Dataset(ds) => format!("/{ds}"),
            Owner::Server => "the server".into(),
        }
    }

    /// The built-in defaults of `kind`, as this build knows them.
    fn defaults(&self, kind: &str) -> serde_json::Value {
        let k = match self {
            Owner::Dataset(_) => crate::settings::kind(kind),
            Owner::Server => crate::settings::server_kind(kind),
        };
        k.map(|k| k.default_value())
            .unwrap_or_else(|| serde_json::json!({}))
    }
}

/// The server-wide kind that `--global` reads when none is named.
const SERVER_KIND: &str = "models";

/// Percent-encode a path segment or a query parameter value.
#[cfg_attr(not(feature = "auth"), allow(dead_code))]
pub(crate) fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// `KIND.FIELD`, `\.` escaping a dot in a member name: the kind and the field, which is
/// empty for `KIND` alone.
#[cfg_attr(not(feature = "auth"), allow(dead_code))]
fn split_target(s: &str) -> Result<(String, Vec<String>)> {
    let mut p = crate::settings::merge::parse_path(s)
        .with_context(|| format!("{s:?} is not KIND or KIND.FIELD, such as assistant.send"))?;
    let kind = p.remove(0);
    Ok((kind, p))
}

/// A value of `set`: JSON, else a string.
#[cfg_attr(not(feature = "auth"), allow(dead_code))]
fn parse_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
}

#[cfg(not(feature = "auth"))]
mod remote {
    pub fn run(_: super::SettingsCmd, _: &super::ConnArgs) -> anyhow::Result<()> {
        anyhow::bail!("built without the remote client (cargo feature \"auth\")")
    }
}

#[cfg(feature = "auth")]
mod remote {
    use super::{ConnArgs, Owner, SERVER_KIND, SettingsCmd, enc, parse_value, split_target};
    use crate::remote::Remote;
    use crate::settings::merge::{
        at, diff, leaves, merged, overlaid, parse_path, path_string, set_at, starts_with,
    };
    use anyhow::{Context, Result, bail};
    use reqwest::Method;
    use serde_json::{Map, Value as J, json};
    use std::io::{BufRead, Write};
    use std::path::Path;

    /// A server's answer: status, `ETag` and body (a string when it is not JSON).
    struct Answer {
        status: u16,
        etag: Option<String>,
        body: J,
    }

    struct Client {
        r: Remote,
        json: bool,
    }

    impl Client {
        fn call(
            &self,
            method: Method,
            path: &str,
            if_match: Option<&str>,
            body: Option<&J>,
        ) -> Result<Answer> {
            let mut req = self
                .r
                .req(method, path)
                .header("accept", "application/json");
            if let Some(m) = if_match {
                req = req.header("if-match", m);
            }
            if let Some(b) = body {
                req = req
                    .header("content-type", "application/json")
                    .body(b.to_string());
            }
            let resp = req
                .send()
                .with_context(|| format!("cannot reach {}", self.r.base))?;
            let status = resp.status().as_u16();
            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes = resp.bytes()?;
            let body = serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| J::String(String::from_utf8_lossy(&bytes).trim().to_string()));
            Ok(Answer { status, etag, body })
        }

        /// A successful answer, or an error that says what the server refused.
        fn ok(&self, a: Answer, owner: &Owner) -> Result<Answer> {
            if (200..300).contains(&a.status) {
                return Ok(a);
            }
            bail!("{}", self.explain(&a, owner))
        }

        fn explain(&self, a: &Answer, owner: &Owner) -> String {
            let msg = a.body["error"]
                .as_str()
                .map(str::to_string)
                .or_else(|| a.body.as_str().map(str::to_string))
                .unwrap_or_default();
            let base = &self.r.base;
            match (a.status, a.body["code"].as_str(), owner) {
                (401, _, _) => {
                    format!("not logged in to {base} (run: sparkles auth login --server {base})")
                }
                (404, Some("unknown-kind"), _) => msg,
                (404, _, Owner::Dataset(ds)) if crate::settings::server_kind(ds).is_some() => {
                    format!(
                        "no such dataset: /{ds} (or no access). For the server-wide {ds} settings, add --global"
                    )
                }
                (404, _, Owner::Dataset(_)) => {
                    format!("no such dataset: {} (or no access)", owner.label())
                }
                (403, _, Owner::Server) if !msg.contains("read-only") => {
                    "the server-wide settings need the server-admin permission (403)".into()
                }
                (409, Some("locked-by-config"), _) => format!(
                    "refused: {msg}. A locked field can be changed only in the server's settings file"
                ),
                (412, _, _) => msg,
                (s, _, _) if msg.is_empty() => format!("HTTP {s}"),
                (s, _, _) => format!("{msg} ({s})"),
            }
        }

        fn get(&self, owner: &Owner, kind: Option<&str>) -> Result<Answer> {
            let a = self.call(Method::GET, &owner.path(kind), None, None)?;
            self.ok(a, owner)
        }

        fn print_json(&self, v: &J) -> Result<()> {
            println!("{}", serde_json::to_string_pretty(v)?);
            Ok(())
        }
    }

    pub fn run(cmd: SettingsCmd, conn: &ConnArgs) -> Result<()> {
        let c = Client {
            r: Remote::open(conn.server.as_deref(), conn.insecure_http)?,
            json: conn.json,
        };
        let ds = |d: Option<String>| Owner::dataset(&d.unwrap_or_default());
        match cmd {
            SettingsCmd::Get {
                dataset,
                kind,
                layer,
                global,
            } => {
                if global {
                    if kind.is_some() {
                        bail!(
                            "with --global, name only the kind, as in: sparkles settings get --global models"
                        );
                    }
                    let kind = dataset.unwrap_or_else(|| SERVER_KIND.into());
                    get(&c, &Owner::Server, Some(&kind), layer.as_deref())
                } else {
                    get(&c, &ds(dataset)?, kind.as_deref(), layer.as_deref())
                }
            }
            SettingsCmd::Set {
                dataset,
                assignments,
                global,
                preset: Some(preset),
            } if global => {
                let name = dataset.context(
                    "name the new provider, as in: sparkles settings set --global --preset anthropic claude",
                )?;
                add_preset(&c, &preset, &name, &assignments)
            }
            SettingsCmd::Set {
                dataset,
                assignments,
                global,
                ..
            } => {
                if global {
                    let all: Vec<String> = dataset.into_iter().chain(assignments).collect();
                    if all.is_empty() {
                        bail!(
                            "name the fields to set, as in: sparkles settings set --global models.routing.complexityThreshold=4"
                        );
                    }
                    set(&c, &Owner::Server, &all)
                } else {
                    set(&c, &ds(dataset)?, &assignments)
                }
            }
            SettingsCmd::Edit {
                dataset,
                kind,
                global,
            } => {
                if global {
                    if kind.is_some() {
                        bail!(
                            "with --global, name only the kind, as in: sparkles settings edit --global models"
                        );
                    }
                    let kind = dataset.unwrap_or_else(|| SERVER_KIND.into());
                    edit(&c, &Owner::Server, &kind)
                } else {
                    let kind = kind.context(
                        "name the kind to edit, as in: sparkles settings edit DS assistant",
                    )?;
                    edit(&c, &ds(dataset)?, &kind)
                }
            }
            SettingsCmd::Reset {
                dataset,
                target,
                global,
            } => {
                if global {
                    if target.is_some() {
                        bail!(
                            "with --global, name only the kind or field, as in: sparkles settings reset --global models.routing"
                        );
                    }
                    reset(&c, &Owner::Server, &dataset.unwrap_or_default())
                } else {
                    reset(&c, &ds(dataset)?, &target.unwrap_or_default())
                }
            }
            SettingsCmd::Diff { dataset, global } => {
                if global {
                    diff_server(&c)
                } else {
                    diff_cmd(&c, dataset.as_deref())
                }
            }
            SettingsCmd::Apply { file } => apply(&c, &file),
            SettingsCmd::Check { .. } => unreachable!("check runs offline"),
        }
    }

    /// A value in one line.
    fn show(v: &J) -> String {
        serde_json::to_string(v).unwrap_or_default()
    }

    /// The `overrides` of a kind's answer: each field with the declared value it
    /// replaces and the runtime value.
    fn overrides(k: &J) -> Vec<(String, J, J)> {
        k["overrides"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some((
                            o["path"].as_str()?.to_string(),
                            o["declared"].clone(),
                            o["runtime"].clone(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The source of a field of an answer: its own, or for a field above the leaves,
    /// the strongest source of the leaves below it, as the UI shows it.
    fn source_of(k: &J, field: &[String]) -> String {
        let rank = |s: &str| match s {
            "runtime" => 3,
            "declared" => 2,
            "locked" => 1,
            _ => 0,
        };
        let mut best: Option<&str> = None;
        for (f, s) in k["sources"].as_object().into_iter().flatten() {
            let (Some(p), Some(s)) = (parse_path(f), s.as_str()) else {
                continue;
            };
            if (starts_with(&p, field) || starts_with(field, &p))
                && best.is_none_or(|b| rank(s) > rank(b))
            {
                best = Some(s);
            }
        }
        best.unwrap_or("default").to_string()
    }

    // ------------------------------------------------------------------- get ------

    fn get(c: &Client, owner: &Owner, kind: Option<&str>, layer: Option<&str>) -> Result<()> {
        let a = c.get(owner, kind)?;
        let kinds: Vec<&J> = match kind {
            Some(_) => vec![&a.body],
            None => a.body["kinds"]
                .as_object()
                .map(|m| m.values().collect())
                .unwrap_or_default(),
        };
        if c.json {
            return match layer {
                None => c.print_json(&a.body),
                Some(l) if kind.is_some() => c.print_json(&a.body[l]),
                Some(l) => {
                    let m: Map<String, J> = kinds
                        .iter()
                        .map(|k| (k["kind"].as_str().unwrap_or("").to_string(), k[l].clone()))
                        .collect();
                    c.print_json(&J::Object(m))
                }
            };
        }
        for (i, k) in kinds.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_kind(k, owner, layer.unwrap_or("effective"));
        }
        Ok(())
    }

    /// One kind as text: each field with its source and value, or the fields of one
    /// layer. A runtime value that replaces a different declared value names it, as
    /// `runtime, overrides declared VALUE`.
    fn print_kind(k: &J, owner: &Owner, layer: &str) {
        let name = k["kind"].as_str().unwrap_or("");
        let paths = |key: &str| -> Vec<String> {
            k[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let overridden = paths("overridden");
        let ignored = |field: &str| {
            overridden
                .iter()
                .any(|o| field == o || field.starts_with(&format!("{o}.")))
        };
        let over = overrides(k);
        let declared_of = |field: &str| over.iter().find(|o| o.0 == field);
        if layer != "effective" {
            println!("{name} on {}, {layer} layer", owner.label());
            let fields = leaves(&k[layer]);
            if fields.is_empty() {
                println!("  (empty)");
            }
            let w = fields
                .iter()
                .map(|p| path_string(p).len())
                .max()
                .unwrap_or(0);
            for p in fields {
                let f = path_string(&p);
                let note = match declared_of(&f) {
                    _ if layer == "runtime" && ignored(&f) => "  (ignored: locked)".to_string(),
                    Some(o) if layer == "runtime" => {
                        format!("  (overrides declared {})", show(&o.1))
                    }
                    _ => String::new(),
                };
                let v = at(&k[layer], &p).cloned().unwrap_or(J::Null);
                println!("  {f:<w$}  {}{note}", show(&v));
            }
            return;
        }
        println!("{name} on {}", owner.label());
        if k["status"]["valid"] == false {
            println!(
                "  not valid: {}",
                k["status"]["error"].as_str().unwrap_or("")
            );
        }
        let sources = k["sources"].as_object().cloned().unwrap_or_default();
        // (field, source, value and note)
        let mut rows: Vec<(String, String, String)> = Vec::new();
        for (f, s) in &sources {
            let v = parse_path(f)
                .and_then(|p| at(&k["effective"], &p).cloned())
                .unwrap_or(J::Null);
            let note = if ignored(f) {
                let rt = parse_path(f).and_then(|p| at(&k["runtime"], &p).cloned());
                match rt {
                    Some(r) => format!("  (the runtime value {} is ignored)", show(&r)),
                    None => String::new(),
                }
            } else {
                String::new()
            };
            let s = s.as_str().unwrap_or("");
            let source = match declared_of(f) {
                Some(o) => format!("{s}, overrides declared {}", show(&o.1)),
                None => s.to_string(),
            };
            rows.push((f.clone(), source, format!("{}{note}", show(&v))));
        }
        // a field that has no leaf of its own: a removed provider, or an object in place
        // of a declared value
        for (f, d, r) in &over {
            if !sources.contains_key(f) {
                let v = if r.is_null() {
                    "(removed)".to_string()
                } else {
                    show(r)
                };
                rows.push((
                    f.clone(),
                    format!("runtime, overrides declared {}", show(d)),
                    v,
                ));
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
        // the column of the plain sources; a source that names a declared value is wider
        let ws = rows
            .iter()
            .map(|r| r.1.len())
            .filter(|n| *n <= 8)
            .max()
            .unwrap_or(8)
            .max(8);
        for (f, s, v) in rows {
            println!("  {f:<w$}  {s:<ws$}  {v}");
        }
    }

    // ------------------------------------------------------------------- set ------

    fn set(c: &Client, owner: &Owner, assignments: &[String]) -> Result<()> {
        // the patch of each kind, in the order the kinds first appear
        let mut patches: Vec<(String, J, Vec<Vec<String>>)> = Vec::new();
        for a in assignments {
            let Some((lhs, value)) = a.split_once('=') else {
                bail!("{a:?} is not KIND.FIELD=VALUE");
            };
            let (kind, field) = split_target(lhs)?;
            if field.is_empty() {
                bail!(
                    "{a:?} names no field: write KIND.FIELD=VALUE, such as assistant.send=documents"
                );
            }
            let i = match patches.iter().position(|(k, _, _)| *k == kind) {
                Some(i) => i,
                None => {
                    patches.push((kind, json!({}), Vec::new()));
                    patches.len() - 1
                }
            };
            set_at(&mut patches[i].1, &field, parse_value(value));
            patches[i].2.push(field);
        }
        let mut out = Map::new();
        for (kind, patch, fields) in patches {
            let a = c.call(Method::PATCH, &owner.path(Some(&kind)), None, Some(&patch))?;
            let a = c
                .ok(a, owner)
                .with_context(|| format!("{kind} on {}", owner.label()))?;
            if !c.json {
                for f in &fields {
                    let fs = path_string(f);
                    let v = at(&a.body["effective"], f).cloned().unwrap_or(J::Null);
                    let s = a.body["sources"][&fs].as_str().unwrap_or("runtime");
                    println!("{kind}.{fs} = {}  ({s})", show(&v));
                }
            }
            out.insert(kind, a.body);
        }
        if c.json {
            let mut v = json!({ "kinds": out });
            match owner {
                Owner::Dataset(ds) => v["dataset"] = ds.as_str().into(),
                Owner::Server => v["scope"] = "server".into(),
            }
            c.print_json(&v)?;
        }
        Ok(())
    }

    /// `set --global --preset NAME PROVIDER [FIELD=VALUE…]`: add a provider from a
    /// preset, refused when one of that name exists, with `If-Match` so that a provider
    /// added meanwhile is not overwritten.
    fn add_preset(c: &Client, preset: &str, name: &str, assignments: &[String]) -> Result<()> {
        use crate::models::presets::{PRESETS, preset as find};
        let Some(p) = find(preset) else {
            let names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
            bail!(
                "no preset named {preset:?}; use {}. An OpenAI-compatible gateway has no preset: set models.providers.NAME.kind=openai and its endpoint",
                names.join(", ")
            );
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            bail!("provider name {name:?}: use letters, digits, '_', '-' and '.'");
        }
        let mut provider = p.provider();
        for a in assignments {
            let Some((lhs, value)) = a.split_once('=') else {
                bail!("{a:?} is not FIELD=VALUE, such as endpoint=https://proxy.example");
            };
            let field = parse_path(lhs)
                .filter(|f| !f.is_empty())
                .with_context(|| format!("{lhs:?} is not a field of a provider"))?;
            set_at(&mut provider, &field, parse_value(value));
        }
        let owner = Owner::Server;
        let cur = c.get(&owner, Some(SERVER_KIND))?;
        if at(&cur.body["effective"], &["providers".into(), name.into()]).is_some() {
            bail!(
                "a provider named {name} exists; change its fields with: sparkles settings set --global models.providers.{name}.FIELD=VALUE"
            );
        }
        let patch = json!({ "providers": { name: provider } });
        let a = c.call(
            Method::PATCH,
            &owner.path(Some(SERVER_KIND)),
            cur.etag.as_deref(),
            Some(&patch),
        )?;
        let a = c
            .ok(a, &owner)
            .with_context(|| format!("provider {name} from preset {preset}"))?;
        if c.json {
            return c.print_json(&json!({ "kinds": { SERVER_KIND: a.body }, "scope": "server" }));
        }
        let added = at(&a.body["effective"], &["providers".into(), name.into()])
            .cloned()
            .unwrap_or(J::Null);
        println!("added provider {name} from preset {preset}:");
        for f in leaves(&added) {
            let v = at(&added, &f).cloned().unwrap_or(J::Null);
            println!("  {}  {}", path_string(&f), show(&v));
        }
        if let Some(s) = added["apiKey"]["secret"].as_str() {
            println!("set its key with: sparkles secrets set {s}");
        }
        Ok(())
    }

    fn owner_name(owner: &Owner) -> &str {
        match owner {
            Owner::Dataset(ds) => ds,
            Owner::Server => "server",
        }
    }

    // ------------------------------------------------------------------ edit ------

    /// Ask a yes-or-no question on the terminal; no input is no.
    fn confirm(question: &str) -> bool {
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(n) if n > 0 => matches!(line.trim(), "y" | "Y" | "yes" | "Yes"),
            _ => false,
        }
    }

    /// Run `$VISUAL`, else `$EDITOR`, else `vi`, on `path`, through the shell so that the
    /// variable may hold arguments.
    fn run_editor(path: &Path) -> Result<()> {
        let editor = ["VISUAL", "EDITOR"]
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()))
            .unwrap_or_else(|| "vi".into());
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(path)
            .status()
            .with_context(|| format!("cannot run the editor {editor:?}"))?;
        if !status.success() {
            bail!("the editor {editor:?} exited with {status}; nothing was sent");
        }
        Ok(())
    }

    fn edit(c: &Client, owner: &Owner, kind: &str) -> Result<()> {
        let dir = tempfile::Builder::new()
            .prefix("sparkles-settings-")
            .tempdir()?;
        let file = dir.path().join(format!("{kind}.json"));
        // the text to open, when it is the user's own after a refusal
        let mut keep: Option<String> = None;
        let mut saved = 0;
        loop {
            let cur = c.get(owner, Some(kind))?;
            let runtime = cur.body["runtime"].clone();
            let etag = cur
                .etag
                .clone()
                .or_else(|| cur.body["etag"].as_str().map(str::to_string))
                .context("the server sent no ETag")?;
            let original = format!("{}\n", serde_json::to_string_pretty(&runtime)?);
            std::fs::write(&file, keep.take().unwrap_or_else(|| original.clone()))?;
            run_editor(&file)?;
            let text = std::fs::read_to_string(&file)?;
            let new: J = match serde_json::from_str(&text) {
                Ok(v @ J::Object(_)) => v,
                Ok(_) => {
                    eprintln!("the runtime layer must be a JSON object");
                    if confirm("Edit it again?") {
                        keep = Some(text);
                        continue;
                    }
                    bail!("nothing was sent");
                }
                Err(e) => {
                    eprintln!("not JSON: {e}");
                    if confirm("Edit it again?") {
                        keep = Some(text);
                        continue;
                    }
                    bail!("nothing was sent");
                }
            };
            let Some(patch) = diff(&runtime, &new) else {
                if c.json {
                    c.print_json(&cur.body)?;
                } else {
                    println!("no changes");
                }
                return Ok(());
            };
            let a = c.call(
                Method::PATCH,
                &owner.path(Some(kind)),
                Some(&etag),
                Some(&patch),
            )?;
            match a.status {
                200..=299 => {
                    if c.json {
                        c.print_json(&a.body)?;
                    } else {
                        let n = leaves(&patch).len();
                        println!(
                            "{kind} on {}: {n} field{} changed",
                            owner.label(),
                            if n == 1 { "" } else { "s" }
                        );
                    }
                    return Ok(());
                }
                412 => {
                    saved += 1;
                    let copy = std::env::temp_dir().join(format!(
                        "sparkles-settings-{}-{kind}-{}-{saved}.json",
                        owner_name(owner),
                        std::process::id()
                    ));
                    std::fs::write(&copy, &text)?;
                    eprintln!(
                        "someone changed the {kind} settings of {} since they were read, so \
                         nothing was sent. Your version is in {}.",
                        owner.label(),
                        copy.display()
                    );
                    if confirm("Edit the current settings?") {
                        continue;
                    }
                    bail!(
                        "the {kind} settings of {} changed on the server",
                        owner.label()
                    );
                }
                400 | 409 => {
                    eprintln!("{}", c.explain(&a, owner));
                    if confirm("Edit it again?") {
                        keep = Some(text);
                        continue;
                    }
                    bail!("nothing was changed");
                }
                _ => {
                    c.ok(a, owner)?;
                    unreachable!("an error status");
                }
            }
        }
    }

    // ----------------------------------------------------------------- reset ------

    /// What applies to `field` after a reset, in words: its value and whether it comes
    /// from the server's configuration, a lock or the built-in default.
    fn now_applies(k: &J, field: &[String]) -> String {
        let value = match at(&k["effective"], field) {
            Some(v) => show(v),
            None => "unset".into(),
        };
        let what = match source_of(k, field).as_str() {
            "declared" => "the declared value applies",
            "locked" => "the locked value applies",
            "runtime" => "a runtime value still applies below it",
            _ => "the default applies",
        };
        format!("{value}, {what}")
    }

    fn reset(c: &Client, owner: &Owner, target: &str) -> Result<()> {
        let (kind, field) = split_target(target)?;
        let mut path = owner.path(Some(&kind));
        // a whole kind: the fields that had a runtime value, to say what applies now
        let before = if field.is_empty() {
            c.get(owner, Some(&kind))
                .ok()
                .map(|a| leaves(&a.body["runtime"]))
                .unwrap_or_default()
        } else {
            path.push_str(&format!("?field={}", enc(&path_string(&field))));
            Vec::new()
        };
        let a = c.call(Method::DELETE, &path, None, None)?;
        let a = c.ok(a, owner)?;
        if c.json {
            return c.print_json(&a.body);
        }
        if field.is_empty() {
            println!("{kind} on {}: runtime layer cleared", owner.label());
            let w = before
                .iter()
                .map(|p| path_string(p).len())
                .max()
                .unwrap_or(0);
            for p in &before {
                println!("  {:<w$}  {}", path_string(p), now_applies(&a.body, p));
            }
        } else {
            println!(
                "{kind}.{} on {} = {}",
                path_string(&field),
                owner.label(),
                now_applies(&a.body, &field)
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------ diff ------

    /// The runtime values of one kind that differ from the declared values and the
    /// built-in defaults beneath them. The answer's `overrides` gives the fields that
    /// replace a declared value, and the other runtime leaves are compared with the
    /// defaults of this build.
    fn kind_diff(owner: &Owner, k: &J) -> Vec<J> {
        let name = k["kind"].as_str().unwrap_or("");
        let base = merged(&owner.defaults(name), &k["declared"]);
        let overridden: Vec<Vec<String>> = k["overridden"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().and_then(parse_path))
                    .collect()
            })
            .unwrap_or_default();
        let entry = |field: &str, runtime: Option<&J>, base: Option<&J>, source: &str| {
            let mut d = json!({
                "kind": name,
                "field": field,
                "runtime": runtime,
                "base": base,
                "baseSource": source,
            });
            match owner {
                Owner::Dataset(ds) => d["dataset"] = ds.as_str().into(),
                Owner::Server => d["scope"] = "server".into(),
            }
            d
        };
        let mut out = Vec::new();
        let over = overrides(k);
        let over_paths: Vec<Vec<String>> = over.iter().filter_map(|o| parse_path(&o.0)).collect();
        for (f, d, r) in &over {
            out.push(entry(f, Some(r), Some(d), "declared"));
        }
        let has_overrides = k.get("overrides").is_some();
        for p in leaves(&k["runtime"]) {
            if over_paths.iter().any(|o| starts_with(&p, o)) {
                continue;
            }
            let rv = at(&k["runtime"], &p);
            let bv = at(&base, &p);
            if rv == bv {
                continue;
            }
            let declared = at(&k["declared"], &p);
            let ignored = overridden.iter().any(|o| p.starts_with(o));
            // a server that answers `overrides` lists every unlocked declared value it
            // replaces, so what is left here differs from a default or is ignored
            if has_overrides && declared.is_some() && !ignored {
                continue;
            }
            let mut d = entry(
                &path_string(&p),
                rv,
                bv,
                if declared.is_some() {
                    "declared"
                } else {
                    "default"
                },
            );
            if ignored {
                d["ignored"] = true.into();
            }
            out.push(d);
        }
        out.sort_by(|a, b| a["field"].as_str().cmp(&b["field"].as_str()));
        out
    }

    fn print_diff(c: &Client, out: &[J], what: &str) -> Result<()> {
        if c.json {
            return c.print_json(&json!({ "differences": out }));
        }
        if out.is_empty() {
            println!("no runtime value differs from the declared {what}");
        }
        for d in out {
            let base = match &d["base"] {
                J::Null => "unset".to_string(),
                v => show(v),
            };
            let owner = match d["dataset"].as_str() {
                Some(ds) => format!("/{ds}"),
                None => "server".into(),
            };
            println!(
                "{owner} {}.{}: runtime {}, {} {}{}",
                d["kind"].as_str().unwrap_or(""),
                d["field"].as_str().unwrap_or(""),
                match &d["runtime"] {
                    J::Null => "null (removed)".to_string(),
                    v => show(v),
                },
                d["baseSource"].as_str().unwrap_or(""),
                base,
                if d["ignored"] == true {
                    " (locked, so the runtime value is ignored)"
                } else {
                    ""
                }
            );
        }
        Ok(())
    }

    fn diff_cmd(c: &Client, dataset: Option<&str>) -> Result<()> {
        let names: Vec<String> = match dataset {
            Some(d) => vec![d.to_string()],
            None => {
                let a = c.call(Method::GET, "/$/datasets", None, None)?;
                let a = c.ok(a, &Owner::Dataset(String::new()))?;
                a.body["datasets"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|d| d["name"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };
        let mut out = Vec::new();
        for name in &names {
            let owner = Owner::dataset(name)?;
            let a = c.get(&owner, None)?;
            if let Some(kinds) = a.body["kinds"].as_object() {
                for k in kinds.values() {
                    out.extend(kind_diff(&owner, k));
                }
            }
        }
        print_diff(c, &out, "settings")
    }

    /// `diff --global`: the server-wide kinds this build knows.
    fn diff_server(c: &Client) -> Result<()> {
        let mut out = Vec::new();
        for k in crate::settings::SERVER_KINDS {
            let a = c.get(&Owner::Server, Some(k.name))?;
            out.extend(kind_diff(&Owner::Server, &a.body));
        }
        print_diff(c, &out, "server configuration")
    }

    // ----------------------------------------------------------------- apply ------

    fn apply(c: &Client, file: &Path) -> Result<()> {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        crate::settings::Declared::parse(&text, crate::settings::Providers::Unchecked)
            .map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
        let doc: J = serde_json::from_str(&text)?;
        // the locks, which only the server's settings file can set
        let mut locks = Vec::new();
        let mut lock_list = |scope: String, v: &J| {
            for l in v.as_array().into_iter().flatten() {
                if let Some(f) = l.as_str() {
                    locks.push(json!({ "scope": scope, "field": f }));
                }
            }
        };
        lock_list("defaults".into(), &doc["defaults"]["locked"]);
        if let Some(m) = doc["datasets"].as_object() {
            for (name, e) in m {
                lock_list(format!("datasets.{name}"), &e["locked"]);
            }
        }
        lock_list("server".into(), &doc["server"]["locked"]);
        let entry_kinds = |e: &J| -> Map<String, J> {
            e.as_object()
                .map(|m| {
                    m.iter()
                        .filter(|(k, _)| *k != "locked")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let defaults = entry_kinds(&doc["defaults"]);
        let entries: Map<String, J> = doc["datasets"].as_object().cloned().unwrap_or_default();
        let a = c.call(Method::GET, "/$/datasets", None, None)?;
        let a = c.ok(a, &Owner::Dataset(String::new()))?;
        let on_server: Vec<String> = a.body["datasets"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|d| d["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut applied = Vec::new();
        let mut failed = Vec::new();
        let mut skipped = Vec::new();
        for name in entries.keys() {
            if !on_server.contains(name) {
                skipped.push(json!({ "dataset": name, "reason": "no such dataset on the server" }));
            }
        }
        for name in &on_server {
            let mut kinds = defaults.clone();
            if let Some(e) = entries.get(name) {
                for (k, v) in entry_kinds(e) {
                    // keep a `null`, which resets that field
                    let m = overlaid(kinds.get(&k).unwrap_or(&J::Null), &v);
                    kinds.insert(k, m);
                }
            }
            let owner = Owner::dataset(name)?;
            for (kind, patch) in kinds {
                let fields: Vec<String> = leaves(&patch).iter().map(|p| path_string(p)).collect();
                if fields.is_empty() {
                    continue;
                }
                let a = c.call(Method::PATCH, &owner.path(Some(&kind)), None, Some(&patch))?;
                if (200..300).contains(&a.status) {
                    applied.push(json!({ "dataset": name, "kind": kind, "fields": fields }));
                } else {
                    let mut f = json!({
                        "dataset": name,
                        "kind": kind,
                        "error": c.explain(&a, &owner),
                    });
                    if let Some(code) = a.body["code"].as_str() {
                        f["code"] = code.into();
                    }
                    if a.body["fields"].is_array() {
                        f["fields"] = a.body["fields"].clone();
                    }
                    failed.push(f);
                }
            }
        }
        if c.json {
            c.print_json(&json!({
                "applied": applied,
                "failed": failed,
                "skipped": skipped,
                "locksNotApplied": locks,
            }))?;
        } else {
            for a in &applied {
                println!(
                    "/{} {}: {}",
                    a["dataset"].as_str().unwrap_or(""),
                    a["kind"].as_str().unwrap_or(""),
                    a["fields"]
                        .as_array()
                        .map(|f| f
                            .iter()
                            .filter_map(J::as_str)
                            .collect::<Vec<_>>()
                            .join(", "))
                        .unwrap_or_default()
                );
            }
            for s in &skipped {
                println!(
                    "/{}: skipped, {}",
                    s["dataset"].as_str().unwrap_or(""),
                    s["reason"].as_str().unwrap_or("")
                );
            }
            for f in &failed {
                eprintln!(
                    "/{} {}: {}",
                    f["dataset"].as_str().unwrap_or(""),
                    f["kind"].as_str().unwrap_or(""),
                    f["error"].as_str().unwrap_or("")
                );
            }
            if !locks.is_empty() {
                println!(
                    "not applied, since only the server's settings file can lock: {}",
                    locks
                        .iter()
                        .map(|l| format!(
                            "{} ({})",
                            l["field"].as_str().unwrap_or(""),
                            l["scope"].as_str().unwrap_or("")
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        if !failed.is_empty() {
            bail!(
                "{} of {} patches failed",
                failed.len(),
                failed.len() + applied.len()
            );
        }
        Ok(())
    }
}
