//! `sparkles auth login`: a browser loopback redirect (RFC 8252 §7.3 with PKCE) when a
//! browser can be opened, else the RFC 8628 device flow.

use super::{JsonBody, Remote};
use anyhow::{Context, Result, bail};
use serde_json::Value as J;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// How long the browser flow waits for its callback.
const WEB_TIMEOUT: Duration = Duration::from_secs(600);

/// Whether this session can plausibly show a browser: not over SSH; macOS and Windows
/// always; elsewhere a display server.
pub fn can_open_browser() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return false;
    }
    cfg!(any(target_os = "macos", target_os = "windows"))
        || set("DISPLAY")
        || set("WAYLAND_DISPLAY")
}

/// `$BROWSER`, else the platform opener.
fn open_browser(url: &str) -> Result<()> {
    let mut cmd = match std::env::var("BROWSER").ok().filter(|b| !b.is_empty()) {
        Some(b) => std::process::Command::new(b),
        None if cfg!(target_os = "macos") => std::process::Command::new("open"),
        None if cfg!(target_os = "windows") => {
            let mut c = std::process::Command::new("rundll32");
            c.arg("url.dll,FileProtocolHandler");
            c
        }
        None => std::process::Command::new("xdg-open"),
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("launching a browser")?;
    Ok(())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
        })
        .unwrap_or_default()
}

/// A token obtained by a login flow.
pub struct Obtained {
    pub token: String,
    /// the identity that approved the login (`oidc:alice@…`)
    pub principal: Option<String>,
}

/// Why the browser flow could not start (falls back to the device flow).
pub struct WebUnavailable(pub String);

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("OS random number generator");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn random_b64(n: usize) -> String {
    use base64::Engine;
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("OS random number generator");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn pkce(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier))
}

/// The browser loopback flow. `Err(Ok(..))`: the flow could not start (fall back);
/// `Err(Err(..))`: it failed.
pub fn web(
    r: &Remote,
    config: &J,
    label: &str,
) -> std::result::Result<Obtained, std::result::Result<WebUnavailable, anyhow::Error>> {
    let unavailable = |m: String| Err(Ok(WebUnavailable(m)));
    let Some(authorize) = config["cli"]["authorizeUrl"].as_str() else {
        return unavailable("the server offers no browser login".into());
    };
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => return unavailable(format!("cannot listen on 127.0.0.1: {e}")),
    };
    let port = listener.local_addr().map_err(|e| Err(e.into()))?.port();
    let state = random_hex(16);
    let verifier = random_b64(32);
    let q: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("port", &port.to_string())
        .append_pair("state", &state)
        .append_pair("code_challenge", &pkce(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("label", label)
        .append_pair("hostname", &hostname())
        .finish();
    let url = format!("{}?{q}", r.url(authorize));
    if let Err(e) = open_browser(&url) {
        return unavailable(format!("{e:#}"));
    }
    eprintln!(
        "Opened your browser to authorize this device:\n\n    {url}\n\nWaiting for authorization (rerun with --device for a headless login)…\n"
    );
    let code = wait_for_callback(&listener, &state).map_err(Err)?;
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let resp = r
        .http
        .post(r.url("/$/auth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", redirect.as_str()),
        ])
        .send()
        .map_err(|e| Err(e.into()))?;
    let status = resp.status();
    let j = resp.json_value().map_err(Err)?;
    if !status.is_success() {
        return Err(Err(anyhow::anyhow!(
            "the server refused the login: {}",
            j["error_description"]
                .as_str()
                .or(j["error"].as_str())
                .unwrap_or("unknown error")
        )));
    }
    let token = j["access_token"]
        .as_str()
        .context("the server returned no token")
        .map_err(Err)?;
    Ok(Obtained {
        token: token.to_string(),
        principal: j["principal"].as_str().map(str::to_string),
    })
}

const PAGE_OK: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sparkles CLI</title></head><body style=\"font-family:system-ui;margin:3em\"><p>Sparkles CLI is authorized; you can close this tab.</p></body></html>";
const PAGE_DENIED: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sparkles CLI</title></head><body style=\"font-family:system-ui;margin:3em\"><p>The login was denied; you can close this tab.</p></body></html>";

/// Accept connections until `/callback` arrives with our state; returns its code.
fn wait_for_callback(listener: &std::net::TcpListener, state: &str) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + WEB_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            bail!("browser authorization timed out");
        }
        let (mut conn, _) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        conn.set_nonblocking(false)?;
        conn.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut buf = vec![0u8; 8192];
        let n = conn.read(&mut buf).unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        let respond = |conn: &mut std::net::TcpStream, status: &str, body: &str| {
            let _ = write!(
                conn,
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        };
        let Some(query) = target.strip_prefix("/callback?") else {
            respond(&mut conn, "404 Not Found", "");
            continue;
        };
        let q: std::collections::HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        if q.get("state").map(String::as_str) != Some(state) {
            respond(&mut conn, "400 Bad Request", "unexpected callback");
            continue;
        }
        if q.contains_key("error") {
            respond(&mut conn, "200 OK", PAGE_DENIED);
            bail!("the login was denied in the browser");
        }
        let Some(code) = q.get("code") else {
            respond(&mut conn, "400 Bad Request", "missing code");
            continue;
        };
        respond(&mut conn, "200 OK", PAGE_OK);
        return Ok(code.clone());
    }
}

/// The device flow: print the code, poll until approved.
pub fn device(r: &Remote, label: &str) -> Result<Obtained> {
    let resp = r
        .http
        .post(r.url("/$/auth/device"))
        .form(&[("label", label), ("hostname", hostname().as_str())])
        .send()
        .with_context(|| format!("cannot reach {}", r.base))?;
    let status = resp.status();
    let g = resp.json_value()?;
    if !status.is_success() {
        bail!(
            "cannot start a device login: {}",
            g["error"].as_str().unwrap_or("unknown error")
        );
    }
    let device_code = g["device_code"].as_str().context("no device_code")?;
    eprintln!(
        "To authorize this device, visit:\n\n    {}\n\nand confirm the code {}\n",
        g["verification_uri_complete"].as_str().unwrap_or_default(),
        g["user_code"].as_str().unwrap_or_default()
    );
    let mut interval = g["interval"].as_u64().unwrap_or(5).max(1);
    let deadline = Instant::now() + Duration::from_secs(g["expires_in"].as_u64().unwrap_or(600));
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(interval));
        let resp = r
            .http
            .post(r.url("/$/auth/token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
            ])
            .send()
            .with_context(|| format!("cannot reach {}", r.base))?;
        let ok = resp.status().is_success();
        let j = resp.json_value()?;
        if ok {
            let token = j["access_token"].as_str().context("no access_token")?;
            return Ok(Obtained {
                token: token.to_string(),
                principal: j["principal"].as_str().map(str::to_string),
            });
        }
        match j["error"].as_str().unwrap_or("") {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => bail!("the login was denied"),
            "expired_token" => bail!("the device code expired; run the login again"),
            e => bail!("device login failed: {e}"),
        }
    }
    bail!("device authorization timed out")
}
