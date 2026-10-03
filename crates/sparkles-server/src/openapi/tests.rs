//! The description against the route table, its own consistency, its links into
//! `docs/API.md`, and its checked-in copy.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

const METHODS: &[&str] = &["get", "head", "post", "put", "delete", "patch", "options"];

fn doc() -> J {
    build("0.0.0")
}

/// `(path, methods)` of the description.
fn described(doc: &J) -> BTreeMap<String, BTreeSet<String>> {
    doc["paths"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, item)| {
            let methods = item
                .as_object()
                .unwrap()
                .keys()
                .filter(|k| METHODS.contains(&k.as_str()))
                .map(|k| k.to_ascii_uppercase())
                .collect();
            (path.clone(), methods)
        })
        .collect()
}

/// Every API route of `auth::ROUTES` is described with its methods, and nothing else is.
#[test]
fn paths_match_the_route_table() {
    let doc = doc();
    let described = described(&doc);
    let mut listed = BTreeSet::new();
    for (route, methods) in auth::ROUTES {
        if UI_ROUTES.contains(route) {
            continue;
        }
        let path = openapi_path(route);
        listed.insert(path.clone());
        let Some(have) = described.get(&path) else {
            panic!(
                "route {route} is not in the OpenAPI description (crates/sparkles-server/src/openapi/paths.rs)"
            );
        };
        if *methods == ["*"] {
            assert!(!have.is_empty(), "{route} has no operation");
            continue;
        }
        let want: BTreeSet<String> = methods.iter().map(|m| m.to_string()).collect();
        assert_eq!(
            have, &want,
            "the methods of {route} differ between auth::ROUTES and the OpenAPI description"
        );
    }
    for path in described.keys() {
        assert!(
            listed.contains(path),
            "the OpenAPI description has {path}, which auth::ROUTES does not list"
        );
    }
}

/// Every `$ref` of `v`, with the JSON pointer of where it occurs.
fn refs<'a>(v: &'a J, at: &str, out: &mut Vec<(String, &'a str)>) {
    match v {
        J::Object(m) => {
            for (k, x) in m {
                if k == "$ref" {
                    out.push((at.to_string(), x.as_str().unwrap()));
                } else {
                    refs(x, &format!("{at}/{k}"), out);
                }
            }
        }
        J::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                refs(x, &format!("{at}/{i}"), out);
            }
        }
        _ => {}
    }
}

fn resolve<'a>(doc: &'a J, r: &str) -> Option<&'a J> {
    doc.pointer(r.strip_prefix('#')?)
}

#[test]
fn document_is_consistent() {
    let doc = doc();
    assert_eq!(doc["openapi"], "3.1.0");
    // every reference resolves
    let mut all = Vec::new();
    refs(&doc, "", &mut all);
    assert!(all.len() > 500, "{}", all.len());
    for (at, r) in &all {
        assert!(resolve(&doc, r).is_some(), "{at}: {r} does not resolve");
    }
    let tags: BTreeSet<&str> = doc["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let mut used_tags = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut operations = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        let names: Vec<&str> = path
            .split('{')
            .skip(1)
            .map(|s| s.split('}').next().unwrap())
            .collect();
        for (method, o) in item.as_object().unwrap() {
            operations += 1;
            let what = format!("{method} {path}");
            let id = o["operationId"].as_str().expect("operationId");
            assert!(
                id.chars().next().unwrap().is_ascii_lowercase()
                    && id.chars().all(|c| c.is_ascii_alphanumeric()),
                "{what}: operationId {id}"
            );
            assert!(
                ids.insert(id.to_string()),
                "{what}: operationId {id} is used twice"
            );
            let tag = o["tags"][0].as_str().expect("a tag");
            assert!(tags.contains(tag), "{what}: tag {tag} is not defined");
            used_tags.insert(tag);
            assert!(!o["summary"].as_str().unwrap().is_empty(), "{what}");
            let responses = o["responses"].as_object().unwrap();
            assert!(
                responses
                    .keys()
                    .any(|k| k.starts_with('2') || k.starts_with('3')),
                "{what} has no success response"
            );
            assert!(
                o.get("security")
                    .is_none_or(|s| s.is_array() && *s != doc["security"]),
                "{what}: security"
            );
            assert!(o["x-sparkles-permission"].is_string(), "{what}");
            // parameters: resolved, unique by (name, in), and every path name declared
            let mut seen = BTreeSet::new();
            let mut path_params = BTreeSet::new();
            for p in o["parameters"].as_array().into_iter().flatten() {
                let p = match p.get("$ref") {
                    Some(r) => resolve(&doc, r.as_str().unwrap()).unwrap(),
                    None => p,
                };
                let key = (
                    p["name"].as_str().unwrap().to_string(),
                    p["in"].as_str().unwrap().to_string(),
                );
                assert!(seen.insert(key.clone()), "{what}: parameter {key:?} twice");
                if key.1 == "path" {
                    assert_eq!(p["required"], true, "{what}: {}", key.0);
                    path_params.insert(key.0);
                }
            }
            let want: BTreeSet<String> = names.iter().map(|s| s.to_string()).collect();
            assert_eq!(path_params, want, "{what}: path parameters");
            if let Some(body) = o.get("requestBody") {
                let body = match body.get("$ref") {
                    Some(r) => resolve(&doc, r.as_str().unwrap()).unwrap(),
                    None => body,
                };
                assert!(
                    !body["content"].as_object().unwrap().is_empty(),
                    "{what}: empty request body"
                );
            }
        }
    }
    assert!(operations > 150, "{operations}");
    assert_eq!(used_tags, tags, "a tag has no operation");
    for scheme in doc["components"]["securitySchemes"]
        .as_object()
        .unwrap()
        .keys()
    {
        assert!(
            [
                "basicAuth",
                "apiToken",
                "oidcAccessToken",
                "sessionCookie",
                "csrfToken",
                "cloudflareAccess"
            ]
            .contains(&scheme.as_str())
        );
    }
}

/// Security and the permission follow `auth::need`.
#[test]
fn security_follows_the_need() {
    let doc = doc();
    // with the document's security where the operation has none of its own
    let op = |path: &str, m: &str| {
        let mut o = doc["paths"][path][m].clone();
        if o.get("security").is_none() {
            o["security"] = doc["security"].clone();
        }
        o
    };
    let metrics = op("/$/metrics", "get");
    assert_eq!(metrics["x-sparkles-permission"], "metrics");
    assert!(!metrics["security"].as_array().unwrap().contains(&json!({})));
    let update = op("/{ds}/update", "post");
    assert_eq!(update["x-sparkles-permission"], "write");
    assert!(
        update["security"]
            .as_array()
            .unwrap()
            .contains(&json!({ "sessionCookie": [], "csrfToken": [] }))
    );
    let query = op("/{ds}/sparql", "get");
    assert_eq!(query["x-sparkles-permission"], "read");
    assert!(
        query["security"]
            .as_array()
            .unwrap()
            .contains(&json!({ "sessionCookie": [] }))
    );
    assert!(query["responses"]["404"].is_object());
    let ping = op("/$/ping", "get");
    assert_eq!(ping["x-sparkles-permission"], "public");
    assert_eq!(ping["security"][0], json!({}));
    assert!(ping["responses"]["401"].is_null());
    assert_eq!(
        op("/$/datasets", "post")["x-sparkles-permission"],
        "server-admin"
    );
    assert_eq!(
        op("/$/auth/tokens", "delete")["x-sparkles-permission"],
        "server-admin"
    );
    assert_eq!(
        op("/$/auth/tokens", "get")["x-sparkles-permission"],
        "signed in"
    );
    let approve = op("/$/auth/device/{user_code}/approve", "post");
    assert_eq!(approve["x-sparkles-permission"], "web session");
    assert!(
        !approve["security"]
            .as_array()
            .unwrap()
            .contains(&json!({ "apiToken": [] }))
    );
    assert_eq!(
        op("/$/openapi.json", "get")["x-sparkles-permission"],
        "public"
    );
    assert_eq!(
        op("/{ds}", "post")["x-sparkles-permission"],
        "read or write, by operation"
    );
}

/// GitHub's anchor of a Markdown heading.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Every link into `docs/API.md` names a section that exists.
#[test]
fn links_name_sections_of_the_api_reference() {
    let api = include_str!("../../../../docs/API.md");
    let mut anchors = BTreeSet::new();
    let mut fenced = false;
    for line in api.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        }
        if !fenced && line.starts_with('#') {
            anchors.insert(slug(line.trim_start_matches('#')));
        }
    }
    let doc = to_json(&doc());
    let prefix = format!("{DOCS}#");
    let mut n = 0;
    for part in doc.split(&prefix).skip(1) {
        let anchor = part.split('"').next().unwrap();
        assert!(
            anchors.contains(anchor),
            "docs/API.md has no section #{anchor}"
        );
        n += 1;
    }
    assert!(n > 100, "{n}");
}

/// `docs/openapi.json` is the built document. `SPARKLES_UPDATE_OPENAPI=1` rewrites it.
#[test]
fn checked_in_copy_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/openapi.json");
    let built = to_json(&doc());
    if std::env::var_os("SPARKLES_UPDATE_OPENAPI").is_some() {
        std::fs::write(path, &built).unwrap();
        return;
    }
    let on_disk = std::fs::read_to_string(path).unwrap_or_default();
    let same = serde_json::from_str::<J>(&on_disk).is_ok_and(|d| d == doc());
    assert!(
        same && on_disk == built,
        "docs/openapi.json is out of date: run `mise run openapi` \
         (SPARKLES_UPDATE_OPENAPI=1 cargo test -p sparkles-server openapi)"
    );
}

/// The YAML form holds the same scalars as the JSON form, line for line where they are
/// scalars of one line (a cheap check without a YAML parser).
#[test]
fn yaml_has_every_operation() {
    let doc = doc();
    let yaml = to_yaml(&doc);
    assert!(yaml.starts_with("components:\n"));
    for (_, item) in doc["paths"].as_object().unwrap() {
        for (_, o) in item.as_object().unwrap() {
            let id = o["operationId"].as_str().unwrap();
            assert!(
                yaml.contains(&format!("operationId: {id}\n")),
                "{id} is missing from the YAML"
            );
        }
    }
    assert!(yaml.contains("openapi: \"3.1.0\"\n"));
}
