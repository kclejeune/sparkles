//! The resource catalog of spec C30: resources, versions, licence reports, vocabulary
//! headers and prefix installs.

use super::kit::*;
use super::*;

const CLASSES: &[&str] = &[
    "open",
    "attribution",
    "share-alike",
    "restricted",
    "unknown",
];
const GIVEN_CLASSES: &[&str] = &["open", "attribution", "share-alike", "restricted"];

fn stamp() -> J {
    closed(
        &["by", "at"],
        json!({ "by": string(), "at": with_desc(string(), "An RFC 3339 time.") }),
    )
}

fn pin() -> J {
    closed(
        &["resource", "version"],
        json!({ "resource": string(), "version": string() }),
    )
}

/// The members of a resource that its writer gives.
fn resource_members() -> J {
    json!({
        "kind": string_enum(&["source", "vocabulary", "dataset"]),
        "title": string(),
        "description": string(),
        "homepage": with_desc(string(), "An absolute IRI."),
        "ownership": with_desc(string_enum(&["owned", "mirrored", "synced"]), "`owned`; this build refuses `mirrored` and `synced`."),
        "license": with_desc(string(), "An SPDX licence expression. Absent means no licence is known, which forbids redistribution."),
        "licenseClass": with_desc(string_enum(GIVEN_CLASSES), "The licence's class, needed for identifiers outside the built-in table and for `LicenseRef-`."),
        "licenseUrl": with_desc(string(), "The licence's page, for a licence outside the SPDX list."),
        "attribution": with_desc(string(), "The attribution text that the NOTICE carries."),
        "namespaces": with_desc(array(closed(&["prefix", "iri"], json!({ "prefix": string(), "iri": string() }))), "The namespaces a vocabulary owns. A prefix and a namespace belong to one vocabulary of the server."),
        "graphs": with_desc(array(closed(&["dataset", "graph"], json!({ "dataset": string(), "graph": string() }))), "The dataset graphs that hold a vocabulary."),
        "dataset": with_desc(string(), "The server dataset a dataset resource describes."),
    })
}

fn version_members() -> J {
    json!({
        "version": with_desc(string(), "1 to 64 letters, digits, `.`, `_`, `+` and `-`."),
        "issued": with_desc(string(), "A date (`YYYY-MM-DD`) or an RFC 3339 time."),
        "notes": string(),
        "license": with_desc(string(), "The version's SPDX licence expression, which overrides the resource's."),
        "licenseClass": string_enum(GIVEN_CLASSES),
        "licenseUrl": string(),
        "distributions": array(closed(&["url"], json!({
            "url": string(),
            "mediaType": string(),
            "byteSize": int(),
            "sha256": with_desc(string(), "The SHA-256 digest in hexadecimal."),
        }))),
        "imports": with_desc(array(pin()), "Pinned versions this version imports."),
        "graphs": with_desc(array(closed(&["graph"], json!({
            "graph": string(),
            "sources": with_desc(array(pin()), "The pinned source versions the graph was built from."),
        }))), "A dataset version's graphs."),
    })
}

fn merged(a: J, b: J) -> J {
    let mut a = a;
    if let (Some(m), J::Object(n)) = (a.as_object_mut(), b) {
        m.extend(n);
    }
    a
}

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    put(
        "ResourceVersionInput",
        doc(
            closed(&["version"], version_members()),
            "A version to add. It never changes once it is added.",
            "resource-catalog",
        ),
    );
    put(
        "ResourceVersion",
        doc(
            obj(
                &["version", "added"],
                merged(version_members(), json!({ "added": stamp() })),
            ),
            "A version, with who added it and when.",
            "resource-catalog",
        ),
    );
    put(
        "ResourceUpdate",
        doc(
            closed(
                &["kind", "title"],
                merged(
                    resource_members(),
                    json!({ "id": with_desc(string(), "The route's id, when given.") }),
                ),
            ),
            "The descriptive members of a resource.",
            "resource-catalog",
        ),
    );
    put(
        "ResourceCreate",
        doc(
            closed(
                &["id", "kind", "title"],
                merged(
                    resource_members(),
                    json!({
                        "id": with_desc(string(), "1 to 64 letters, digits, `.`, `_` and `-`, starting with a letter or digit."),
                        "versions": array(sref("ResourceVersionInput")),
                    }),
                ),
            ),
            "A resource to add, with any initial versions.",
            "resource-catalog",
        ),
    );
    put(
        "Resource",
        doc(
            obj(
                &[
                    "id",
                    "kind",
                    "title",
                    "ownership",
                    "versions",
                    "created",
                    "modified",
                ],
                merged(
                    resource_members(),
                    json!({
                        "id": string(),
                        "acceptance": with_desc(closed(&["by", "at", "licenses"], json!({
                            "by": string(),
                            "at": string(),
                            "licenses": with_desc(strings(), "The licences of the resource and its versions that were accepted."),
                        })), "An administrator's acceptance of restricted terms."),
                        "versions": array(sref("ResourceVersion")),
                        "created": stamp(),
                        "modified": stamp(),
                    }),
                ),
            ),
            "A resource of the catalog.",
            "resource-catalog",
        ),
    );
    put(
        "ResourceList",
        doc(
            obj(
                &["resources"],
                json!({ "resources": array(sref("Resource")) }),
            ),
            "The resources of the catalog.",
            "resource-catalog",
        ),
    );
    let obligation = obj(
        &["resource", "title", "version", "class"],
        json!({
            "resource": string(),
            "title": string(),
            "version": with_desc(string(), "Empty for a resource without versions."),
            "license": string(),
            "class": string_enum(CLASSES),
            "url": string(),
            "attribution": string(),
            "accepted": with_desc(boolean(), "For restricted terms, whether an administrator accepted them."),
        }),
    );
    put(
        "LicenseReport",
        doc(
            obj(
                &[
                    "resource",
                    "class",
                    "redistributable",
                    "policy",
                    "verdict",
                    "obligations",
                    "warnings",
                ],
                json!({
                    "resource": string(),
                    "version": or_null(string()),
                    "class": with_desc(string_enum(CLASSES), "The most restrictive class of the obligations."),
                    "redistributable": boolean(),
                    "policy": string_enum(&["warn", "enforce"]),
                    "verdict": with_desc(string_enum(&["allow", "warn", "refuse"]), "`refuse` only under `--license-policy enforce`."),
                    "obligations": array(obligation),
                    "graphs": array(obj(&["graph", "class", "redistributable", "obligations"], json!({
                        "graph": string(),
                        "class": string_enum(CLASSES),
                        "redistributable": boolean(),
                        "obligations": with_desc(strings(), "`resource version` keys of the obligations."),
                    }))),
                    "unaccepted": with_desc(strings(), "Resources with restricted terms that no administrator accepted."),
                    "warnings": strings(),
                }),
            ),
            "The licence obligations of a version and the gate's verdict.",
            "resource-licences",
        ),
    );
    put(
        "VocabularyHeader",
        doc(
            obj(
                &["resource", "version", "graphs"],
                json!({
                    "resource": string(),
                    "version": or_null(string()),
                    "graphs": array(obj(&["dataset", "graph"], json!({
                        "dataset": string(),
                        "graph": string(),
                        "found": boolean(),
                        "error": string(),
                        "header": string(),
                        "versionIRI": string(),
                        "versionInfo": string(),
                        "license": string(),
                        "title": string(),
                        "prefix": string(),
                        "namespace": string(),
                        "disagreements": strings(),
                    }))),
                }),
            ),
            "The header of each graph that holds a vocabulary.",
            "vocabulary-resources",
        ),
    );
    let binding = obj(
        &["prefix", "iri"],
        json!({ "prefix": string(), "iri": string() }),
    );
    put(
        "PrefixInstall",
        doc(
            obj(
                &["dataset", "installed", "replaced", "unchanged"],
                json!({
                    "dataset": string(),
                    "installed": array(binding.clone()),
                    "replaced": array(obj(&["prefix", "iri", "bound", "resource"], json!({
                        "prefix": string(),
                        "iri": string(),
                        "bound": with_desc(string(), "The IRI the dataset bound before."),
                        "resource": string(),
                    }))),
                    "unchanged": array(binding),
                }),
            ),
            "The prefixes an install bound.",
            "vocabulary-resources",
        ),
    );
}
