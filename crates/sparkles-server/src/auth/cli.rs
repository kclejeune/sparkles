//! `sparkles auth …`: offline helpers (hashes, static tokens, configuration checks) and
//! the remote login and token commands.

use super::config::FileConfig;
use super::policy;
use crate::remote::client;
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
    /// Generate a static API token for the configuration file: the token on stdout
    /// (shown once), the `[[tokens]]` entry on stderr
    GenToken {
        /// Name of the token (its id is cfg-NAME)
        #[arg(long, default_value = "token")]
        name: String,
    },
    /// Validate an auth configuration and print a summary; exits with 1 on errors
    Check {
        #[arg(long)]
        config: PathBuf,
    },
    /// Log in to a server (browser, or device code on headless machines) and store an
    /// API token in ~/.config/sparkles/credentials.toml
    Login {
        #[arg(long, env = "SPARKLES_SERVER")]
        server: String,
        /// Authenticate in a local browser
        #[arg(long, conflicts_with = "device")]
        web: bool,
        /// Authenticate with a device code (headless)
        #[arg(long)]
        device: bool,
        /// Label of the token (shown on the server's token list)
        #[arg(long)]
        name: Option<String>,
        /// Store this existing token instead of logging in
        #[arg(long, conflicts_with_all = ["web", "device"])]
        token: Option<String>,
        /// Make this the default server even when another is
        #[arg(long)]
        set_default: bool,
        /// Allow plain http to a host other than localhost
        #[arg(long)]
        insecure_http: bool,
    },
    /// Revoke the stored token on the server and forget it
    Logout {
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        #[arg(long)]
        insecure_http: bool,
    },
    /// Show the stored logins and what they may do
    Status {
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        #[arg(long)]
        insecure_http: bool,
    },
    /// Create, list or revoke API tokens on a server (with the stored login)
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Mint a token: printed once on stdout, id and expiry on stderr
    Create {
        #[arg(long)]
        name: String,
        /// A dataset grant NAME=LEVEL (read, write or admin; NAME may use `*`); repeat
        /// for several (default: all your access)
        #[arg(long = "dataset", value_name = "DS=LEVEL")]
        datasets: Vec<String>,
        /// A server permission (metrics, federate, server-admin, or `*`)
        #[arg(long = "server-perm", value_name = "PERM")]
        server_perms: Vec<String>,
        /// Lifetime, e.g. 7d (default: the server's default)
        #[arg(long)]
        expires: Option<String>,
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        #[arg(long)]
        insecure_http: bool,
    },
    /// List your tokens (--all: every token, with server-admin)
    List {
        #[arg(long)]
        all: bool,
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        #[arg(long)]
        insecure_http: bool,
    },
    /// Revoke a token by id
    Revoke {
        id: String,
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        #[arg(long)]
        insecure_http: bool,
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
        AuthCmd::GenToken { name } => {
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
        AuthCmd::Login {
            server,
            web,
            device,
            name,
            token,
            set_default,
            insecure_http,
        } => client::login(
            &server,
            web,
            device,
            name.as_deref(),
            token.as_deref(),
            set_default,
            insecure_http,
        ),
        AuthCmd::Logout {
            server,
            insecure_http,
        } => client::logout(server.as_deref(), insecure_http),
        AuthCmd::Status {
            server,
            insecure_http,
        } => client::status(server.as_deref(), insecure_http),
        AuthCmd::Token { cmd } => match cmd {
            TokenCmd::Create {
                name,
                datasets,
                server_perms,
                expires,
                server,
                insecure_http,
            } => client::token_create(
                server.as_deref(),
                insecure_http,
                &name,
                &datasets,
                &server_perms,
                expires.as_deref(),
            ),
            TokenCmd::List {
                all,
                server,
                insecure_http,
            } => client::token_list(server.as_deref(), insecure_http, all),
            TokenCmd::Revoke {
                id,
                server,
                insecure_http,
            } => client::token_revoke(server.as_deref(), insecure_http, &id),
        },
    }
}
