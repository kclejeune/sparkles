//! SPARQL 1.2 keywords and built-in names. They match case-insensitively, except `a`
//! (SPARQL 1.1 §19.8 grammar notes; 1.2 §4.2.4), and print in the spelling of the
//! grammar's terminals (§19.7): `SELECT`, `sameTerm`, `isIRI`, `hasLANGDIR`, `a`, `true`.

macro_rules! keywords {
    ($($variant:ident => $spelling:literal,)*) => {
        /// A keyword or a built-in function name.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Kw {
            $($variant,)*
        }

        impl Kw {
            /// Every keyword, in declaration order.
            pub const ALL: &[Kw] = &[$(Kw::$variant,)*];

            /// The grammar's spelling, which the formatter prints.
            pub fn canonical(self) -> &'static str {
                match self {
                    $(Kw::$variant => $spelling,)*
                }
            }
        }
    };
}

keywords! {
    // prologue and query forms
    Base => "BASE",
    Prefix => "PREFIX",
    Version => "VERSION",
    Select => "SELECT",
    Distinct => "DISTINCT",
    Reduced => "REDUCED",
    As => "AS",
    Construct => "CONSTRUCT",
    Where => "WHERE",
    Describe => "DESCRIBE",
    Ask => "ASK",
    From => "FROM",
    Named => "NAMED",
    // solution modifiers
    Group => "GROUP",
    By => "BY",
    Having => "HAVING",
    Order => "ORDER",
    Asc => "ASC",
    Desc => "DESC",
    Limit => "LIMIT",
    Offset => "OFFSET",
    Values => "VALUES",
    Undef => "UNDEF",
    // graph patterns
    Optional => "OPTIONAL",
    Minus => "MINUS",
    // Jena ARQ's lateral join, assignment, UNFOLD, half joins and path functions
    Lateral => "LATERAL",
    Let => "LET",
    Unfold => "UNFOLD",
    Semijoin => "SEMIJOIN",
    Antijoin => "ANTIJOIN",
    Multi => "MULTI",
    Shortest => "SHORTEST",
    Union => "UNION",
    Graph => "GRAPH",
    Service => "SERVICE",
    Silent => "SILENT",
    Filter => "FILTER",
    Bind => "BIND",
    Not => "NOT",
    In => "IN",
    Exists => "EXISTS",
    Separator => "SEPARATOR",
    // updates
    Load => "LOAD",
    Into => "INTO",
    Clear => "CLEAR",
    Drop => "DROP",
    Create => "CREATE",
    Add => "ADD",
    Move => "MOVE",
    Copy => "COPY",
    To => "TO",
    Insert => "INSERT",
    Delete => "DELETE",
    Data => "DATA",
    With => "WITH",
    Using => "USING",
    Default => "DEFAULT",
    All => "ALL",
    // case-sensitive or lowercase terminals
    A => "a",
    True => "true",
    False => "false",
    // built-in calls
    Str => "STR",
    Lang => "LANG",
    Langmatches => "LANGMATCHES",
    Langdir => "LANGDIR",
    Datatype => "DATATYPE",
    Bound => "BOUND",
    Iri => "IRI",
    Uri => "URI",
    Bnode => "BNODE",
    Rand => "RAND",
    Abs => "ABS",
    Ceil => "CEIL",
    Floor => "FLOOR",
    Round => "ROUND",
    Concat => "CONCAT",
    Strlen => "STRLEN",
    Ucase => "UCASE",
    Lcase => "LCASE",
    EncodeForUri => "ENCODE_FOR_URI",
    Contains => "CONTAINS",
    Strstarts => "STRSTARTS",
    Strends => "STRENDS",
    Strbefore => "STRBEFORE",
    Strafter => "STRAFTER",
    Year => "YEAR",
    Month => "MONTH",
    Day => "DAY",
    Hours => "HOURS",
    Minutes => "MINUTES",
    Seconds => "SECONDS",
    Timezone => "TIMEZONE",
    Tz => "TZ",
    Now => "NOW",
    Uuid => "UUID",
    Struuid => "STRUUID",
    Md5 => "MD5",
    Sha1 => "SHA1",
    Sha256 => "SHA256",
    Sha384 => "SHA384",
    Sha512 => "SHA512",
    Coalesce => "COALESCE",
    If => "IF",
    Strlang => "STRLANG",
    Strlangdir => "STRLANGDIR",
    Strdt => "STRDT",
    SameTerm => "sameTerm",
    IsIri => "isIRI",
    IsUri => "isURI",
    IsBlank => "isBLANK",
    IsLiteral => "isLITERAL",
    IsNumeric => "isNUMERIC",
    HasLang => "hasLANG",
    HasLangdir => "hasLANGDIR",
    Regex => "REGEX",
    Substr => "SUBSTR",
    Replace => "REPLACE",
    IsTriple => "isTRIPLE",
    Triple => "TRIPLE",
    Subject => "SUBJECT",
    Predicate => "PREDICATE",
    Object => "OBJECT",
    // aggregates
    Count => "COUNT",
    Sum => "SUM",
    Min => "MIN",
    Max => "MAX",
    Avg => "AVG",
    Sample => "SAMPLE",
    GroupConcat => "GROUP_CONCAT",
    // Jena ARQ's aggregates
    Median => "MEDIAN",
    Mode => "MODE",
    Stdev => "STDEV",
    StdevSamp => "STDEV_SAMP",
    StdevPop => "STDEV_POP",
    Variance => "VARIANCE",
    VarSamp => "VAR_SAMP",
    VarPop => "VAR_POP",
    // ARQ's `AGG <iri>(…)`, a custom aggregate by its IRI
    Agg => "AGG",
    // ARQ's FOLD into a cdt:List or cdt:Map literal
    Fold => "FOLD",
}

impl Kw {
    /// The keyword a word spells: ASCII case-insensitive, except `a`, which is
    /// case-sensitive.
    pub fn from_word(word: &str) -> Option<Kw> {
        if word == "a" {
            return Some(Kw::A);
        }
        if word.len() > 14 || word.eq_ignore_ascii_case("a") {
            return None;
        }
        Kw::ALL
            .iter()
            .copied()
            .find(|k| k.canonical().eq_ignore_ascii_case(word))
    }

    /// Whether this is a built-in function name (`BuiltInCall`, aggregates included).
    pub fn is_builtin(self) -> bool {
        self >= Kw::Str
    }

    /// Whether this is an aggregate (`COUNT` … `GROUP_CONCAT`, and ARQ's `MEDIAN` …
    /// `VAR_POP` and `AGG`).
    pub fn is_aggregate(self) -> bool {
        self >= Kw::Count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_spellings_round_trip() {
        for &k in Kw::ALL {
            assert_eq!(Kw::from_word(k.canonical()), Some(k), "{k:?}");
            if k != Kw::A {
                assert_eq!(Kw::from_word(&k.canonical().to_lowercase()), Some(k));
                assert_eq!(Kw::from_word(&k.canonical().to_uppercase()), Some(k));
            }
        }
        assert_eq!(Kw::from_word("A"), None);
        assert_eq!(Kw::from_word("select"), Some(Kw::Select));
        assert_eq!(
            Kw::from_word("SAMETERM").map(Kw::canonical),
            Some("sameTerm")
        );
        assert_eq!(
            Kw::from_word("haslangdir").map(Kw::canonical),
            Some("hasLANGDIR")
        );
        assert_eq!(Kw::from_word("TRUE").map(Kw::canonical), Some("true"));
        assert_eq!(Kw::from_word("lateral"), Some(Kw::Lateral));
        assert_eq!(Kw::from_word("unfold"), Some(Kw::Unfold));
        assert_eq!(Kw::from_word("UNFOLDS"), None);
        assert!(Kw::Fold.is_aggregate() && !Kw::Let.is_builtin());
        assert!(Kw::Str.is_builtin() && Kw::GroupConcat.is_builtin());
        assert!(!Kw::A.is_builtin() && !Kw::Select.is_builtin());
        assert!(Kw::Count.is_aggregate() && !Kw::Object.is_aggregate());
    }
}
