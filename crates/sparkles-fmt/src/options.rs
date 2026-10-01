//! The style keys shared by every front end: `.sparklesfmt.toml` (kebab-case), the CLI
//! flags, and the HTTP options (camelCase). Each value goes through [`set`], so the three
//! accept and reject exactly the same things.

use crate::{DirectiveStyle, OperatorPosition, Options, QuoteStyle, TurtleLayout};
use std::ops::RangeInclusive;

/// A configuration value before it is checked against its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Str(String),
    /// `prefix-groups`: arrays of prefix labels (`""` is the empty label)
    Groups(Vec<Vec<String>>),
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "a boolean",
            Value::Int(_) => "an integer",
            Value::Str(_) => "a string",
            Value::Groups(_) => "an array of arrays",
        }
    }
}

/// Every key, `(kebab-case, camelCase)`: the config file spelling and the HTTP spelling.
pub const KEYS: &[(&str, &str)] = &[
    ("line-width", "lineWidth"),
    ("indent-width", "indentWidth"),
    ("sort", "sort"),
    ("prune-prefixes", "prunePrefixes"),
    ("directive-style", "directiveStyle"),
    ("prefix-groups", "prefixGroups"),
    ("type-shorthand", "typeShorthand"),
    ("compact-iris", "compactIris"),
    ("quote-style", "quoteStyle"),
    ("operator-position", "operatorPosition"),
    ("turtle-layout", "turtleLayout"),
    ("align-values", "alignValues"),
];

/// `line-width`
pub const LINE_WIDTH: RangeInclusive<i64> = 40..=400;
/// `indent-width`
pub const INDENT_WIDTH: RangeInclusive<i64> = 1..=8;

/// A bad key or value. `key` is the kebab-case name (or the unknown name as given).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {message}")]
pub struct OptionError {
    pub key: String,
    pub message: String,
}

/// The kebab-case key of a kebab-case or camelCase name.
pub fn kebab(name: &str) -> Option<&'static str> {
    KEYS.iter()
        .find(|(k, c)| *k == name || *c == name)
        .map(|(k, _)| *k)
}

/// The camelCase spelling of a kebab-case key.
pub fn camel(kebab_key: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _)| *k == kebab_key).map(|(_, c)| *c)
}

/// Set one key (kebab-case) from a value, checking its type, range or enumeration.
pub fn set(o: &mut Options, kebab_key: &str, v: Value) -> Result<(), OptionError> {
    let err = |message: String| OptionError {
        key: kebab_key.to_string(),
        message,
    };
    match kebab_key {
        "line-width" => o.line_width = int(&v, LINE_WIDTH).map_err(err)? as u16,
        "indent-width" => o.indent_width = int(&v, INDENT_WIDTH).map_err(err)? as u8,
        "sort" => o.sort = boolean(&v).map_err(err)?,
        "prune-prefixes" => o.prune_prefixes = boolean(&v).map_err(err)?,
        "type-shorthand" => o.type_shorthand = boolean(&v).map_err(err)?,
        "compact-iris" => o.compact_iris = boolean(&v).map_err(err)?,
        "align-values" => o.align_values = boolean(&v).map_err(err)?,
        "directive-style" => {
            o.directive_style = match choice(&v, &["sparql", "turtle"]).map_err(err)? {
                0 => DirectiveStyle::Sparql,
                _ => DirectiveStyle::Turtle,
            }
        }
        "quote-style" => {
            o.quote_style = match choice(&v, &["double", "preserve"]).map_err(err)? {
                0 => QuoteStyle::Double,
                _ => QuoteStyle::Preserve,
            }
        }
        "operator-position" => {
            o.operator_position = match choice(&v, &["leading", "trailing"]).map_err(err)? {
                0 => OperatorPosition::Leading,
                _ => OperatorPosition::Trailing,
            }
        }
        "turtle-layout" => {
            o.turtle_layout = match choice(&v, &["diff", "conventional"]).map_err(err)? {
                0 => TurtleLayout::Diff,
                _ => TurtleLayout::Conventional,
            }
        }
        "prefix-groups" => {
            let Value::Groups(g) = v else {
                return Err(err(format!(
                    "expected an array of arrays of prefix labels, got {}",
                    v.type_name()
                )));
            };
            check_groups(&g).map_err(err)?;
            o.prefix_groups = g;
        }
        _ => return Err(err("unknown option".to_string())),
    }
    Ok(())
}

/// The value of one key (kebab-case), in the form [`set`] takes.
pub fn get(o: &Options, kebab_key: &str) -> Option<Value> {
    Some(match kebab_key {
        "line-width" => Value::Int(o.line_width.into()),
        "indent-width" => Value::Int(o.indent_width.into()),
        "sort" => Value::Bool(o.sort),
        "prune-prefixes" => Value::Bool(o.prune_prefixes),
        "type-shorthand" => Value::Bool(o.type_shorthand),
        "compact-iris" => Value::Bool(o.compact_iris),
        "align-values" => Value::Bool(o.align_values),
        "directive-style" => Value::Str(
            match o.directive_style {
                DirectiveStyle::Sparql => "sparql",
                DirectiveStyle::Turtle => "turtle",
            }
            .into(),
        ),
        "quote-style" => Value::Str(
            match o.quote_style {
                QuoteStyle::Double => "double",
                QuoteStyle::Preserve => "preserve",
            }
            .into(),
        ),
        "operator-position" => Value::Str(
            match o.operator_position {
                OperatorPosition::Leading => "leading",
                OperatorPosition::Trailing => "trailing",
            }
            .into(),
        ),
        "turtle-layout" => Value::Str(
            match o.turtle_layout {
                TurtleLayout::Diff => "diff",
                TurtleLayout::Conventional => "conventional",
            }
            .into(),
        ),
        "prefix-groups" => Value::Groups(o.prefix_groups.clone()),
        _ => return None,
    })
}

/// Check the ranges and the prefix groups of options built without [`set`].
pub fn validate(o: &Options) -> Result<(), OptionError> {
    let err = |key: &str, message: String| OptionError {
        key: key.to_string(),
        message,
    };
    int(&Value::Int(o.line_width.into()), LINE_WIDTH).map_err(|m| err("line-width", m))?;
    int(&Value::Int(o.indent_width.into()), INDENT_WIDTH).map_err(|m| err("indent-width", m))?;
    check_groups(&o.prefix_groups).map_err(|m| err("prefix-groups", m))
}

fn int(v: &Value, range: RangeInclusive<i64>) -> Result<i64, String> {
    match v {
        Value::Int(i) if range.contains(i) => Ok(*i),
        Value::Int(i) => Err(format!(
            "expected an integer from {} to {}, got {i}",
            range.start(),
            range.end()
        )),
        _ => Err(format!("expected an integer, got {}", v.type_name())),
    }
}

fn boolean(v: &Value) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(format!("expected true or false, got {}", v.type_name())),
    }
}

/// The index of the value among `names` (exact lowercase strings).
fn choice(v: &Value, names: &[&str]) -> Result<usize, String> {
    let expected = names
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(" or ");
    match v {
        Value::Str(s) => names
            .iter()
            .position(|n| n == s)
            .ok_or_else(|| format!("expected {expected}, got \"{s}\"")),
        _ => Err(format!("expected {expected}, got {}", v.type_name())),
    }
}

/// Every label is a `PN_PREFIX` or `""`, no label appears twice, no group is empty.
fn check_groups(groups: &[Vec<String>]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for (i, g) in groups.iter().enumerate() {
        if g.is_empty() {
            return Err(format!("group {} is empty", i + 1));
        }
        for label in g {
            if !label.is_empty() && !crate::lex::is_pn_prefix(label) {
                return Err(format!("\"{label}\" is not a prefix label"));
            }
            if !seen.insert(label.as_str()) {
                return Err(format!("\"{label}\" is in more than one place"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(g: &[&[&str]]) -> Value {
        Value::Groups(
            g.iter()
                .map(|g| g.iter().map(|s| s.to_string()).collect())
                .collect(),
        )
    }

    fn set_err(key: &str, v: Value) -> String {
        set(&mut Options::default(), key, v)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn twelve_keys_both_spellings() {
        assert_eq!(KEYS.len(), 12);
        for (k, c) in KEYS {
            assert_eq!(kebab(k), Some(*k));
            assert_eq!(kebab(c), Some(*k));
            assert_eq!(camel(k), Some(*c));
            // every key reads back what it was set to
            let o = Options::default();
            let v = get(&o, k).unwrap();
            let mut o2 = Options::default();
            set(&mut o2, k, v).unwrap();
            assert_eq!(o, o2);
        }
        assert_eq!(kebab("line_width"), None);
        assert_eq!(kebab("canonicalize"), None);
    }

    #[test]
    fn defaults() {
        let o = Options::default();
        assert_eq!(o.line_width, 100);
        assert_eq!(o.indent_width, 2);
        assert!(!o.sort && !o.prune_prefixes && !o.canonicalize && !o.align_values);
        assert!(o.type_shorthand && o.compact_iris);
        assert_eq!(o.directive_style, DirectiveStyle::Sparql);
        assert_eq!(o.quote_style, QuoteStyle::Double);
        assert_eq!(o.operator_position, OperatorPosition::Leading);
        assert_eq!(o.turtle_layout, TurtleLayout::Diff);
        assert!(o.prefix_groups.is_empty());
        validate(&o).unwrap();
    }

    #[test]
    fn sets_every_key() {
        let mut o = Options::default();
        set(&mut o, "line-width", Value::Int(40)).unwrap();
        set(&mut o, "indent-width", Value::Int(8)).unwrap();
        for (k, b) in [
            ("sort", true),
            ("prune-prefixes", false),
            ("type-shorthand", true),
            ("compact-iris", false),
            ("align-values", true),
        ] {
            set(&mut o, k, Value::Bool(b)).unwrap();
        }
        set(&mut o, "directive-style", Value::Str("turtle".into())).unwrap();
        set(&mut o, "quote-style", Value::Str("preserve".into())).unwrap();
        set(&mut o, "operator-position", Value::Str("trailing".into())).unwrap();
        set(&mut o, "turtle-layout", Value::Str("conventional".into())).unwrap();
        set(
            &mut o,
            "prefix-groups",
            groups(&[&["rdf", "rdfs", "xsd", "owl"], &["", "ex"]]),
        )
        .unwrap();
        assert_eq!(o.line_width, 40);
        assert_eq!(o.indent_width, 8);
        assert!(o.sort && !o.prune_prefixes && o.type_shorthand && !o.compact_iris);
        assert!(o.align_values);
        assert_eq!(o.directive_style, DirectiveStyle::Turtle);
        assert_eq!(o.quote_style, QuoteStyle::Preserve);
        assert_eq!(o.operator_position, OperatorPosition::Trailing);
        assert_eq!(o.turtle_layout, TurtleLayout::Conventional);
        assert_eq!(o.prefix_groups[1], vec!["".to_string(), "ex".to_string()]);
        validate(&o).unwrap();
    }

    #[test]
    fn rejects_types_ranges_and_enumerations() {
        assert_eq!(
            set_err("line-width", Value::Int(39)),
            "line-width: expected an integer from 40 to 400, got 39"
        );
        assert!(set(&mut Options::default(), "line-width", Value::Int(400)).is_ok());
        assert_eq!(
            set_err("line-width", Value::Int(401)),
            "line-width: expected an integer from 40 to 400, got 401"
        );
        assert_eq!(
            set_err("indent-width", Value::Int(0)),
            "indent-width: expected an integer from 1 to 8, got 0"
        );
        assert_eq!(
            set_err("line-width", Value::Str("100".into())),
            "line-width: expected an integer, got a string"
        );
        assert_eq!(
            set_err("sort", Value::Str("true".into())),
            "sort: expected true or false, got a string"
        );
        assert_eq!(
            set_err("quote-style", Value::Str("Double".into())),
            "quote-style: expected \"double\" or \"preserve\", got \"Double\""
        );
        assert_eq!(
            set_err("operator-position", Value::Bool(true)),
            "operator-position: expected \"leading\" or \"trailing\", got a boolean"
        );
        assert_eq!(
            set_err("prefix-groups", Value::Str("rdf".into())),
            "prefix-groups: expected an array of arrays of prefix labels, got a string"
        );
        assert_eq!(
            set_err("line-widht", Value::Int(1)),
            "line-widht: unknown option"
        );
        // camelCase is an HTTP spelling, mapped by `kebab` first
        assert_eq!(
            set_err("lineWidth", Value::Int(80)),
            "lineWidth: unknown option"
        );
    }

    #[test]
    fn checks_prefix_groups() {
        let ok = |g: &[&[&str]]| set(&mut Options::default(), "prefix-groups", groups(g));
        assert!(ok(&[]).is_ok());
        assert!(ok(&[&["rdf"], &["", "ex", "a.b", "é-1"]]).is_ok());
        assert_eq!(
            ok(&[&["rdf"], &[]]).unwrap_err().message,
            "group 2 is empty"
        );
        assert_eq!(
            ok(&[&["rdf", "rdfs"], &["rdf"]]).unwrap_err().message,
            "\"rdf\" is in more than one place"
        );
        assert_eq!(
            ok(&[&["", ""]]).unwrap_err().message,
            "\"\" is in more than one place"
        );
        for bad in ["ex:", "1ex", "_x", "a.", "-a", "a b"] {
            assert_eq!(
                ok(&[&[bad]]).unwrap_err().message,
                format!("\"{bad}\" is not a prefix label")
            );
        }
        let o = Options {
            prefix_groups: vec![vec!["x".into()], vec!["x".into()]],
            ..Options::default()
        };
        assert_eq!(validate(&o).unwrap_err().key, "prefix-groups");
        let o = Options {
            line_width: 20,
            ..Options::default()
        };
        assert_eq!(validate(&o).unwrap_err().key, "line-width");
    }
}
