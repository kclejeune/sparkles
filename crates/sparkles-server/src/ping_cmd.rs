//! `sparkles ping`: ask a server whether it is ready, for health checks. The container
//! image's probe runs it, so that the probe works whether the server speaks HTTP or,
//! with `--tls-cert`, HTTPS.

use anyhow::{Result, bail};

#[derive(clap::Args, Debug)]
pub struct PingArgs {
    /// The server: a URL, or HOST:PORT to try over plain HTTP and then HTTPS. A
    /// certificate is verified, except one of a loopback address (localhost, 127.0.0.1
    /// or ::1), which the server's own probe reaches by an address its certificate does
    /// not name.
    #[arg(default_value = "127.0.0.1:3030")]
    server: String,
    /// The path to ask for
    #[arg(long, default_value = "/$/ready")]
    path: String,
    /// Seconds to wait for an answer, for all attempts together
    #[arg(long, default_value_t = 4.0)]
    timeout: f64,
    /// Print nothing; the exit status says whether the server answered 200
    #[arg(long, short)]
    quiet: bool,
}

/// The URLs to try for `server`: as given with a scheme, else plain HTTP first, which a
/// TLS server refuses at once (a plain server waits on a TLS handshake instead).
fn candidates(server: &str, path: &str) -> Vec<String> {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let server = server.trim_end_matches('/');
    if server.starts_with("http://") || server.starts_with("https://") {
        let has_path = server
            .split_once("://")
            .is_some_and(|(_, rest)| rest.contains('/'));
        return vec![if has_path {
            server.to_string()
        } else {
            format!("{server}{path}")
        }];
    }
    vec![
        format!("http://{server}{path}"),
        format!("https://{server}{path}"),
    ]
}

fn loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback())
}

pub fn run(a: PingArgs) -> Result<()> {
    let urls = candidates(&a.server, &a.path);
    let per_try = std::time::Duration::from_secs_f64(a.timeout.max(0.1) / urls.len() as f64);
    let mut last = String::new();
    for u in &urls {
        let url = reqwest::Url::parse(u).map_err(|e| anyhow::anyhow!("{u}: {e}"))?;
        let client = reqwest::blocking::Client::builder()
            .timeout(per_try)
            .connect_timeout(per_try)
            .danger_accept_invalid_certs(url.scheme() == "https" && loopback(&url))
            .user_agent(concat!("sparkles-ping/", env!("CARGO_PKG_VERSION")))
            .build()?;
        match client.get(url.clone()).send() {
            Ok(r) if r.status() == reqwest::StatusCode::OK => {
                if !a.quiet {
                    println!("{u}: ready");
                }
                return Ok(());
            }
            Ok(r) => {
                // the server answered: another scheme would not change that
                let status = r.status();
                if !a.quiet {
                    eprintln!("{u}: {status}");
                }
                std::process::exit(1);
            }
            Err(e) => last = format!("{u}: {}", error_chain(&e)),
        }
    }
    if a.quiet {
        std::process::exit(1);
    }
    bail!("{last}")
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_to_try() {
        assert_eq!(
            candidates("127.0.0.1:3030", "/$/ready"),
            [
                "https://127.0.0.1:3030/$/ready",
                "http://127.0.0.1:3030/$/ready"
            ]
        );
        assert_eq!(
            candidates("http://localhost:3030/", "$/ping"),
            ["http://localhost:3030/$/ping"]
        );
        assert_eq!(
            candidates("https://h:1/$/ready", "/x"),
            ["https://h:1/$/ready"]
        );
        let l = |u: &str| loopback(&reqwest::Url::parse(u).unwrap());
        assert!(l("https://localhost:1/"));
        assert!(l("https://127.0.0.1:1/"));
        assert!(l("https://[::1]:1/"));
        assert!(!l("https://example.org/"));
    }
}
