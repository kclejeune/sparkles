//! The `embedding` object of a vector index's configuration: which literals are embedded
//! and the endpoint that embeds them.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Most inputs per request.
pub const MAX_BATCH: usize = 2048;
/// Most source predicates, classes and language ranges.
pub const MAX_LIST: usize = 16;
/// Longest input, in characters.
pub const MAX_INPUT_CHARS: usize = 1_000_000;
/// Most chunks one text is split into; the text past them is not embedded.
pub const MAX_CHUNKS: usize = 1024;
/// Characters per token, for the estimates of `tokensPerMinute` and of chunk sizes in
/// tokens. OpenAI's tokenizers average about four characters of English text a token.
pub const CHARS_PER_TOKEN: usize = 4;

/// Where the bearer token of the requests comes from. The key itself is never stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum ApiKey {
    /// a secret the operator defined (`serve --embedding-secret NAME=…`)
    Secret(String),
    /// an environment variable of the process (local use only)
    Env(String),
    /// a file holding the key (local use only); re-read for every request
    File(String),
}

impl ApiKey {
    /// Whether this form reads the process's environment or files directly, which only
    /// the operator may configure.
    pub fn is_local(&self) -> bool {
        !matches!(self, ApiKey::Secret(_))
    }
}

fn yes() -> bool {
    true
}
fn default_batch() -> usize {
    64
}
fn default_max_input_chars() -> usize {
    8000
}
fn default_retries() -> u32 {
    5
}
fn default_timeout() -> f64 {
    60.0
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The unit of a chunk's size and overlap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkUnit {
    /// characters (Unicode scalar values)
    #[default]
    Chars,
    /// approximate tokens, [`CHARS_PER_TOKEN`] characters each
    Tokens,
}

/// How long texts are split (`EmbeddingConfig::chunking`). Each chunk is embedded as its
/// own input, so a subject gets one vector per chunk, and a search finds the subject by
/// its nearest chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Chunking {
    /// the most characters or tokens a chunk holds
    pub size: usize,
    /// how much of the end of a chunk the next one repeats
    #[serde(default)]
    pub overlap: usize,
    #[serde(default)]
    pub unit: ChunkUnit,
}

impl Chunking {
    /// The size and overlap in characters.
    fn chars(&self) -> (usize, usize) {
        let k = match self.unit {
            ChunkUnit::Chars => 1,
            ChunkUnit::Tokens => CHARS_PER_TOKEN,
        };
        (self.size.saturating_mul(k), self.overlap.saturating_mul(k))
    }

    /// The chunks of `text`: windows of at most `size` characters, each ending after
    /// whitespace when the second half of its window has some, each starting `overlap`
    /// characters before the end of the previous one (moved forward to the start of a
    /// word when one is near). A text that fits is one chunk.
    pub fn split(&self, text: &str) -> Vec<String> {
        let (size, overlap) = self.chars();
        let size = size.max(1);
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= size {
            return vec![text.to_string()];
        }
        let mut out = Vec::new();
        let mut start = 0;
        while start < chars.len() && out.len() < MAX_CHUNKS {
            let mut end = (start + size).min(chars.len());
            if end < chars.len() {
                // break after the last whitespace of the window's second half
                if let Some(i) = (start + size / 2..end)
                    .rev()
                    .find(|&i| chars[i].is_whitespace())
                {
                    end = i + 1;
                }
            }
            out.push(chars[start..end].iter().collect::<String>());
            if end >= chars.len() {
                break;
            }
            let mut next = end.saturating_sub(overlap).max(start + 1);
            if overlap > 0 {
                // start at a word when one starts within the overlap
                if let Some(i) = (next..end).find(|&i| i > 0 && chars[i - 1].is_whitespace()) {
                    next = i;
                }
            }
            start = next;
        }
        out
    }
}

/// How a vector index computes its vectors (`VectorIndexConfig::embedding`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// the embeddings endpoint (OpenAI's `POST /v1/embeddings` protocol)
    pub url: String,
    /// the `model` of each request
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKey>,
    /// send the index's dimension as the request's `dimensions`
    #[serde(default, skip_serializing_if = "is_false")]
    pub send_dimensions: bool,
    /// the predicates whose string literals are embedded
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<String>,
    /// language ranges (RFC 4647 basic filtering); `""` matches untagged literals
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
    /// the subject must have one of these types in the literal's graph
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<String>,
    /// a SELECT query binding `?s`, `?text` and optionally `?g` (instead of `predicates`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// one input per subject and graph, the texts joined by newlines
    #[serde(default, skip_serializing_if = "is_false")]
    pub combine: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input_prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query_prefix: String,
    /// searches may pass text, embedded with this provider
    #[serde(default = "yes")]
    pub query_text: bool,
    #[serde(default = "default_batch")]
    pub batch_size: usize,
    #[serde(default = "default_max_input_chars")]
    pub max_input_chars: usize,
    /// 0: no ceiling
    #[serde(default)]
    pub requests_per_minute: u32,
    /// the most tokens sent a minute, estimated from the inputs' lengths
    /// ([`CHARS_PER_TOKEN`]); 0: no ceiling
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tokens_per_minute: u64,
    /// split long texts into chunks, each embedded as its own vector (`None`: a text is
    /// one input, cut at `maxInputChars`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunking: Option<Chunking>,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: f64,
}

impl Default for EmbeddingConfig {
    fn default() -> EmbeddingConfig {
        EmbeddingConfig::new("", "")
    }
}

impl EmbeddingConfig {
    /// A configuration with the defaults and no source yet.
    pub fn new(url: &str, model: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            url: url.into(),
            model: model.into(),
            api_key: None,
            send_dimensions: false,
            predicates: Vec::new(),
            languages: None,
            classes: Vec::new(),
            query: None,
            combine: false,
            input_prefix: String::new(),
            query_prefix: String::new(),
            query_text: true,
            batch_size: default_batch(),
            max_input_chars: default_max_input_chars(),
            requests_per_minute: 0,
            tokens_per_minute: 0,
            chunking: None,
            max_retries: default_retries(),
            timeout_secs: default_timeout(),
        }
    }

    /// The same configuration embedding the literals of `predicates`.
    pub fn from_predicates(mut self, predicates: &[&str]) -> EmbeddingConfig {
        self.predicates = predicates.iter().map(|p| p.to_string()).collect();
        self
    }

    /// Check the configuration of an index on `index_predicate`; the error names the
    /// field (`embedding.…`).
    pub fn validate(&self, index_predicate: &str) -> Result<()> {
        let bad = |m: String| Err(Error::invalid(format!("embedding.{m}")));
        match reqwest::Url::parse(&self.url) {
            Ok(u) if !u.username().is_empty() || u.password().is_some() => {
                return bad("url: credentials go in apiKey, not in the URL".into());
            }
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
            _ => return bad(format!("url: {:?} is not an http(s) URL", self.url)),
        }
        if self.model.is_empty() || self.model.len() > 256 {
            return bad("model: 1 to 256 bytes".into());
        }
        let iri = |field: &str, v: &[String]| -> Result<()> {
            if v.len() > MAX_LIST {
                return Err(Error::invalid(format!(
                    "embedding.{field}: at most {MAX_LIST}"
                )));
            }
            for i in v {
                if oxiri::Iri::parse(i.as_str()).is_err() {
                    return Err(Error::invalid(format!(
                        "embedding.{field}: {i:?} is not an IRI"
                    )));
                }
            }
            Ok(())
        };
        iri("predicates", &self.predicates)?;
        iri("classes", &self.classes)?;
        match (&self.query, self.predicates.is_empty()) {
            (None, true) => {
                return bad("predicates: name the predicates to embed, or a query".into());
            }
            (Some(_), false) => return bad("query: give predicates or a query, not both".into()),
            (Some(q), true) => {
                check_query(q).map_err(|e| Error::invalid(format!("embedding.query: {e}")))?
            }
            (None, false) => {}
        }
        if self.query.is_some() && (!self.classes.is_empty() || self.languages.is_some()) {
            return bad(
                "query: classes and languages apply to predicates only; filter in the query".into(),
            );
        }
        if self.predicates.iter().any(|p| p == index_predicate) {
            return bad(format!(
                "predicates: <{index_predicate}> holds the index's vectors and cannot be a source"
            ));
        }
        if let Some(l) = &self.languages {
            if l.is_empty() || l.len() > MAX_LIST {
                return bad(format!("languages: 1 to {MAX_LIST} ranges"));
            }
            for r in l {
                let ok = r.is_empty()
                    || r == "*"
                    || r.split('-').all(|t| {
                        (1..=8).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric())
                    });
                if !ok {
                    return bad(format!("languages: {r:?} is not a language range"));
                }
            }
        }
        if !(1..=MAX_BATCH).contains(&self.batch_size) {
            return bad(format!("batchSize: 1 to {MAX_BATCH}"));
        }
        if !(1..=MAX_INPUT_CHARS).contains(&self.max_input_chars) {
            return bad(format!("maxInputChars: 1 to {MAX_INPUT_CHARS}"));
        }
        if self.requests_per_minute > 1_000_000 {
            return bad("requestsPerMinute: at most 1000000".into());
        }
        if self.tokens_per_minute > 1_000_000_000 {
            return bad("tokensPerMinute: at most 1000000000".into());
        }
        if let Some(c) = &self.chunking {
            let (size, overlap) = c.chars();
            if c.size == 0 || size > MAX_INPUT_CHARS {
                return bad(format!(
                    "chunking.size: 1 to {MAX_INPUT_CHARS} characters ({} tokens)",
                    MAX_INPUT_CHARS / CHARS_PER_TOKEN
                ));
            }
            if overlap >= size {
                return bad("chunking.overlap: less than the size".into());
            }
        }
        if self.max_retries > 20 {
            return bad("maxRetries: at most 20".into());
        }
        if !(self.timeout_secs.is_finite()
            && self.timeout_secs > 0.0
            && self.timeout_secs <= 3600.0)
        {
            return bad("timeoutSecs: more than 0, at most 3600".into());
        }
        if self.input_prefix.len() > 4096 || self.query_prefix.len() > 4096 {
            return bad("inputPrefix, queryPrefix: at most 4096 bytes".into());
        }
        match &self.api_key {
            Some(ApiKey::Secret(n) | ApiKey::Env(n)) if n.is_empty() || n.len() > 256 => {
                return bad("apiKey: a name of 1 to 256 bytes".into());
            }
            Some(ApiKey::File(p)) if p.is_empty() => return bad("apiKey: an empty path".into()),
            _ => {}
        }
        Ok(())
    }

    /// The identity of the vectors this configuration produces for an index of
    /// `dimension`: a different identity embeds every input again. The URL, key, batch
    /// and rate settings are not part of it, and the prefixes are part of each input.
    pub fn identity(&self, dimension: usize) -> u64 {
        let key = serde_json::json!([1, self.model, dimension, self.send_dimensions, self.combine]);
        super::fnv(&serde_json::to_vec(&key).expect("serializable"))
    }

    /// The URL without credentials, query or fragment (for status and logs).
    pub fn endpoint(&self) -> String {
        reqwest::Url::parse(&self.url).map_or_else(
            |_| String::new(),
            |mut u| {
                let _ = u.set_password(None);
                let _ = u.set_username("");
                u.set_query(None);
                u.set_fragment(None);
                u.to_string()
            },
        )
    }

    /// Whether `lang` (lowercase, `""` for none) is selected.
    pub fn language_selected(&self, lang: &str) -> bool {
        let Some(ranges) = &self.languages else {
            return true;
        };
        ranges.iter().any(|r| {
            if r.is_empty() {
                return lang.is_empty();
            }
            if r == "*" {
                return !lang.is_empty();
            }
            let r = r.to_ascii_lowercase();
            lang == r || (lang.starts_with(&r) && lang.as_bytes().get(r.len()) == Some(&b'-'))
        })
    }

    /// One stored input from its text: the prefix and the text, cut to
    /// `maxInputChars` characters.
    pub fn input(&self, text: &str) -> String {
        cut(
            &format!("{}{text}", self.input_prefix),
            self.max_input_chars,
        )
    }

    /// The stored inputs of a text: one per chunk with `chunking`, else one
    /// ([`input`](Self::input)).
    pub fn inputs(&self, text: &str) -> Vec<String> {
        match &self.chunking {
            Some(c) => c.split(text).iter().map(|t| self.input(t)).collect(),
            None => vec![self.input(text)],
        }
    }

    /// The input of a search's text.
    pub fn query_input(&self, text: &str) -> String {
        cut(
            &format!("{}{text}", self.query_prefix),
            self.max_input_chars,
        )
    }
}

fn cut(s: &str, chars: usize) -> String {
    match s.char_indices().nth(chars) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// A query source must be a SELECT that projects `?s` and `?text`.
fn check_query(q: &str) -> std::result::Result<(), String> {
    use spargebra::Query;
    let parsed = spargebra::SparqlParser::new()
        .parse_query(q)
        .map_err(|e| e.to_string())?;
    let Query::Select { pattern, .. } = parsed else {
        return Err("a SELECT query is required".into());
    };
    let vars = projected(&pattern);
    for v in ["s", "text"] {
        if !vars.iter().any(|x| x == v) {
            return Err(format!("the query must select ?{v}"));
        }
    }
    Ok(())
}

/// The variables a SELECT's pattern projects.
fn projected(p: &spargebra::algebra::GraphPattern) -> Vec<String> {
    use spargebra::algebra::GraphPattern as G;
    match p {
        G::Project { variables, .. } => variables.iter().map(|v| v.as_str().to_string()).collect(),
        G::Distinct { inner } | G::Reduced { inner } | G::Slice { inner, .. } => projected(inner),
        G::OrderBy { inner, .. } => projected(inner),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EmbeddingConfig {
        EmbeddingConfig::new("http://localhost:1/v1/embeddings", "m")
            .from_predicates(&["http://www.w3.org/2000/01/rdf-schema#label"])
    }

    #[test]
    fn json_and_validation() {
        let c: EmbeddingConfig = serde_json::from_str(
            r#"{"url":"https://api.example/v1/embeddings","model":"x",
                "predicates":["http://x/p"],"apiKey":{"secret":"openai"}}"#,
        )
        .unwrap();
        assert_eq!(c.api_key, Some(ApiKey::Secret("openai".into())));
        assert_eq!((c.batch_size, c.query_text), (64, true));
        assert!(c.validate("http://x/emb").is_ok());
        let err = |c: EmbeddingConfig| c.validate("http://x/emb").unwrap_err().to_string();
        assert!(
            err(EmbeddingConfig::new("ftp://x/", "m").from_predicates(&["http://x/p"]))
                .contains("url")
        );
        assert!(err(EmbeddingConfig::new("http://x/", "m")).contains("predicates"));
        assert!(
            err(EmbeddingConfig::new("http://x/", "m").from_predicates(&["http://x/emb"]))
                .contains("source")
        );
        let mut q = EmbeddingConfig::new("http://x/", "m");
        q.query = Some("SELECT ?s WHERE { ?s ?p ?o }".into());
        assert!(err(q.clone()).contains("?text"));
        q.query = Some("SELECT ?s ?text WHERE { ?s <http://x/p> ?text }".into());
        assert!(q.validate("http://x/emb").is_ok());
        let mut l = cfg();
        l.languages = Some(vec!["en-GB".into(), "".into()]);
        assert!(l.validate("http://x/emb").is_ok());
        l.languages = Some(vec!["en gb".into()]);
        assert!(err(l).contains("languages"));
        assert!(
            serde_json::from_str::<EmbeddingConfig>(r#"{"url":"http://x/","model":"m","typo":1}"#)
                .is_err()
        );
    }

    #[test]
    fn languages_and_inputs() {
        let mut c = cfg();
        assert!(c.language_selected("") && c.language_selected("fr"));
        c.languages = Some(vec!["en".into(), "".into()]);
        assert!(
            c.language_selected("en") && c.language_selected("en-gb") && c.language_selected("")
        );
        assert!(!c.language_selected("eng") && !c.language_selected("fr"));
        c.languages = Some(vec!["*".into()]);
        assert!(c.language_selected("fr") && !c.language_selected(""));
        c.input_prefix = "passage: ".into();
        c.max_input_chars = 12;
        assert_eq!(c.input("héllo world"), "passage: hél");
        assert_eq!(c.inputs("héllo world"), ["passage: hél"]);
        assert_ne!(c.identity(8), c.identity(16));
        let mut d = c.clone();
        d.url = "http://elsewhere/".into();
        d.batch_size = 1;
        assert_eq!(c.identity(8), d.identity(8));
    }

    #[test]
    fn chunks() {
        let c = |size, overlap, unit| Chunking {
            size,
            overlap,
            unit,
        };
        // a text that fits is one chunk
        assert_eq!(c(20, 5, ChunkUnit::Chars).split("short"), ["short"]);
        // windows end after whitespace, and the next one starts at a word of the overlap
        assert_eq!(
            c(10, 0, ChunkUnit::Chars).split("aaaa bbbb cccc dddd"),
            ["aaaa bbbb ", "cccc dddd"]
        );
        assert_eq!(
            c(10, 5, ChunkUnit::Chars).split("aaaa bbbb cccc dddd"),
            ["aaaa bbbb ", "bbbb cccc ", "cccc dddd"]
        );
        // no whitespace: hard cuts, the overlap repeated
        assert_eq!(
            c(4, 1, ChunkUnit::Chars).split("abcdefghij"),
            ["abcd", "defg", "ghij"]
        );
        // characters, not bytes; tokens are four characters
        assert_eq!(c(2, 0, ChunkUnit::Chars).split("ééé"), ["éé", "é"]);
        assert_eq!(
            c(1, 0, ChunkUnit::Tokens).split("abcdefgh"),
            ["abcd", "efgh"]
        );
        // every character is in some chunk, in order
        let text: String = (0..500).map(|i| format!("w{i} ")).collect();
        let parts = c(37, 9, ChunkUnit::Chars).split(&text);
        assert!(parts.iter().all(|p| p.chars().count() <= 37));
        assert!(parts.first().unwrap().starts_with("w0 "));
        assert!(parts.last().unwrap().ends_with("w499 "));
        for w in parts.windows(2) {
            let tail: String = w[0]
                .chars()
                .rev()
                .take(9)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            assert!(tail.contains(w[1].split(' ').next().unwrap()), "{w:?}");
        }
        // the validation
        let mut e = cfg();
        e.chunking = Some(c(10, 10, ChunkUnit::Chars));
        assert!(
            e.validate("http://x/emb")
                .unwrap_err()
                .to_string()
                .contains("overlap")
        );
        e.chunking = Some(c(0, 0, ChunkUnit::Chars));
        assert!(
            e.validate("http://x/emb")
                .unwrap_err()
                .to_string()
                .contains("size")
        );
        e.chunking = Some(c(500, 50, ChunkUnit::Tokens));
        assert!(e.validate("http://x/emb").is_ok());
        let j: EmbeddingConfig = serde_json::from_str(
            r#"{"url":"http://x/","model":"m","predicates":["http://x/p"],"tokensPerMinute":1000,
                "chunking":{"size":200,"overlap":20,"unit":"tokens"}}"#,
        )
        .unwrap();
        assert_eq!(j.tokens_per_minute, 1000);
        assert_eq!(j.chunking, Some(c(200, 20, ChunkUnit::Tokens)));
    }
}
