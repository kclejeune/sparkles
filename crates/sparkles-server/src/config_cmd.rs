//! `sparkles config import SOURCE` and `sparkles config check SOURCE`: another
//! server's configuration converted into Sparkles settings. Fuseki is the only source,
//! and its conversion lives in `fuseki_config` (spec G08).

use crate::fuseki_config::{self, UserSource};
use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    cmd: ConfigCmd,
}

/// The servers whose configurations convert.
#[derive(Clone, Copy, ValueEnum)]
enum Source {
    /// Apache Jena Fuseki: config.ttl, the service files of run/configuration/, and
    /// shiro.ini or the password file of fuseki:passwd
    Fuseki,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Convert another server's configuration into a `serve` script, settings files and
    /// an auth configuration, and report what was converted, approximated or
    /// unsupported (exit status 1 when something important could not be converted, 2
    /// on an error)
    Import {
        /// The kind of configuration to read
        #[arg(value_enum)]
        source: Source,
        #[command(flatten)]
        input: Input,
        /// Directory to write serve.sh, load.sh, auth.toml, the dataset settings and
        /// report.txt into
        #[arg(long, default_value = "sparkles-config")]
        out: PathBuf,
        /// Only print the report: write nothing and hash no password
        #[arg(long)]
        check: bool,
        /// Write into --out even when it is not empty
        #[arg(long)]
        force: bool,
    },
    /// Report what `config import` would convert, without writing anything (the same
    /// as `config import --check`, with the same exit statuses)
    Check {
        /// The kind of configuration to read
        #[arg(value_enum)]
        source: Source,
        #[command(flatten)]
        input: Input,
    },
}

/// The configuration, its users and the report format.
#[derive(Args)]
struct Input {
    /// Fuseki configuration files (config.ttl and service files), or Fuseki base
    /// directories holding config.ttl, configuration/ and shiro.ini
    #[arg(required = true, value_name = "PATH")]
    inputs: Vec<PathBuf>,
    /// Shiro's shiro.ini with the users and URL rules (default: a shiro.ini next to the
    /// configuration)
    #[arg(long, value_name = "FILE", conflicts_with = "passwd")]
    shiro: Option<PathBuf>,
    /// The password file of fuseki:passwd (default: the file the configuration names)
    #[arg(long, value_name = "FILE")]
    passwd: Option<PathBuf>,
    /// Report format: text or json
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
}

/// `sparkles config …`
pub fn run(args: ConfigArgs) -> Result<()> {
    let (source, input, out) = match args.cmd {
        ConfigCmd::Import {
            source,
            input,
            out,
            check,
            force,
        } => (source, input, (!check).then_some((out, force))),
        ConfigCmd::Check { source, input } => (source, input, None),
    };
    match source {
        Source::Fuseki => fuseki_config::import(
            &input.inputs,
            &UserSource {
                shiro: input.shiro,
                passwd: input.passwd,
            },
            out.as_ref().map(|(dir, force)| (dir.as_path(), *force)),
            input.format == "json",
        ),
    }
}
