//! `sparkles rsparql` and `sparkles rupdate`: queries and updates sent to any SPARQL 1.1
//! Protocol endpoint, given by its full URL (spec G05 §3.5). Unlike `query --server`,
//! nothing comes from the Sparkles credentials file, because the endpoint may be any
//! server.

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, RequestBuilder, Response};
use std::io::Write;
use std::path::PathBuf;

/// Longest URL sent as a GET; a longer query goes as a POST form.
const MAX_GET_URL: usize = 2000;

/// The connection flags of both commands.
#[derive(clap::Args)]
pub struct EndpointArgs {
    /// The endpoint's URL, as it is (e.g. https://query.wikidata.org/sparql)
    #[arg(long, visible_alias = "endpoint", value_name = "URL")]
    service: String,
    /// An HTTP header to send, `Name: value` (repeatable)
    #[arg(long, short = 'H', value_name = "HEADER")]
    header: Vec<String>,
    /// HTTP Basic credentials, NAME or NAME:PASSWORD (without a password, asks for one)
    #[arg(long, value_name = "NAME[:PASSWORD]")]
    user: Option<String>,
    /// Seconds to wait for the whole response (default: no limit)
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,
    /// Allow credentials and headers over plain http to a host other than localhost
    #[arg(long)]
    insecure_http: bool,
}

#[derive(clap::Args)]
pub struct RsparqlArgs {
    #[command(flatten)]
    endpoint: EndpointArgs,
    /// The query (default: --query, else standard input)
    text: Option<String>,
    /// File holding the query (`-` for standard input)
    #[arg(long)]
    query: Option<PathBuf>,
    /// Output format: text (a table, or Turtle for graphs), json, xml, csv, tsv, or for
    /// CONSTRUCT and DESCRIBE ttl, nt, nq, trig, jsonld, rdfxml
    #[arg(long, default_value = "text", value_name = "FMT")]
    results: String,
    /// Send the query as a POST form even when it would fit in a GET
    #[arg(long)]
    post: bool,
    /// The protocol's default-graph-uri (repeatable)
    #[arg(long, value_name = "IRI")]
    default_graph_uri: Vec<String>,
    /// The protocol's named-graph-uri (repeatable)
    #[arg(long, value_name = "IRI")]
    named_graph_uri: Vec<String>,
}

#[derive(clap::Args)]
pub struct RupdateArgs {
    #[command(flatten)]
    endpoint: EndpointArgs,
    /// The update (default: --update, else standard input)
    text: Option<String>,
    /// File holding the update (`-` for standard input)
    #[arg(long)]
    update: Option<PathBuf>,
    /// Send the update as a form (`update=`) instead of application/sparql-update
    #[arg(long)]
    form: bool,
    /// The protocol's using-graph-uri (repeatable)
    #[arg(long, value_name = "IRI")]
    using_graph_uri: Vec<String>,
    /// The protocol's using-named-graph-uri (repeatable)
    #[arg(long, value_name = "IRI")]
    using_named_graph_uri: Vec<String>,
}

struct Endpoint {
    url: reqwest::Url,
    http: Client,
    headers: Vec<(String, String)>,
    basic: Option<(String, String)>,
}

impl Endpoint {
    fn new(a: &EndpointArgs) -> Result<Endpoint> {
        let url = reqwest::Url::parse(&a.service)
            .with_context(|| format!("--service '{}': not a URL", a.service))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            bail!("--service '{}': expected an http(s) URL", a.service);
        }
        let headers = a
            .header
            .iter()
            .map(|h| {
                let (k, v) = h
                    .split_once(':')
                    .with_context(|| format!("--header '{h}': expected 'Name: value'"))?;
                Ok((k.trim().to_string(), v.trim().to_string()))
            })
            .collect::<Result<Vec<_>>>()?;
        let basic = match &a.user {
            Some(u) => Some(match u.split_once(':') {
                Some((n, p)) => (n.to_string(), p.to_string()),
                None => (
                    u.clone(),
                    rpassword::prompt_password(format!("Password for {u}: "))
                        .context("reading the password")?,
                ),
            }),
            None => None,
        };
        let host = url.host_str().unwrap_or_default();
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1");
        if url.scheme() == "http"
            && !loopback
            && (basic.is_some() || !headers.is_empty())
            && !a.insecure_http
        {
            bail!(
                "refusing to send credentials or headers over plain http to {host} (use https, or --insecure-http)"
            );
        }
        let mut b = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")));
        b = b.timeout(a.timeout.map(std::time::Duration::from_secs_f64));
        Ok(Endpoint {
            url,
            http: b.build()?,
            headers,
            basic,
        })
    }

    fn with_auth(&self, mut r: RequestBuilder) -> RequestBuilder {
        for (k, v) in &self.headers {
            r = r.header(k, v);
        }
        if let Some((n, p)) = &self.basic {
            r = r.basic_auth(n, Some(p));
        }
        r
    }

    /// The response if it is a success; otherwise an error with the server's message.
    fn check(&self, r: reqwest::Result<Response>) -> Result<Response> {
        let r = r.with_context(|| format!("cannot reach {}", self.url))?;
        let status = r.status();
        if status.is_success() {
            return Ok(r);
        }
        let body = r.text().unwrap_or_default();
        let msg: String = body.trim().chars().take(500).collect();
        bail!("{status} from {}: {msg}", self.url)
    }
}

/// The `Accept` header of `--results`.
fn accept_for(results: &str) -> Result<&'static str> {
    Ok(match results {
        "text" => {
            "application/sparql-results+json, application/sparql-results+xml;q=0.9, \
             text/turtle;q=0.8, application/n-triples;q=0.7, */*;q=0.1"
        }
        "json" => "application/sparql-results+json, application/ld+json;q=0.9",
        "xml" => "application/sparql-results+xml, application/rdf+xml;q=0.9",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "ttl" | "turtle" => "text/turtle",
        "nt" | "ntriples" => "application/n-triples",
        "nq" | "nquads" => "application/n-quads",
        "trig" => "application/trig",
        "jsonld" => "application/ld+json",
        "rdfxml" => "application/rdf+xml",
        other => bail!("unknown result format '{other}'"),
    })
}

/// `sparkles rsparql`: the response body to stdout; results as a table with `text`.
pub fn rsparql(a: RsparqlArgs) -> Result<()> {
    let query = super::sparql::read_text(a.text, a.query)?;
    let ep = Endpoint::new(&a.endpoint)?;
    let accept = accept_for(&a.results)?;
    let mut params: Vec<(&str, &str)> = vec![("query", query.as_str())];
    params.extend(
        a.default_graph_uri
            .iter()
            .map(|g| ("default-graph-uri", g.as_str())),
    );
    params.extend(
        a.named_graph_uri
            .iter()
            .map(|g| ("named-graph-uri", g.as_str())),
    );
    let mut get = ep.url.clone();
    get.query_pairs_mut().extend_pairs(&params);
    let req = if a.post || get.as_str().len() > MAX_GET_URL {
        ep.http.post(ep.url.clone()).form(&params)
    } else {
        ep.http.get(get)
    };
    let resp = ep.check(ep.with_auth(req.header("accept", accept)).send())?;
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let table_format = (a.results == "text")
        .then(|| sparesults::QueryResultsFormat::from_media_type(&ctype))
        .flatten()
        .filter(|f| *f != sparesults::QueryResultsFormat::Csv);
    match table_format {
        Some(f) => super::rset::convert_results(f, resp, "text", &mut out)?,
        None => {
            let mut resp = resp;
            resp.copy_to(&mut out)?;
        }
    }
    out.flush()?;
    Ok(())
}

/// `sparkles rupdate`: the response body (if any) to stdout.
pub fn rupdate(a: RupdateArgs) -> Result<()> {
    let update = super::sparql::read_text(a.text, a.update)?;
    let ep = Endpoint::new(&a.endpoint)?;
    let mut graphs: Vec<(&str, &str)> = Vec::new();
    graphs.extend(
        a.using_graph_uri
            .iter()
            .map(|g| ("using-graph-uri", g.as_str())),
    );
    graphs.extend(
        a.using_named_graph_uri
            .iter()
            .map(|g| ("using-named-graph-uri", g.as_str())),
    );
    let req = if a.form {
        let mut form = vec![("update", update.as_str())];
        form.extend(graphs);
        ep.http.post(ep.url.clone()).form(&form)
    } else {
        let mut url = ep.url.clone();
        if !graphs.is_empty() {
            url.query_pairs_mut().extend_pairs(&graphs);
        }
        ep.http
            .post(url)
            .header("content-type", "application/sparql-update")
            .body(update)
    };
    let resp = ep.check(ep.with_auth(req).send())?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !body.trim().is_empty() {
        println!("{}", body.trim_end());
    }
    eprintln!("update done ({status})");
    Ok(())
}
