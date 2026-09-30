//! `sparkles auth …`: hashes, tokens and configuration checks.

use super::config::FileConfig;
use super::policy;
use anyhow::{Result, bail};
use clap::Subcommand;
use std::io::{BufRead, IsTerminal};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Read a password (or, with --token, an API token) and print its hash for the
    /// auth configuration
    Hash {
        /// Hash an `spk_…` token (sha256) instead of a password (argon2id)
        #[arg(long)]
        token: bool,
    },
    /// Generate an API token for the configuration file: the token on stdout (shown
    /// once), the `[[tokens]]` entry on stderr
    Token {
        /// Name of the token (its log name is token:NAME)
        #[arg(long, default_value = "token")]
        name: String,
    },
    /// Validate an auth configuration and print a summary; exits with 1 on errors
    Check {
        #[arg(long)]
        config: PathBuf,
    },
}

/// A secret from a no-echo prompt (twice) on a terminal, else the first stdin line.
fn read_secret(what: &str) -> Result<zeroize::Zeroizing<String>> {
    if std::io::stdin().is_terminal() {
        let a = zeroize::Zeroizing::new(rpassword::prompt_password(format!("{what}: "))?);
        let b = zeroize::Zeroizing::new(rpassword::prompt_password(format!("{what} (again): "))?);
        if *a != *b {
            bail!("the two entries differ");
        }
        return Ok(a);
    }
    let mut line = zeroize::Zeroizing::new(String::new());
    std::io::stdin().lock().read_line(&mut line)?;
    let s = line.trim_end_matches(['\n', '\r']).to_string();
    Ok(zeroize::Zeroizing::new(s))
}

pub fn run(cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Hash { token } => {
            if token {
                let t = read_secret("Token")?;
                if !policy::well_formed_token(&t) {
                    bail!("not an API token (expected spk_ followed by 43 characters)");
                }
                println!("{}", policy::token_hash(&t));
            } else {
                let pw = read_secret("Password")?;
                if pw.is_empty() {
                    bail!("empty password");
                }
                println!("{}", policy::hash_password(&pw)?);
            }
            Ok(())
        }
        AuthCmd::Token { name } => {
            if !super::config::valid_principal_name(&name) {
                bail!("invalid token name '{name}': use [A-Za-z0-9_.@-], at most 64 characters");
            }
            let t = policy::new_token();
            println!("{t}");
            eprintln!(
                "\n[[tokens]]\nname = \"{name}\"\nhash = \"{}\"\ndatasets = {{ }}  # e.g. {{ mydata = \"read\" }}",
                policy::token_hash(&t)
            );
            Ok(())
        }
        AuthCmd::Check { config } => match FileConfig::load(&config) {
            Ok((cfg, warnings)) => {
                let n = warnings.len();
                println!(
                    "{}; {n} warning{}",
                    cfg.summary(),
                    if n == 1 { "" } else { "s" }
                );
                for w in warnings {
                    println!("warning: {w}");
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        },
    }
}
