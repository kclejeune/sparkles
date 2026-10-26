//! Question-specific ordering of offered choices using scoped linker evidence.
//! No preference changes a linker verdict or proves unique intent.
use serde_json::Value;

/// Restore an abbreviated display identity only from a recognized full IRI. Keep
/// ordinary compact choice values stable across the evidence opt-in.
pub(super) fn restore_identity(candidate: &mut Value) {
    if candidate["evidence"]["version"] != 1
        || !candidate["iri"].as_str().is_some_and(|iri| {
            iri.rsplit_once(">…(+")
                .and_then(|(_, tail)| tail.strip_suffix(" chars)"))
                .is_some_and(|count| count.parse::<usize>().is_ok_and(|count| count > 0))
        })
    {
        return;
    }
    if let Some(iri) = candidate["evidence"]["iri"].as_str()
        && let Ok(iri) = oxrdf::NamedNode::new(iri)
    {
        candidate["iri"] = iri.to_string().into();
    }
}

fn adjacent_kind(mention: &str, question: &str) -> Option<&'static str> {
    let question = question.to_lowercase();
    let mention = mention.to_lowercase();
    if mention.is_empty() {
        return None;
    }
    let mut kinds = std::collections::BTreeSet::new();
    for (start, _) in question.match_indices(&mention) {
        let end = start + mention.len();
        if question[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            || question[end..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        let before = question[..start].trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '“' | '‘' | '’')
        });
        let mut words = before.split_whitespace().rev();
        let mut word = words.next()?;
        if matches!(word, "called" | "named" | "titled") {
            word = words.next()?;
        }
        // Do not turn a negated or contrasted reading into a preference.
        if before.split_whitespace().rev().take(8).any(|word| {
            matches!(
                word,
                "not" | "except" | "without" | "rather" | "neither" | "nor"
            ) || word.ends_with("n't")
                || word.ends_with("n’t")
        }) {
            return None;
        }
        let kind = match word {
            "film" | "movie" => "Film",
            "book" | "novel" => "Book",
            "song" => "Song",
            "album" => "Album",
            "city" => "City",
            "town" => "Town",
            "country" => "Country",
            "river" => "River",
            "person" => "Person",
            _ => continue,
        };
        kinds.insert(kind);
    }
    let mut kinds = kinds.into_iter();
    let kind = kinds.next()?;
    kinds.next().is_none().then_some(kind)
}

fn matches_kind(iri: &str, kind: &str) -> bool {
    match kind {
        "Film" => matches!(
            iri,
            "http://dbpedia.org/ontology/Film"
                | "http://schema.org/Movie"
                | "https://schema.org/Movie"
        ),
        "Book" => matches!(
            iri,
            "http://dbpedia.org/ontology/Book"
                | "http://dbpedia.org/ontology/Novel"
                | "http://schema.org/Book"
                | "https://schema.org/Book"
        ),
        "Song" => matches!(
            iri,
            "http://dbpedia.org/ontology/Song"
                | "http://schema.org/MusicRecording"
                | "https://schema.org/MusicRecording"
        ),
        "Album" => matches!(
            iri,
            "http://dbpedia.org/ontology/Album"
                | "http://schema.org/MusicAlbum"
                | "https://schema.org/MusicAlbum"
        ),
        "City" => matches!(
            iri,
            "http://dbpedia.org/ontology/City"
                | "http://schema.org/City"
                | "https://schema.org/City"
        ),
        "Town" => iri == "http://dbpedia.org/ontology/Town",
        "Country" => matches!(
            iri,
            "http://dbpedia.org/ontology/Country"
                | "http://schema.org/Country"
                | "https://schema.org/Country"
        ),
        "River" => iri == "http://dbpedia.org/ontology/River",
        "Person" => matches!(
            iri,
            "http://dbpedia.org/ontology/Person"
                | "http://xmlns.com/foaf/0.1/Person"
                | "http://schema.org/Person"
                | "https://schema.org/Person"
        ),
        _ => false,
    }
}

/// A sole positively typed choice may come first when the person explicitly names
/// its kind. The candidate universe may still be incomplete; clarification remains.
pub(super) fn contextual_preference(
    mention: &str,
    question: &str,
    mention_evidence: &Value,
    candidates: &[Value],
) -> Option<usize> {
    if mention_evidence["version"] != 1 || candidates.is_empty() {
        return None;
    }
    let kind = adjacent_kind(mention, question)?;
    let mut matching = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let evidence = &candidate["evidence"];
        if evidence["version"] != 1 || evidence["typesComplete"] != true {
            return None;
        }
        let types = evidence["types"].as_array()?;
        if types.iter().any(|value| !value.is_string()) {
            return None;
        }
        let taxonomic = evidence["iri"]
            .as_str()
            .is_some_and(|iri| iri.starts_with("http://dbpedia.org/resource/Category:"))
            || types.iter().filter_map(Value::as_str).any(|iri| {
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
            });
        let disambiguation = types
            .iter()
            .filter_map(Value::as_str)
            .any(|iri| iri.to_lowercase().contains("disambiguation"));
        if !taxonomic
            && !disambiguation
            && candidate["typeMatch"] != false
            && types
                .iter()
                .filter_map(Value::as_str)
                .any(|iri| matches_kind(iri, kind))
        {
            matching.push(index);
        }
    }
    let [index] = matching.as_slice() else {
        return None;
    };
    candidates[*index]["matchedBy"]
        .as_array()?
        .iter()
        .any(|method| method == "exact" || method == "normalized")
        .then_some(*index)
}

/// Apply the existing resource/category display rule with all retained types. A
/// capped list cannot establish absence of a fourth disambiguation or kind type.
pub(super) fn resource_preference(
    mention: &str,
    question: &str,
    mention_evidence: &Value,
    candidates: &[Value],
    prefixes: &Value,
) -> Option<usize> {
    if mention_evidence["version"] != 1 {
        return None;
    }
    let mut full = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let evidence = &candidate["evidence"];
        if evidence["version"] != 1
            || evidence["typesComplete"] != true
            || !evidence["types"].as_array()?.iter().all(Value::is_string)
        {
            return None;
        }
        let mut candidate = candidate.clone();
        candidate["iri"] = evidence["iri"].as_str()?.into();
        candidate["types"] = evidence["types"].clone();
        full.push(candidate);
    }
    super::grounding::preferred_display_candidate(mention, question, &full, prefixes)
}

/// Ordinary fallback keeps the shipped display rule. Recognized rich evidence
/// must never fall through to truncated types after revealing disambiguation or
/// incomplete detail. An unknown evidence version cannot establish a preference.
pub(super) fn preferred_display_candidate(
    mention: &str,
    question: &str,
    evidence: &Value,
    candidates: &[Value],
    prefixes: &Value,
) -> Option<usize> {
    if evidence.is_null() {
        return super::grounding::preferred_display_candidate(
            mention, question, candidates, prefixes,
        );
    }
    if evidence["version"] != 1 {
        return None;
    }
    contextual_preference(mention, question, evidence, candidates)
        .or_else(|| resource_preference(mention, question, evidence, candidates, prefixes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidate(iri: &str, types: &[&str]) -> Value {
        json!({"iri":iri,"matchedBy":["exact"],"typeMatch":true,
            "evidence":{"version":1,"iri":iri,"types":types,"typesComplete":true}})
    }

    #[test]
    fn explicit_kind_ranks_full_typed_evidence_without_claiming_a_complete_universe() {
        let candidates = vec![
            candidate("urn:city", &["http://dbpedia.org/ontology/City"]),
            candidate(
                "urn:film",
                &[
                    "urn:A",
                    "urn:B",
                    "urn:C",
                    "http://dbpedia.org/ontology/Film",
                ],
            ),
        ];
        let evidence =
            json!({"version":1,"candidateUniverseComplete":false,"displayTruncated":true});
        for question in [
            "Who directed the film Casablanca?",
            "Who directed the movie called ‘Casablanca’?",
            "Who directed the film \"Casablanca\"?",
        ] {
            assert_eq!(
                contextual_preference("Casablanca", question, &evidence, &candidates),
                Some(1)
            );
        }
        for question in [
            "Who directed Casablanca?",
            "Where is Casablanca?",
            "Not the film Casablanca?",
            "The film Casablanca or the city Casablanca?",
        ] {
            assert_eq!(
                contextual_preference("Casablanca", question, &evidence, &candidates),
                None,
                "{question}"
            );
        }
    }

    #[test]
    fn incomplete_unknown_and_competing_evidence_cannot_select_a_kind() {
        let evidence = json!({"version":1});
        let base = vec![
            candidate("urn:city", &["http://dbpedia.org/ontology/City"]),
            candidate("urn:film", &["http://dbpedia.org/ontology/Film"]),
        ];
        for field in ["version", "typesComplete"] {
            let mut candidates = base.clone();
            candidates[0]["evidence"][field] = if field == "version" {
                json!(2)
            } else {
                json!(false)
            };
            assert_eq!(
                contextual_preference("Casablanca", "The film Casablanca?", &evidence, &candidates),
                None
            );
        }
        let mut candidates = base.clone();
        candidates.push(candidate(
            "urn:otherfilm",
            &["http://dbpedia.org/ontology/Film"],
        ));
        assert_eq!(
            contextual_preference("Casablanca", "The film Casablanca?", &evidence, &candidates),
            None
        );
        candidates = base.clone();
        candidates[1]["evidence"]["types"]
            .as_array_mut()
            .unwrap()
            .push(json!("urn:WikimediaDisambiguation"));
        assert_eq!(
            contextual_preference("Casablanca", "The film Casablanca?", &evidence, &candidates),
            None
        );
        candidates[1] = candidate("urn:localFilm", &["http://example.org/Film"]);
        assert_eq!(
            contextual_preference("Casablanca", "The film Casablanca?", &evidence, &candidates),
            None
        );
        assert_eq!(
            contextual_preference(
                "Casablanca",
                "The film Casablanca?",
                &json!({"version":2}),
                &base
            ),
            None
        );
    }

    #[test]
    fn only_recognized_evidence_restores_an_abbreviated_identity() {
        let iri = format!("urn:entity:{}", "x".repeat(400));
        let mut c = candidate(&iri, &[]);
        c["iri"] = "<urn:entity:x>…(+399 chars)".into();
        restore_identity(&mut c);
        assert_eq!(c["iri"], format!("<{iri}>"));
        c["iri"] = "ex:entity".into();
        restore_identity(&mut c);
        assert_eq!(c["iri"], "ex:entity");
        for (version, identity) in [(2, iri.as_str()), (1, "not an absolute iri")] {
            let mut c = candidate(identity, &[]);
            c["evidence"]["version"] = version.into();
            c["iri"] = "<urn:entity:x>…(+399 chars)".into();
            restore_identity(&mut c);
            assert_eq!(c["iri"], "<urn:entity:x>…(+399 chars)");
        }
        c["iri"] = "urn:actual:…(+1chars)".into();
        restore_identity(&mut c);
        assert_eq!(c["iri"], "urn:actual:…(+1chars)");
    }
}
