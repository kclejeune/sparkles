//! `sparkles check`: read-only integrity check of one database (`--loc`) or of every
//! database of a server data directory (`--data`). Exit status: 0 clean, 1 errors,
//! 2 warnings only.

use anyhow::{Context, Result, bail};
use sparkles::check::{CheckOptions, CheckReport, Status};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn run(loc: Option<PathBuf>, data: Option<PathBuf>, format: &str, quick: bool) -> Result<()> {
    if format != "text" && format != "json" {
        bail!("--format must be text or json");
    }
    let opts = CheckOptions { quick };
    let targets: Vec<(Option<String>, PathBuf)> = match (loc, data) {
        (Some(loc), _) => vec![(None, loc)],
        (None, Some(data)) => datasets(&data)?,
        (None, None) => bail!("give --loc DB or --data DIR"),
    };
    let mut reports: Vec<(Option<String>, CheckReport)> = Vec::new();
    for (name, dir) in targets {
        let r = sparkles::check::check(&dir, &opts)
            .with_context(|| format!("checking {}", dir.display()))?;
        reports.push((name, r));
    }
    let status = reports
        .iter()
        .map(|(_, r)| r.status)
        .max()
        .unwrap_or(Status::Ok);
    let mut out = std::io::stdout().lock();
    if format == "json" {
        #[derive(serde::Serialize)]
        struct Named<'a> {
            name: &'a str,
            report: &'a CheckReport,
        }
        #[derive(serde::Serialize)]
        struct All<'a> {
            status: Status,
            datasets: Vec<Named<'a>>,
        }
        let json = if reports.len() == 1 && reports[0].0.is_none() {
            serde_json::to_string_pretty(&reports[0].1)?
        } else {
            serde_json::to_string_pretty(&All {
                status,
                datasets: reports
                    .iter()
                    .map(|(n, r)| Named {
                        name: n.as_deref().unwrap_or(""),
                        report: r,
                    })
                    .collect(),
            })?
        };
        writeln!(out, "{json}")?;
    } else {
        for (i, (name, r)) in reports.iter().enumerate() {
            if i > 0 {
                writeln!(out)?;
            }
            if let Some(n) = name {
                writeln!(out, "== /{n} ==")?;
            }
            write!(out, "{}", r.to_text())?;
        }
        if reports.len() != 1 {
            writeln!(
                out,
                "\n{}: {} databases checked",
                status.name(),
                reports.len()
            )?;
        }
    }
    out.flush()?;
    drop(out);
    std::process::exit(match status {
        Status::Ok => 0,
        Status::Error => 1,
        Status::Warning => 2,
    })
}

/// The databases of a server data directory (`databases/*`), by name; unfinished
/// clones are left out.
fn datasets(data: &Path) -> Result<Vec<(Option<String>, PathBuf)>> {
    let dir = data.join("databases");
    let mut out: Vec<(Option<String>, PathBuf)> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with(".clone-"))
        .map(|n| (Some(n.clone()), dir.join(n)))
        .collect();
    out.sort();
    Ok(out)
}
