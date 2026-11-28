//! Deterministic previews of public catalog metadata and Qleverfile input rules.
//! A preview never registers a source or evaluates a command.

use super::{Distribution, Result, invalid};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub kind: Kind,
    pub document: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Kind {
    OboFoundry,
    Bioregistry,
    Lov,
    Ckan,
    Databus,
    Dcat,
    Qleverfile,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preview {
    pub candidates: Vec<Candidate>,
    pub inputs: Vec<QleverInput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candidate {
    id: String,
    title: String,
    version: Option<String>,
    namespace: Option<String>,
    distributions: Vec<Distribution>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QleverInput {
    files: Vec<String>,
    rdf_format: String,
    graph: Option<String>,
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn distribution(url: String) -> Distribution {
    Distribution {
        url,
        media_type: None,
        byte_size: None,
        sha256: None,
    }
}

fn candidate(id: String, row: &Value, urls: impl IntoIterator<Item = String>) -> Candidate {
    Candidate {
        title: string(row, "title")
            .or_else(|| string(row, "name"))
            .unwrap_or_else(|| id.clone()),
        version: string(row, "version"),
        namespace: string(row, "namespace").or_else(|| {
            let template = string(row, "uri_format")?;
            template.strip_suffix("$1").map(str::to_owned)
        }),
        id,
        distributions: urls.into_iter().map(distribution).collect(),
    }
}

fn rows<'a>(value: &'a Value, description: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(format!("expected {description}")))
}

fn id(row: &Value, key: &str) -> Result<String> {
    string(row, key).ok_or_else(|| invalid(format!("harvested entry has no {key}")))
}

fn json(kind: Kind, value: &Value) -> Result<Vec<Candidate>> {
    match kind {
        Kind::OboFoundry => rows(&value["ontologies"], "an OBO ontologies array")?
            .iter()
            .filter(|row| row["is_obsolete"] != true)
            .map(|row| Ok(candidate(id(row, "id")?, row, string(row, "ontology_purl"))))
            .collect(),
        Kind::Bioregistry => {
            let map = value
                .as_object()
                .ok_or_else(|| invalid("expected the Bioregistry prefix map"))?;
            Ok(map
                .iter()
                .map(|(prefix, row)| {
                    candidate(
                        prefix.clone(),
                        row,
                        string(row, "download_owl").or_else(|| string(row, "download_rdf")),
                    )
                })
                .collect())
        }
        Kind::Lov => {
            let id = id(value, "prefix")?;
            rows(&value["versions"], "a LOV versions array")?
                .iter()
                .map(|row| {
                    let mut entry = candidate(id.clone(), value, string(row, "fileURL"));
                    entry.version = string(row, "name").or_else(|| string(row, "issued"));
                    Ok(entry)
                })
                .collect()
        }
        Kind::Ckan => {
            if value["success"] != true {
                return Err(invalid("expected a successful CKAN package_show response"));
            }
            let row = &value["result"];
            let mut entry = candidate(id(row, "name")?, row, []);
            for resource in rows(&row["resources"], "a CKAN resources array")? {
                let mut file = distribution(id(resource, "url")?);
                file.media_type = string(resource, "mimetype");
                file.byte_size = resource["size"].as_u64();
                file.sha256 = string(resource, "hash").filter(|hash| is_digest(hash));
                entry.distributions.push(file);
            }
            Ok(vec![entry])
        }
        Kind::Databus => {
            let mut versions = BTreeMap::new();
            for row in rows(
                &value["results"]["bindings"],
                "a Databus SPARQL SELECT result",
            )? {
                let version = id(&row["version"], "value")?;
                let mut file = distribution(id(&row["file"], "value")?);
                file.sha256 = string(&row["sha256"], "value").filter(|hash| is_digest(hash));
                file.byte_size = string(&row["bytes"], "value").and_then(|s| s.parse().ok());
                versions.entry(version).or_insert_with(Vec::new).push(file);
            }
            Ok(versions
                .into_iter()
                .map(|(version, distributions)| Candidate {
                    id: version.clone(),
                    title: version.clone(),
                    version: Some(version),
                    namespace: None,
                    distributions,
                })
                .collect())
        }
        Kind::Dcat | Kind::Qleverfile => unreachable!("non-JSON harvester"),
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn dcat(document: &str) -> Result<Vec<Candidate>> {
    use oxrdf::{NamedOrBlankNode, Term};
    use oxrdfio::{RdfFormat, RdfParser};
    const DCAT: &str = "http://www.w3.org/ns/dcat#";
    let mut datasets = HashMap::<NamedOrBlankNode, Vec<NamedOrBlankNode>>::new();
    let mut distributions = HashMap::<NamedOrBlankNode, Vec<Distribution>>::new();
    let mut titles = HashMap::<_, String>::new();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(document.as_bytes()) {
        let quad = quad.map_err(|e| invalid(format!("invalid DCAT Turtle: {e}")))?;
        let subject = quad.subject;
        match quad.predicate.as_str().strip_prefix(DCAT) {
            Some("distribution") => {
                if let Ok(term) = NamedOrBlankNode::try_from(quad.object) {
                    datasets.entry(subject).or_default().push(term);
                }
            }
            Some("downloadURL") => {
                if let Term::NamedNode(url) = quad.object {
                    distributions
                        .entry(subject)
                        .or_default()
                        .push(distribution(url.into_string()));
                }
            }
            _ if quad.predicate.as_str() == "http://purl.org/dc/terms/title" => {
                if let Term::Literal(title) = quad.object {
                    let value = title.value().to_owned();
                    let current = titles.entry(subject).or_insert_with(|| value.clone());
                    if value < *current {
                        *current = value;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(datasets
        .into_iter()
        .filter_map(|(dataset, links)| {
            let NamedOrBlankNode::NamedNode(ref name) = dataset else {
                return None;
            };
            let id = name.as_str().to_owned();
            let files = links
                .iter()
                .filter_map(|link| distributions.get(link))
                .flatten()
                .cloned()
                .collect();
            Some(Candidate {
                title: titles.remove(&dataset).unwrap_or_else(|| id.clone()),
                id,
                version: None,
                namespace: None,
                distributions: files,
            })
        })
        .collect())
}

fn qleverfile(document: &str) -> Result<Vec<QleverInput>> {
    let mut index = false;
    let mut continuation = None;
    let mut settings: BTreeMap<&str, String> = BTreeMap::new();
    for raw in document.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            index = line == "[index]";
            continuation = None;
        } else if index {
            if raw.starts_with(char::is_whitespace)
                && let Some(key) = continuation
            {
                let value = settings.get_mut(key).expect("continuation has a setting");
                value.push(' ');
                value.push_str(line);
                continue;
            }
            continuation = None;
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if matches!(key, "INPUT_FILES" | "MULTI_INPUT_JSON") {
                    if settings.insert(key, value.trim().to_owned()).is_some() {
                        return Err(invalid(format!("duplicate Qleverfile setting {key}")));
                    }
                    continuation = Some(key);
                }
            }
        }
    }
    if let Some(inputs) = settings.get("MULTI_INPUT_JSON") {
        let value: Value = serde_json::from_str(&format!("[{inputs}]"))
            .map_err(|e| invalid(format!("invalid Qleverfile MULTI_INPUT_JSON: {e}")))?;
        return rows(&value, "Qleverfile input objects")?
            .iter()
            .map(|row| {
                let patterns = id(row, "for-each")?;
                let format = id(row, "format")?;
                if patterns.trim().is_empty() || format.trim().is_empty() {
                    return Err(invalid(
                        "Qleverfile input needs file patterns and an RDF format",
                    ));
                }
                let graph = string(row, "graph").filter(|s| s != "-");
                if let Some(g) = &graph {
                    super::iri("graph", g)?;
                }
                Ok(QleverInput {
                    files: patterns.split_whitespace().map(str::to_owned).collect(),
                    rdf_format: format,
                    graph,
                })
            })
            .collect();
    }
    let files = settings.get("INPUT_FILES").ok_or_else(|| {
        invalid("the Qleverfile needs INPUT_FILES or MULTI_INPUT_JSON in [index]")
    })?;
    if files.trim().is_empty() {
        return Err(invalid("Qleverfile INPUT_FILES needs at least one pattern"));
    }
    Ok(vec![QleverInput {
        files: files.split_whitespace().map(str::to_owned).collect(),
        rdf_format: String::new(),
        graph: None,
    }])
}

pub(crate) fn preview(request: Request) -> Result<Preview> {
    if request.document.len() > super::MAX_FILE_BYTES {
        return Err(super::Error::Limit(
            "the harvest document exceeds 16 MiB".into(),
        ));
    }
    let mut inputs = Vec::new();
    let mut candidates = match request.kind {
        Kind::Qleverfile => {
            inputs = qleverfile(&request.document)?;
            Vec::new()
        }
        Kind::Dcat => dcat(&request.document)?,
        kind => {
            let value =
                serde_json::from_str(&request.document).map_err(|e| invalid(e.to_string()))?;
            json(kind, &value)?
        }
    };
    if candidates.len() > super::MAX_RESOURCES || inputs.len() > super::MAX_LIST {
        return Err(super::Error::Limit(
            "too many entries in the harvest".into(),
        ));
    }
    candidates.sort_by(|a, b| (&a.id, &a.version).cmp(&(&b.id, &b.version)));
    for item in &mut candidates {
        super::list("distributions", &item.distributions)?;
        item.distributions.sort_by(|a, b| {
            (&a.url, &a.media_type, a.byte_size, &a.sha256).cmp(&(
                &b.url,
                &b.media_type,
                b.byte_size,
                &b.sha256,
            ))
        });
        item.distributions.dedup();
    }
    Ok(Preview { candidates, inputs })
}
