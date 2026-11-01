//! Repair turns of the asking pipeline that look at the data or the result after a
//! draft (C18 §4.2): the exploration of a question whose draft found nothing, the
//! check of a result's shape against the form of the question, and the vote of
//! several first drafts by their results. Each piece is a plain function of what the
//! pipeline already holds, so that a later tool loop can call the same pieces.

use serde_json::Value;

/// The opening of an exploration turn that follows a draft without a query.
pub(super) const NO_QUERY: &str =
    "Your previous answer gave no query and said that the data cannot answer the question.";

/// The repair section of a draft prompt after an exploration: the failed query, or
/// [`NO_QUERY`] when the draft gave none, and what the exploration found.
pub(super) fn explore_text(query: Option<&str>, found: &str) -> String {
    let head = match query {
        Some(q) => format!(
            "Your previous query was:\n```sparql\n{}\n```\nIt returned no rows.",
            q.trim()
        ),
        None => NO_QUERY.to_string(),
    };
    format!(
        "{head} This is more of the data around the question's entities, explored after that answer:\n{}\nWrite the query again with these terms. A relation may run in the other direction than the question's wording suggests, and a sibling predicate may hold it. Give an empty query only if the data still cannot answer the question.",
        super::prompts::data_block(found)
    )
}

/// The repair section of a draft prompt when a result's shape does not fit the
/// question: the query, the hint and, when the repair pair may receive rows, the first
/// rows it returned.
pub(super) fn shape_text(query: &str, hint: &str, rows: Option<&str>) -> String {
    let rows = rows
        .map(|r| format!(" Its first rows:\n{}\n", super::prompts::data_block(r)))
        .unwrap_or_else(|| "\n".into());
    format!(
        "Your previous query was:\n```sparql\n{}\n```\nIts result does not fit the question. {hint}{rows}Write a corrected query.",
        query.trim(),
    )
}

/// One line of an exploration: a predicate with its count and, with `samples`, a
/// sample value.
pub(super) fn usage_line(
    subject: &str,
    rows: &Value,
    prefixes: &serde_json::Map<String, Value>,
    max: usize,
    samples: bool,
) -> Option<String> {
    let result_prefixes = &rows["prefixes"];
    let items: Vec<String> = rows["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let p = super::grounding::result_iri(row.get(0)?, result_prefixes)?;
            let n = super::grounding::result_count(row.get(1)?)?;
            let sample = row
                .get(2)
                .filter(|_| samples)
                .and_then(Value::as_str)
                .map(|s| format!(", such as {}", super::prompts::cut(s, 80)))
                .unwrap_or_default();
            Some(format!(
                "{} ({n}{sample})",
                super::grounding::compact(&p, prefixes)
            ))
        })
        .take(max)
        .collect();
    (!items.is_empty()).then(|| format!("{subject}: {}\n", items.join("; ")))
}

/// The words around a mention in the question: up to three words before it and two
/// after it, which often name what the mention is, as in "the film X" or "X river".
pub(super) fn mention_context(question: &str, mention: &str) -> String {
    let Some(at) = question.find(mention) else {
        return String::new();
    };
    let before: Vec<&str> = question[..at].split_whitespace().collect();
    let after: Vec<&str> = question[at + mention.len()..].split_whitespace().collect();
    let start = before.len().saturating_sub(3);
    let mut words: Vec<&str> = before[start..].to_vec();
    words.extend(after.iter().take(2));
    words.join(" ")
}

/// The kind of answer the question's form asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Form {
    /// "How many …": one number
    Count,
    /// "Is …", "Did …": yes or no
    Boolean,
    /// "When …": a date or a year
    Date,
    /// "Which …", "Who …", "List …": the things themselves
    Things,
    Other,
}

const AUXILIARIES: [&str; 14] = [
    "is", "are", "was", "were", "do", "does", "did", "can", "could", "has", "have", "had", "will",
    "would",
];
const THINGS: [&str; 7] = ["which", "who", "whom", "list", "give", "name", "show"];

/// The form of an English question, from its first words.
pub(super) fn form(question: &str) -> Form {
    let q = question.trim().to_lowercase();
    let words: Vec<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    match words.as_slice() {
        ["how", "many" | "much", ..] => Form::Count,
        ["when", ..] => Form::Date,
        [w, ..] if AUXILIARIES.contains(w) => Form::Boolean,
        [w, ..] if THINGS.contains(w) => Form::Things,
        _ => Form::Other,
    }
}

/// Whether a compact result cell holds a number.
fn numeric(cell: &str) -> bool {
    let lexical = cell
        .strip_prefix('"')
        .and_then(|t| t.split_once('"').map(|(v, _)| v))
        .unwrap_or(cell);
    !lexical.is_empty() && lexical.parse::<f64>().is_ok()
}

/// Whether a compact result cell holds a date, a time or a year.
fn datelike(cell: &str) -> bool {
    const KINDS: [&str; 5] = ["#date", "#dateTime", "#gYear", "#gYearMonth", ":date"];
    let lexical = cell
        .strip_prefix('"')
        .and_then(|t| t.split_once('"').map(|(v, _)| v))
        .unwrap_or(cell);
    KINDS.iter().any(|k| cell.contains(k))
        || cell.contains("^^xsd:date")
        || cell.contains("^^xsd:gYear")
        || (lexical.len() == 4 && lexical.chars().all(|c| c.is_ascii_digit()))
}

/// A hint when a result's shape does not fit what the question's form asks for, such
/// as several rows for "How many", a SELECT for a yes or no question, no date for
/// "When", a count for "Which", or repeated rows.
pub(super) fn shape_hint(question: &str, query: &str, doc: &Value) -> Option<String> {
    let form = form(question);
    let boolean = doc["boolean"].is_boolean();
    let rows: Vec<Vec<&str>> = doc["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            r.as_array()
                .into_iter()
                .flatten()
                .map(|c| c.as_str().unwrap_or(""))
                .collect()
        })
        .collect();
    let one_number = rows.len() == 1 && rows[0].len() == 1 && numeric(rows[0][0]);
    let counted = query
        .to_uppercase()
        .split_whitespace()
        .collect::<String>()
        .contains("COUNT(");
    match form {
        Form::Count | Form::Date | Form::Things if boolean => {
            return Some(
                "The question does not ask for yes or no, but the query is an ASK. Select the values that answer it.".into(),
            );
        }
        Form::Count if !one_number => {
            return Some(format!(
                "The question asks how many, so its answer is one number, but the query returned {} rows of {} columns. Count the distinct answers with COUNT(DISTINCT ...), or select the one property whose value is the number.",
                rows.len(),
                rows.first().map_or(0, Vec::len)
            ));
        }
        Form::Boolean if !boolean => {
            return Some("The question asks for yes or no, so answer it with an ASK query.".into());
        }
        Form::Date if !rows.iter().flatten().any(|c| datelike(c)) => {
            return Some(
                "The question asks when, but no value of the result is a date or a year. Select the property that holds the date.".into(),
            );
        }
        Form::Things if one_number && counted => {
            return Some(
                "The question asks for the things themselves, not how many there are. Select them instead of counting them.".into(),
            );
        }
        _ => {}
    }
    let mut seen = std::collections::BTreeSet::new();
    if !rows.iter().all(|r| seen.insert(r.clone())) {
        return Some("The result repeats rows. Add DISTINCT.".into());
    }
    None
}

/// What a draft's result is, for the vote of several drafts: the sorted rows of a
/// result with rows, the boolean of an ASK, and `None` for an empty result or none.
/// A result cut to its first rows or with shortened terms may hide a difference, so
/// it has no signature either.
pub(super) fn signature(doc: &Value) -> Option<String> {
    if !doc["truncated"].is_null() || doc["termsShortened"].as_u64().unwrap_or(0) > 0 {
        return None;
    }
    if let Some(b) = doc["boolean"].as_bool() {
        return Some(b.to_string());
    }
    let mut rows: Vec<String> = doc["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(Value::to_string)
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort();
    rows.dedup();
    Some(rows.join("\n"))
}

/// The draft the vote chooses: the earliest of the largest group of drafts with the
/// same result. With no result at all, the first draft.
pub(super) fn vote(signatures: &[Option<String>]) -> usize {
    let mut best = (0usize, 0usize);
    for (i, s) in signatures.iter().enumerate() {
        let Some(s) = s else { continue };
        let n = signatures.iter().filter(|t| t.as_ref() == Some(s)).count();
        if n > best.1 {
            best = (i, n);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forms_of_english_questions() {
        assert_eq!(form("How many moons does Mars have?"), Form::Count);
        assert_eq!(form("how much did it cost?"), Form::Count);
        assert_eq!(form("Is Berlin a city?"), Form::Boolean);
        assert_eq!(form("Did Tolkien write a novel?"), Form::Boolean);
        assert_eq!(form("When was the bridge built?"), Form::Date);
        assert_eq!(form("Which rivers cross Berlin?"), Form::Things);
        assert_eq!(form("Give me all films."), Form::Things);
        assert_eq!(form("What is the capital?"), Form::Other);
    }

    #[test]
    fn shapes_that_do_not_fit() {
        let rows = |r: Value| json!({ "vars": ["x"], "rows": r });
        let two = rows(json!([["ex:a"], ["ex:b"]]));
        assert!(shape_hint("How many films are there?", "SELECT ?x", &two).is_some());
        let n = rows(json!([["\"2\"^^xsd:integer"]]));
        assert!(shape_hint("How many films are there?", "SELECT (COUNT(?x) AS ?n)", &n).is_none());
        assert!(shape_hint("Is it a film?", "SELECT ?x", &two).is_some());
        assert!(shape_hint("Is it a film?", "ASK {}", &json!({"boolean": true})).is_none());
        assert!(shape_hint("When was it built?", "SELECT ?x", &two).is_some());
        let year = rows(json!([["\"1999\"^^xsd:gYear"]]));
        assert!(shape_hint("When was it built?", "SELECT ?x", &year).is_none());
        assert!(shape_hint("Which films are there?", "SELECT (COUNT(?x) AS ?n)", &n).is_some());
        assert!(shape_hint("Which films are there?", "SELECT ?x", &two).is_none());
        assert!(shape_hint("Which films are there?", "SELECT (COUNT (?x) AS ?n)", &n).is_some());
        let yes = json!({ "boolean": true });
        assert!(shape_hint("How many films are there?", "ASK {}", &yes).is_some());
        assert!(shape_hint("When was it built?", "ASK {}", &yes).is_some());
        assert!(shape_hint("Which films are there?", "ASK {}", &yes).is_some());
        let repeated = rows(json!([["ex:a"], ["ex:a"]]));
        assert_eq!(
            shape_hint("What is it?", "SELECT ?x", &repeated).as_deref(),
            Some("The result repeats rows. Add DISTINCT.")
        );
    }

    #[test]
    fn votes_choose_the_largest_group_then_the_earliest() {
        let s = |x: &str| Some(x.to_string());
        assert_eq!(vote(&[s("a"), s("b"), s("b")]), 1);
        assert_eq!(vote(&[s("a"), s("b"), None]), 0);
        assert_eq!(vote(&[None, None, s("c")]), 2);
        assert_eq!(vote(&[None, None]), 0);
        let doc = json!({ "rows": [["ex:b"], ["ex:a"], ["ex:a"]] });
        assert_eq!(
            signature(&doc),
            signature(&json!({ "rows": [["ex:a"], ["ex:b"]] }))
        );
        assert_eq!(signature(&json!({ "rows": [] })), None);
        let cut = json!({ "rows": [["ex:a"]], "truncated": { "reason": "maxRows" } });
        assert_eq!(signature(&cut), None);
        assert_eq!(
            signature(&json!({ "rows": [["ex:a"]], "termsShortened": 1 })),
            None
        );
        assert!(
            signature(&json!({ "rows": [["ex:a"]], "truncated": null, "termsShortened": 0 }))
                .is_some()
        );
    }

    #[test]
    fn samples_and_rows_only_when_rows_may_leave() {
        let rows = json!({ "rows": [["<http://ex.org/p>", "3", "Secret value"]], "prefixes": {} });
        let p = serde_json::Map::new();
        let with = usage_line("e", &rows, &p, 5, true).unwrap();
        let without = usage_line("e", &rows, &p, 5, false).unwrap();
        assert!(with.contains("Secret value"), "{with}");
        assert!(
            !without.contains("Secret value") && without.contains("(3)"),
            "{without}"
        );
        assert!(shape_text("SELECT ?x {}", "Hint.", Some("Secret row")).contains("Secret row"));
        let hidden = shape_text("SELECT ?x {}", "Hint.", None);
        assert!(
            hidden.contains("Hint.") && !hidden.contains("first rows"),
            "{hidden}"
        );
    }

    #[test]
    fn words_around_a_mention() {
        assert_eq!(
            mention_context("Who directed the film Titanic in 1997?", "Titanic"),
            "directed the film in 1997?"
        );
        assert_eq!(mention_context("Titanic", "Avatar"), "");
    }
}
