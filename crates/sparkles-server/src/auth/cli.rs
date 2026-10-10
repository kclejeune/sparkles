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
    /// Print the grants of a template as `[[roles.NAME.grants]]` for the auth
    /// configuration. `--template agent` writes an agent's memory grants (spec C18
    /// §8.6): read on the dataset, write on its own graphs on `main` and on its proposal
    /// branches, write on the curated graphs only on its proposal branches, and never
    /// merge. Give the role to the agent's token or user with `roles = ["NAME"]`
    Grant {
        /// The template
        #[arg(long, value_parser = ["agent"])]
        template: String,
        /// The agent's name, which names the role and its proposal branches
        /// `proposals.NAME.*`
        #[arg(long)]
        agent: String,
        /// The dataset
        #[arg(long)]
        dataset: String,
        /// The IRI prefix of the agent's own graphs, such as
        /// https://example.org/memory/agents/agent-7/
        #[arg(long, value_name = "IRI")]
        session_graphs: String,
        /// A consolidated or curated graph, or a pattern with `*`, that the agent may
        /// change only on its proposal branches (repeatable)
        #[arg(long = "curated", value_name = "IRI")]
        curated: Vec<String>,
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

/// The endpoints an agent's write grants cover: everything but merges.
const AGENT_ENDPOINTS: [&str; 6] = ["query", "update", "gsp-r", "gsp-rw", "info", "branches"];

/// The `agent` template of C18 §8.6 as TOML grants of the role `agent`.
pub fn agent_template(
    agent: &str,
    dataset: &str,
    session_graphs: &str,
    curated: &[String],
) -> Result<String> {
    if !super::config::valid_principal_name(agent) {
        bail!("invalid agent name '{agent}': use [A-Za-z0-9_.@-], at most 64 characters");
    }
    if !sparkles::branch::valid_name(&format!("proposals.{agent}.x")) {
        bail!("agent name '{agent}' cannot name branches: use [A-Za-z0-9_.-]");
    }
    if dataset.is_empty()
        || !dataset
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'*'))
    {
        bail!("invalid dataset name '{dataset}'");
    }
    let iri = |what: &str, s: &str| -> Result<()> {
        if oxrdf::NamedNode::new(s.replace('*', "x")).is_err() {
            bail!("{what} {s:?} is not an IRI or an IRI pattern with *");
        }
        Ok(())
    };
    iri("--session-graphs", session_graphs)?;
    for c in curated {
        iri("--curated", c)?;
        if crate::auth::glob(c, &format!("{}x", session_graphs.trim_end_matches('*'))) {
            bail!("--curated {c} covers the agent's own graphs");
        }
    }
    let own = if session_graphs.ends_with('*') {
        session_graphs.to_string()
    } else {
        format!("{session_graphs}*")
    };
    let list = |v: &[&str]| {
        v.iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let endpoints = list(&AGENT_ENDPOINTS);
    // branch names hold no slash, so proposal branches are proposals.{agent}.*
    let proposals = format!("proposals.{agent}.*");
    let mut out = format!(
        "# The memory grants of agent {agent} on dataset {dataset} (sparkles auth grant --template agent).\n\
         # It reads the dataset, writes its own graphs on main and on {proposals}, and changes\n\
         # curated graphs only on {proposals}. No grant covers merges.\n\
         [[roles.{agent:?}.grants]]\n\
         dataset = {dataset:?}\n\
         level = \"read\"\n\n\
         [[roles.{agent:?}.grants]]\n\
         dataset = {dataset:?}\n\
         level = \"write\"\n\
         graphs = [{own:?}]\n\
         branches = [\"main\", {proposals:?}]\n\
         endpoints = [{endpoints}]\n"
    );
    if !curated.is_empty() {
        let graphs: Vec<&str> = curated.iter().map(String::as_str).collect();
        out.push_str(&format!(
            "\n[[roles.{agent:?}.grants]]\n\
             dataset = {dataset:?}\n\
             level = \"write\"\n\
             graphs = [{}]\n\
             branches = [{proposals:?}]\n\
             endpoints = [{endpoints}]\n",
            list(&graphs)
        ));
    }
    Ok(out)
}

pub fn run(cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Grant {
            template: _,
            agent,
            dataset,
            session_graphs,
            curated,
        } => {
            print!(
                "{}",
                agent_template(&agent, &dataset, &session_graphs, &curated)?
            );
            Ok(())
        }
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
