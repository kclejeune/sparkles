//! `sparkles patch`: apply RDF Patch files to a local database, one commit per file, or
//! send them to a server's patch endpoint.

use crate::tools::rdfpatch::{binary_input, open};
use anyhow::Result;
use sparkles::store::{PatchOptions, StoreOptions};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct PatchArgs {
    #[arg(long, required_unless_present = "server")]
    loc: Option<PathBuf>,
    /// The patch files, `-` for standard input. `.trp` is the binary form, anything else
    /// the text form, unless --format says otherwise.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// The form of the patches: text or binary (RDF Thrift)
    #[arg(long, value_parser = ["text", "binary"])]
    format: Option<String>,
    /// A message recorded with each commit (shown by `log` and in /$/commits)
    #[arg(long)]
    message: Option<String>,
    /// A server to send the patches to instead of a local database (with --dataset)
    #[arg(long, env = "SPARKLES_SERVER")]
    server: Option<String>,
    /// The dataset on --server
    #[arg(long)]
    dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    insecure_http: bool,
}

/// The line `sparkles patch` prints for an outcome, as `sparkles update` prints one.
fn summary(o: &sparkles::store::PatchOutcome, ms: f64) -> String {
    let r = &o.receipt;
    let commit = if o.aborted {
        format!("aborted · head {}", r.commit.seq)
    } else if r.committed {
        format!("commit {}", r.commit.seq)
    } else {
        format!("no change · head {}", r.commit.seq)
    };
    let mut s = format!("inserted {} · deleted {} · {commit}", o.inserted, o.deleted);
    if o.prefixes_set + o.prefixes_removed > 0 {
        s.push_str(&format!(
            " · prefixes set {} · removed {}",
            o.prefixes_set, o.prefixes_removed
        ));
    }
    s.push_str(&format!(" · {ms:.2} ms"));
    s.push_str(&crate::validation_note(r.validation.as_deref()));
    s
}

pub fn run(a: PatchArgs, opts: StoreOptions, no_validate: bool) -> Result<()> {
    let message = a
        .message
        .as_deref()
        .map(sparkles::annotations::validate_message)
        .transpose()?
        .flatten();
    let Some(loc) = a.loc else {
        let ds = crate::remote_dataset(a.server.as_deref(), a.dataset.as_deref())?;
        #[cfg(feature = "auth")]
        return crate::remote::client::patch(
            a.server.as_deref(),
            a.insecure_http,
            ds,
            &a.files,
            a.format.as_deref(),
            message.as_deref(),
        );
        #[cfg(not(feature = "auth"))]
        return crate::no_remote(ds, a.insecure_http);
    };
    let store = crate::open_for_write(&loc, opts, no_validate)?;
    let many = a.files.len() > 1;
    for path in &a.files {
        let t0 = std::time::Instant::now();
        let o = store.apply_patch(
            open(path)?,
            &PatchOptions {
                binary: binary_input(path, a.format.as_deref()),
                write: sparkles::guard::WriteOptions {
                    message: message.clone(),
                    ..Default::default()
                },
            },
        );
        let o = o.map_err(|e| anyhow::anyhow!("{}: {e}", name(path)))?;
        let line = summary(&o, t0.elapsed().as_secs_f64() * 1000.0);
        if many {
            eprintln!("{}: {line}", name(path));
        } else {
            eprintln!("{line}");
        }
    }
    Ok(())
}

fn name(p: &Path) -> String {
    if p.as_os_str() == "-" {
        "<stdin>".into()
    } else {
        p.display().to_string()
    }
}
