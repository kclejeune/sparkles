//! The full-text, vector and spatial indexes.
//!
//! The vector and spatial index builds run in the store's background threads, so their
//! `put`, `enable` and `rebuild` return once the build starts, and `wait` blocks until it
//! ends. The full-text index builds in the call.

use crate::Dataset;
use crate::error::Result;
use crate::geo::{GeoConfig, GeoStatus};
use crate::sparql::QueryOptions;
use crate::text::{TextConfig, TextStatus};
use crate::vector::config::VectorRecall;
use crate::vector::embed::EmbeddingStatus;
use crate::vector::{VectorIndexConfig, VectorIndexStatus};
use oxrdf::{Literal, NamedNode, Term};
use serde::{Serialize, Serializer};
use std::time::Duration;

/// The dataset's indexes (from [`Dataset::indexes`]).
#[derive(Clone)]
pub struct Indexes {
    pub(crate) ds: Dataset,
}

impl Indexes {
    /// The full-text index (feature `text`; without it the calls fail with
    /// `Unsupported`).
    pub fn text(&self) -> TextIndex {
        TextIndex {
            ds: self.ds.clone(),
        }
    }

    /// The vector indexes.
    pub fn vector(&self) -> VectorIndexes {
        VectorIndexes {
            ds: self.ds.clone(),
        }
    }

    /// The spatial index (feature `geo`; without it the calls fail with
    /// `Unsupported`).
    pub fn geo(&self) -> GeoIndex {
        GeoIndex {
            ds: self.ds.clone(),
        }
    }
}

#[cfg(not(feature = "text"))]
fn no_text() -> crate::Error {
    crate::Error::unsupported("full-text search needs the `text` feature")
}

/// The full-text index.
#[derive(Clone)]
pub struct TextIndex {
    #[cfg_attr(not(feature = "text"), allow(dead_code))]
    ds: Dataset,
}

impl TextIndex {
    /// Its status, or `None` when it is not enabled.
    pub fn status(&self) -> Option<TextStatus> {
        #[cfg(feature = "text")]
        return self.ds.store().text_status();
        #[cfg(not(feature = "text"))]
        None
    }

    /// Whether the index is enabled, which costs less than its
    /// [`status`](Self::status).
    pub fn enabled(&self) -> bool {
        self.ds.store().text_enabled()
    }

    /// Enable (or reconfigure) the index and build it from the current state.
    pub fn enable(&self, cfg: TextConfig) -> Result<TextStatus> {
        #[cfg(feature = "text")]
        return self.ds.store().enable_text(cfg);
        #[cfg(not(feature = "text"))]
        {
            let _ = cfg;
            Err(no_text())
        }
    }

    /// Turn the index off and remove its files.
    pub fn disable(&self) -> Result<()> {
        #[cfg(feature = "text")]
        return self.ds.store().disable_text();
        #[cfg(not(feature = "text"))]
        Err(no_text())
    }

    /// Rebuild the index from the current state (writes wait meanwhile).
    pub fn rebuild(&self) -> Result<TextStatus> {
        #[cfg(feature = "text")]
        return self.ds.store().rebuild_text();
        #[cfg(not(feature = "text"))]
        Err(no_text())
    }

    /// The best hits of a search, best first, read from the current state. The search
    /// is the `text:query` call that `GET /{ds}/text` runs, and each hit has its subject,
    /// score, literal, predicate and graph, and, when `req.highlight` is set, an HTML
    /// snippet of the literal with the matches in `<mark>`.
    pub fn search(&self, req: &TextSearch) -> Result<TextHits> {
        #[cfg(feature = "text")]
        return text_search(&self.ds, req);
        #[cfg(not(feature = "text"))]
        {
            let _ = req;
            Err(no_text())
        }
    }
}

/// A full-text search (see [`TextIndex::search`]).
#[derive(Clone, Debug)]
pub struct TextSearch {
    /// the query, in the syntax of the index's analyzer
    pub query: String,
    /// the indexed predicates to search, or every one when empty
    pub predicates: Vec<NamedNode>,
    /// only literals with this language tag
    pub lang: Option<String>,
    /// only matches in this named graph
    pub graph: Option<NamedNode>,
    /// the most hits returned
    pub limit: usize,
    /// whether each hit has an HTML snippet with the matches marked
    pub highlight: bool,
    /// the options of the query that runs the search. The dataset's query defaults
    /// fill the fields they leave unset, as in [`Dataset::query_with`].
    pub options: QueryOptions,
}

impl Default for TextSearch {
    /// No query, every predicate, 20 hits, with snippets.
    fn default() -> TextSearch {
        TextSearch {
            query: String::new(),
            predicates: Vec::new(),
            lang: None,
            graph: None,
            limit: 20,
            highlight: true,
            options: QueryOptions::default(),
        }
    }
}

/// The hits of a [`TextSearch`]: the commit they were read at, the hits, and whether
/// the limit cut them short.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct TextHits {
    pub commit: u64,
    /// whether there are as many hits as the limit allows, so more may match
    pub limited: bool,
    pub hits: Vec<TextHit>,
}

/// One hit of a [`TextSearch`]. Its serde form is that of `GET /{ds}/text`: `s`,
/// `score`, `literal`, `snippet`, `g` and `p`, with terms in the SPARQL JSON results
/// form.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TextHit {
    pub subject: Option<Term>,
    pub score: Option<f64>,
    /// the matching literal, without marks
    pub literal: Option<Literal>,
    /// the literal as HTML, with the matches in `<mark>` (searches with `highlight`)
    pub snippet: Option<String>,
    pub graph: Option<Term>,
    pub predicate: Option<Term>,
}

impl Serialize for TextHit {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use crate::sparql::results::term_json;
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        if let Some(t) = &self.subject {
            m.serialize_entry("s", &term_json(t))?;
        }
        if let Some(sc) = self.score {
            m.serialize_entry("score", &sc)?;
        }
        if let Some(h) = &self.snippet {
            m.serialize_entry("snippet", h)?;
        }
        if let Some(l) = &self.literal {
            m.serialize_entry("literal", &term_json(&Term::Literal(l.clone())))?;
        }
        if let Some(t) = &self.graph {
            m.serialize_entry("g", &term_json(t))?;
        }
        if let Some(t) = &self.predicate {
            m.serialize_entry("p", &term_json(t))?;
        }
        m.end()
    }
}

/// Marks the engine puts around a match in a snippet, replaced by `<mark>` and `</mark>`
/// once the text is HTML-escaped (private-use characters).
#[cfg(feature = "text")]
const MARK: (char, char) = ('\u{e000}', '\u{e001}');

#[cfg(feature = "text")]
fn text_search(ds: &Dataset, req: &TextSearch) -> Result<TextHits> {
    // the call's arguments, serialized by oxrdf (escaped)
    let mut args: Vec<String> = req.predicates.iter().map(|p| p.to_string()).collect();
    args.push(Literal::new_simple_literal(req.query.as_str()).to_string());
    args.push(req.limit.to_string());
    if let Some(l) = &req.lang {
        args.push(Literal::new_simple_literal(format!("lang:{l}")).to_string());
    }
    if req.highlight {
        args.push(
            Literal::new_simple_literal(format!("highlight:s:{} | e:{} | f:…", MARK.0, MARK.1))
                .to_string(),
        );
    }
    let call = format!(
        "(?s ?score ?lit ?g ?p) <http://jena.apache.org/text#query> ({})",
        args.join(" ")
    );
    let pattern = match &req.graph {
        Some(g) => format!("GRAPH {g} {{ {call} }}"),
        None => call,
    };
    let query = format!("SELECT ?s ?score ?lit ?g ?p WHERE {{ {pattern} }} ORDER BY DESC(?score)");
    let snap = ds.snapshot();
    let commit = snap.commit;
    let r = crate::sparql::query(snap, &query, &ds.with_query_defaults(&req.options))?;
    let hits: Vec<TextHit> = r
        .rows()
        .into_iter()
        .map(|row| {
            let get = |i: usize| row.get(i).cloned().flatten();
            let score = match get(1) {
                Some(Term::Literal(sc)) => sc.value().parse::<f64>().ok(),
                _ => None,
            };
            let (literal, snippet) = match get(2) {
                Some(Term::Literal(l)) => {
                    let text = l.value();
                    let snippet = req.highlight.then(|| {
                        let mut html = String::with_capacity(text.len() + 16);
                        for c in text.chars() {
                            match c {
                                '&' => html.push_str("&amp;"),
                                '<' => html.push_str("&lt;"),
                                '>' => html.push_str("&gt;"),
                                '"' => html.push_str("&quot;"),
                                '\'' => html.push_str("&#39;"),
                                c if c == MARK.0 => html.push_str("<mark>"),
                                c if c == MARK.1 => html.push_str("</mark>"),
                                c => html.push(c),
                            }
                        }
                        html
                    });
                    let plain: String = text
                        .chars()
                        .filter(|c| *c != MARK.0 && *c != MARK.1)
                        .collect();
                    let lit = match l.language() {
                        Some(lang) => Literal::new_language_tagged_literal_unchecked(plain, lang),
                        None => Literal::new_typed_literal(plain, l.datatype()),
                    };
                    (Some(lit), snippet)
                }
                _ => (None, None),
            };
            TextHit {
                subject: get(0),
                score,
                literal,
                snippet,
                graph: get(3),
                predicate: get(4),
            }
        })
        .collect();
    Ok(TextHits {
        commit,
        limited: hits.len() == req.limit,
        hits,
    })
}

/// Options of [`VectorIndexes::recall`]: `samples` stored vectors are the queries, and
/// recall@`k` is measured against the exact search. `ef` overrides the index's
/// `efSearch`.
#[derive(Clone, Debug)]
pub struct RecallOptions {
    pub samples: usize,
    pub k: usize,
    pub ef: Option<usize>,
}

impl Default for RecallOptions {
    fn default() -> RecallOptions {
        RecallOptions {
            samples: 100,
            k: 10,
            ef: None,
        }
    }
}

/// The vector indexes.
#[derive(Clone)]
pub struct VectorIndexes {
    ds: Dataset,
}

impl VectorIndexes {
    pub fn list(&self) -> Vec<VectorIndexStatus> {
        self.ds.store().vector_indexes()
    }

    pub fn get(&self, name: &str) -> Option<VectorIndexStatus> {
        self.ds.store().vector_index(name)
    }

    /// Create or reconfigure index `name`; whether its build started. The build runs in
    /// the background ([`wait`](Self::wait) blocks until it ends).
    pub fn put(&self, name: &str, cfg: VectorIndexConfig) -> Result<bool> {
        self.ds.store().create_vector_index(name, cfg)
    }

    /// Remove index `name`.
    pub fn drop(&self, name: &str) -> Result<()> {
        self.ds.store().drop_vector_index(name)
    }

    /// Rebuild index `name` in the background.
    pub fn rebuild(&self, name: &str) -> Result<()> {
        self.ds.store().rebuild_vector_index(name)
    }

    /// Block until index `name`'s build ends, and return its status.
    pub fn wait(&self, name: &str) -> Option<VectorIndexStatus> {
        self.ds.store().wait_vector_index(name)
    }

    /// Measure index `name`'s recall against the exact search.
    pub fn recall(&self, name: &str, opts: &RecallOptions) -> Result<VectorRecall> {
        self.ds
            .store()
            .vector_recall(name, opts.samples, opts.k, opts.ef)
    }

    /// Compute index `name`'s embeddings again with its embedding service.
    pub fn reembed(&self, name: &str) -> Result<()> {
        self.ds.store().reembed(name)
    }

    /// The embedding worker's status of index `name`.
    pub fn embedding_status(&self, name: &str) -> Option<EmbeddingStatus> {
        self.ds.store().embedding_status(name)
    }

    /// Block until every embedding worker is idle, or fail after `timeout`.
    pub fn embed_until_idle(&self, timeout: Duration) -> Result<()> {
        self.ds.store().embed_until_idle(timeout)
    }
}

/// The spatial index.
#[derive(Clone)]
pub struct GeoIndex {
    ds: Dataset,
}

impl GeoIndex {
    /// Its status, or `None` when it is not enabled.
    pub fn status(&self) -> Option<GeoStatus> {
        self.ds.store().geo_status()
    }

    /// Whether the index is enabled.
    pub fn enabled(&self) -> bool {
        self.ds.store().geo_enabled()
    }

    /// Enable (or reconfigure) the index; the build runs in the background.
    pub fn enable(&self, cfg: GeoConfig) -> Result<GeoStatus> {
        self.ds.store().enable_geo(cfg)
    }

    /// Turn the index off.
    pub fn disable(&self) -> Result<()> {
        self.ds.store().disable_geo()
    }

    /// Rebuild the index of the current generation in the background.
    pub fn rebuild(&self) -> Result<GeoStatus> {
        self.ds.store().rebuild_geo()
    }

    /// Block until the build ends, and return the status.
    pub fn wait(&self) -> Option<GeoStatus> {
        self.ds.store().wait_geo()
    }

    /// The GeoJSON `FeatureCollection` of the indexed geometries in a box, of the graphs
    /// `graphs` reads (every graph when `None`).
    #[cfg(feature = "geo")]
    pub fn features(
        &self,
        q: &crate::geo::map::BoxQuery,
        graphs: Option<&crate::access::GraphAccess>,
    ) -> Result<serde_json::Value> {
        crate::geo::map::features_in_box_of(&self.ds.snapshot(), q, graphs)
    }
}
