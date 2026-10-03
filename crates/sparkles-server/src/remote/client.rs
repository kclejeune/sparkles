//! Remote commands: `auth login|logout|status|token …`, and `query`, `update` and
//! `load` against a server.

use super::credentials::ServerCreds;
use super::{Credentials, JsonBody, Remote, login, normalize};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Value as J, json};
use std::io::{Read, Write};
use std::path::PathBuf;

/// `sparkles auth login`
pub fn login(
    server: &str,
    web: bool,
    device: bool,
    name: Option<&str>,
    token: Option<&str>,
    set_default: bool,
    insecure: bool,
) -> Result<()> {
    let base = normalize(server, insecure)?;
    let r = Remote {
        base: base.clone(),
        token: None,
        http: super::client()?,
    };
    let resp = r.check(r.req(Method::GET, "/$/auth/config").send(), None)?;
    let config = resp.json_value()?;
    if config["enabled"] != true {
        println!("{base} does not require authentication");
        return Ok(());
    }
    let label = name.unwrap_or("sparkles CLI");
    let obtained = match token {
        Some(t) => login::Obtained {
            token: t.to_string(),
            principal: None,
        },
        None if device || (!web && !login::can_open_browser()) => login::device(&r, label)?,
        None => match login::web(&r, &config, label) {
            Ok(o) => o,
            Err(Ok(login::WebUnavailable(why))) if !web => {
                eprintln!("Browser login unavailable ({why}); falling back to a device code.\n");
                login::device(&r, label)?
            }
            Err(Ok(login::WebUnavailable(why))) => bail!("browser login unavailable: {why}"),
            Err(Err(e)) => return Err(e),
        },
    };
    let token = obtained.token;
    let r = Remote {
        token: Some(token.clone()),
        ..r
    };
    let who = r
        .check(r.req(Method::GET, "/$/whoami").send(), None)
        .context("the token does not work")?
        .json_value()?;
    // a token acts for its owner: show who that is
    let principal = obtained
        .principal
        .or_else(|| who["principal"]["owner"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            format!(
                "{}:{}",
                who["principal"]["kind"].as_str().unwrap_or("?"),
                who["principal"]["name"].as_str().unwrap_or("?")
            )
        });
    let creds = ServerCreds {
        token,
        token_id: who["tokenId"].as_str().map(str::to_string),
        principal: Some(principal.clone()),
        expires: who["expires"].as_str().map(str::to_string),
    };
    let mut all = Credentials::load()?;
    all.servers.insert(base.clone(), creds.clone());
    if all.default_server.is_none() || set_default {
        all.default_server = Some(base.clone());
    }
    all.save()?;
    let mut detail = Vec::new();
    if let Some(id) = &creds.token_id {
        detail.push(format!("token {id}"));
    }
    if let Some(e) = &creds.expires {
        detail.push(format!("expires {}", e.get(..10).unwrap_or(e)));
    }
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({})", detail.join(", "))
    };
    println!("Logged in to {base} as {principal}{detail}");
    Ok(())
}

/// `sparkles auth logout`: revoke the token on the server and forget it.
pub fn logout(server: Option<&str>, insecure: bool) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    let mut all = Credentials::load()?;
    if !all.servers.contains_key(&r.base) {
        bail!("not logged in to {}", r.base);
    }
    match r.check(r.req(Method::DELETE, "/$/auth/tokens/self").send(), None) {
        Ok(_) => {}
        Err(e) => eprintln!("warning: the token was not revoked on the server: {e:#}"),
    }
    all.servers.remove(&r.base);
    if all.default_server.as_deref() == Some(r.base.as_str()) {
        all.default_server = all.servers.keys().next().cloned();
    }
    all.save()?;
    println!("Logged out of {}", r.base);
    Ok(())
}

/// `sparkles auth status`
pub fn status(server: Option<&str>, insecure: bool) -> Result<()> {
    let all = Credentials::load()?;
    let servers: Vec<String> = match server {
        Some(s) => vec![normalize(s, insecure)?],
        None => all.servers.keys().cloned().collect(),
    };
    if servers.is_empty() {
        println!("not logged in to any server (run: sparkles auth login --server URL)");
        return Ok(());
    }
    for base in servers {
        let default = all.default_server.as_deref() == Some(base.as_str());
        println!("{base}{}", if default { " (default)" } else { "" });
        let r = Remote::open(Some(&base), true)?;
        if r.token.is_none() {
            println!("  not logged in");
            continue;
        }
        match r.check(r.req(Method::GET, "/$/whoami").send(), None) {
            Ok(resp) => {
                let w = resp.json_value()?;
                let principal = w["principal"]["owner"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "{}:{}",
                            w["principal"]["kind"].as_str().unwrap_or("?"),
                            w["principal"]["name"].as_str().unwrap_or("?")
                        )
                    });
                println!("  principal  {principal}");
                if let Some(id) = w["tokenId"].as_str() {
                    println!("  token      {id}");
                }
                if let Some(e) = w["expires"].as_str() {
                    println!("  expires    {e}");
                }
                let perms: Vec<&str> = w["server"]
                    .as_array()
                    .map(|a| a.iter().filter_map(J::as_str).collect())
                    .unwrap_or_default();
                if !perms.is_empty() {
                    println!("  server     {}", perms.join(", "));
                }
                if let Some(ds) = w["datasets"].as_object() {
                    for (name, level) in ds {
                        println!("  dataset    {name}: {}", level.as_str().unwrap_or("?"));
                    }
                }
            }
            Err(_) => {
                println!("  token invalid or expired: run sparkles auth login --server {base}")
            }
        }
    }
    Ok(())
}

/// `sparkles auth token create`
pub fn token_create(
    server: Option<&str>,
    insecure: bool,
    name: &str,
    datasets: &[String],
    perms: &[String],
    expires: Option<&str>,
) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    let mut ds = serde_json::Map::new();
    for d in datasets {
        let (k, v) = d
            .split_once('=')
            .with_context(|| format!("--dataset {d}: expected NAME=LEVEL"))?;
        ds.insert(k.to_string(), v.into());
    }
    let mut body = json!({ "name": name, "server": perms });
    if !ds.is_empty() {
        body["datasets"] = J::Object(ds);
    }
    if let Some(e) = expires {
        body["expiresIn"] = e.into();
    }
    let j = r
        .check(
            r.req(Method::POST, "/$/auth/tokens")
                .header("content-type", "application/json")
                .body(body.to_string())
                .send(),
            None,
        )?
        .json_value()?;
    println!("{}", j["token"].as_str().unwrap_or_default());
    eprintln!(
        "token {} expires {}",
        j["id"].as_str().unwrap_or("?"),
        j["expires"].as_str().unwrap_or("?")
    );
    Ok(())
}

/// `sparkles auth token list`
pub fn token_list(server: Option<&str>, insecure: bool, all: bool) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    let path = if all {
        "/$/auth/tokens?all=true"
    } else {
        "/$/auth/tokens"
    };
    let j = r.get_json(path)?;
    let short = |s: &J| {
        s.as_str()
            .map_or("-".to_string(), |s| s.chars().take(10).collect())
    };
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{:<18} {:<24} {:<30} {:<10} {:<10}",
        "ID", "NAME", "SCOPE", "EXPIRES", "LAST USED"
    )?;
    for t in j["tokens"].as_array().into_iter().flatten() {
        let scope = if t["static"] == true {
            format!("static: {}", t["grants"].as_str().unwrap_or(""))
        } else {
            let mut parts: Vec<String> = t["scope"]["datasets"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("?")))
                        .collect()
                })
                .unwrap_or_default();
            parts.extend(
                t["scope"]["server"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(J::as_str)
                    .map(str::to_string),
            );
            parts.join(",")
        };
        writeln!(
            out,
            "{:<18} {:<24} {:<30} {:<10} {:<10}",
            t["id"].as_str().unwrap_or("?"),
            t["name"].as_str().unwrap_or(""),
            scope,
            short(&t["expires"]),
            short(&t["lastUsed"]),
        )?;
    }
    Ok(())
}

/// `sparkles auth token revoke ID`
pub fn token_revoke(server: Option<&str>, insecure: bool, id: &str) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    let path = format!(
        "/$/auth/tokens/{}",
        percent_encoding::utf8_percent_encode(id, percent_encoding::NON_ALPHANUMERIC)
    );
    r.check(r.req(Method::DELETE, &path).send(), None)?;
    println!("revoked {id}");
    Ok(())
}

fn ds_path(ds: &str) -> String {
    percent_encoding::utf8_percent_encode(ds, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The `Accept` of `query --results F`.
fn accept_for(results: &str) -> Result<&'static str> {
    Ok(match results {
        "text" => "text/tab-separated-values, text/turtle;q=0.9",
        "json" => "application/sparql-results+json, application/ld+json;q=0.9",
        "xml" => "application/sparql-results+xml, application/rdf+xml;q=0.9",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "sparkles" => "application/x-sparkles+json",
        "ttl" | "turtle" => "text/turtle",
        "nt" | "ntriples" => "application/n-triples",
        "nq" | "nquads" => "application/n-quads",
        "trig" => "application/trig",
        "jsonld" => "application/ld+json",
        "rdfxml" => "application/rdf+xml",
        other => bail!("unknown result format '{other}'"),
    })
}

/// `sparkles query --server URL --dataset DS`: the response body goes to stdout.
pub fn query(
    server: Option<&str>,
    insecure: bool,
    dataset: &str,
    query: &str,
    results: &str,
    timeout: Option<f64>,
    explain: bool,
) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    let mut path = format!(
        "/{}/{}",
        ds_path(dataset),
        if explain { "explain" } else { "sparql" }
    );
    if let Some(t) = timeout {
        path.push_str(&format!("?timeout={t}"));
    }
    let req = if explain {
        r.req(Method::POST, &path)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("query", query)
                    .finish(),
            )
    } else {
        r.req(Method::POST, &path)
            .header("content-type", "application/sparql-query")
            .header("accept", accept_for(results)?)
            .body(query.to_string())
    };
    let mut resp = r.check(req.send(), Some(dataset))?;
    let mut out = std::io::stdout().lock();
    if explain {
        let j = resp.json_value()?;
        writeln!(out, "{}", j["algebra"].as_str().unwrap_or_default())?;
        writeln!(out, "{}", serde_json::to_string_pretty(&j["plan"])?)?;
    } else {
        resp.copy_to(&mut out)?;
    }
    out.flush()?;
    Ok(())
}

/// The `Sparkles-Commit-Message` value of `m`: as is when it is ASCII, else an RFC 8187
/// extended value (`UTF-8''…`, percent-encoded), which every HTTP stack passes through.
fn message_header(m: &str) -> String {
    let ext = m
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("utf-8''"));
    if m.is_ascii() && !ext {
        m.to_string()
    } else {
        format!(
            "UTF-8''{}",
            percent_encoding::utf8_percent_encode(m, percent_encoding::NON_ALPHANUMERIC)
        )
    }
}

/// Add `Sparkles-Commit-Message` to a write request.
fn with_message(
    req: reqwest::blocking::RequestBuilder,
    message: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    match message {
        Some(m) => req.header("sparkles-commit-message", message_header(m)),
        None => req,
    }
}

/// `sparkles update --server URL --dataset DS`: prints the stats JSON.
pub fn update(
    server: Option<&str>,
    insecure: bool,
    dataset: &str,
    update: &str,
    message: Option<&str>,
) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    let req = r
        .req(Method::POST, &format!("/{}/update", ds_path(dataset)))
        .header("content-type", "application/sparql-update")
        .header("accept", "application/json");
    let resp = r.check(
        with_message(req, message).body(update.to_string()).send(),
        Some(dataset),
    )?;
    println!("{}", resp.text()?);
    Ok(())
}

/// `sparkles load --server URL --dataset DS FILES…`: each file streamed to the Graph
/// Store endpoint (`.gz` decompressed on the way).
pub fn load(
    server: Option<&str>,
    insecure: bool,
    dataset: &str,
    graph: Option<&str>,
    files: &[PathBuf],
    message: Option<&str>,
) -> Result<()> {
    if files.is_empty() {
        bail!("no files given");
    }
    let r = Remote::open(server, insecure)?;
    for f in files {
        let (format, _) = sparkles::io::format_for_path(f)
            .with_context(|| format!("{}: unknown RDF format", f.display()))?;
        let mut src = sparkles::io::Source::from_path(f, None)?;
        src.compression = None;
        let codec = src.codec()?;
        let quads = matches!(
            format,
            oxrdfio::RdfFormat::NQuads | oxrdfio::RdfFormat::TriG
        );
        let target = match graph {
            Some(g) => format!(
                "graph={}",
                percent_encoding::utf8_percent_encode(g, percent_encoding::NON_ALPHANUMERIC)
            ),
            None if quads => String::new(),
            None => "default".into(),
        };
        let path = format!(
            "/{}/data{}{target}",
            ds_path(dataset),
            if target.is_empty() { "" } else { "?" }
        );
        let file = std::fs::File::open(f).with_context(|| format!("opening {}", f.display()))?;
        // gzip, zstd and brotli travel compressed (`Content-Encoding`); LZ4 has no HTTP
        // encoding and is decompressed here
        let (body, encoding) = match codec.content_encoding() {
            Some(e) => (reqwest::blocking::Body::new(file), Some(e)),
            None if codec == sparkles::codec::Codec::None => {
                (reqwest::blocking::Body::new(file), None)
            }
            None => {
                let mut tmp = tempfile::tempfile()?;
                std::io::copy(&mut codec.reader(file, None)?, &mut tmp)?;
                std::io::Seek::rewind(&mut tmp)?;
                (reqwest::blocking::Body::new(tmp), None)
            }
        };
        let mut req = with_message(r.req(Method::POST, &path), message);
        if let Some(e) = encoding {
            req = req.header("content-encoding", e);
        }
        let resp = r.check(
            req.header("content-type", format.media_type())
                .header("accept", "application/json")
                .body(body)
                .send(),
            Some(dataset),
        )?;
        let j = resp.json_value().unwrap_or(J::Null);
        println!(
            "loaded {} quads from {}",
            j["count"].as_u64().unwrap_or(0),
            f.display()
        );
    }
    Ok(())
}

/// `sparkles patch --server URL --dataset DS FILES…`: each patch sent to the dataset's
/// patch endpoint, one commit per file.
pub fn patch(
    server: Option<&str>,
    insecure: bool,
    dataset: &str,
    files: &[PathBuf],
    format: Option<&str>,
    message: Option<&str>,
) -> Result<()> {
    let r = Remote::open(server, insecure)?;
    for f in files {
        let binary = crate::tools::rdfpatch::binary_input(f, format);
        let mut body = Vec::new();
        crate::tools::rdfpatch::open(f)?.read_to_end(&mut body)?;
        let req = with_message(
            r.req(
                Method::POST,
                &format!("/{}/patch?receipt=true", ds_path(dataset)),
            ),
            message,
        )
        .header(
            "content-type",
            if binary {
                sparkles::patch::MEDIA_TYPE_BINARY
            } else {
                sparkles::patch::MEDIA_TYPE
            },
        )
        .header("accept", "application/json")
        .body(body);
        let j = r.check(req.send(), Some(dataset))?.json_value()?;
        let seq = j["commit"]["seq"].as_u64().unwrap_or(0);
        let outcome = if j["aborted"] == true {
            format!("aborted · head {seq}")
        } else if j["committed"] == true {
            format!("commit {seq}")
        } else {
            format!("no change · head {seq}")
        };
        let line = format!(
            "inserted {} · deleted {} · {outcome}",
            j["inserted"].as_u64().unwrap_or(0),
            j["deleted"].as_u64().unwrap_or(0)
        );
        if files.len() > 1 {
            eprintln!("{}: {line}", f.display());
        } else {
            eprintln!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod message_tests {
    #[test]
    fn non_ascii_messages_travel_as_extended_values() {
        assert_eq!(super::message_header("fix labels"), "fix labels");
        assert_eq!(super::message_header("café"), "UTF-8''caf%C3%A9");
        assert_eq!(super::message_header("utf-8''x"), "UTF-8''utf%2D8%27%27x");
    }
}
