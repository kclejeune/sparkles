//! The mapping schema (§3.2, §3.3): SDL with `@rdf`, `@prefix`, `@lang` and `@single`,
//! parsed and checked into the classes, predicates and IRIs each type, field and enum
//! value stands for.

use crate::scalars::{CUSTOM, Scalar};
use apollo_compiler::ast::{self, Type, Value};
use apollo_compiler::diagnostic::ToCliReport;
use apollo_compiler::schema::{Component, ExtendedType, FieldDefinition};
use apollo_compiler::validation::Valid;
use apollo_compiler::{Node, Schema};
use oxrdf::NamedNode;
use std::collections::HashSet;

/// The directives the server declares for every mapping schema.
pub const DIRECTIVES: &str = r#"
directive @prefix(name: String!, iri: String!) repeatable on SCHEMA
directive @rdf(
  iri: String
  vocab: String
  inverse: Boolean = false
  subclasses: Boolean = true
) on SCHEMA | OBJECT | INTERFACE | FIELD_DEFINITION | ENUM_VALUE
directive @lang(prefer: [String!]!) on SCHEMA | FIELD_DEFINITION
directive @single(onMany: OnMany = ERROR) on FIELD_DEFINITION
enum OnMany { ERROR MIN }
"#;

pub const LANG_STRING: &str =
    "type LangString { value: String! language: String direction: String }";
pub const RDF_TERM: &str = "type RDFTerm { kind: String! value: String! datatype: String language: String direction: String }";

/// Type names the server generates or declares, which the SDL cannot define.
pub const RESERVED_TYPES: [&str; 21] = [
    "Node",
    "Resource",
    "PageInfo",
    "LangString",
    "RDFTerm",
    "OnMany",
    "ValueOrder",
    "StringFilter",
    "BooleanFilter",
    "IntFilter",
    "IntegerFilter",
    "DecimalFilter",
    "FloatFilter",
    "DateTimeFilter",
    "DateFilter",
    "TimeFilter",
    "DurationFilter",
    "IRIFilter",
    "IDFilter",
    "Mutation",
    "Subscription",
];

/// What happens when a single-valued field finds several values (§4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnMany {
    Error,
    Min,
}

/// The type of a field's values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// an object type, interface or union: the values are nodes
    Object(String),
    Scalar(Scalar),
    Enum(String),
}

/// A field of a mapped type.
#[derive(Clone, Debug)]
pub struct FieldMap {
    pub name: String,
    pub predicate: NamedNode,
    pub inverse: bool,
    pub target: Target,
    /// `[T]` in any form; otherwise single-valued
    pub list: bool,
    /// the outer type is non-null (`T!` or `[T]!`)
    pub non_null: bool,
    pub lang: Option<Vec<String>>,
    pub on_many: OnMany,
    pub description: Option<String>,
    /// the type as written, such as `[Person!]!`
    pub ty: String,
}

impl FieldMap {
    pub fn is_object(&self) -> bool {
        matches!(self.target, Target::Object(_))
    }
}

/// An object type or interface with a class.
#[derive(Clone, Debug)]
pub struct TypeMap {
    pub name: String,
    pub interface: bool,
    pub class: NamedNode,
    pub subclasses: bool,
    pub implements: Vec<String>,
    pub fields: Vec<FieldMap>,
    pub description: Option<String>,
}

impl TypeMap {
    pub fn field(&self, name: &str) -> Option<&FieldMap> {
        self.fields.iter().find(|f| f.name == name)
    }
}

#[derive(Clone, Debug)]
pub struct EnumMap {
    pub name: String,
    pub values: Vec<(String, NamedNode, Option<String>)>,
    pub description: Option<String>,
}

impl EnumMap {
    pub fn iri(&self, value: &str) -> Option<&NamedNode> {
        self.values
            .iter()
            .find(|(v, _, _)| v == value)
            .map(|(_, i, _)| i)
    }
    pub fn value_of(&self, iri: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(_, i, _)| i.as_str() == iri)
            .map(|(v, _, _)| v.as_str())
    }
}

/// A root field of `Query`.
#[derive(Clone, Debug)]
pub enum RootKind {
    /// `t(id: ID!): T`
    Lookup(String),
    /// `allT(...): TConnection!`
    Connection(String),
    /// `[T!]!` with `filter`, `orderBy`, `first` and `offset`
    List(String),
    /// `node(id: ID!): Node`
    Node,
}

#[derive(Clone, Debug)]
pub struct RootField {
    pub name: String,
    pub kind: RootKind,
    pub description: Option<String>,
    /// the type as written (hand-written `Query`)
    pub ty: Option<String>,
}

/// A checked mapping schema.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub prefixes: Vec<(String, String)>,
    pub lang: Option<Vec<String>>,
    /// mapped object types and interfaces, in SDL order
    pub types: Vec<TypeMap>,
    pub unions: Vec<(String, Vec<String>, Option<String>)>,
    pub enums: Vec<EnumMap>,
    pub roots: Vec<RootField>,
    /// the `Query` type's description, when the SDL has one
    pub query_description: Option<String>,
    /// scalars the SDL declares or uses, besides the built-in ones
    pub scalars: Vec<String>,
    pub warnings: Vec<String>,
}

impl Mapping {
    pub fn ty(&self, name: &str) -> Option<&TypeMap> {
        self.types.iter().find(|t| t.name == name)
    }
    pub fn object_types(&self) -> impl Iterator<Item = &TypeMap> {
        self.types.iter().filter(|t| !t.interface)
    }
    pub fn enum_(&self, name: &str) -> Option<&EnumMap> {
        self.enums.iter().find(|e| e.name == name)
    }
    pub fn union(&self, name: &str) -> Option<&[String]> {
        self.unions
            .iter()
            .find(|u| u.0 == name)
            .map(|u| u.1.as_slice())
    }

    /// The object types a value of type `name` may have, in SDL order: the type itself,
    /// an interface's implementations, or a union's members.
    pub fn possible_types(&self, name: &str) -> Vec<String> {
        if let Some(t) = self.ty(name) {
            if !t.interface {
                return vec![t.name.clone()];
            }
            return self
                .object_types()
                .filter(|o| self.implements(&o.name, name))
                .map(|o| o.name.clone())
                .collect();
        }
        if let Some(m) = self.union(name) {
            return self
                .object_types()
                .filter(|o| m.contains(&o.name))
                .map(|o| o.name.clone())
                .collect();
        }
        if name == "Node" {
            return self.object_types().map(|o| o.name.clone()).collect();
        }
        Vec::new()
    }

    fn implements(&self, ty: &str, iface: &str) -> bool {
        let Some(t) = self.ty(ty) else { return false };
        t.implements
            .iter()
            .any(|i| i == iface || self.implements(i, iface))
    }
}

/// Whether a write-time guard backs a non-null field: the class of the type, the
/// field's predicate and whether it is read inverted.
pub type Backing<'a> = &'a dyn Fn(&str, &str, bool) -> bool;

/// An error of a mapping schema, with the line it is on.
#[derive(Clone, Debug)]
pub struct SdlError {
    pub message: String,
    pub line: Option<usize>,
}

impl std::fmt::Display for SdlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

fn err(message: impl Into<String>, line: Option<usize>) -> SdlError {
    SdlError {
        message: message.into(),
        line,
    }
}

const SDL_PATH: &str = "schema.graphql";

/// Parse and check a mapping schema. Every error is returned, each with its line.
pub fn parse(sdl: &str, backing: Backing) -> Result<(Mapping, Valid<Schema>), Vec<SdlError>> {
    let doc = ast::Document::parse(sdl, SDL_PATH).map_err(|e| {
        e.errors
            .iter()
            .map(|d| {
                err(
                    d.error.to_string(),
                    d.line_column_range().map(|r| r.start.line),
                )
            })
            .collect::<Vec<_>>()
    })?;
    let mut errors = Vec::new();
    let line_of = |loc: Option<apollo_compiler::parser::SourceSpan>| {
        loc.and_then(|l| l.line_column(&doc.sources))
            .map(|l| l.line)
    };
    let mut defined: HashSet<String> = HashSet::new();
    let mut objects: HashSet<String> = HashSet::new();
    let mut scalars: Vec<String> = Vec::new();
    for d in &doc.definitions {
        match d {
            ast::Definition::OperationDefinition(_) | ast::Definition::FragmentDefinition(_) => {
                errors.push(err(
                    "a mapping schema holds type definitions, not operations",
                    line_of(d.location()),
                ));
            }
            ast::Definition::DirectiveDefinition(x) => errors.push(err(
                format!(
                    "directive @{} cannot be declared: the server declares @rdf, @prefix, @lang and @single",
                    x.name
                ),
                line_of(d.location()),
            )),
            _ => {}
        }
        if let Some(n) = d.name() {
            let n = n.as_str();
            if let ast::Definition::ObjectTypeDefinition(_) = d {
                objects.insert(n.to_string());
            }
            if let ast::Definition::ScalarTypeDefinition(_) = d {
                if Scalar::from_name(n).is_none() {
                    errors.push(err(
                        format!(
                            "scalar {n} is not a scalar the adapter knows (String, Boolean, Int, Float, ID, Integer, Decimal, DateTime, Date, Time, Duration, IRI)"
                        ),
                        line_of(d.location()),
                    ));
                }
                scalars.push(n.to_string());
            } else if RESERVED_TYPES.contains(&n) || Scalar::from_name(n).is_some() {
                errors.push(err(
                    format!("the name {n} is reserved for a type the server generates"),
                    line_of(d.location()),
                ));
            }
            defined.insert(n.to_string());
        }
    }
    // generated names: TFilter, TOrderBy, TConnection, TEdge of a mapped type
    for d in &doc.definitions {
        let Some(n) = d.name() else { continue };
        for suffix in ["Filter", "OrderBy", "Connection", "Edge"] {
            if let Some(stem) = n.as_str().strip_suffix(suffix)
                && (objects.contains(stem)
                    || doc.definitions.iter().any(|x| {
                        matches!(x, ast::Definition::InterfaceTypeDefinition(_))
                            && x.name().is_some_and(|m| m == stem)
                    }))
            {
                errors.push(err(
                    format!("the name {n} is generated for the type {stem}"),
                    line_of(d.location()),
                ));
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    // the prelude: the directives, the scalars the SDL does not declare, the two term
    // types, Node, and stubs of generated types that a hand-written Query names
    let mut prelude = String::from(DIRECTIVES);
    for (s, _) in CUSTOM {
        if !defined.contains(s) {
            prelude.push_str(&format!("scalar {s}\n"));
        }
    }
    prelude.push_str(LANG_STRING);
    prelude.push('\n');
    prelude.push_str(RDF_TERM);
    prelude.push('\n');
    prelude.push_str("interface Node { id: ID! }\n");
    let has_query = defined.contains("Query");
    if !has_query {
        prelude.push_str("type Query { _stub: Boolean }\n");
    }
    for o in &objects {
        let c = format!("{o}Connection");
        if sdl.contains(&c) && !defined.contains(&c) {
            prelude.push_str(&format!("type {c} {{ _stub: Boolean }}\n"));
        }
    }
    let schema = match Schema::builder()
        .adopt_orphan_extensions()
        .parse(prelude, "prelude.graphql")
        .add_ast(&doc)
        .build()
        .map_err(Box::new)
        .and_then(|s| s.validate().map_err(Box::new))
    {
        Ok(s) => s,
        Err(e) => {
            return Err(e
                .errors
                .iter()
                .map(|d| {
                    let range = d.line_column_range();
                    let in_sdl = d
                        .error
                        .location()
                        .and_then(|l| d.sources.get(&l.file_id()))
                        .is_some_and(|f| f.path().ends_with(SDL_PATH));
                    err(
                        d.error.to_string(),
                        range.filter(|_| in_sdl).map(|r| r.start.line),
                    )
                })
                .collect());
        }
    };
    let m = read(&schema, &defined, has_query, scalars, backing)?;
    Ok((m, schema))
}

fn line(schema: &Schema, loc: Option<apollo_compiler::parser::SourceSpan>) -> Option<usize> {
    loc.and_then(|l| l.line_column(&schema.sources))
        .map(|l| l.line)
}

fn str_arg(d: &ast::Directive, name: &str) -> Option<String> {
    match d.specified_argument_by_name(name).map(|v| v.as_ref()) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn bool_arg(d: &ast::Directive, name: &str) -> Option<bool> {
    match d.specified_argument_by_name(name).map(|v| v.as_ref()) {
        Some(Value::Boolean(b)) => Some(*b),
        _ => None,
    }
}

fn str_list_arg(d: &ast::Directive, name: &str) -> Option<Vec<String>> {
    match d.specified_argument_by_name(name).map(|v| v.as_ref()) {
        Some(Value::List(l)) => Some(
            l.iter()
                .filter_map(|v| match v.as_ref() {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
        ),
        Some(Value::String(s)) => Some(vec![s.clone()]),
        _ => None,
    }
}

/// Schemes written as absolute IRIs without `//`.
const SCHEMES: [&str; 7] = ["urn", "tag", "mailto", "data", "did", "file", "doi"];

/// Expand an IRI written in the SDL: a prefixed name of a declared prefix, or an
/// absolute IRI.
pub fn expand(s: &str, prefixes: &[(String, String)]) -> Result<NamedNode, String> {
    let s = s.trim();
    let s = s
        .strip_prefix('<')
        .and_then(|x| x.strip_suffix('>'))
        .unwrap_or(s);
    if let Some((p, local)) = s.split_once(':')
        && !local.starts_with("//")
    {
        if let Some((_, ns)) = prefixes.iter().find(|(n, _)| n == p) {
            return NamedNode::new(format!("{ns}{local}"))
                .map_err(|e| format!("'{s}' is not an IRI after expansion: {e}"));
        }
        if !SCHEMES.contains(&p.to_ascii_lowercase().as_str()) {
            return Err(format!("'{s}' uses the undeclared prefix '{p}'"));
        }
    }
    if !s.contains(':') {
        return Err(format!("'{s}' is not an absolute IRI"));
    }
    NamedNode::new(s).map_err(|e| format!("'{s}' is not an absolute IRI: {e}"))
}

fn description(d: Option<&Node<str>>) -> Option<String> {
    d.map(|s| s.to_string())
}

/// The named type of a field type, and whether it is a list, with the outer
/// nullability; `None` for lists of lists.
fn shape(ty: &Type) -> Option<(&str, bool, bool)> {
    match ty {
        Type::Named(n) => Some((n.as_str(), false, false)),
        Type::NonNullNamed(n) => Some((n.as_str(), false, true)),
        Type::List(i) | Type::NonNullList(i) => match i.as_ref() {
            Type::Named(n) | Type::NonNullNamed(n) => {
                Some((n.as_str(), true, matches!(ty, Type::NonNullList(_))))
            }
            _ => None,
        },
    }
}

fn read(
    schema: &Valid<Schema>,
    defined: &HashSet<String>,
    has_query: bool,
    scalars: Vec<String>,
    backing: Backing,
) -> Result<Mapping, Vec<SdlError>> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let sd = &schema.schema_definition;
    let mut prefixes: Vec<(String, String)> = Vec::new();
    for d in sd.directives.get_all("prefix") {
        let (Some(n), Some(i)) = (str_arg(d, "name"), str_arg(d, "iri")) else {
            continue;
        };
        if prefixes.iter().any(|(p, _)| *p == n) {
            errors.push(err(
                format!("the prefix '{n}' is declared twice"),
                line(schema, d.location()),
            ));
        }
        if NamedNode::new(format!("{i}x")).is_err() || !i.contains(':') {
            errors.push(err(
                format!("@prefix {n}: '{i}' is not an absolute IRI"),
                line(schema, d.location()),
            ));
        }
        prefixes.push((n, i));
    }
    let mut vocab = None;
    if let Some(d) = sd.directives.get("rdf") {
        if let Some(v) = str_arg(d, "vocab") {
            match expand(&v, &prefixes) {
                Ok(_) if v.contains(':') => {
                    // a namespace, possibly prefixed: expand it as written
                    let ns = match v.split_once(':') {
                        Some((p, rest)) if !rest.starts_with("//") => prefixes
                            .iter()
                            .find(|(n, _)| n == p)
                            .map(|(_, ns)| format!("{ns}{rest}"))
                            .unwrap_or(v.clone()),
                        _ => v.clone(),
                    };
                    vocab = Some(ns);
                }
                Ok(_) => vocab = Some(v),
                Err(e) => errors.push(err(
                    format!("@rdf(vocab:): {e}"),
                    line(schema, d.location()),
                )),
            }
        }
        if str_arg(d, "iri").is_some() {
            errors.push(err(
                "@rdf on the schema takes vocab only",
                line(schema, d.location()),
            ));
        }
    }
    let lang = sd
        .directives
        .get("lang")
        .and_then(|d| str_list_arg(d, "prefer"));
    let user = |n: &str| defined.contains(n);
    let iri_of = |name: &str,
                  dir: Option<&ast::Directive>,
                  what: &str,
                  loc: Option<usize>,
                  errors: &mut Vec<SdlError>|
     -> Option<NamedNode> {
        match dir.and_then(|d| str_arg(d, "iri")) {
            Some(i) => match expand(&i, &prefixes) {
                Ok(n) => Some(n),
                Err(e) => {
                    errors.push(err(format!("{what}: {e}"), loc));
                    None
                }
            },
            None => match &vocab {
                Some(v) => match NamedNode::new(format!("{v}{name}")) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        errors.push(err(format!("{what}: {e}"), loc));
                        None
                    }
                },
                None => {
                    errors.push(err(
                        format!("{what} has no @rdf(iri:) and the schema has no @rdf(vocab:)"),
                        loc,
                    ));
                    None
                }
            },
        }
    };

    // enums
    let mut enums = Vec::new();
    for (name, t) in &schema.types {
        let ExtendedType::Enum(e) = t else { continue };
        if !user(name.as_str()) {
            continue;
        }
        let mut values = Vec::new();
        for (v, def) in &e.values {
            let loc = line(schema, def.location());
            if let Some(i) = iri_of(
                v.as_str(),
                def.directives.get("rdf").map(|d| -> &ast::Directive { d }),
                &format!("enum value {name}.{v}"),
                loc,
                &mut errors,
            ) {
                values.push((v.to_string(), i, description(def.description.as_ref())));
            }
        }
        enums.push(EnumMap {
            name: name.to_string(),
            values,
            description: description(e.description.as_ref()),
        });
    }
    // unions
    let mut unions: Vec<(String, Vec<String>, Option<String>)> = Vec::new();
    for (name, t) in &schema.types {
        let ExtendedType::Union(u) = t else { continue };
        if user(name.as_str()) {
            unions.push((
                name.to_string(),
                u.members.iter().map(|m| m.to_string()).collect(),
                description(u.description.as_ref()),
            ));
        }
    }
    let is_mapped = |n: &str| {
        user(n)
            && n != "Query"
            && matches!(
                schema.types.get(n),
                Some(ExtendedType::Object(_) | ExtendedType::Interface(_))
            )
    };
    // object types and interfaces
    let mut types: Vec<TypeMap> = Vec::new();
    for (name, t) in &schema.types {
        let (fields, directives, desc, implements, interface, loc) = match t {
            ExtendedType::Object(o) => (
                &o.fields,
                &o.directives,
                &o.description,
                &o.implements_interfaces,
                false,
                o.location(),
            ),
            ExtendedType::Interface(i) => (
                &i.fields,
                &i.directives,
                &i.description,
                &i.implements_interfaces,
                true,
                i.location(),
            ),
            _ => continue,
        };
        if !is_mapped(name.as_str()) {
            continue;
        }
        let rdf = directives.get("rdf").map(|d| -> &ast::Directive { d });
        if let Some(d) = rdf
            && (str_arg(d, "vocab").is_some() || bool_arg(d, "inverse").is_some())
        {
            errors.push(err(
                format!("@rdf on the type {name} takes iri and subclasses only"),
                line(schema, loc),
            ));
        }
        let Some(class) = iri_of(
            name.as_str(),
            rdf,
            &format!("type {name}"),
            line(schema, loc),
            &mut errors,
        ) else {
            continue;
        };
        let subclasses = rdf.and_then(|d| bool_arg(d, "subclasses")).unwrap_or(true);
        let mut fm = Vec::new();
        for (fname, f) in fields {
            if let Some(m) = field_map(
                schema,
                name.as_str(),
                fname.as_str(),
                f,
                &iri_of,
                &is_mapped,
                &enums,
                &mut errors,
            ) {
                fm.push(m);
            }
        }
        types.push(TypeMap {
            name: name.to_string(),
            interface,
            class,
            subclasses,
            implements: implements
                .iter()
                .map(|i| i.to_string())
                .filter(|i| i != "Node")
                .collect(),
            fields: fm,
            description: description(desc.as_ref()),
        });
    }
    for u in &unions {
        for m in &u.1 {
            if !types.iter().any(|t: &TypeMap| t.name == *m) {
                errors.push(err(
                    format!("union {}: member {m} is not mapped", u.0),
                    None,
                ));
            }
        }
    }
    // the root fields
    let mut roots = Vec::new();
    let mut query_description = None;
    if has_query {
        if let Some(q) = schema.get_object("Query") {
            query_description = description(q.description.as_ref());
            if q.directives.get("rdf").is_some() {
                errors.push(err("Query cannot carry @rdf", line(schema, q.location())));
            }
            for (fname, f) in &q.fields {
                let loc = line(schema, f.location());
                let ty = f.ty.to_string();
                let bad = |errors: &mut Vec<SdlError>| {
                    errors.push(err(
                        format!(
                            "Query.{fname}: the planner serves lookups `f(id: ID!): T`, lists `[T!]!`, connections `TConnection!` and `node(id: ID!): Node`, not {ty}"
                        ),
                        loc,
                    ))
                };
                let named = f.ty.inner_named_type().as_str();
                let args: Vec<&str> = f.arguments.iter().map(|a| a.name.as_str()).collect();
                let kind = if named == "Node" && !f.ty.is_list() && args == ["id"] {
                    RootKind::Node
                } else if !f.ty.is_list()
                    && is_mapped(named)
                    && !schema.get_interface(named).is_some()
                {
                    if args == ["id"]
                        && f.arguments[0].ty.as_ref()
                            == &Type::NonNullNamed(apollo_compiler::name!("ID"))
                    {
                        RootKind::Lookup(named.to_string())
                    } else {
                        bad(&mut errors);
                        continue;
                    }
                } else if f.ty.is_list()
                    && is_mapped(named)
                    && schema.get_object(named).is_some()
                    && args.is_empty()
                {
                    RootKind::List(named.to_string())
                } else if !f.ty.is_list()
                    && args.is_empty()
                    && let Some(stem) = named.strip_suffix("Connection")
                    && is_mapped(stem)
                    && schema.get_object(stem).is_some()
                {
                    RootKind::Connection(stem.to_string())
                } else {
                    bad(&mut errors);
                    continue;
                };
                if !f.directives.is_empty() {
                    errors.push(err(format!("Query.{fname}: no directives apply here"), loc));
                }
                roots.push(RootField {
                    name: fname.to_string(),
                    kind,
                    description: description(f.description.as_ref()),
                    ty: Some(ty),
                });
            }
        }
    } else {
        for t in types.iter().filter(|t| !t.interface) {
            roots.push(RootField {
                name: crate::names::lower_first(&t.name),
                kind: RootKind::Lookup(t.name.clone()),
                description: Some(format!("The {} with this id, or null.", t.name)),
                ty: None,
            });
            roots.push(RootField {
                name: format!("all{}", t.name),
                kind: RootKind::Connection(t.name.clone()),
                description: Some(format!("The members of {}, a page at a time.", t.name)),
                ty: None,
            });
        }
    }
    if !roots.iter().any(|r| matches!(r.kind, RootKind::Node)) {
        if roots.iter().any(|r| r.name == "node") {
            errors.push(err("Query.node is reserved for node(id: ID!): Node", None));
        }
        roots.push(RootField {
            name: "node".into(),
            kind: RootKind::Node,
            description: Some("Any node by its id, as the mapped type it belongs to.".into()),
            ty: None,
        });
    }
    // non-null fields a guard does not back
    for t in &types {
        for f in &t.fields {
            if f.non_null && !f.list && !backing(t.class.as_str(), f.predicate.as_str(), f.inverse)
            {
                warnings.push(format!(
                    "{}.{} is non-null, but no write-time guard in reject mode with a strict baseline requires a value (sh:minCount 1 on its path for the class <{}>): a node without a value makes the field a MISSING_VALUE error at run time",
                    t.name, f.name, t.class
                ));
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Mapping {
        prefixes,
        lang,
        types,
        unions,
        enums,
        roots,
        query_description,
        scalars,
        warnings,
    })
}

/// The IRI of a type, field or enum value: its `@rdf(iri:)`, or the vocabulary's.
type IriOf<'a> = dyn Fn(&str, Option<&ast::Directive>, &str, Option<usize>, &mut Vec<SdlError>) -> Option<NamedNode>
    + 'a;

#[allow(clippy::too_many_arguments)]
fn field_map(
    schema: &Valid<Schema>,
    tname: &str,
    fname: &str,
    f: &Component<FieldDefinition>,
    iri_of: &IriOf,
    is_mapped: &dyn Fn(&str) -> bool,
    enums: &[EnumMap],
    errors: &mut Vec<SdlError>,
) -> Option<FieldMap> {
    let loc = line(schema, f.location());
    let what = format!("{tname}.{fname}");
    let rdf = f.directives.get("rdf").map(|d| -> &ast::Directive { d });
    if fname == "id" {
        if rdf.is_some() {
            errors.push(err(
                format!("{what}: the field id is the node itself and has no predicate"),
                loc,
            ));
        } else if !matches!(f.ty.inner_named_type().as_str(), "ID") || f.ty.is_list() {
            errors.push(err(format!("{what} must have the type ID!"), loc));
        }
        return None;
    }
    if !f.arguments.is_empty() {
        errors.push(err(
            format!("{what} declares arguments: the server generates them"),
            loc,
        ));
        return None;
    }
    let Some((named, list, non_null)) = shape(&f.ty) else {
        errors.push(err(
            format!("{what}: lists of lists are not supported"),
            loc,
        ));
        return None;
    };
    let target = if let Some(s) = Scalar::from_name(named) {
        Target::Scalar(s)
    } else if enums.iter().any(|e| e.name == named) {
        Target::Enum(named.to_string())
    } else if is_mapped(named) || schema.get_union(named).is_some() {
        Target::Object(named.to_string())
    } else {
        errors.push(err(
            format!("{what}: the type {named} is not a mapped type, an enum or a scalar the adapter knows"),
            loc,
        ));
        return None;
    };
    if let Some(d) = rdf
        && str_arg(d, "vocab").is_some()
    {
        errors.push(err(
            format!("{what}: @rdf on a field takes iri and inverse"),
            loc,
        ));
    }
    let predicate = iri_of(fname, rdf, &what, loc, errors)?;
    let inverse = rdf.and_then(|d| bool_arg(d, "inverse")).unwrap_or(false);
    if inverse && !matches!(target, Target::Object(_)) {
        errors.push(err(
            format!("{what}: inverse: true applies to object fields only, since literals cannot be subjects"),
            loc,
        ));
    }
    let lang = f
        .directives
        .get("lang")
        .and_then(|d| str_list_arg(d, "prefer"));
    if lang.is_some() && !matches!(target, Target::Scalar(s) if s.is_text()) {
        errors.push(err(
            format!("{what}: @lang applies to String and LangString fields"),
            loc,
        ));
    }
    let on_many = match f.directives.get("single") {
        Some(d) => {
            if list {
                errors.push(err(
                    format!("{what}: @single applies to single-valued fields"),
                    loc,
                ));
            }
            match d.specified_argument_by_name("onMany").map(|v| v.as_ref()) {
                Some(Value::Enum(e)) if e == "MIN" => OnMany::Min,
                _ => OnMany::Error,
            }
        }
        None => OnMany::Error,
    };
    Some(FieldMap {
        name: fname.to_string(),
        predicate,
        inverse,
        target,
        list,
        non_null,
        lang,
        on_many,
        description: description(f.description.as_ref()),
        ty: f.ty.to_string(),
    })
}
