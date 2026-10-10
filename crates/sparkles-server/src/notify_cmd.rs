//! `sparkles notify`: outbound notifications on a running server (spec C21 §9), through
//! `GET /$/notifications` and `POST /$/notifications/test/{channel}`. The channels and
//! routes are changed with `sparkles settings set --global notifications.…` and their
//! secrets with `sparkles secrets set`.

use crate::settings_cmd::ConnArgs;
use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub struct NotifyArgs {
    #[command(flatten)]
    conn: ConnArgs,
    #[command(subcommand)]
    cmd: NotifyCmd,
}

#[derive(Subcommand, Debug)]
enum NotifyCmd {
    /// Show whether notifications are on, each channel with its last success and last
    /// failure, the conditions that were notified and the recent deliveries
    Status,
    /// Send a test notification to one channel now, whether or not notifications are on,
    /// and print the outcome
    Test {
        /// The channel's name in the notifications settings
        channel: String,
    },
}

pub fn run(args: NotifyArgs) -> Result<()> {
    remote::run(args.cmd, &args.conn)
}

#[cfg(not(feature = "auth"))]
mod remote {
    pub fn run(_: super::NotifyCmd, _: &super::ConnArgs) -> anyhow::Result<()> {
        anyhow::bail!("built without the remote client (cargo feature \"auth\")")
    }
}

#[cfg(feature = "auth")]
mod remote {
    use super::{ConnArgs, NotifyCmd};
    use crate::remote::Remote;
    use crate::settings_cmd::enc;
    use anyhow::{Context, Result, bail};
    use reqwest::Method;
    use serde_json::Value as J;

    pub fn run(cmd: NotifyCmd, conn: &ConnArgs) -> Result<()> {
        let r = Remote::open(conn.server.as_deref(), conn.insecure_http)?;
        match cmd {
            NotifyCmd::Status => status(&r, conn.json),
            NotifyCmd::Test { channel } => test(&r, &channel, conn.json),
        }
    }

    /// A refused request, in words.
    fn refused(r: &Remote, status: u16, body: &str) -> anyhow::Error {
        let j: J = serde_json::from_str(body).unwrap_or(J::Null);
        let msg = j["error"].as_str().unwrap_or("").to_string();
        let base = &r.base;
        match status {
            401 => anyhow::anyhow!(
                "not logged in to {base} (run: sparkles auth login --server {base})"
            ),
            403 => anyhow::anyhow!("notifications need the server-admin permission (403)"),
            s if msg.is_empty() => anyhow::anyhow!("HTTP {s}"),
            s => anyhow::anyhow!("{msg} ({s})"),
        }
    }

    fn call(r: &Remote, method: Method, path: &str) -> Result<(u16, J, String)> {
        let resp = r
            .req(method, path)
            .header("accept", "application/json")
            .send()
            .with_context(|| format!("cannot reach {}", r.base))?;
        let status = resp.status().as_u16();
        let body = resp.text()?;
        let j = serde_json::from_str(&body).unwrap_or(J::Null);
        Ok((status, j, body))
    }

    fn when(v: &J) -> String {
        match v["at"].as_str() {
            None => "-".into(),
            Some(at) => match v["error"].as_str() {
                Some(e) => format!("{at} ({}): {e}", v["event"].as_str().unwrap_or("")),
                None => format!("{at} ({})", v["event"].as_str().unwrap_or("")),
            },
        }
    }

    fn status(r: &Remote, json: bool) -> Result<()> {
        let (code, v, body) = call(r, Method::GET, "/$/notifications")?;
        if !(200..300).contains(&code) {
            return Err(refused(r, code, &body));
        }
        if json {
            println!("{}", serde_json::to_string_pretty(&v)?);
            return Ok(());
        }
        println!(
            "notifications are {}{}",
            if v["enabled"] == true { "on" } else { "off" },
            match v["queued"].as_u64() {
                Some(n) if n > 0 => format!(", {n} queued"),
                _ => String::new(),
            }
        );
        let channels = v["channels"].as_array().cloned().unwrap_or_default();
        if channels.is_empty() {
            println!(
                "no channels: add one with sparkles settings set --global notifications.channels.NAME=…"
            );
        }
        for c in &channels {
            println!(
                "\n{} ({}) {}",
                c["name"].as_str().unwrap_or(""),
                c["type"].as_str().unwrap_or(""),
                c["target"].as_str().unwrap_or("")
            );
            println!(
                "  delivered {}, failed {}",
                c["sent"].as_u64().unwrap_or(0),
                c["failed"].as_u64().unwrap_or(0)
            );
            println!("  last success  {}", when(&c["lastSuccess"]));
            println!("  last failure  {}", when(&c["lastFailure"]));
        }
        if let Some(routes) = v["routes"].as_object()
            && !routes.is_empty()
        {
            println!("\nroutes");
            for (k, chans) in routes {
                let names: Vec<&str> = chans
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(J::as_str)
                    .collect();
                println!("  {k} -> {}", names.join(", "));
            }
        }
        let active = v["active"].as_array().cloned().unwrap_or_default();
        if !active.is_empty() {
            println!("\nconditions notified");
            for a in &active {
                println!(
                    "  {}  {} times, last {}",
                    a["key"].as_str().unwrap_or(""),
                    a["count"].as_u64().unwrap_or(0),
                    a["last"].as_str().unwrap_or("")
                );
            }
        }
        Ok(())
    }

    fn test(r: &Remote, channel: &str, json: bool) -> Result<()> {
        let path = format!("/$/notifications/test/{}", enc(channel));
        let (code, v, body) = call(r, Method::POST, &path)?;
        if json && (200..300).contains(&code) {
            println!("{}", serde_json::to_string_pretty(&v)?);
            return Ok(());
        }
        match code {
            200..=299 => {
                println!(
                    "delivered a test notification to {channel} (HTTP {}, {} ms)",
                    v["status"].as_u64().unwrap_or(0),
                    v["latencyMs"].as_u64().unwrap_or(0)
                );
                Ok(())
            }
            404 if v["code"] == "unknown-channel" => bail!(
                "no notification channel named {channel:?}; sparkles notify status lists them"
            ),
            502 => bail!(
                "the test notification to {channel} failed: {}",
                v["error"].as_str().unwrap_or("")
            ),
            _ => Err(refused(r, code, &body)),
        }
    }
}
