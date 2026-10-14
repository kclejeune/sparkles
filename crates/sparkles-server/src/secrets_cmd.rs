//! `sparkles secrets`: the runtime values of model secrets on a running server (spec C19
//! §11.2 and §11.4), through `/$/server/secrets`. `set` reads the value from standard
//! input, or from a prompt with echo off on a terminal, and never from an argument or an
//! environment variable, so that a key ends up neither in the shell history nor in the
//! process list. No subcommand prints a value, and the server never returns one.
//!
//! `unseal` and `rekey` work offline on a stopped server's data directory, holding its
//! catalog lock: `unseal` writes the sealed values back as plaintext for a downgrade, and
//! `rekey` seals them again under another key.

use crate::settings_cmd::ConnArgs;
use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct SecretsArgs {
    #[command(flatten)]
    conn: ConnArgs,
    #[command(subcommand)]
    cmd: SecretsCmd,
}

#[derive(Subcommand, Debug)]
enum SecretsCmd {
    /// List the secrets the server knows: each one's source (declared, runtime or
    /// missing), whether the settings file locks it, when a runtime value was set, and
    /// the providers that use it. No value is shown
    List,
    /// Store a runtime value for a secret, read from standard input or, on a terminal,
    /// from a prompt that does not echo. It overrides the declared source until it is
    /// unset
    Set {
        /// The secret's name, as a provider's apiKey.secret names it
        name: String,
    },
    /// Remove a secret's runtime value, so that its declared source applies again
    Unset {
        /// The secret's name
        name: String,
    },
    /// Write the sealed values of a stopped server back as plaintext files with mode
    /// 0600, for a release that does not seal them. Works offline and holds the data
    /// directory's catalog lock. The key file stays
    Unseal {
        /// The stopped server's data directory
        #[arg(long, value_name = "DIR")]
        data: PathBuf,
        /// The key the values are sealed with, as for serve --secrets-key. Without it,
        /// the key that serve would use
        #[arg(long, value_name = "SOURCE", env = "SPARKLES_SECRETS_KEY")]
        secrets_key: Option<String>,
    },
    /// Seal the stored values of a stopped server again under another key. Every value
    /// is opened before any is written. Works offline and holds the data directory's
    /// catalog lock. A generated secrets.key is removed afterwards
    Rekey {
        /// The stopped server's data directory
        #[arg(long, value_name = "DIR")]
        data: PathBuf,
        /// The key the values are sealed with now, as for serve --secrets-key. Without
        /// it, the key that serve would use
        #[arg(long, value_name = "SOURCE", env = "SPARKLES_SECRETS_KEY")]
        secrets_key: Option<String>,
        /// The new key: file:PATH, env:VAR, credential:NAME or command:CMD. Start the
        /// server with --secrets-key and the same source afterwards
        #[arg(long, value_name = "SOURCE")]
        new_key: String,
    },
}

pub fn run(args: SecretsArgs) -> Result<()> {
    use crate::settings::secrets;
    match args.cmd {
        SecretsCmd::Unseal { data, secrets_key } => {
            let done = secrets::unseal(&data, secrets_key.as_deref())?;
            println!(
                "wrote {} sealed value(s) in {} back as plaintext",
                done.values,
                data.join(secrets::SECRETS_DIR).display()
            );
            Ok(())
        }
        SecretsCmd::Rekey {
            data,
            secrets_key,
            new_key,
        } => {
            let done = secrets::rekey(&data, secrets_key.as_deref(), &new_key)?;
            let already = if done.already > 0 {
                format!(" ({} already sealed with the new key)", done.already)
            } else {
                String::new()
            };
            println!(
                "sealed {} value(s){already} in {} with the new key; start the server with --secrets-key and the same new key source",
                done.values,
                data.join(secrets::SECRETS_DIR).display()
            );
            if let Some(p) = done.removed_key {
                println!("removed the old generated key {}", p.display());
            }
            Ok(())
        }
        cmd => remote::run(cmd, &args.conn),
    }
}

#[cfg(not(feature = "auth"))]
mod remote {
    pub fn run(_: super::SecretsCmd, _: &super::ConnArgs) -> anyhow::Result<()> {
        anyhow::bail!("built without the remote client (cargo feature \"auth\")")
    }
}

#[cfg(feature = "auth")]
mod remote {
    use super::{ConnArgs, SecretsCmd};
    use crate::remote::Remote;
    use crate::settings_cmd::enc;
    use anyhow::{Context, Result, bail};
    use reqwest::Method;
    use serde_json::{Value as J, json};
    use std::io::{IsTerminal, Read};
    use zeroize::Zeroizing;

    pub fn run(cmd: SecretsCmd, conn: &ConnArgs) -> Result<()> {
        let r = Remote::open(conn.server.as_deref(), conn.insecure_http)?;
        match cmd {
            SecretsCmd::List => list(&r, conn.json),
            SecretsCmd::Set { name } => set(&r, &name, conn.json),
            SecretsCmd::Unset { name } => unset(&r, &name, conn.json),
            SecretsCmd::Unseal { .. } | SecretsCmd::Rekey { .. } => {
                unreachable!("offline subcommands run without a server")
            }
        }
    }

    /// A refused request, in words. The server's messages never quote a value.
    fn refused(r: &Remote, status: u16, body: &str, name: Option<&str>) -> anyhow::Error {
        let j: J = serde_json::from_str(body).unwrap_or(J::Null);
        let msg = j["error"].as_str().unwrap_or("").to_string();
        let base = &r.base;
        match (status, j["code"].as_str()) {
            (401, _) => anyhow::anyhow!(
                "not logged in to {base} (run: sparkles auth login --server {base})"
            ),
            (403, _) if !msg.contains("read-only") => {
                anyhow::anyhow!("model secrets need the server-admin permission (403)")
            }
            (409, Some("locked-by-config")) => anyhow::anyhow!(
                "refused: the server's settings file locks secrets.{}, so only its declared source applies",
                name.unwrap_or("")
            ),
            (s, _) if msg.is_empty() => anyhow::anyhow!("HTTP {s}"),
            (s, _) => anyhow::anyhow!("{msg} ({s})"),
        }
    }

    fn secrets(r: &Remote) -> Result<J> {
        let resp = r
            .req(Method::GET, "/$/server/secrets")
            .header("accept", "application/json")
            .send()
            .with_context(|| format!("cannot reach {}", r.base))?;
        let status = resp.status().as_u16();
        let body = resp.text()?;
        if !(200..300).contains(&status) {
            return Err(refused(r, status, &body, None));
        }
        Ok(serde_json::from_str(&body)?)
    }

    fn entry<'a>(list: &'a J, name: &str) -> Option<&'a J> {
        list["secrets"]
            .as_array()?
            .iter()
            .find(|s| s["name"] == name)
    }

    /// The providers and the notification channels that use a secret.
    fn providers(s: &J) -> Vec<String> {
        let names = |k: &str, prefix: &str| -> Vec<String> {
            s[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(|p| format!("{prefix}{p}")))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut out = names("providers", "");
        out.extend(names("channels", "channel "));
        out
    }

    /// What the source of a secret means, in a few words.
    fn note(s: &J) -> String {
        let mut out = Vec::new();
        if s["overridden"] == true {
            out.push("the stored value is ignored, since the secret is locked".to_string());
        } else if s["source"] == "runtime" && s["declared"] == true {
            out.push("overrides the declared source".to_string());
        }
        if s["source"] == "missing" && !providers(s).is_empty() {
            out.push("no value: its providers and channels cannot use it".to_string());
        }
        out.join("; ")
    }

    fn list(r: &Remote, json: bool) -> Result<()> {
        let v = secrets(r)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&v)?);
            return Ok(());
        }
        let rows: Vec<[String; 6]> = v["secrets"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let p = providers(s);
                [
                    s["name"].as_str().unwrap_or("").to_string(),
                    s["source"].as_str().unwrap_or("").to_string(),
                    if s["locked"] == true { "yes" } else { "no" }.to_string(),
                    s["setAt"].as_str().unwrap_or("-").to_string(),
                    if p.is_empty() {
                        "-".into()
                    } else {
                        p.join(", ")
                    },
                    note(s),
                ]
            })
            .collect();
        if rows.is_empty() {
            println!("no secrets: no provider names a key and none is stored");
            if let Some(note) = storage_note(&v) {
                println!("{note}");
            }
            return Ok(());
        }
        let head = ["NAME", "SOURCE", "LOCKED", "SET AT", "USED BY", ""];
        let mut w = head.map(str::len);
        for row in &rows {
            for (i, c) in row.iter().enumerate() {
                w[i] = w[i].max(c.len());
            }
        }
        let line = |row: &[&str]| {
            let s: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c:<width$}", width = w[i]))
                .collect();
            println!("{}", s.join("  ").trim_end());
        };
        line(&head);
        for row in &rows {
            line(&row.each_ref().map(String::as_str));
        }
        if let Some(note) = storage_note(&v) {
            println!("\n{note}");
        }
        Ok(())
    }

    /// A line about how the server keeps stored values, when they are not sealed or are
    /// sealed with a key that lives beside them.
    fn storage_note(v: &J) -> Option<&'static str> {
        if v["storage"] == "plaintext" {
            return Some(
                "stored values are kept unencrypted in the data directory (files with mode 0600): the server runs with --secrets-key none or was built without the backup feature",
            );
        }
        (v["keySource"] == "generated").then_some(
            "stored values are sealed with a key that the server generated in its data directory (secrets.key); it protects copies of the values made without that file, and --secrets-key keeps the key elsewhere",
        )
    }

    /// The value from standard input, or from a prompt with echo off on a terminal.
    fn read_value(name: &str) -> Result<Zeroizing<String>> {
        let v = if std::io::stdin().is_terminal() {
            Zeroizing::new(
                rpassword::prompt_password(format!("Value of secret {name} (not echoed): "))
                    .context("cannot read the value from the terminal")?,
            )
        } else {
            let mut buf = Zeroizing::new(String::new());
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("cannot read the value from standard input")?;
            buf
        };
        let t = Zeroizing::new(v.trim().to_string());
        if t.is_empty() {
            bail!("no value: write it to standard input or type it at the prompt");
        }
        Ok(t)
    }

    fn set(r: &Remote, name: &str, json_out: bool) -> Result<()> {
        crate::models::check_secret_name(name).map_err(|e| anyhow::anyhow!("{e}"))?;
        let value = read_value(name)?;
        let body = Zeroizing::new(json!({ "value": value.as_str() }).to_string());
        drop(value);
        let resp = r
            .req(Method::PUT, &format!("/$/server/secrets/{}", enc(name)))
            .header("content-type", "application/json")
            .body(body.as_bytes().to_vec())
            .send()
            .with_context(|| format!("cannot reach {}", r.base))?;
        drop(body);
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().unwrap_or_default();
            return Err(refused(r, status, &text, Some(name)));
        }
        report(r, name, json_out, "stored a runtime value for")
    }

    fn unset(r: &Remote, name: &str, json_out: bool) -> Result<()> {
        crate::models::check_secret_name(name).map_err(|e| anyhow::anyhow!("{e}"))?;
        let resp = r
            .req(Method::DELETE, &format!("/$/server/secrets/{}", enc(name)))
            .send()
            .with_context(|| format!("cannot reach {}", r.base))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().unwrap_or_default();
            return Err(refused(r, status, &text, Some(name)));
        }
        report(r, name, json_out, "removed the runtime value of")
    }

    /// After a change: the secret as the server lists it now.
    fn report(r: &Remote, name: &str, json_out: bool, done: &str) -> Result<()> {
        let v = secrets(r).ok();
        let s = v.as_ref().and_then(|v| entry(v, name));
        if json_out {
            let out = s.cloned().unwrap_or_else(|| json!({ "name": name }));
            println!("{}", serde_json::to_string_pretty(&out)?);
            return Ok(());
        }
        println!("{done} secret {name}");
        let Some(s) = s else {
            return Ok(());
        };
        let p = providers(s);
        let used = if p.is_empty() {
            "no provider or channel uses it yet".to_string()
        } else {
            format!("used by {}", p.join(", "))
        };
        let source = match s["source"].as_str().unwrap_or("") {
            "runtime" if s["declared"] == true => {
                "the runtime value applies, in place of the declared source"
            }
            "runtime" => "the runtime value applies",
            "declared" => "the declared source applies",
            _ => "it has no source now",
        };
        println!("  {source}; {used}");
        Ok(())
    }
}
