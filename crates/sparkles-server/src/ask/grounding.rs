//! Conservative question-time choices over candidates already visible to the caller.
use serde_json::Value;

/// Retain common vocabulary for structural joins, then add question matches. The
/// split is fixed independently of every question's gold or model answer.
pub(super) fn balanced_entries(common: &Value, relevant: &Value, max: usize) -> Vec<Value> {
    let common = common.as_array().map(Vec::as_slice).unwrap_or_default();
    let relevant = relevant.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    common
        .iter()
        .take(max / 2)
        .chain(relevant)
        .chain(common)
        .filter(|entry| {
            entry["iri"]
                .as_str()
                .is_some_and(|iri| seen.insert(iri.to_string()))
        })
        .take(max)
        .cloned()
        .collect()
}

fn local(iri: &str) -> Option<String> {
    let iri = iri.trim_matches(['<', '>']);
    // A prefixed category keeps its namespace marker in the local component.
    let part = iri.rsplit(['/', '#']).next()?;
    let part = if part.contains(':') && !part.starts_with("Category:") {
        part.split_once(':').map_or(part, |(_, p)| p)
    } else {
        part
    };
    Some(
        percent_encoding::percent_decode_str(part)
            .decode_utf8()
            .ok()?
            .replace('_', " "),
    )
}

fn folded(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn taxonomic(candidate: &Value) -> bool {
    candidate["iri"]
        .as_str()
        .is_some_and(|iri| iri.contains("/Category:") || iri.starts_with("dbc:"))
        || candidate["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(local)
            .any(|t| {
                matches!(
                    t.as_str(),
                    "Concept" | "ConceptScheme" | "Class" | "Property"
                )
            })
}

fn matching_label(candidate: &Value, normalized: &str) -> bool {
    candidate["label"]
        .as_str()
        .is_some_and(|label| folded(label) == normalized)
        || candidate["altLabels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|label| folded(label) == normalized)
}

/// A unique unqualified name is stronger evidence than a category or a qualified
/// namesake. Equally canonical names remain ambiguous. An explicit kind immediately
/// before the mention must agree with the candidate's visible types.
pub(super) fn preferred_candidate(
    mention: &str,
    question: &str,
    candidates: &[Value],
) -> Option<usize> {
    let normalized = folded(mention);
    if normalized.is_empty() || normalized.contains("category:") {
        return None;
    }
    let q = question.to_lowercase();
    // A request about categories should retain the choices rather than silently
    // imposing the resource reading.
    if q.split(|c: char| !c.is_alphanumeric()).any(|w| {
        matches!(
            w,
            "category"
                | "categories"
                | "class"
                | "classes"
                | "property"
                | "properties"
                | "concept"
                | "concepts"
        )
    }) {
        return None;
    }
    // This shortcut removes ambiguity introduced by a category. Without one,
    // qualified namesakes can express a different intended entity even when their
    // displayed labels differ, so the person must still choose.
    if !candidates.iter().any(taxonomic) {
        return None;
    }
    let matches: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            !taxonomic(candidate)
                && candidate["iri"]
                    .as_str()
                    .and_then(local)
                    .is_some_and(|s| folded(&s) == normalized)
                && candidate["typeMatch"] != false
        })
        .map(|(i, _)| i)
        .collect();
    let [index] = matches.as_slice() else {
        return None;
    };
    let candidate = &candidates[*index];
    let types = candidate["types"].as_array()?;
    if !types
        .iter()
        .filter_map(Value::as_str)
        .filter_map(local)
        .any(|t| !matches!(t.as_str(), "Thing" | "Resource" | "NamedIndividual"))
        || taxonomic(candidate)
        || !matching_label(candidate, &normalized)
        || types
            .iter()
            .filter_map(Value::as_str)
            .any(|t| t.to_lowercase().contains("disambiguation"))
        || candidates
            .iter()
            .enumerate()
            .any(|(i, c)| i != *index && !taxonomic(c) && matching_label(c, &normalized))
    {
        return None;
    }
    // A different kind of substantive entity can be the intended namesake even
    // with a qualified label, such as a film and the city it is named after.
    let concrete_types = |c: &Value| -> std::collections::BTreeSet<String> {
        c["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(local)
            .filter(|t| !matches!(t.as_str(), "Thing" | "Resource" | "NamedIndividual"))
            .collect()
    };
    let selected_types = concrete_types(candidate);
    if candidates.iter().enumerate().any(|(i, c)| {
        i != *index && !taxonomic(c) && selected_types.is_disjoint(&concrete_types(c))
    }) {
        return None;
    }
    // A canonical lexical match cannot overrule a person explicitly naming a kind.
    if let Some(pos) = q.find(&mention.to_lowercase()) {
        let before =
            q[..pos].trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '“'));
        let before = before
            .strip_suffix("called")
            .or_else(|| before.strip_suffix("named"))
            .or_else(|| before.strip_suffix("titled"))
            .unwrap_or(before)
            .trim_end();
        let kind = before.split_whitespace().last().unwrap_or("");
        if matches!(
            kind,
            "book" | "film" | "movie" | "song" | "album" | "city" | "town" | "country" | "river"
        ) {
            let expected = if kind == "movie" { "film" } else { kind };
            if !candidate["types"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(local)
                .any(|t| folded(&t) == expected)
            {
                return None;
            }
        }
    }
    Some(*index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn candidates() -> Vec<Value> {
        vec![
            json!({"iri":"<http://dbpedia.org/resource/Category:Salt_Lake_City>","label":"Salt Lake City","types":["skos:Concept"]}),
            json!({"iri":"<http://dbpedia.org/resource/Salt_Lake_City_(fictional_city)>","label":"Salt Lake City fictional city","types":["dbo:City"]}),
            json!({"iri":"<http://dbpedia.org/resource/Salt_Lake_City>","label":"Salt Lake City","types":["dbo:City"]}),
        ]
    }
    #[test]
    fn resource_beats_its_category_and_qualified_namesake() {
        assert_eq!(
            preferred_candidate(
                "Salt Lake City",
                "What is the time zone of Salt Lake City?",
                &candidates()
            ),
            Some(2)
        );
    }
    #[test]
    fn true_ambiguity_explicit_categories_and_conflicting_kinds_remain() {
        let mut c = candidates();
        c.push(json!({"iri":"<https://example.org/Salt_Lake_City>"}));
        assert_eq!(
            preferred_candidate("Salt Lake City", "Salt Lake City?", &c),
            None
        );
        assert_eq!(
            preferred_candidate(
                "Salt Lake City",
                "Which category represents Salt Lake City?",
                &candidates()
            ),
            None
        );
        assert_eq!(
            preferred_candidate(
                "Salt Lake City",
                "Who wrote the book Salt Lake City?",
                &candidates()
            ),
            None
        );
        assert_eq!(
            preferred_candidate(
                "Ana Lima",
                "Who is Ana Lima?",
                &[json!({"iri":"res:ana"}), json!({"iri":"res:ana2"})]
            ),
            None
        );
    }
    #[test]
    fn prefixed_and_percent_encoded_names_and_matching_explicit_kind() {
        assert_eq!(
            preferred_candidate(
                "São Paulo",
                "São Paulo?",
                &[
                    json!({"iri":"<http://dbpedia.org/resource/S%C3%A3o_Paulo>","label":"São Paulo","types":["dbo:City"]}),
                    json!({"iri":"dbc:São_Paulo","label":"São Paulo","types":["skos:Concept"]})
                ]
            ),
            Some(0)
        );
        assert_eq!(
            preferred_candidate(
                "Berlin",
                "What is the city Berlin?",
                &[
                    json!({"iri":"dbr:Berlin","label":"Berlin","types":["dbo:City"]}),
                    json!({"iri":"dbc:Berlin","label":"Berlin","types":["skos:Concept"]})
                ]
            ),
            Some(0)
        );
        assert_eq!(
            preferred_candidate(
                "Berlin",
                "Berlin?",
                &[json!({"iri":"dbr:Berlin","typeMatch":false})]
            ),
            None
        );
    }
    #[test]
    fn disambiguation_pages_and_genuine_same_label_namesakes_remain_ambiguous() {
        let c = vec![
            json!({"iri":"dbr:Mercury","label":"Mercury","types":["dbo:WikimediaDisambiguation"]}),
            json!({"iri":"dbr:Mercury_(planet)","label":"Mercury","types":["dbo:Planet"]}),
            json!({"iri":"dbc:Mercury","label":"Mercury","types":["skos:Concept"]}),
        ];
        assert_eq!(preferred_candidate("Mercury", "What is Mercury?", &c), None);
        let c = vec![
            json!({"iri":"dbr:Washington","label":"Washington","types":["dbo:State"]}),
            json!({"iri":"dbr:Washington,_D.C.","label":"Washington","types":["dbo:City"]}),
            json!({"iri":"dbc:Washington","label":"Washington","types":["skos:Concept"]}),
        ];
        assert_eq!(
            preferred_candidate("Washington", "Where is Washington?", &c),
            None
        );
        assert_eq!(
            preferred_candidate(
                "Berlin",
                "Berlin?",
                &[
                    json!({"iri":"dbr:Berlin","label":"Berlin"}),
                    json!({"iri":"dbc:Berlin","label":"Berlin","types":["skos:Concept"]})
                ]
            ),
            None
        );
    }
    #[test]
    fn category_free_namesakes_and_generic_thing_types_do_not_get_auto_selected() {
        let c = vec![
            json!({"iri":"dbr:Casablanca","label":"Casablanca","types":["dbo:City"]}),
            json!({"iri":"dbr:Casablanca_(film)","label":"Casablanca (film)","types":["dbo:Film"]}),
        ];
        assert_eq!(
            preferred_candidate("Casablanca", "Who is the director of Casablanca?", &c),
            None
        );
        let mut with_category = c.clone();
        with_category
            .push(json!({"iri":"dbc:Casablanca","label":"Casablanca","types":["skos:Concept"]}));
        assert_eq!(
            preferred_candidate(
                "Casablanca",
                "Who is the director of Casablanca?",
                &with_category
            ),
            None
        );
        let c = vec![
            json!({"iri":"dbr:German","label":"German","types":["owl:Thing"]}),
            json!({"iri":"dbc:German","label":"German","types":["skos:Concept"]}),
        ];
        assert_eq!(
            preferred_candidate("German", "Which German cities?", &c),
            None
        );
    }
    #[test]
    fn balanced_schema_keeps_structure_question_matches_and_unique_terms() {
        let common =
            json!([{"iri":"rdf:type"},{"iri":"rdfs:label"},{"iri":"ex:common"},{"iri":"ex:other"}]);
        let relevant = json!([{"iri":"rdfs:label"},{"iri":"ex:timeZone"},{"iri":"ex:birthPlace"},{"iri":"ex:common"}]);
        let actual = balanced_entries(&common, &relevant, 4);
        assert_eq!(
            actual
                .iter()
                .map(|e| e["iri"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["rdf:type", "rdfs:label", "ex:timeZone", "ex:birthPlace"]
        );
        assert_eq!(balanced_entries(&Value::Null, &relevant, 4).len(), 4);
        assert_eq!(balanced_entries(&common, &Value::Null, 4).len(), 4);
        assert!(balanced_entries(&common, &relevant, 0).is_empty());
    }
}
