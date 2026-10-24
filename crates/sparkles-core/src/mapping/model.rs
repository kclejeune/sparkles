//! The intermediate form that R2RML, both RML vocabularies and YARRRML are read into
//! (spec F19 §3.1).

use super::template::Template;
use crate::tabular::csvw::DialectDesc;
use oxrdf::{NamedNode, Term};

/// The language a mapping was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vocabulary {
    /// The W3C R2RML vocabulary alone.
    R2rml,
    /// R2RML's term maps with the rml.io logical sources (`http://semweb.mmlab.be/ns/rml#`).
    RmlIo,
    /// The RML-Core vocabulary of the W3C Knowledge Graph Construction Community Group
    /// (`http://w3id.org/rml/`).
    RmlCore,
    /// YARRRML.
    Yarrrml,
}

impl Vocabulary {
    pub fn name(self) -> &'static str {
        match self {
            Vocabulary::R2rml => "R2RML",
            Vocabulary::RmlIo => "RML (rml.io)",
            Vocabulary::RmlCore => "RML-Core",
            Vocabulary::Yarrrml => "YARRRML",
        }
    }
}

/// A mapping: triples maps over logical sources.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub maps: Vec<TriplesMap>,
    /// The distinct logical sources: triples maps with equal sources share one, so the
    /// source is read once for all of them.
    pub sources: Vec<SourceDecl>,
    /// The base IRI that relative generated IRIs resolve against.
    pub base: Option<String>,
    pub vocabulary: Vocabulary,
    /// Constructs that were read and ignored.
    pub warnings: Vec<String>,
    /// The prefixes declared by the mapping, for `convert` and Turtle output.
    pub prefixes: Vec<(String, String)>,
}

/// A logical source or logical table.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceDecl {
    pub kind: SourceKind,
    /// The JSONPath iterator of a JSON source.
    pub iterator: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceKind {
    /// A CSV or TSV file, with the dialect of a CSVW description in the mapping.
    Csv {
        file: String,
        tsv: bool,
        dialect: DialectDesc,
        /// Cell values that are NULL besides the empty string.
        null: Vec<String>,
    },
    /// A JSON document, or JSON Lines when `lines`.
    Json { file: String, lines: bool },
    /// A Parquet file.
    Parquet { file: String },
    /// A table or query of a database connection.
    Sql {
        /// The connection's name (`urn:x-sparkles:connection:NAME`, YARRRML `access`, or a
        /// D2RQ description's `jdbcDSN`).
        connection: String,
        query: SqlQuery,
    },
}

impl SourceKind {
    /// A short description for messages.
    pub fn describe(&self) -> String {
        match self {
            SourceKind::Csv { file, tsv, .. } => {
                format!("{} file {file}", if *tsv { "TSV" } else { "CSV" })
            }
            SourceKind::Json { file, lines } => {
                format!("{} file {file}", if *lines { "JSON Lines" } else { "JSON" })
            }
            SourceKind::Parquet { file } => format!("Parquet file {file}"),
            SourceKind::Sql { connection, query } => match query {
                SqlQuery::Table(t) => format!("table {t} of connection {connection}"),
                SqlQuery::Query(_) => format!("a query of connection {connection}"),
            },
        }
    }

    /// The file a file source reads.
    pub fn file(&self) -> Option<&str> {
        match self {
            SourceKind::Csv { file, .. }
            | SourceKind::Json { file, .. }
            | SourceKind::Parquet { file } => Some(file),
            SourceKind::Sql { .. } => None,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            SourceKind::Csv { tsv: false, .. } => "csv",
            SourceKind::Csv { tsv: true, .. } => "tsv",
            SourceKind::Json { lines: false, .. } => "json",
            SourceKind::Json { lines: true, .. } => "jsonl",
            SourceKind::Parquet { .. } => "parquet",
            SourceKind::Sql { .. } => "sql",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SqlQuery {
    /// `rr:tableName`: a table or view name, as written (with any delimiters).
    Table(String),
    /// `rr:sqlQuery`: run as a derived table.
    Query(String),
}

#[derive(Clone, Debug)]
pub struct TriplesMap {
    /// The map's IRI or YARRRML name, for messages.
    pub id: String,
    /// Index into [`Mapping::sources`].
    pub source: usize,
    pub subject: TermMap,
    pub classes: Vec<NamedNode>,
    /// The subject map's graph maps; empty is the default graph.
    pub graphs: Vec<TermMap>,
    pub predicate_objects: Vec<PredicateObject>,
    /// The map's own base IRI (RML-Core `rml:baseIRI`), over the run's and the
    /// mapping's.
    pub base: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PredicateObject {
    pub predicates: Vec<TermMap>,
    pub objects: Vec<ObjectMap>,
    pub graphs: Vec<TermMap>,
}

#[derive(Clone, Debug)]
pub enum ObjectMap {
    Term(TermMap),
    /// A referencing object map: the parent triples map's subject, joined on the
    /// conditions (child reference, parent reference).
    Join {
        parent: usize,
        conditions: Vec<(String, String)>,
    },
}

#[derive(Clone, Debug)]
pub struct TermMap {
    pub value: Value,
    pub term_type: TermType,
    pub datatype: Option<Dt>,
    pub language: Option<Lang>,
}

impl TermMap {
    pub fn constant(t: impl Into<Term>) -> TermMap {
        let t = t.into();
        let term_type = match &t {
            Term::NamedNode(_) => TermType::Iri,
            Term::BlankNode(_) => TermType::BlankNode,
            _ => TermType::Literal,
        };
        TermMap {
            value: Value::Constant(t),
            term_type,
            datatype: None,
            language: None,
        }
    }

    /// The references this term map reads, in order.
    pub fn references(&self, out: &mut Vec<String>) {
        match &self.value {
            Value::Constant(_) | Value::Fresh => {}
            Value::Reference(r) => out.push(r.clone()),
            Value::Template(t) => out.extend(t.references().map(str::to_string)),
            Value::Function(f) => {
                for (_, a) in &f.args {
                    a.references(out);
                }
            }
        }
        if let Some(Dt::Map(m)) = &self.datatype {
            m.references(out);
        }
        if let Some(Lang::Map(m)) = &self.language {
            m.references(out);
        }
    }

    /// The term map as written, for messages.
    pub fn describe(&self) -> String {
        match &self.value {
            Value::Constant(t) => format!("constant {t}"),
            Value::Reference(r) => format!("reference {r:?}"),
            Value::Template(t) => format!("template {:?}", t.as_str()),
            Value::Function(f) => format!("function <{}>", f.function.as_str()),
            Value::Fresh => "a blank node per row".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Value {
    Constant(Term),
    /// A column name (CSV, SQL, Parquet) or a JSONPath relative to the iteration.
    Reference(String),
    Template(Template),
    Function(Box<FunctionCall>),
    /// A new blank node for each iteration (an RML subject map with a blank node term
    /// type and no value).
    Fresh,
}

#[derive(Clone, Debug)]
pub struct FunctionCall {
    pub function: NamedNode,
    /// The parameters with their values.
    pub args: Vec<(NamedNode, TermMap)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermType {
    Iri,
    BlankNode,
    Literal,
}

/// A literal's datatype: constant or generated.
#[derive(Clone, Debug)]
pub enum Dt {
    Constant(NamedNode),
    Map(Box<TermMap>),
}

/// A literal's language tag: constant or generated.
#[derive(Clone, Debug)]
pub enum Lang {
    Constant(String),
    Map(Box<TermMap>),
}

impl Mapping {
    /// Index of `decl` in the sources, adding it when it is new.
    pub fn source_index(sources: &mut Vec<SourceDecl>, decl: SourceDecl) -> usize {
        match sources.iter().position(|s| *s == decl) {
            Some(i) => i,
            None => {
                sources.push(decl);
                sources.len() - 1
            }
        }
    }

    /// The constant graphs every triple of the mapping can go to, or `None` when a graph
    /// map is not constant. The default graph is `None` inside the list.
    pub fn constant_graphs(&self) -> Option<Vec<Option<NamedNode>>> {
        let mut out: Vec<Option<NamedNode>> = Vec::new();
        let mut add = |g: Option<NamedNode>| {
            if !out.contains(&g) {
                out.push(g);
            }
        };
        let graph = |tm: &TermMap| -> Option<Option<NamedNode>> {
            match &tm.value {
                Value::Constant(Term::NamedNode(n))
                    if n.as_str() == super::DEFAULT_GRAPH_RR
                        || n.as_str() == super::DEFAULT_GRAPH_RML =>
                {
                    Some(None)
                }
                Value::Constant(Term::NamedNode(n)) => Some(Some(n.clone())),
                _ => None,
            }
        };
        for m in &self.maps {
            let mut subject_graphs = Vec::new();
            for g in &m.graphs {
                subject_graphs.push(graph(g)?);
            }
            if m.predicate_objects.is_empty() || !m.classes.is_empty() {
                if subject_graphs.is_empty() {
                    add(None);
                }
                for g in &subject_graphs {
                    add(g.clone());
                }
            }
            for po in &m.predicate_objects {
                let mut gs = subject_graphs.clone();
                for g in &po.graphs {
                    gs.push(graph(g)?);
                }
                if gs.is_empty() {
                    add(None);
                }
                for g in gs {
                    add(g);
                }
            }
        }
        Some(out)
    }
}
