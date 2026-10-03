//! The output of a conversion (spec G08 §3): `serve.sh`, `load.sh`, `auth.toml`, the
//! settings files of each dataset and the report.

use super::access::{AuthPlan, Grants};
use super::convert::{DatasetPlan, Mode, Plan, SCHEMA_GRAPH};
use super::report::Report;
use super::users::Password;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Files a dataset's settings need next to `serve.sh`.
pub struct DatasetFiles {
    pub text: Option<PathBuf>,
    pub geo: Option<PathBuf>,
    pub rdfs: Option<PathBuf>,
    pub rules: Option<PathBuf>,
}

/// Where the settings files of `ds` go, relative to the output directory.
pub fn dataset_files(ds: &DatasetPlan) -> DatasetFiles {
    let dir = PathBuf::from("datasets").join(&ds.name);
    let reasoning_rules = ds
        .reasoning
        .as_ref()
        .is_some_and(|r| !r.rule_files.is_empty() || !r.rule_texts.is_empty());
    DatasetFiles {
        text: ds.text.as_ref().map(|_| dir.join("text.json")),
        geo: ds.geo.as_ref().map(|_| dir.join("geo.json")),
        rdfs: ds.rdfs.as_ref().map(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "rdfs-schema.ttl".into());
            dir.join(format!("rdfs-{name}"))
        }),
        rules: reasoning_rules.then(|| dir.join("rules.rules")),
    }
}

/// The database directory of a persistent dataset, relative to the output directory.
pub fn db_dir(ds: &DatasetPlan) -> PathBuf {
    PathBuf::from("db").join(&ds.name)
}

/// The arguments of `sparkles serve`, with paths relative to the output directory.
pub fn serve_args(plan: &Plan) -> Vec<String> {
    let mut a: Vec<String> = vec!["serve".into(), "--data".into(), "data".into()];
    let s = &plan.server;
    if s.union_default_graph {
        a.push("--union-default-graph".into());
    }
    if let Some(t) = s.timeout {
        a.extend(["--timeout".into(), num(t)]);
        if t > 1800.0 {
            a.extend(["--max-timeout".into(), num(t)]);
        }
    }
    if let Some(t) = s.update_timeout {
        a.extend(["--update-timeout".into(), num(t)]);
    }
    if s.read_only {
        a.push("--read-only".into());
    }
    if s.gsp_direct_naming {
        a.push("--gsp-direct-naming".into());
    }
    if s.metrics_fuseki_names {
        a.push("--metrics-fuseki-names".into());
    }
    if s.auto_reason && !s.read_only {
        a.extend(["--auto-reason".into(), "5".into()]);
    }
    if plan.auth.is_some() {
        a.extend(["--auth-config".into(), "auth.toml".into()]);
    }
    for ds in &plan.datasets {
        if ds.persistent(plan.mode) {
            a.extend([
                "--loc".into(),
                format!("{}={}", ds.name, db_dir(ds).display()),
            ]);
        } else {
            a.extend(["--mem".into(), ds.name.clone()]);
        }
        let files = dataset_files(ds);
        if let Some(p) = files.text {
            a.extend(["--text".into(), format!("{}={}", ds.name, p.display())]);
        }
        if let Some(p) = files.geo {
            a.extend(["--geo".into(), format!("{}={}", ds.name, p.display())]);
        }
        if let Some(p) = files.rdfs {
            a.extend(["--rdfs".into(), format!("{}={}", ds.name, p.display())]);
        }
    }
    a
}

fn num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as u64)
    } else {
        format!("{x}")
    }
}

/// A word for `sh`: quoted unless it is plain.
pub fn sh(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,@%+".contains(&b))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn serve_script(plan: &Plan) -> String {
    let args = serve_args(plan);
    let mut s = String::from(
        "#!/bin/sh\n\
         # Sparkles, converted from a Fuseki configuration by `sparkles fuseki-config convert`.\n\
         # Run load.sh once before the first start. Extra arguments go to `sparkles serve`.\n\
         set -eu\n\
         cd \"$(dirname \"$0\")\"\n\
         exec \"${SPARKLES:-sparkles}\"",
    );
    let mut first = true;
    for w in args {
        if w.starts_with("--") || first {
            s.push_str(" \\\n  ");
        } else {
            s.push(' ');
        }
        first = false;
        s.push_str(&sh(&w));
    }
    s.push_str(" \\\n  \"$@\"\n");
    s
}

fn load_script(plan: &Plan) -> String {
    let mut s = String::from(
        "#!/bin/sh\n\
         # Moves the data of the Fuseki datasets into the Sparkles databases under db/.\n\
         # Run it once, with Fuseki stopped, before the first start of serve.sh.\n\
         set -eu\n\
         cd \"$(dirname \"$0\")\"\n\
         SPARKLES=\"${SPARKLES:-sparkles}\"\n",
    );
    let mut any = false;
    for ds in &plan.datasets {
        if !ds.persistent(plan.mode) {
            if ds.reasoning.is_some() {
                s.push_str(&format!(
                    "\n# /{0} is in memory: after loading data, materialize its inferences with\n\
                     # POST /$/reason/{0}.\n",
                    ds.name
                ));
            }
            continue;
        }
        let db = db_dir(ds).display().to_string();
        let mut lines = Vec::new();
        if let Some(loc) = &ds.tdb {
            let dump = format!("{}.nq", ds.name);
            let tool = if ds.tdb1 { "tdbdump" } else { "tdb2.tdbdump" };
            lines.push(format!(
                "# Fuseki's {} database: export it with Jena's {tool}, then load the dump.",
                if ds.tdb1 { "TDB1" } else { "TDB2" }
            ));
            lines.push(format!(
                "{tool} --loc {} > {}",
                sh(&loc.to_string_lossy()),
                sh(&dump)
            ));
            lines.push(format!(
                "\"$SPARKLES\" load --loc {} {}",
                sh(&db),
                sh(&dump)
            ));
        }
        for f in &ds.data {
            let graph = match &f.graph {
                Some(g) => format!(" --graph {}", sh(g)),
                None => String::new(),
            };
            lines.push(format!(
                "\"$SPARKLES\" load --loc {}{graph} {}",
                sh(&db),
                sh(&f.path.to_string_lossy())
            ));
        }
        if let Some(r) = &ds.reasoning {
            for f in &r.schema {
                lines.push(format!(
                    "\"$SPARKLES\" load --loc {} --graph {} {}",
                    sh(&db),
                    SCHEMA_GRAPH,
                    sh(&f.to_string_lossy())
                ));
            }
            let mut cmd = format!("\"$SPARKLES\" infer --loc {}", sh(&db));
            match (&dataset_files(ds).rules, r.profile) {
                (Some(rules), _) => {
                    cmd.push_str(&format!(" --rules {}", sh(&rules.to_string_lossy())))
                }
                (None, Some(p)) => cmd.push_str(&format!(" --profile {p}")),
                (None, None) => cmd.push_str(" --profile rdfs"),
            }
            if r.geo_vocab {
                cmd.push_str(" --vocab geosparql");
            }
            if r.default_geometry {
                cmd.push_str(" --geo-default-geometry");
            }
            if !r.schema.is_empty() {
                cmd.push_str(&format!(
                    " --data-graph default --ontology-graph {SCHEMA_GRAPH}"
                ));
            }
            lines.push(cmd);
        }
        if lines.is_empty() {
            continue;
        }
        any = true;
        s.push_str(&format!("\n# /{}\n", ds.name));
        for l in lines {
            s.push_str(&l);
            s.push('\n');
        }
    }
    if !any {
        s.push_str("\n# Nothing to load: every dataset starts empty, as in Fuseki.\n");
    }
    s
}

/// A TOML basic string.
fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(v: &[String]) -> String {
    let items: Vec<String> = v.iter().map(|s| q(s)).collect();
    format!("[{}]", items.join(", "))
}

/// The keys of a grants table (`[anonymous]`, `[roles.x]`, `[[users]]`).
fn grant_keys(g: &Grants) -> Vec<String> {
    let mut out = Vec::new();
    if !g.datasets.is_empty() {
        let items: Vec<String> = g
            .datasets
            .iter()
            .map(|(d, l)| format!("{} = {}", q(d), q(l)))
            .collect();
        out.push(format!("datasets = {{ {} }}", items.join(", ")));
    }
    if !g.server.is_empty() {
        let s: Vec<String> = g.server.iter().map(|s| s.to_string()).collect();
        out.push(format!("server = {}", list(&s)));
    }
    out
}

/// The `[[TABLE.grants]]` entries of a grants table.
fn grant_tables(table: &str, g: &Grants) -> Vec<String> {
    let mut out = Vec::new();
    for gr in &g.grants {
        out.push(String::new());
        out.push(format!("[[{table}.grants]]"));
        out.push(format!("dataset = {}", q(&gr.dataset)));
        out.push(format!("level = {}", q(gr.level)));
        if let Some(e) = &gr.endpoints {
            out.push(format!("endpoints = {}", list(e)));
        }
        if let Some(gs) = &gr.graphs {
            out.push(format!("graphs = {}", list(gs)));
        }
    }
    out
}

/// `auth.toml`. With `hash`, plain passwords are hashed; without it (`--check`), the
/// file is not written and nothing is hashed.
pub fn auth_toml(plan: &AuthPlan, realm: Option<&str>, report: &mut Report) -> Result<String> {
    let mut lines: Vec<String> = vec![
        "# Converted from a Fuseki configuration by `sparkles fuseki-config convert`.".into(),
        "# Check it with `sparkles auth check --config auth.toml`. Keep it at mode 0600.".into(),
        "version = 1".into(),
    ];
    if let Some(r) = realm {
        lines.push(format!("realm = {}", q(r)));
    }
    let anon = grant_keys(&plan.anonymous);
    if !anon.is_empty() || !plan.anonymous.grants.is_empty() {
        lines.push(String::new());
        lines.push("[anonymous]".into());
        lines.extend(anon);
        lines.extend(grant_tables("anonymous", &plan.anonymous));
    }
    for (name, g) in &plan.roles {
        lines.push(String::new());
        lines.push(format!("[roles.{}]", q(name)));
        lines.extend(grant_keys(g));
        lines.extend(grant_tables(&format!("roles.{}", q(name)), g));
    }
    for u in &plan.users {
        let mut block: Vec<String> = vec![String::new(), "[[users]]".into()];
        block.push(format!("name = {}", q(&u.name)));
        let mut usable = true;
        match &u.password {
            Password::Plain(pw) => match hash(pw) {
                Ok(h) => block.push(format!("password = {}", q(&h))),
                Err(e) => {
                    usable = false;
                    report.unsupported(
                        &u.source,
                        format!("user {}: the password could not be hashed: {e}", u.name),
                    );
                    block.push("password = \"$argon2id$…\"  # from `sparkles auth hash`".into());
                }
            },
            Password::Opaque(_) => {
                usable = false;
                block.push("password = \"$argon2id$…\"  # from `sparkles auth hash`".into());
            }
        }
        if !u.roles.is_empty() {
            block.push(format!("roles = {}", list(&u.roles)));
        }
        block.extend(grant_keys(&u.grants));
        block.extend(grant_tables("users", &u.grants));
        if usable {
            lines.extend(block);
        } else {
            lines.push(String::new());
            lines.push(format!(
                "# {}: set a password hash from `sparkles auth hash` and remove the comment marks.",
                u.name
            ));
            lines.extend(block.into_iter().skip(1).map(|l| {
                if l.is_empty() {
                    "#".to_string()
                } else {
                    format!("# {l}")
                }
            }));
        }
    }
    let mut s = lines.join("\n");
    s.push('\n');
    Ok(s)
}

#[cfg(feature = "auth")]
fn hash(pw: &str) -> Result<String> {
    if pw.is_empty() {
        bail!("it is empty");
    }
    crate::auth::hash_password(pw)
}

#[cfg(not(feature = "auth"))]
fn hash(_: &str) -> Result<String> {
    bail!("this build has no authentication (cargo feature \"auth\")")
}

/// Write a file readable by its owner only.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    std::io::Write::write_all(&mut f, bytes)?;
    Ok(())
}

fn write_script(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// The settings files of the datasets (text, geo, RDFS schema, rules) under `out`.
pub fn write_dataset_files(plan: &Plan, out: &Path, report: &mut Report) -> Result<()> {
    for ds in &plan.datasets {
        let files = dataset_files(ds);
        let place = format!("dataset /{}", ds.name);
        let mkdir = |p: &Path| -> Result<()> {
            if let Some(d) = out.join(p).parent() {
                std::fs::create_dir_all(d)?;
            }
            Ok(())
        };
        if let (Some(p), Some(v)) = (&files.text, &ds.text) {
            mkdir(p)?;
            std::fs::write(out.join(p), serde_json::to_string_pretty(v)? + "\n")?;
        }
        if let (Some(p), Some(v)) = (&files.geo, &ds.geo) {
            mkdir(p)?;
            std::fs::write(out.join(p), serde_json::to_string_pretty(v)? + "\n")?;
        }
        if let (Some(p), Some(src)) = (&files.rdfs, &ds.rdfs) {
            mkdir(p)?;
            if let Err(e) = std::fs::copy(src, out.join(p)) {
                report.unsupported(
                    &place,
                    format!("the RDFS schema {} cannot be read: {e}", src.display()),
                );
            }
        }
        if let (Some(p), Some(r)) = (&files.rules, &ds.reasoning) {
            mkdir(p)?;
            let mut text = String::new();
            for f in &r.rule_files {
                match std::fs::read_to_string(f) {
                    Ok(t) => {
                        text.push_str(&format!("# from {}\n", f.display()));
                        text.push_str(&t);
                        text.push('\n');
                    }
                    Err(e) => report.unsupported(
                        &place,
                        format!("the rule file {} cannot be read: {e}", f.display()),
                    ),
                }
            }
            for t in &r.rule_texts {
                text.push_str(t);
                text.push('\n');
            }
            if r.profile.is_some() {
                // a reasoner with both a profile and rules keeps the profile's rules too
                text = format!("@include <{}> .\n{text}", r.profile.unwrap_or("rdfs"));
            }
            std::fs::write(out.join(p), text)?;
        }
    }
    Ok(())
}

/// Write the whole output directory.
pub fn write_dir(plan: &mut Plan, out: &Path, force: bool) -> Result<()> {
    if out.exists() {
        let empty = std::fs::read_dir(out)
            .with_context(|| format!("reading {}", out.display()))?
            .next()
            .is_none();
        if !empty && !force {
            bail!(
                "{} is not empty; choose another --out or pass --force",
                out.display()
            );
        }
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    debug_assert_eq!(plan.mode, Mode::Files);
    let mut report = std::mem::take(&mut plan.report);
    write_dataset_files(plan, out, &mut report)?;
    if let Some(a) = &plan.auth {
        let toml = auth_toml(a, plan.server.realm.as_deref(), &mut report)?;
        write_private(&out.join("auth.toml"), toml.as_bytes())?;
    }
    write_script(&out.join("serve.sh"), &serve_script(plan))?;
    write_script(&out.join("load.sh"), &load_script(plan))?;
    plan.report = report;
    std::fs::write(out.join("report.txt"), plan.report.text())?;
    Ok(())
}
