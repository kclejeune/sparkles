//! Declarative source selection and reproducible import locks.

use super::{CatalogFile, Distribution, Error, Kind, Pin, Resource, Result, Version, find};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use sparkles::io::{RdfSyntax, format_for_media_type};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The version of a recipe, import lock or manifest. Deserialize before using any
/// document members, and reject unknown versions even when the rest could be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub(crate) struct Format;

impl TryFrom<u32> for Format {
    type Error = String;

    fn try_from(value: u32) -> std::result::Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err(format!("unsupported document format {value}; expected 1"))
        }
    }
}

impl From<Format> for u32 {
    fn from(_: Format) -> Self {
        1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DocumentKind {
    Recipe,
    LoadPlan,
    ImportLock,
    LoadManifest,
}

pub(crate) trait Document: DeserializeOwned {
    const KIND: DocumentKind;
}

/// Interpret the envelope before deserializing any version-specific body fields.
pub(crate) fn decode<T: Document>(bytes: &[u8]) -> Result<T> {
    #[derive(Deserialize)]
    struct FormatHeader {
        format: Format,
    }
    #[derive(Deserialize)]
    struct KindHeader {
        kind: DocumentKind,
    }
    let FormatHeader { format: Format } =
        serde_json::from_slice(bytes).map_err(|e| super::invalid(e.to_string()))?;
    let KindHeader { kind } =
        serde_json::from_slice(bytes).map_err(|e| super::invalid(e.to_string()))?;
    if kind != T::KIND {
        return Err(super::invalid(format!(
            "expected a {:?} document, got {kind:?}",
            T::KIND
        )));
    }
    serde_json::from_slice(bytes).map_err(|e| super::invalid(e.to_string()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Recipe {
    pub format: Format,
    pub kind: DocumentKind,
    pub inputs: Vec<Input>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prefixes: BTreeMap<String, String>,
    /// Recommendations shown in the plan. Setting indexes is a separate operation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recommended_indexes: BTreeMap<String, String>,
}

impl Document for Recipe {
    const KIND: DocumentKind = DocumentKind::Recipe;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Input {
    pub source: Pin,
    pub graph: String,
    /// Matches the last URL path segment. Without a pattern, select every distribution.
    pub files: Option<String>,
    pub rdf_format: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct File {
    pub graph: String,
    pub source: Pin,
    pub url: String,
    pub rdf_format: String,
    pub sha256: String,
    pub byte_size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Plan {
    pub format: Format,
    pub kind: DocumentKind,
    pub files: Vec<File>,
    pub prefixes: BTreeMap<String, String>,
    pub recommended_indexes: BTreeMap<String, String>,
}

impl Document for Plan {
    const KIND: DocumentKind = DocumentKind::LoadPlan;
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportLock {
    pub format: Format,
    pub kind: DocumentKind,
    pub root: LockedPin,
    pub imports: Vec<LockedPin>,
}

impl Document for ImportLock {
    const KIND: DocumentKind = DocumentKind::ImportLock;
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedPin {
    pub pin: Pin,
    pub sha256: String,
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn resolve<'a>(
    catalog: &'a CatalogFile,
    pin: &Pin,
) -> Result<(&'a Resource, &'a Version)> {
    let resource = find(catalog, &pin.resource).ok_or_else(|| super::no_such(&pin.resource))?;
    let version = resource
        .version(&pin.version)
        .ok_or_else(|| super::no_such_version(&pin.resource, &pin.version))?;
    Ok((resource, version))
}

/// The digest identifies the immutable version record. It excludes the local stamp.
fn locked(catalog: &CatalogFile, pin: &Pin) -> Result<LockedPin> {
    let (_, version) = resolve(catalog, pin)?;
    let bytes = canonical_version(version)?;
    Ok(LockedPin {
        pin: pin.clone(),
        sha256: hash(&bytes),
    })
}

/// Digest format 1. Its ordered tuples fix the field positions independently of
/// the resource file's version and JSON member names. Sort all unordered lists.
fn canonical_version(version: &Version) -> Result<Vec<u8>> {
    let v = &version.input;
    let mut distributions = v
        .distributions
        .iter()
        .map(|d| (&d.url, &d.media_type, d.byte_size, &d.sha256))
        .collect::<Vec<_>>();
    distributions.sort_unstable();
    let mut imports = v
        .imports
        .iter()
        .map(|pin| (&pin.resource, &pin.version))
        .collect::<Vec<_>>();
    imports.sort_unstable();
    let mut graphs = v
        .graphs
        .iter()
        .map(|graph| {
            let mut pins = graph
                .sources
                .iter()
                .map(|pin| (&pin.resource, &pin.version))
                .collect::<Vec<_>>();
            pins.sort_unstable();
            (&graph.graph, pins)
        })
        .collect::<Vec<_>>();
    graphs.sort_unstable();
    serde_json::to_vec(&(
        "sparkles-version-digest-1",
        &v.version,
        (&v.version_iri, &v.snapshot, &v.issued, &v.notes),
        (&v.license, &v.license_class, &v.license_url),
        distributions,
        imports,
        graphs,
    ))
    .map_err(|e| Error::Io(e.to_string()))
}

/// Visit every import once. The ordered set makes the output independent of traversal.
pub(crate) fn import_pins(catalog: &CatalogFile, root: &Pin) -> Result<BTreeSet<Pin>> {
    let (_, version) = resolve(catalog, root)?;
    let mut imports = BTreeSet::new();
    let mut versions = BTreeMap::<&str, BTreeSet<&str>>::new();
    versions
        .entry(&root.resource)
        .or_default()
        .insert(&root.version);
    let mut pending: Vec<&Pin> = version.input.imports.iter().collect();
    while let Some(pin) = pending.pop() {
        if imports.insert(pin.clone()) {
            versions
                .entry(&pin.resource)
                .or_default()
                .insert(&pin.version);
            if imports.len() > super::MAX_RESOURCES {
                return Err(Error::Limit("too many transitive imports".into()));
            }
            let (_, imported) = resolve(catalog, pin)?;
            pending.extend(&imported.input.imports);
        }
    }
    let conflicts = versions
        .into_iter()
        .filter(|(_, versions)| versions.len() > 1)
        .map(|(id, versions)| {
            format!(
                "{id} ({})",
                versions.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        return Err(Error::Conflict(
            "import-version-conflict",
            format!(
                "imports pin conflicting versions of {}",
                conflicts.join("; ")
            ),
        ));
    }
    Ok(imports)
}

pub(crate) fn lock(catalog: &CatalogFile, root: &Pin) -> Result<ImportLock> {
    Ok(ImportLock {
        format: Format,
        kind: DocumentKind::ImportLock,
        root: locked(catalog, root)?,
        imports: import_pins(catalog, root)?
            .iter()
            .map(|pin| locked(catalog, pin))
            .collect::<Result<_>>()?,
    })
}

fn syntax(input: &Input, d: &Distribution, name: &str) -> Result<RdfSyntax> {
    let format = if let Some(explicit) = &input.rdf_format {
        RdfSyntax::from_name(explicit)
    } else {
        d.media_type
            .as_deref()
            .and_then(format_for_media_type)
            .map(RdfSyntax::from)
            .or_else(|| RdfSyntax::from_path(Path::new(name)))
    };
    format
        .filter(|f| f.rdf().is_some() && !f.supports_datasets())
        .ok_or_else(|| super::invalid(format!("{} needs a single-graph RDF syntax", d.url)))
}

/// Plan a load without fetching anything. Each step names explicit graph targets and
/// verifies that an input selects at least one immutable, checksum-pinned file.
pub(crate) fn plan(catalog: &CatalogFile, recipe: &Recipe) -> Result<Plan> {
    super::list("inputs", &recipe.inputs)?;
    if recipe.inputs.is_empty() {
        return Err(super::invalid("a recipe needs at least one input"));
    }
    if recipe.prefixes.len() > super::MAX_LIST || recipe.recommended_indexes.len() > super::MAX_LIST
    {
        return Err(Error::Limit(
            "too many prefixes or index recommendations".into(),
        ));
    }
    for (name, iri) in &recipe.prefixes {
        sparkles::store::check_prefix(name, iri).map_err(|e| super::invalid(e.to_string()))?;
    }
    let mut files = BTreeMap::new();
    for input in &recipe.inputs {
        super::iri("input.graph", &input.graph)?;
        if input
            .graph
            .get(.."urn:x-sparkles:".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:x-sparkles:"))
        {
            return Err(super::invalid(
                "recipes cannot replace a reserved Sparkles graph",
            ));
        }
        if let Some(glob) = &input.files {
            super::text("input.files", glob, 256, true)?;
        }
        let (resource, version) = resolve(catalog, &input.source)?;
        if resource.input.kind == Kind::Dataset {
            return Err(super::invalid(
                "recipe inputs must be sources or vocabularies",
            ));
        }
        let mut matched = false;
        for d in &version.input.distributions {
            let url = url::Url::parse(&d.url)
                .map_err(|e| super::invalid(format!("distribution URL: {e}")))?;
            let filename = url.path_segments().and_then(Iterator::last).unwrap_or("");
            if input
                .files
                .as_ref()
                .is_some_and(|glob| !sparkles::access::glob(glob, filename))
            {
                continue;
            }
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(super::invalid(
                    "recipe distributions need HTTP(S) URLs without user credentials or fragments",
                ));
            }
            let format = syntax(input, d, filename)?;
            let sha256 = d.sha256.clone().ok_or_else(|| {
                super::invalid(format!(
                    "distribution {} has no pinned SHA-256 digest",
                    d.url
                ))
            })?;
            let file = File {
                graph: input.graph.clone(),
                source: input.source.clone(),
                url: d.url.clone(),
                rdf_format: format.media_type().into(),
                sha256,
                byte_size: d.byte_size,
            };
            if files
                .insert((&input.graph, &input.source, &d.url), file)
                .is_some()
            {
                return Err(super::invalid(
                    "a recipe selected the same distribution twice for one graph",
                ));
            }
            if files.len() > super::MAX_LIST {
                return Err(Error::Limit("too many files in the recipe".into()));
            }
            matched = true;
        }
        if !matched {
            return Err(super::invalid(format!(
                "{} {} has no matching distributions",
                input.source.resource, input.source.version
            )));
        }
    }
    Ok(Plan {
        format: Format,
        kind: DocumentKind::LoadPlan,
        files: files.into_values().collect(),
        prefixes: recipe.prefixes.clone(),
        recommended_indexes: recipe.recommended_indexes.clone(),
    })
}

pub(crate) fn from_version(catalog: &CatalogFile, root: &Pin) -> Result<Recipe> {
    let (resource, version) = resolve(catalog, root)?;
    if resource.input.kind != Kind::Dataset {
        return Err(super::invalid("graph recipes belong to dataset versions"));
    }
    Ok(Recipe {
        format: Format,
        kind: DocumentKind::Recipe,
        inputs: version
            .input
            .graphs
            .iter()
            .flat_map(|graph| {
                graph.sources.iter().map(|pin| Input {
                    source: pin.clone(),
                    graph: graph.graph.clone(),
                    files: None,
                    rdf_format: None,
                })
            })
            .collect(),
        prefixes: BTreeMap::new(),
        recommended_indexes: BTreeMap::new(),
    })
}

/// A vocabulary bundle includes every pinned vocabulary import and binds its
/// namespaces to the destination dataset. Each version has a stable named graph.
pub(crate) fn bundle(catalog: &CatalogFile, root: &Pin) -> Result<Recipe> {
    let mut pins = import_pins(catalog, root)?;
    pins.insert(root.clone());
    let mut recipe = Recipe {
        format: Format,
        kind: DocumentKind::Recipe,
        inputs: Vec::new(),
        prefixes: BTreeMap::new(),
        recommended_indexes: BTreeMap::new(),
    };
    for pin in pins {
        let (resource, _) = resolve(catalog, &pin)?;
        if resource.input.kind != Kind::Vocabulary {
            return Err(super::invalid(
                "a vocabulary bundle may contain only pinned vocabulary versions",
            ));
        }
        for namespace in &resource.input.namespaces {
            recipe
                .prefixes
                .insert(namespace.prefix.clone(), namespace.iri.clone());
        }
        recipe.inputs.push(Input {
            graph: format!("urn:sparkles-vocabulary:{}:{}", pin.resource, pin.version),
            source: pin,
            rdf_format: None,
            files: None,
        });
    }
    Ok(recipe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{Stamp, VersionInput};

    #[test]
    fn digest_v1_vectors_cover_earlier_catalog_formats() {
        let mut version = Version {
            input: VersionInput {
                version: "1".into(),
                ..Default::default()
            },
            added: Stamp {
                at: "ignored".into(),
                by: "ignored".into(),
            },
        };
        assert_eq!(
            hash(&canonical_version(&version).unwrap()),
            "c9952228227abd8854cd72ebb118b5369cc6069437d646d7d846363d4d725d8e"
        );
        version.input.version_iri = Some("urn:example:release:1".into());
        version.input.snapshot = Some("commit:7".into());
        assert_eq!(
            hash(&canonical_version(&version).unwrap()),
            "4fa82f8577b063bb60e3e952b78aaf87a21fe48eff30219db7cffe3b4ab676eb"
        );
    }
}
