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

/// How many of the first `entries` that [`balanced_entries`] chose come from the head
/// of `common`, the part of the list that is the same for every question.
pub(super) fn common_head(common: &Value, entries: &[Value], max: usize) -> usize {
    let head: std::collections::BTreeSet<&str> = common
        .as_array()
        .into_iter()
        .flatten()
        .take(max / 2)
        .filter_map(|e| e["iri"].as_str())
        .collect();
    entries
        .iter()
        .take_while(|e| e["iri"].as_str().is_some_and(|i| head.contains(i)))
        .count()
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

pub(super) fn expanded(iri: &str, prefixes: &Value) -> String {
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

/// The compact form of `iri` under the longest matching namespace of `prefixes`, when
/// its local part is a plain name, else the IRI in angle brackets.
pub(super) fn compact(iri: &str, prefixes: &serde_json::Map<String, Value>) -> String {
    let iri = iri.trim_matches(['<', '>']);
    let best = prefixes
        .iter()
        .filter_map(|(p, ns)| Some((p, ns.as_str()?)))
        .filter(|(_, ns)| !ns.is_empty() && iri.starts_with(ns) && iri.len() > ns.len())
        .max_by_key(|(_, ns)| ns.len());
    if let Some((p, ns)) = best {
        let local = &iri[ns.len()..];
        let plain = local
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            && local
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if plain {
            return format!("{p}:{local}");
        }
    }
    format!("<{iri}>")
}

/// `ranked` predicates with their counts, grouped by namespace in order of first
/// appearance, as `<ns> local n, local n; prefix: local n`. A predicate whose local
/// part is not a plain name keeps its full IRI in a group of its own.
pub(super) fn grouped(
    ranked: &[(String, u64)],
    prefixes: &serde_json::Map<String, Value>,
) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (iri, n) in ranked {
        let short = compact(iri, prefixes);
        let (head, item) = if let Some((p, local)) = short.split_once(':')
            && !short.starts_with('<')
        {
            (format!("{p}:"), format!("{local} {n}"))
        } else {
            let cut = iri.rfind(['/', '#']).map_or(0, |i| i + 1);
            let local = &iri[cut..];
            let plain = !local.is_empty()
                && local
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
            if plain && cut > 0 {
                (format!("<{}>", &iri[..cut]), format!("{local} {n}"))
            } else {
                (String::new(), format!("<{iri}> {n}"))
            }
        };
        match groups.iter_mut().find(|(h, _)| *h == head) {
            Some((_, items)) => items.push(item),
            None => groups.push((head, vec![item])),
        }
    }
    groups
        .iter()
        .map(|(h, items)| {
            if h.is_empty() {
                items.join(", ")
            } else {
                format!("{h} {}", items.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// An IRI from a term string of a compact query result, such as `<http://x>` or a
/// prefixed name under the result's `prefixes`.
pub(super) fn result_iri(cell: &Value, prefixes: &Value) -> Option<String> {
    let s = cell.as_str()?;
    if let Some(inner) = s.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        return (!inner.is_empty()).then(|| inner.to_string());
    }
    let (p, local) = s.split_once(':')?;
    let ns = prefixes[p].as_str()?;
    (!s.starts_with('"')).then(|| format!("{ns}{local}"))
}

/// An integer from a term string of a compact query result, such as `"12"^^xsd:integer`.
pub(super) fn result_count(cell: &Value) -> Option<u64> {
    let s = cell.as_str()?;
    let lexical = s
        .strip_prefix('"')
        .and_then(|t| t.split_once('"').map(|(v, _)| v))
        .unwrap_or(s);
    lexical.parse().ok()
}

/// The predicates of one usage query result, as `(predicate IRI, count)`, ranked by
/// how many question words their local name shares and then by count, without the ones
/// every context lists anyway, at most `max`.
pub(super) fn ranked_usage(
    result: &Value,
    question: &std::collections::BTreeSet<String>,
    skip: &[&str],
    max: usize,
) -> Vec<(String, u64)> {
    let mut found: Vec<(usize, u64, String)> = result["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let iri = result_iri(row.get(0)?, &result["prefixes"])?;
            let n = result_count(row.get(1)?)?;
            (!skip.contains(&iri.as_str())).then(|| {
                let score = crate::mcp::relevance::entry_score(&iri, std::iter::empty(), question);
                (score, n, iri)
            })
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    found
        .into_iter()
        .take(max)
        .map(|(_, n, iri)| (iri, n))
        .collect()
}

/// The classes of `classes` whose name or label shares a word with the question, best
/// first, at most `max`.
pub(super) fn matched_classes(
    classes: &[Value],
    question: &std::collections::BTreeSet<String>,
    prefixes: &Value,
    max: usize,
) -> Vec<String> {
    let mut scored: Vec<(usize, u64, String)> = classes
        .iter()
        .filter_map(|c| {
            let iri = expanded(c["iri"].as_str()?, prefixes);
            let label = c["label"].as_str();
            let score = crate::mcp::relevance::entry_score(&iri, label.into_iter(), question);
            (score > 0).then(|| (score, c["instances"].as_u64().unwrap_or(0), iri))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    scored
        .into_iter()
        .take(max)
        .map(|(_, _, iri)| iri)
        .collect()
}

/// An absolute IRI that a clarification answer names, in angle brackets or under one
/// of `prefixes`.
pub(super) fn answered_iri(answer: &str, prefixes: &Value) -> Option<String> {
    let a = answer.trim();
    let iri = if a.starts_with('<') && a.ends_with('>') {
        a.trim_matches(['<', '>']).to_string()
    } else {
        let e = expanded(a, prefixes);
        if e == a {
            return None;
        }
        e
    };
    let scheme = iri.split_once(':').map(|(s, _)| s)?;
    (!scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        && !iri
            .chars()
            .any(|c| c.is_whitespace() || "<>\"{}|^`\\".contains(c)))
    .then_some(iri)
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
    fn usage_lines_rank_question_words_then_counts_and_compact_iris() {
        let prefixes = json!({"ex": "http://example.org/", "exv": "http://example.org/v/"});
        let p = prefixes.as_object().unwrap();
        assert_eq!(
            compact("http://example.org/v/elevation", p),
            "exv:elevation"
        );
        assert_eq!(
            compact("<http://example.org/a(b)>", p),
            "<http://example.org/a(b)>"
        );
        assert_eq!(compact("http://other.org/x", p), "<http://other.org/x>");
        let rows = json!({"prefixes": {"ex2": "http://example.org/"}, "rows": [
            ["ex2:wikiLink", "\"900\"^^xsd:integer"],
            ["<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>", "\"40\"^^xsd:integer"],
            ["<http://example.org/elevation>", "\"3\"^^xsd:integer"],
            ["<http://example.org/name>", "12"],
            ["\"not an iri\"", "1"],
            ["unknown:x", "1"],
        ]});
        let words = crate::mcp::relevance::words("How high is the elevation of Mont Blanc?");
        let ranked = ranked_usage(
            &rows,
            &words,
            &["http://www.w3.org/1999/02/22-rdf-syntax-ns#type"],
            3,
        );
        assert_eq!(
            ranked,
            [
                ("http://example.org/elevation".to_string(), 3),
                ("http://example.org/wikiLink".to_string(), 900),
                ("http://example.org/name".to_string(), 12)
            ]
        );
    }

    #[test]
    fn usage_groups_by_namespace() {
        let prefixes = json!({"ex": "http://example.org/"});
        let p = prefixes.as_object().unwrap();
        let ranked = [
            ("http://dbpedia.org/property/satellites".to_string(), 1),
            ("http://example.org/name".to_string(), 3),
            ("http://dbpedia.org/property/mass".to_string(), 2),
            ("http://dbpedia.org/property/a(b)".to_string(), 4),
        ];
        assert_eq!(
            grouped(&ranked, p),
            "<http://dbpedia.org/property/> satellites 1, mass 2; ex: name 3; <http://dbpedia.org/property/a(b)> 4"
        );
    }

    #[test]
    fn matched_classes_and_clarification_iris() {
        let prefixes = json!({"ex": "http://example.org/"});
        let classes = vec![
            json!({"iri": "ex:Mountain", "label": "mountain", "instances": 10}),
            json!({"iri": "ex:Person", "label": "person", "instances": 99}),
            json!({"iri": "ex:MountainRange", "instances": 5}),
        ];
        let words = crate::mcp::relevance::words("What is the highest mountain in Germany?");
        assert_eq!(
            matched_classes(&classes, &words, &prefixes, 2),
            [
                "http://example.org/Mountain",
                "http://example.org/MountainRange"
            ]
        );
        assert_eq!(
            answered_iri("<http://example.org/Mars>", &prefixes).as_deref(),
            Some("http://example.org/Mars")
        );
        assert_eq!(
            answered_iri("ex:Mars", &prefixes).as_deref(),
            Some("http://example.org/Mars")
        );
        assert_eq!(answered_iri("the planet", &prefixes), None);
        assert_eq!(answered_iri("<http://example.org/a b>", &prefixes), None);
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
