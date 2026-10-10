//! `sparkles settings`: the layered dataset settings of spec C19 §8. `check` validates
//! a settings file of `serve --settings` without a server.

use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct SettingsArgs {
    #[command(subcommand)]
    cmd: SettingsCmd,
}

#[derive(Subcommand, Debug)]
enum SettingsCmd {
    /// Validate a settings file offline: its form, the kinds and fields it names, and
    /// every effective object it declares (exit status 1 when it is not valid). With
    /// --model-config, roles and sendByProvider must name configured providers and
    /// models they allow
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
    }
}
