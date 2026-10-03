//! The kinds of syntax tree nodes. One enum for every language: the SPARQL section first,
//! then Turtle/TriG and JSON-LD. New kinds are only ever appended.

/// The kind of a [`crate::tree::Tree`] node. Its children are nodes and significant
/// tokens; trivia is found by token adjacency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    // ---- SPARQL: units and prologue
    /// the root of a query; its range is the whole input (after a BOM), trivia included
    QueryUnit,
    /// the root of an update request; its range is the whole input, trivia included
    UpdateUnit,
    /// the declarations before a query or before each update operation
    Prologue,
    BaseDecl,
    PrefixDecl,
    VersionDecl,

    // ---- SPARQL: query forms and clauses
    SelectQuery,
    ConstructQuery,
    DescribeQuery,
    AskQuery,
    /// `{ SELECT … }` inside a group
    SubSelect,
    /// `SELECT [DISTINCT|REDUCED] projection`
    SelectClause,
    /// a projected variable or `(expr AS ?v)`
    ProjectionItem,
    /// the `{ … }` of `CONSTRUCT`
    ConstructTemplate,
    /// `DESCRIBE` and its terms (or `*`)
    DescribeClause,
    /// `FROM [NAMED] <g>`
    DatasetClause,
    /// `[WHERE] { … }`
    WhereClause,
    GroupBy,
    GroupCondition,
    Having,
    OrderBy,
    OrderCondition,
    Limit,
    Offset,
    /// the `VALUES` block after a query
    ValuesClause,

    // ---- SPARQL: update operations
    LoadOp,
    ClearOp,
    DropOp,
    CreateOp,
    AddOp,
    MoveOp,
    CopyOp,
    InsertDataOp,
    DeleteDataOp,
    DeleteWhereOp,
    /// `[WITH <g>] DELETE {…} INSERT {…} USING … WHERE {…}`
    ModifyOp,
    WithClause,
    DeleteClause,
    InsertClause,
    UsingClause,
    /// the `{ … }` of quad data and quad templates
    QuadPattern,
    /// `GRAPH g { … }` inside a quad pattern
    QuadsGraph,

    // ---- SPARQL: group graph patterns
    GroupGraphPattern,
    /// one subject with its property list, and its `.`
    TriplesStmt,
    /// a verb (or path) with its objects, and its `;`
    PropertyListEntry,
    /// one object with its reifiers and annotations, and its `,`
    Object,
    Optional,
    /// `LATERAL { … }` (Jena ARQ)
    Lateral,
    Minus,
    /// a chain of `{…} UNION {…}`
    Union,
    UnionBranch,
    /// `GRAPH g { … }`
    GraphPattern,
    Service,
    Filter,
    Bind,
    /// `VALUES` inside a group
    InlineValues,
    /// `( v₁ v₂ )` of a multi-variable `VALUES`
    ValuesRow,
    DataValue,

    // ---- SPARQL: RDF terms (RDF 1.2 included)
    /// `[ p o ; … ]`
    BNodePropertyList,
    /// `( a b c )`
    Collection,
    CollectionItem,
    /// `<< s p o ~ r >>`
    ReifiedTriple,
    /// `<<( s p o )>>`
    TripleTerm,
    /// `~ r` (or a lone `~`)
    Reifier,
    /// `{| … |}`
    AnnotationBlock,
    /// a string with its language tag or `^^` datatype
    Literal,

    // ---- SPARQL: property paths
    PathAlternative,
    PathSequence,
    /// a path primary with its `?`, `*` or `+`, or ARQ's range (`{2}`, `{1,3}`, `{2,}`,
    /// `{,3}`, `{*}`, `{+}`)
    PathElt,
    /// `^elt`
    PathInverse,
    /// `!p` or `!( … )`
    PathNegated,
    /// `( path )`
    PathBracketed,

    // ---- SPARQL: expressions
    /// a maximal `||` chain
    OrChain,
    /// a maximal `&&` chain
    AndChain,
    /// one operand of an `||`/`&&` chain, with the operator after it
    ChainOperand,
    /// a relational, additive or multiplicative operation
    Binary,
    /// `!e`, `+e`, `-e`
    Unary,
    /// `( expr )`
    Bracketed,
    /// a built-in call or `iri(args)`
    Call,
    ArgList,
    Arg,
    Aggregate,
    /// `e IN (…)` or `e NOT IN (…)`
    InList,
    Exists,
    NotExists,

    // ---- shared
    /// a span kept exactly as written (also what unfinished parsers produce)
    Opaque,

    // ---- Turtle and TriG (directives, statements, entries, objects and terms use the
    // SPARQL kinds above)
    /// the root of a Turtle document; its range is the whole input, trivia included
    TurtleDoc,
    /// the root of a TriG document; its range is the whole input, trivia included
    TrigDoc,
    /// TriG's `GRAPH g { … }`, `g { … }` or `{ … }`
    GraphBlock,

    // ---- JSON-LD
    /// the root of a JSON document; its range is the whole input, trivia included
    JsonDocument,
    /// `{ … }`
    JsonObject,
    /// a key, its `:`, its value, and the `,` after it
    JsonMember,
    /// `[ … ]` with its values and their `,`
    JsonArray,
    /// a string, number, `true`, `false` or `null`
    JsonScalar,

    // ---- SPARQL: Jena ARQ's group elements
    /// `LET (?v := expr)`
    Let,
    /// `UNFOLD(expr AS ?v)` or `UNFOLD(expr AS ?v, ?w)`
    Unfold,
    /// `SEMIJOIN { … }`
    SemiJoin,
    /// `ANTIJOIN { … }`
    AntiJoin,
    /// `distinct(path)`, `multi(path)` or `shortest(path)`
    PathFunction,
}
