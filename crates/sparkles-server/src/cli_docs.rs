//! `sparkles completions` and `sparkles man`: shell completion scripts and man pages,
//! generated from the clap definition of the CLI.

use anyhow::{Context, Result};
use clap::Command;
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct CompletionsArgs {
    /// The shell: bash, zsh, fish, elvish or powershell
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

#[derive(clap::Args)]
pub struct ManArgs {
    /// Write sparkles.1 and one page per subcommand (sparkles-serve.1, …) into this
    /// directory, creating it if needed; without it, print sparkles.1 on stdout
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

/// Print the completion script for `args.shell` on stdout.
pub fn completions(args: CompletionsArgs, mut cmd: Command) -> Result<()> {
    let name = cmd.get_name().to_string();
    let mut out = Vec::new();
    clap_complete::generate(args.shell, &mut cmd, name, &mut out);
    write_stdout(&out)
}

/// Write the man pages.
pub fn man(args: ManArgs, cmd: Command) -> Result<()> {
    match args.dir {
        Some(dir) => {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            clap_mangen::generate_to(cmd, &dir)
                .with_context(|| format!("cannot write man pages to {}", dir.display()))?;
        }
        None => {
            let mut out = Vec::new();
            clap_mangen::Man::new(cmd.disable_help_subcommand(true)).render(&mut out)?;
            write_stdout(&out)?;
        }
    }
    Ok(())
}

/// Write `bytes` to stdout. A closed pipe (`sparkles completions zsh | head`) is not an
/// error.
pub fn write_stdout(bytes: &[u8]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        r => Ok(r?),
    }
}
