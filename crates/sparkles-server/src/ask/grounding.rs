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

fn expanded(iri: &str, prefixes: &Value) -> String {
    let iri = iri.trim_matches(['<', '>']);
    if let Some((prefix, local)) = iri.split_once(':')
        && let Some(namespace) = prefixes[prefix].as_str()
    {
        return format!("{namespace}{local}");
    }
    iri.to_string()
}

fn taxonomic(candidate: &Value) -> bool {
    candidate["iri"]
        .as_str()
        .is_some_and(|iri| iri.starts_with("http://dbpedia.org/resource/Category:"))
        || candidate["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|iri| {
                matches!(
                    iri,
                    "http://www.w3.org/2004/02/skos/core#Concept"
                        | "http://www.w3.org/2004/02/skos/core#ConceptScheme"
                        | "http://www.w3.org/2000/01/rdf-schema#Class"
                        | "http://www.w3.org/2002/07/owl#Class"
                        | "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property"
                        | "http://www.w3.org/2002/07/owl#ObjectProperty"
                        | "http://www.w3.org/2002/07/owl#DatatypeProperty"
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

/// A conservative display preference among choices already offered to the person.
/// Display metadata is bounded and cannot prove uniqueness. This helper must never
/// change an ambiguous verdict or suppress clarification.
pub(super) fn preferred_display_candidate(
    mention: &str,
    question: &str,
    candidates: &[Value],
    prefixes: &Value,
) -> Option<usize> {
    let candidates: Vec<Value> = candidates
        .iter()
        .map(|candidate| {
            let mut candidate = candidate.clone();
            if let Some(iri) = candidate["iri"].as_str() {
                candidate["iri"] = expanded(iri, prefixes).into();
            }
            if let Some(types) = candidate["types"].as_array() {
                candidate["types"] = types
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|iri| Value::from(expanded(iri, prefixes)))
                    .collect();
            }
            candidate
        })
        .collect();
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
    let concrete_types = |c: &Value| -> std::collections::BTreeSet<String> {
        c["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|iri| {
                !matches!(
                    *iri,
                    "http://www.w3.org/2002/07/owl#Thing"
                        | "http://www.w3.org/2000/01/rdf-schema#Resource"
                        | "http://www.w3.org/2002/07/owl#NamedIndividual"
                )
            })
            .map(str::to_owned)
            .collect()
    };
    let selected_types = concrete_types(candidate);
    if selected_types.is_empty()
        || taxonomic(candidate)
        || !matching_label(candidate, &normalized)
        || types
            .iter()
            .filter_map(Value::as_str)
            .any(|t| t.to_lowercase().contains("disambiguation"))
        || candidates.iter().enumerate().any(|(i, c)| {
            i != *index
                && !taxonomic(c)
                && (matching_label(c, &normalized)
                    || c["matchedBy"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|m| m == "exact" || m == "normalized"))
        })
    {
        return None;
    }
    // A different kind of substantive entity can be the intended namesake even
    // with a qualified label, such as a film and the city it is named after.
    if candidates
        .iter()
        .enumerate()
        .any(|(i, c)| i != *index && !taxonomic(c) && selected_types != concrete_types(c))
    {
        return None;
    }
    // A canonical lexical match cannot overrule a person explicitly naming a kind.
    if let Some(pos) = q.find(&mention.to_lowercase()) {
        let before = q[..pos].trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '“' | '‘' | '’')
        });
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
    fn preferred_candidate(mention: &str, question: &str, candidates: &[Value]) -> Option<usize> {
        preferred_display_candidate(
            mention,
            question,
            candidates,
            &json!({
                "dbr":"http://dbpedia.org/resource/", "dbc":"http://dbpedia.org/resource/Category:",
                "dbo":"http://dbpedia.org/ontology/", "skos":"http://www.w3.org/2004/02/skos/core#",
                "owl":"http://www.w3.org/2002/07/owl#", "rdf":"http://www.w3.org/1999/02/22-rdf-syntax-ns#",
                "rdfs":"http://www.w3.org/2000/01/rdf-schema#"
            }),
        )
    }
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
    #[test]
    fn display_preference_respects_type_namespaces_and_rival_match_evidence() {
        let c = vec![
            json!({"iri":"dbr:Berlin","label":"Berlin","types":["dbo:City"]}),
            json!({"iri":"<http://other.example/Berlin>","label":"Berlin","types":["<http://example.org/Concept>"]}),
        ];
        assert_eq!(preferred_candidate("Berlin", "What is Berlin?", &c), None);
        let c = vec![
            json!({"iri":"dbr:Dune","label":"Dune","types":["dbo:Novel","dbo:Work"]}),
            json!({"iri":"dbr:Dune_(film)","label":"Dune (film)","types":["dbo:Film","dbo:Work"]}),
            json!({"iri":"dbc:Dune","label":"Dune","types":["skos:Concept"]}),
        ];
        assert_eq!(preferred_candidate("Dune", "Who directed Dune?", &c), None);
        let c = vec![
            json!({"iri":"dbr:Berlin","label":"Berlin","types":["dbo:City"]}),
            json!({"iri":"dbr:Berlin_(town)","label":"Berlin (town)","types":["dbo:City"],"matchedBy":["exact"]}),
            json!({"iri":"dbc:Berlin","label":"Berlin","types":["skos:Concept"]}),
        ];
        assert_eq!(preferred_candidate("Berlin", "Where is Berlin?", &c), None);
        assert_eq!(
            preferred_candidate(
                "Salt Lake City",
                "Who wrote the book ‘Salt Lake City’?",
                &candidates()
            ),
            None
        );
    }

    #[test]
    fn rebound_vocabulary_prefix_is_not_a_standard_taxonomy_type() {
        let c = vec![
            json!({"iri":"<http://example.org/Berlin>","label":"Berlin","types":["<http://example.org/City>"]}),
            json!({"iri":"<http://other.example/Berlin>","label":"Berlin","types":["skos:Concept"]}),
        ];
        assert_eq!(
            preferred_display_candidate(
                "Berlin",
                "Berlin?",
                &c,
                &json!({"skos":"http://example.org/"})
            ),
            None
        );
    }
}
