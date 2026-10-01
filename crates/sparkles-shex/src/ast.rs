//! The schema AST. It mirrors ShExJ (the JSON-LD form of ShEx 2.1), so ShExC → AST →
//! ShExJ → AST → ShExC round trips keep everything but comments: annotations and
//! semantic actions included.
//!
//! Fields that ShExJ may leave out are `Option`s, so a schema read from ShExJ writes
//! back the same keys. Lists that ShExJ may leave out are empty `Vec`s when absent;
//! [`NodeConstraint::values`] is the exception, because an empty value set (`[]`)
//! matches nothing while an absent one matches anything. IRIs are absolute strings
//! (ShExC prefixed names and relative IRIs are resolved by the parser); literals keep
//! their lexical forms.

/// A schema (ShExJ `Schema`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    /// the base IRI the schema was read with (`BASE`, or the location of the text)
    pub base: Option<String>,
    /// `PREFIX` declarations, in order; kept for writing ShExC and for shape maps
    pub prefixes: crate::PrefixMap,
    /// `IMPORT` IRIs, in order
    pub imports: Vec<String>,
    /// `start = …`
    pub start: Option<ShapeExpr>,
    /// semantic actions run once per validation (`%iri{ … %}` before the shapes)
    pub start_acts: Vec<SemAct>,
    /// shape declarations, in order
    pub shapes: Vec<ShapeDecl>,
}

/// The label of a shape or triple-expression declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Label {
    Iri(String),
    /// a blank-node label, without `_:`
    BNode(String),
}

impl Label {
    /// The ShExJ form: the IRI, or `_:label`.
    pub fn to_shexj(&self) -> String {
        match self {
            Label::Iri(i) => i.clone(),
            Label::BNode(b) => format!("_:{b}"),
        }
    }

    /// From the ShExJ form (`_:label` is a blank node, anything else an IRI).
    pub fn from_shexj(s: &str) -> Label {
        match s.strip_prefix("_:") {
            Some(b) => Label::BNode(b.to_string()),
            None => Label::Iri(s.to_string()),
        }
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Label::Iri(i) => write!(f, "<{i}>"),
            Label::BNode(b) => write!(f, "_:{b}"),
        }
    }
}

/// A labelled shape expression (an element of ShExJ `shapes`, with its `id`).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeDecl {
    pub label: Label,
    pub expr: ShapeExpr,
}

/// A shape expression (ShExJ `shapeExpr`).
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeExpr {
    /// `ShapeOr`
    Or(Vec<ShapeExpr>),
    /// `ShapeAnd`
    And(Vec<ShapeExpr>),
    /// `ShapeNot`
    Not(Box<ShapeExpr>),
    /// `NodeConstraint`
    Nc(Box<NodeConstraint>),
    /// `Shape`
    Shape(Box<Shape>),
    /// `ShapeExternal`: defined outside the schema (see [`crate::Resolver::external`])
    External,
    /// a reference to a declared shape (`@<label>`, or a label string in ShExJ)
    Ref(Label),
}

/// `nodeKind` of a node constraint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Iri,
    BNode,
    NonLiteral,
    Literal,
}

impl NodeKind {
    /// The ShExJ value (`iri`, `bnode`, `nonliteral`, `literal`).
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Iri => "iri",
            NodeKind::BNode => "bnode",
            NodeKind::NonLiteral => "nonliteral",
            NodeKind::Literal => "literal",
        }
    }
}

/// A numeric facet bound, with its lexical form as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumericLiteral {
    Integer(String),
    Decimal(String),
    Double(String),
}

/// A node constraint (ShExJ `NodeConstraint`): kind, datatype, facets and value set.
/// Every part that is present must hold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeConstraint {
    pub node_kind: Option<NodeKind>,
    /// datatype IRI
    pub datatype: Option<String>,
    pub length: Option<u64>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    /// the regular expression, with the ShExC escapes (`\/`, `\uXXXX`) undone
    pub pattern: Option<String>,
    /// flags of `pattern` (`s m i x q`)
    pub flags: Option<String>,
    pub min_inclusive: Option<NumericLiteral>,
    pub min_exclusive: Option<NumericLiteral>,
    pub max_inclusive: Option<NumericLiteral>,
    pub max_exclusive: Option<NumericLiteral>,
    pub total_digits: Option<u64>,
    pub fraction_digits: Option<u64>,
    /// `[ … ]`; `Some(vec![])` is the empty value set
    pub values: Option<Vec<ValueSetValue>>,
}

/// A literal in a value set or an annotation (ShExJ `ObjectLiteral`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectLiteral {
    pub value: String,
    pub language: Option<String>,
    /// datatype IRI (`None`: a simple literal, or a language-tagged one)
    pub datatype: Option<String>,
}

/// An IRI or a literal (ShExJ `objectValue`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ObjectValue {
    Iri(String),
    Literal(ObjectLiteral),
}

/// The stem of a stem range: a value, or the wildcard `.`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stem {
    Value(String),
    Wildcard,
}

/// An exclusion of a stem range: a value or a stem (an IRI, a literal's lexical form or
/// a language tag, by the kind of the range).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exclusion {
    Value(String),
    Stem(String),
}

/// A value-set value (ShExJ `valueSetValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueSetValue {
    /// an IRI or a literal, matched by RDF term equality
    Object(ObjectValue),
    /// `<iri>~`
    IriStem(String),
    /// `<iri>~ - …` or `. - <iri> …`
    IriStemRange {
        stem: Stem,
        exclusions: Vec<Exclusion>,
    },
    /// `"lex"~`
    LiteralStem(String),
    /// `"lex"~ - …` or `. - "lex" …`
    LiteralStemRange {
        stem: Stem,
        exclusions: Vec<Exclusion>,
    },
    /// `@en`
    Language(String),
    /// `@en~`; the empty stem (`@~`) matches every language-tagged literal
    LanguageStem(String),
    /// `@en~ - …` or `. - @en …`
    LanguageStemRange {
        stem: Stem,
        exclusions: Vec<Exclusion>,
    },
}

/// A shape (ShExJ `Shape`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    pub closed: Option<bool>,
    /// `EXTRA` predicates
    pub extra: Vec<String>,
    pub expression: Option<TripleExpr>,
    pub sem_acts: Vec<SemAct>,
    pub annotations: Vec<Annotation>,
}

impl Shape {
    pub fn is_closed(&self) -> bool {
        self.closed == Some(true)
    }
}

/// A triple expression (ShExJ `tripleExpr`).
#[derive(Clone, Debug, PartialEq)]
pub enum TripleExpr {
    /// `;`-separated
    EachOf(Group),
    /// `|`-separated
    OneOf(Group),
    Tc(TripleConstraint),
    /// `&label`: the labelled triple expression, in place
    Include(Label),
}

/// The body of an `EachOf` or a `OneOf`.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// `$label`
    pub id: Option<Label>,
    pub exprs: Vec<TripleExpr>,
    pub min: Option<u32>,
    /// `-1`: unbounded
    pub max: Option<i64>,
    pub sem_acts: Vec<SemAct>,
    pub annotations: Vec<Annotation>,
}

/// A triple constraint (ShExJ `TripleConstraint`).
#[derive(Clone, Debug, PartialEq)]
pub struct TripleConstraint {
    /// `$label`
    pub id: Option<Label>,
    /// `^p`
    pub inverse: Option<bool>,
    /// predicate IRI
    pub predicate: String,
    /// `None`: any value (`.`)
    pub value_expr: Option<Box<ShapeExpr>>,
    pub min: Option<u32>,
    /// `-1`: unbounded
    pub max: Option<i64>,
    pub sem_acts: Vec<SemAct>,
    pub annotations: Vec<Annotation>,
}

impl TripleConstraint {
    pub fn is_inverse(&self) -> bool {
        self.inverse == Some(true)
    }
}

/// The cardinality of a triple expression with ShEx's defaults (`{1,1}`): `(min,
/// max)`, `None` for an unbounded maximum.
pub fn cardinality(min: Option<u32>, max: Option<i64>) -> (u32, Option<u32>) {
    let min = min.unwrap_or(1);
    let max = match max {
        None => Some(1),
        Some(m) if m < 0 => None,
        Some(m) => Some(u32::try_from(m).unwrap_or(u32::MAX)),
    };
    (min, max)
}

/// A semantic action (`%<extension>{ code %}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemAct {
    /// extension IRI
    pub name: String,
    /// `None` for `%<iri>%`
    pub code: Option<String>,
}

/// An annotation (`// <p> object`); it never affects validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub predicate: String,
    pub object: ObjectValue,
}
