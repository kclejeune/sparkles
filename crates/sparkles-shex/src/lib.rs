//! ShEx 2.1 (Shape Expressions) validation for Sparkles (Apache Jena `jena-shex`
//! equivalent).
//!
//! * **Schemas** in ShExC (`text/shex`) and ShExJ (`application/shex+json`), with
//!   imports, EXTERNAL shapes, annotations and semantic actions (the Test extension
//!   runs; other extensions are reported and skipped).
//! * **Shape maps** in the compact syntax (with Jena's `BASE`/`PREFIX`, commas and `a`)
//!   and the JSON form; `{FOCUS p o}` selectors expand over the data graph.
//! * **Validation** computes the typing of the (node, shape) pairs the shape map reaches,
//!   as the greatest fixed point per stratum of the schema: recursion and negation
//!   without a call stack as deep as the data.
//!
//! The data graph is read directly from a store [`Snapshot`] through index scans;
//! nothing is copied into memory.
//!
//! ```no_run
//! # use sparkles::store::{Store, StoreOptions};
//! # fn main() -> anyhow::Result<()> {
//! let store = Store::in_memory(StoreOptions::default());
//! let schema = sparkles_shex::Schema::parse_shexc(
//!     "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
//!      ex:Person { ex:name xsd:string ; ex:knows @ex:Person * }",
//!     None,
//! )?;
//! let schema = sparkles_shex::compile(&schema, &sparkles_shex::NoImports)?;
//! let map = sparkles_shex::ShapeMap::parse(
//!     "{FOCUS a ex:Person}@ex:Person",
//!     schema.prefixes(),
//!     None,
//! )?;
//! let results = sparkles_shex::validate(&store.snapshot(), &schema, &map, &Default::default())?;
//! println!("{}", results.to_text());
//! # Ok(()) }
//! ```

pub mod ast;
mod error;

// The engine's parts. They are public for the conformance harness, benchmarks and
// property tests; they are not a stable API.
#[doc(hidden)]
pub mod check;
#[doc(hidden)]
pub mod compile;
#[doc(hidden)]
pub mod engine;
#[doc(hidden)]
pub mod explain;
pub mod guard;
#[doc(hidden)]
pub mod ir;
#[doc(hidden)]
pub mod matcher;
#[doc(hidden)]
pub mod nc;
#[doc(hidden)]
pub mod neigh;
#[doc(hidden)]
pub mod report;
pub mod resolve;
#[doc(hidden)]
pub mod semact;
#[doc(hidden)]
pub mod shapemap;
#[doc(hidden)]
pub mod shexc;
#[doc(hidden)]
pub mod shexj;
pub mod shexr;
#[doc(hidden)]
pub mod typing;

pub use ast::*;
pub use error::{ParseError, SchemaError, TooManyResults};
pub use resolve::{FileResolver, NoImports, Resolver};

use oxrdf::{NamedNode, Term};
use serde::Serialize;
use sparkles::store::Snapshot;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Prefix declarations: `(prefix, namespace IRI)` in order, e.g. `("ex", "http://ex.org/")`.
pub type PrefixMap = Vec<(String, String)>;

// ------------------------------------------------------------------ schemas ------

/// The syntax of a schema text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaFormat {
    /// the compact syntax (`text/shex`)
    ShExC,
    /// the JSON-LD syntax (`application/shex+json`)
    ShExJ,
    /// ShExR: RDF in the ShEx vocabulary, in an RDF syntax
    ShExR(sparkles::io::RdfFormat),
}

impl SchemaFormat {
    /// `shexc` / `shex`, `shexj` / `json`, or `shexr` (Turtle).
    pub fn from_name(s: &str) -> Option<SchemaFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "shexc" | "shex" => Some(SchemaFormat::ShExC),
            "shexj" | "json" => Some(SchemaFormat::ShExJ),
            "shexr" => Some(SchemaFormat::ShExR(sparkles::io::RdfFormat::Turtle)),
            _ => None,
        }
    }

    /// `text/shex` or `application/shex+json` (parameters ignored). RDF media types are
    /// left to the caller, since `application/json` is also ShExJ.
    pub fn from_media_type(ct: &str) -> Option<SchemaFormat> {
        match ct.split(';').next()?.trim().to_ascii_lowercase().as_str() {
            "text/shex" => Some(SchemaFormat::ShExC),
            "application/shex+json" => Some(SchemaFormat::ShExJ),
            _ => None,
        }
    }
}

impl Schema {
    /// Parse ShExC. Relative IRIs resolve against `BASE`, then `base`.
    pub fn parse_shexc(text: &str, base: Option<&str>) -> Result<Schema, ParseError> {
        shexc::parser::parse(text, base)
    }

    /// Parse ShExJ (ShEx 2.1 `shapes` with `id`s; 2.next `ShapeDecl` wrappers too).
    pub fn from_shexj(json: &str) -> Result<Schema, ParseError> {
        shexj::from_shexj(json)
    }

    /// Parse ShExJ, resolving relative IRIs against `base` (imports included, as in
    /// ShExC). Without a base, relative imports are left to the [`Resolver`].
    pub fn from_shexj_with_base(json: &str, base: Option<&str>) -> Result<Schema, ParseError> {
        shexj::from_shexj_with_base(json, base)
    }

    /// The ShExJ form, with the `@context` of ShEx 2.1.
    pub fn to_shexj(&self) -> serde_json::Value {
        shexj::to_shexj(self)
    }

    /// The ShExC form, pretty-printed with the schema's prefixes.
    pub fn to_shexc(&self) -> String {
        shexc::writer::write(self)
    }

    /// Parse ShExR: RDF text in `format`, in the ShEx vocabulary.
    pub fn from_shexr(
        text: &str,
        format: sparkles::io::RdfFormat,
        base: Option<&str>,
    ) -> Result<Schema, ParseError> {
        shexr::from_text(text, format, base)
    }

    /// The ShExR form: a graph in the ShEx vocabulary.
    pub fn to_shexr(&self) -> oxrdf::Graph {
        shexr::to_graph(self)
    }

    /// The ShExR form as Turtle, with the schema's prefixes.
    pub fn to_shexr_turtle(&self) -> String {
        shexr::to_text(self, sparkles::io::RdfFormat::Turtle)
    }
}

/// Parse a schema in `hint`'s syntax, or sniffed: ShExJ when the text starts with `{`
/// (after whitespace), ShExC otherwise.
pub fn parse_schema(
    text: &str,
    base: Option<&str>,
    hint: Option<SchemaFormat>,
) -> Result<Schema, ParseError> {
    let format = hint.unwrap_or_else(|| {
        if text.trim_start().starts_with('{') {
            SchemaFormat::ShExJ
        } else {
            SchemaFormat::ShExC
        }
    });
    match format {
        SchemaFormat::ShExC => Schema::parse_shexc(text, base),
        SchemaFormat::ShExJ => Schema::from_shexj_with_base(text, base),
        SchemaFormat::ShExR(f) => Schema::from_shexr(text, f, base),
    }
}

/// A schema ready to validate with: imports and EXTERNAL shapes resolved, checked, and
/// compiled. Independent of any snapshot, so it can be reused across snapshots and
/// threads.
#[derive(Debug)]
pub struct CompiledSchema {
    pub(crate) ir: ir::Ir,
    pub(crate) prefixes: PrefixMap,
    pub(crate) base: Option<String>,
}

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<CompiledSchema>();
};

impl CompiledSchema {
    /// Does the schema have a START shape?
    pub fn has_start(&self) -> bool {
        self.ir.start.is_some()
    }

    /// The pair kind of a declared label, by its ShExJ form (the IRI, or `_:label`).
    pub fn label(&self, label: &str) -> Option<ir::PairKind> {
        self.ir.labels.get(label).copied()
    }

    /// The schema's prefixes (for shape maps without their own directives).
    pub fn prefixes(&self) -> &PrefixMap {
        &self.prefixes
    }

    /// The schema's base IRI.
    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    /// The compiled form.
    #[doc(hidden)]
    pub fn ir(&self) -> &ir::Ir {
        &self.ir
    }

    /// A compiled schema from its compiled form (the compiler; hand-built tests).
    #[doc(hidden)]
    pub fn from_ir(ir: ir::Ir, prefixes: PrefixMap, base: Option<String>) -> CompiledSchema {
        CompiledSchema { ir, prefixes, base }
    }
}

/// Resolve a schema's imports and EXTERNAL shapes through `resolver`, check its
/// structure, and compile it.
pub fn compile(schema: &Schema, resolver: &dyn Resolver) -> Result<CompiledSchema, SchemaError> {
    let closed = resolve::close(schema, resolver)?;
    let checked = check::check(&closed)?;
    compile::compile(&closed, &checked)
}

// --------------------------------------------------------------- shape maps ------

/// A shape label in a shape map.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ShapeLabel {
    Iri(String),
    /// a blank-node label, without `_:`
    BNode(String),
    /// `START`
    Start,
}

impl ShapeLabel {
    /// The ShExJ form of a declared label (the IRI, or `_:label`); `None` for START.
    pub fn as_shexj(&self) -> Option<String> {
        match self {
            ShapeLabel::Iri(i) => Some(i.clone()),
            ShapeLabel::BNode(b) => Some(format!("_:{b}")),
            ShapeLabel::Start => None,
        }
    }
}

impl From<Label> for ShapeLabel {
    fn from(l: Label) -> ShapeLabel {
        match l {
            Label::Iri(i) => ShapeLabel::Iri(i),
            Label::BNode(b) => ShapeLabel::BNode(b),
        }
    }
}

/// The node selector of a shape association.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeSelector {
    /// a node, whether or not it occurs in the data
    Term(Term),
    /// `{FOCUS p o}`, `{FOCUS p _}` (`focus_is_subject`, `subject: None`) or `{s p
    /// FOCUS}`, `{_ p FOCUS}` (`object: None`); `_` is `None`
    Focus {
        subject: Option<Term>,
        predicate: NamedNode,
        object: Option<Term>,
        focus_is_subject: bool,
    },
    /// `SPARQL """…"""`: the bindings of `?focus` (or of the first projected variable)
    /// of a SELECT query on the data graph
    Sparql(String),
}

/// A node selector paired with a shape label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Association {
    pub node: NodeSelector,
    pub shape: ShapeLabel,
}

/// A query map (selectors) or a fixed map (nodes only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapeMap(pub Vec<Association>);

impl ShapeMap {
    /// Parse the compact syntax. Without `BASE`/`PREFIX` directives, prefixed names use
    /// `prefixes` (the schema's) and relative IRIs `base`.
    pub fn parse(
        text: &str,
        prefixes: &PrefixMap,
        base: Option<&str>,
    ) -> Result<ShapeMap, ParseError> {
        shapemap::parse(text, prefixes, base)
    }

    /// Parse the JSON syntax (`[{"node": …, "shape": …}]`; `nodeSelector` and
    /// `shapeLabel` accepted).
    pub fn from_json(json: &str) -> Result<ShapeMap, ParseError> {
        shapemap::from_json(json)
    }
}

// ------------------------------------------------------------------ results ------

/// Whether a node conforms to a shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Conformant,
    Nonconformant,
}

/// Why a node does not conform: one of the first failures found (up to 8 per result).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ShexFailure {
    NodeKind {
        #[serde(serialize_with = "term_json")]
        value: Term,
        constraint: String,
    },
    Datatype {
        #[serde(serialize_with = "term_json")]
        value: Term,
        constraint: String,
    },
    Facet {
        #[serde(serialize_with = "term_json")]
        value: Term,
        constraint: String,
    },
    ValueSet {
        #[serde(serialize_with = "term_json")]
        value: Term,
        constraint: String,
    },
    Cardinality {
        predicate: String,
        inverse: bool,
        min: u32,
        /// `None`: unbounded
        max: Option<u32>,
        count: u64,
    },
    /// an outgoing arc a CLOSED shape does not allow
    Closed {
        predicate: String,
        #[serde(serialize_with = "term_json")]
        value: Term,
    },
    /// an arc no triple constraint matches, with a predicate that is not EXTRA
    Extra {
        predicate: String,
        #[serde(serialize_with = "term_json")]
        value: Term,
    },
    /// a OneOf or group cardinality that no partition of the arcs matches
    NoMatch {
        detail: String,
    },
    /// a value that fails a referenced shape
    Reference {
        shape: String,
        #[serde(serialize_with = "term_json")]
        value: Term,
    },
    Not {
        shape: String,
    },
    SemAct {
        extension: String,
        message: String,
    },
    External {
        shape: String,
    },
}

fn term_json<S: serde::Serializer>(t: &Term, s: S) -> Result<S::Ok, S::Error> {
    sparkles::sparql::results::term_json(t).serialize(s)
}

/// The result of one association of the fixed map.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeResult {
    pub node: Term,
    pub shape: ShapeLabel,
    pub status: Status,
    /// the first failure, in one line (nonconformant results)
    pub reason: Option<String>,
    pub failures: Vec<ShexFailure>,
    /// output of the Test extension's `print` (with `semact_trace`)
    pub prints: Vec<String>,
}

/// A result map: the results in shape-map order, with counts over all associations
/// (also those left out by `only_nonconformant`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultMap {
    /// every association conforms
    pub conforms: bool,
    pub conformant: usize,
    pub nonconformant: usize,
    pub results: Vec<ShapeResult>,
    /// e.g. "2 semantic actions with extension <…> were not run"
    pub warnings: Vec<String>,
    pub millis: u64,
    /// what the validation computed
    pub stats: ValidationStats,
}

/// Counters of a validation's typing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationStats {
    /// (node, shape) pairs discovered
    pub pairs: usize,
    /// pair evaluations, in discovery and refinement
    pub evaluations: u64,
    /// refinement waves of each stratum, lowest first
    pub waves: Vec<usize>,
}

impl ResultMap {
    /// The Sparkles JSON report (`{conforms, counts, results, warnings, millis}`, terms
    /// in SPARQL JSON).
    pub fn to_json(&self) -> serde_json::Value {
        report::to_json(self)
    }

    /// The ShapeMap JSON result map (`[{node, shape, status, reason?, appinfo?}]`,
    /// compact-syntax strings).
    pub fn to_shapemap_json(&self) -> serde_json::Value {
        report::to_shapemap_json(self)
    }

    /// The compact result map: `<n>@<S>` or `<n>@!<S>`, one per line.
    pub fn to_smap(&self) -> String {
        report::to_smap(self)
    }

    /// Jena's text report: `OK`, or one line per nonconformant association.
    pub fn to_text(&self) -> String {
        report::to_text(self)
    }
}

// --------------------------------------------------------------- validation ------

/// Validation options.
#[derive(Clone, Debug)]
pub struct ValidateOptions {
    /// The data graph: `None` = the store's default graph (the union of all graphs if
    /// the store uses a union default graph); a named graph IRI; `urn:x-arq:DefaultGraph`
    /// for the default graph or `urn:x-arq:UnionGraph` for the union of all graphs.
    pub data_graph: Option<String>,
    /// Further graphs merged into the data graph (e.g. the reasoner's
    /// `urn:x-sparkles:inferred`). Graphs that do not exist are ignored.
    pub extra_graphs: Vec<String>,
    /// Graphs never part of the data graph, even when it is the union of all graphs.
    pub exclude_graphs: Vec<String>,
    /// Evaluate pairs in parallel (rayon).
    pub parallel: bool,
    /// The thread pool parallel validation runs in (the global rayon pool if `None`).
    pub pool: Option<Arc<rayon::ThreadPool>>,
    pub timeout: Option<Duration>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop with [`TooManyResults`] once the result map would hold more results.
    pub max_results: Option<usize>,
    /// The most (node, shape) pairs the typing may hold; past it the validation fails
    /// with the `validation-work` budget.
    pub max_pairs: Option<usize>,
    /// The most partitions one match of a neighbourhood may try; past it the validation
    /// fails with the `validation-work` budget.
    pub max_partitions: Option<u64>,
    /// Report only nonconformant results (the counts still cover all).
    pub only_nonconformant: bool,
    /// Keep the Test extension's `print` output in the results.
    pub semact_trace: bool,
    /// The options `SPARQL` selectors run with: row and memory budgets, SERVICE and the
    /// outbound policy. The dataset (the validation's data graph), the timeout and the
    /// cancel flag are the validation's. `None`: the defaults (no budgets, no SERVICE).
    pub selector_query: Option<sparkles::sparql::QueryOptions>,
}

/// The default of [`ValidateOptions::max_pairs`].
pub const DEFAULT_MAX_PAIRS: usize = 10_000_000;
/// The default of [`ValidateOptions::max_partitions`].
pub const DEFAULT_MAX_PARTITIONS: u64 = 100_000;

impl Default for ValidateOptions {
    fn default() -> Self {
        ValidateOptions {
            data_graph: None,
            extra_graphs: Vec::new(),
            exclude_graphs: Vec::new(),
            parallel: true,
            pool: None,
            timeout: None,
            cancel: None,
            max_results: None,
            max_pairs: Some(DEFAULT_MAX_PAIRS),
            max_partitions: Some(DEFAULT_MAX_PARTITIONS),
            only_nonconformant: false,
            semact_trace: false,
            selector_query: None,
        }
    }
}

/// Validate the shape map's associations on the data graph of a snapshot.
///
/// Errors (in the `anyhow::Error`): [`sparkles::Error::Timeout`],
/// [`sparkles::Error::Cancelled`], [`sparkles::Error::BudgetExceeded`] (the
/// `validation-work` budget), [`TooManyResults`], [`SchemaError`] (a label the schema
/// does not define, START without a start shape), or an invalid data graph.
pub fn validate(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
) -> anyhow::Result<ResultMap> {
    engine::validate(snap, schema, map, opts)
}

/// Validate one node against one shape.
pub fn validate_node(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    node: &Term,
    shape: &ShapeLabel,
    opts: &ValidateOptions,
) -> anyhow::Result<ShapeResult> {
    engine::validate_node(snap, schema, node, shape, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_defaults() {
        let o = ValidateOptions::default();
        assert!(o.parallel);
        assert_eq!(o.max_partitions, Some(100_000));
        assert_eq!(o.max_pairs, Some(10_000_000));
        assert!(!o.only_nonconformant && !o.semact_trace);
    }

    #[test]
    fn schema_formats() {
        assert_eq!(
            SchemaFormat::from_media_type("text/shex; charset=utf-8"),
            Some(SchemaFormat::ShExC)
        );
        assert_eq!(
            SchemaFormat::from_media_type("application/shex+json"),
            Some(SchemaFormat::ShExJ)
        );
        assert_eq!(SchemaFormat::from_media_type("application/json"), None);
        assert_eq!(SchemaFormat::from_name("ShExJ"), Some(SchemaFormat::ShExJ));
    }

    #[test]
    fn failures_serialize_tagged() {
        let f = ShexFailure::Cardinality {
            predicate: "http://ex.org/p".into(),
            inverse: false,
            min: 1,
            max: None,
            count: 0,
        };
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            serde_json::json!({"kind": "cardinality", "predicate": "http://ex.org/p",
                "inverse": false, "min": 1, "max": null, "count": 0})
        );
        let f = ShexFailure::NodeKind {
            value: oxrdf::Literal::new_simple_literal("x").into(),
            constraint: "IRI".into(),
        };
        let j = serde_json::to_value(&f).unwrap();
        assert_eq!(j["kind"], "nodeKind");
        assert_eq!(j["value"]["type"], "literal");
        assert_eq!(
            serde_json::to_value(Status::Nonconformant).unwrap(),
            "nonconformant"
        );
    }
}
