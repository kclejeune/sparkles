//! `sparkles lsp`: a language server over stdin and stdout (`lsp-server`, `lsp-types`)
//! for the languages `sparkles fmt` formats. It offers `textDocument/formatting` and
//! `textDocument/rangeFormatting` (the whole document, returned as the smallest edit), and
//! publishes syntax errors as diagnostics. Each document is formatted with the options of
//! the `.sparklesfmt.toml` nearest to its file, as `sparkles fmt` would.
//!
//! Not written yet: the command says so and exits.

use anyhow::{Result, bail};
use clap::Args;

#[derive(Args, Debug)]
pub struct LspArgs {
    /// Talk over stdin and stdout (the only transport; accepted because editors pass it)
    #[arg(long)]
    pub stdio: bool,
}

/// Serve until the client says `exit`.
pub fn run(args: LspArgs) -> Result<()> {
    let _ = args.stdio;
    bail!("sparkles lsp is not available yet")
}
