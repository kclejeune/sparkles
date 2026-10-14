//! `sparkles migrate`: upgrade a stopped server's databases (`--data`) or one database
//! (`--loc`) to the file versions this release writes. See `sparkles_core::migrate`.
//! Exit status: 0 when every database is inside the window (after `--check`, or once
//! upgraded), 1 when one is refused.

use anyhow::{Context, Result, bail};
use clap::Args;
use sparkles::migrate::{Outcome, Plan};
use std::io::Write;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct MigrateArgs {
    /// A stopped server's data directory: upgrade the databases its catalog registers
    /// (takes its catalog lock)
    #[arg(long, required_unless_present = "loc", conflicts_with = "loc")]
    data: Option<PathBuf>,
    /// One database directory
    #[arg(long)]
    loc: Option<PathBuf>,
    /// With --data, only this dataset (repeatable)
    #[arg(long = "dataset", value_name = "NAME", requires = "data")]
    datasets: Vec<String>,
    /// List each database's file versions, whether this release opens it, and the
    /// steps an upgrade would run, without changing anything
    #[arg(long)]
    check: bool,
    /// Keep each database as it was in a sibling directory named
    /// `.pre-migrate-<version>-<name>`
    #[arg(long)]
    keep_old: bool,
    /// text or json
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
}

pub fn run(a: MigrateArgs) -> Result<()> {
    // held until the end: no server can open the catalog meanwhile
    let mut _lock = None;
    let targets: Vec<(Option<String>, PathBuf)> = match (&a.loc, &a.data) {
        (Some(loc), _) => vec![(None, loc.clone())],
        (None, Some(data)) => {
            _lock = Some(
                sparkles::catalog::lock_data_dir(data)
                    .with_context(|| format!("locking {}", data.display()))?,
            );
            let all = registered(data)?;
            if a.datasets.is_empty() {
                all
            } else {
                for n in &a.datasets {
                    if !all.iter().any(|(m, _)| m.as_deref() == Some(n.as_str())) {
                        bail!(
                            "{} registers no persistent dataset named {n}",
                            data.display()
                        );
                    }
                }
                all.into_iter()
                    .filter(|(n, _)| n.as_ref().is_some_and(|n| a.datasets.contains(n)))
                    .collect()
            }
        }
        (None, None) => bail!("give --data DIR or --loc DB"),
    };
    // --check only plans, which changes nothing. Otherwise `migrate` plans under its own
    // locks, after it has recovered an interrupted run, and the report is that plan.
    let mut plans: Vec<(Option<String>, Plan)> = Vec::new();
    let mut outcomes: Vec<(Option<String>, Outcome)> = Vec::new();
    for (name, dir) in &targets {
        if a.check {
            let p = sparkles::migrate::plan(dir)
                .with_context(|| format!("reading the formats of {}", dir.display()))?;
            plans.push((name.clone(), p));
        } else {
            let (p, o) = sparkles::migrate::migrate_planned(dir, a.keep_old)
                .with_context(|| format!("upgrading {}", dir.display()))?;
            if let Some(o) = o {
                outcomes.push((name.clone(), o));
            }
            plans.push((name.clone(), p));
        }
    }
    let refused = plans.iter().any(|(_, p)| !p.in_window());
    let mut out = std::io::stdout().lock();
    if a.format == "json" {
        let json = serde_json::json!({
            "formats": sparkles::formats::report(),
            "datasets": plans.iter().map(|(n, p)| serde_json::json!({
                "name": n,
                "root": p.root,
                "inWindow": p.in_window(),
                "dataset": p.dataset,
                "files": p.files,
                "steps": p.steps,
                "refused": p.refused,
                "interrupted": p.interrupted,
                "upgraded": outcomes.iter().find(|(m, o)| m == n && o.root == p.root).map(|(_, o)| o),
            })).collect::<Vec<_>>(),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&json)?)?;
    } else {
        for (i, (name, p)) in plans.iter().enumerate() {
            if i > 0 {
                writeln!(out)?;
            }
            match name {
                Some(n) => writeln!(out, "== /{n} ({}) ==", p.root.display())?,
                None => writeln!(out, "== {} ==", p.root.display())?,
            }
            write_plan(&mut out, p)?;
            if let Some((_, o)) = outcomes.iter().find(|(m, o)| m == name && o.root == p.root) {
                if o.steps.is_empty() {
                    writeln!(out, "nothing to upgrade")?;
                } else {
                    writeln!(out, "upgraded: {} step(s) ran", o.steps.len())?;
                }
                if let Some(k) = &o.kept {
                    writeln!(out, "kept the previous files in {}", k.display())?;
                }
            }
        }
    }
    out.flush()?;
    drop(out);
    if refused {
        std::process::exit(1);
    }
    Ok(())
}

fn write_plan(out: &mut impl Write, p: &Plan) -> std::io::Result<()> {
    if let Some(d) = &p.dataset {
        writeln!(
            out,
            "dataset.json: format {}, minimum reader {}, written by {}",
            d.format,
            d.minimum_reader,
            d.written_by.as_deref().unwrap_or("an unrecorded release")
        )?;
    }
    for f in &p.files {
        let k = sparkles::formats::kind(f.kind).expect("a registered kind");
        let state = if f.version == k.writes {
            "current"
        } else if f.readable {
            "read as it is"
        } else {
            "not read"
        };
        writeln!(
            out,
            "{}: {} version {} ({state})",
            f.file, f.kind, f.version
        )?;
    }
    if let Some(i) = &p.interrupted {
        writeln!(out, "{i}")?;
    }
    if p.in_window() {
        writeln!(out, "inside the window: this release opens it")?;
    } else {
        writeln!(out, "outside the window: this release refuses it")?;
        for r in &p.refused {
            writeln!(out, "  {r}")?;
        }
    }
    for s in &p.steps {
        writeln!(
            out,
            "step: {} {} {} -> {}: {}",
            s.file, s.kind, s.from, s.to, s.summary
        )?;
    }
    Ok(())
}

/// The persistent datasets the catalog of `data` registers, with their directories.
fn registered(data: &std::path::Path) -> Result<Vec<(Option<String>, PathBuf)>> {
    let config = data.join(sparkles::catalog::CONFIG_FILE);
    let reg: serde_json::Value = match std::fs::read(&config) {
        Ok(b) => sparkles::formats::settings_from_slice(sparkles::catalog::CONFIG_FILE, &b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", config.display())),
    };
    let mut out = Vec::new();
    for d in reg["datasets"].as_array().into_iter().flatten() {
        if d["type"].as_str() != Some("persistent") {
            continue;
        }
        if let Some(n) = d["name"].as_str() {
            out.push((Some(n.to_string()), data.join("databases").join(n)));
        }
    }
    Ok(out)
}
