//! MCP access to the source manager using the same catalog and dataset operations as HTTP.

use super::Outcome;
use super::errors::ToolError;
use super::schemas::ToolDef;
use super::tools::{Tools, parse};
use crate::auth::ServerPerm;
use crate::resources::{Pin, harvest, load, manager, mint, recipe};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sparkles::guard::WriteOptions;

pub(super) const READ: &[&str] = &[
    "catalog_sources",
    "catalog_plan",
    "catalog_harvest",
    "catalog_import_lock",
    "catalog_manifest",
];
pub(super) const WRITE: &[&str] = &[
    "catalog_load",
    "catalog_reload",
    "catalog_install_bundle",
    "catalog_mint_iri",
];

fn schema(required: &[&str], properties: Value) -> Value {
    json!({"type":"object", "required":required, "properties":properties, "additionalProperties":false})
}

pub(super) fn tool_defs() -> Vec<ToolDef> {
    let dataset = super::schemas::ds();
    let string = || json!({"type":"string"});
    let object = || json!({"type":"object"});
    let seq = || json!({"type":"integer","minimum":0});
    let pin = || {
        schema(
            &["resource", "version"],
            json!({"resource":string(),"version":string()}),
        )
    };
    let recipe = || {
        schema(
            &["kind", "format", "inputs"],
            json!({
                "kind":{"const":"recipe"}, "format":{"const":1},
                "inputs":{"type":"array","items":schema(&["source", "graph"], json!({
                    "source":pin(),"graph":string(),"files":{"type":["string","null"]},
                    "rdfFormat":{"type":["string","null"]}
                }))},
                "prefixes":object(), "recommendedIndexes":object()
            }),
        )
    };
    let defs = [
        (
            "catalog_sources",
            "List catalog sources",
            "List server catalog sources with their versions. Requires server admin.",
            schema(&[], json!({})),
            false,
            false,
        ),
        (
            "catalog_plan",
            "Plan a catalog load",
            "Resolve a recipe's pinned files and graph targets. Requires server admin.",
            schema(&["recipe"], json!({"recipe":recipe()})),
            false,
            false,
        ),
        (
            "catalog_harvest",
            "Preview resource metadata",
            "Parse an uploaded public catalog document. Requires server admin.",
            schema(
                &["kind", "document"],
                json!({
                    "kind":{"enum":["obo-foundry","bioregistry","lov","ckan","databus","dcat","qleverfile"]},
                    "document":string()
                }),
            ),
            false,
            false,
        ),
        (
            "catalog_import_lock",
            "Read an import lock",
            "Get the transitive import pins and their digests. Requires server admin.",
            pin(),
            false,
            false,
        ),
        (
            "catalog_manifest",
            "Read a source manifest",
            "Read the provenance for a load commit. Requires server admin and full Query access.",
            schema(
                &["dataset", "commit"],
                json!({"dataset":dataset,"commit":seq()}),
            ),
            false,
            false,
        ),
        (
            "catalog_load",
            "Load pinned sources",
            "Verify pinned source files and replace every target graph in one commit. Requires server admin and Query and Upload grants on the target graphs.",
            schema(
                &["dataset", "recipe"],
                json!({"dataset":dataset,"recipe":recipe()}),
            ),
            true,
            true,
        ),
        (
            "catalog_reload",
            "Reload pinned sources",
            "Replace the graphs of a source manifest using the same checksum-pinned files. Requires server admin, full Query access and Upload grants on the target graphs.",
            schema(
                &["dataset", "commit"],
                json!({"dataset":dataset,"commit":seq()}),
            ),
            true,
            true,
        ),
        (
            "catalog_install_bundle",
            "Install a vocabulary bundle",
            "Replace each pinned vocabulary graph and install its prefixes on main. Requires server admin and Query and Upload grants on the target graphs.",
            schema(
                &["dataset", "resource", "version"],
                json!({"dataset":dataset,"resource":string(),"version":string()}),
            ),
            true,
            true,
        ),
        (
            "catalog_mint_iri",
            "Mint a vocabulary IRI",
            "Reserve an IRI according to the vocabulary policy on main. Requires full Query access and Update access to the reservation graph.",
            schema(
                &["dataset", "resource"],
                json!({"dataset":dataset,"resource":string(),"label":string()}),
            ),
            true,
            false,
        ),
    ];
    defs.into_iter()
        .map(
            |(name, title, description, input, write, destructive)| ToolDef {
                name,
                title,
                description,
                input,
                output: None,
                read_only: !write,
                open_world: destructive,
                destructive,
            },
        )
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeArgs {
    recipe: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadArgs {
    dataset: String,
    recipe: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestArgs {
    dataset: String,
    commit: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleArgs {
    dataset: String,
    resource: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MintArgs {
    dataset: String,
    resource: String,
    label: Option<String>,
}

fn result(value: impl Serialize) -> Result<Outcome, ToolError> {
    serde_json::to_value(value)
        .map(Outcome::Structured)
        .map_err(|e| ToolError::new("internal", 500, e.to_string()))
}

fn decode(value: &Value) -> Result<recipe::Recipe, ToolError> {
    recipe::decode(value.to_string().as_bytes()).map_err(catalog_error)
}

fn catalog_error(e: crate::resources::Error) -> ToolError {
    ToolError::from_api(e.into())
}

pub(super) fn listed(server: &super::McpServer, p: &crate::auth::Principal, name: &str) -> bool {
    if READ.contains(&name) {
        return p.has(ServerPerm::ServerAdmin);
    }
    (name == "catalog_mint_iri" || p.has(ServerPerm::ServerAdmin))
        && server.may_write_somewhere(&p.without_oauth_scopes())
}

pub(super) fn run(t: &Tools, name: &str, args: Map<String, Value>) -> Result<Outcome, ToolError> {
    let p = &t.call.principal;
    let st = &t.server.state;
    if name != "catalog_mint_iri" {
        manager::require_admin(p).map_err(ToolError::from_api)?;
    }
    let options = || -> Result<WriteOptions, ToolError> {
        crate::resources::http::writable(st).map_err(ToolError::from_api)?;
        if !t.cfg().allow_update {
            return Err(ToolError::new("forbidden", 403, "MCP writes are disabled"));
        }
        let timeout = st
            .limits
            .update_timeout
            .map_or(t.cfg().max_timeout, |cap| cap.min(t.cfg().max_timeout));
        Ok(WriteOptions {
            author: crate::http::author(p),
            deadline: Some(t.deadline(timeout)),
            cancel: Some(t.call.cancel.clone()),
            ..Default::default()
        })
    };
    let api = ToolError::from_api;
    let out = match name {
        "catalog_sources" => {
            if !args.is_empty() {
                return Err(ToolError::bad_argument(
                    "catalog_sources takes no arguments",
                ));
            }
            Outcome::Structured(manager::source_list(st).map_err(api)?)
        }
        "catalog_plan" => {
            let a: RecipeArgs = parse(args)?;
            let catalog = st.resources.read().map_err(catalog_error)?;
            result(recipe::plan(&catalog, &decode(&a.recipe)?).map_err(catalog_error)?)?
        }
        "catalog_harvest" => result(harvest::preview(parse(args)?).map_err(catalog_error)?)?,
        "catalog_import_lock" => {
            let pin: Pin = parse(args)?;
            let catalog = st.resources.read().map_err(catalog_error)?;
            result(recipe::lock(&catalog, &pin).map_err(catalog_error)?)?
        }
        "catalog_manifest" | "catalog_reload" => {
            let a: ManifestArgs = parse(args)?;
            let ds = t.dataset(Some(&a.dataset))?;
            manager::require_manifest_reader(p, &ds).map_err(api)?;
            let manifest = load::read(st, &ds, a.commit).map_err(api)?;
            if name == "catalog_manifest" {
                result(manifest)?
            } else {
                let loaded = load::execute(
                    st,
                    &ds,
                    p,
                    manifest.recipe,
                    Some((a.commit, manifest.plan)),
                    options()?,
                )
                .map_err(api)?;
                return result(loaded.receipt);
            }
        }
        "catalog_load" => {
            let a: LoadArgs = parse(args)?;
            let ds = t.dataset(Some(&a.dataset))?;
            let loaded =
                load::execute(st, &ds, p, decode(&a.recipe)?, None, options()?).map_err(api)?;
            return result(loaded.receipt);
        }
        "catalog_install_bundle" => {
            let a: BundleArgs = parse(args)?;
            let ds = t.dataset(Some(&a.dataset))?;
            let pin = Pin {
                resource: a.resource,
                version: a.version,
            };
            let catalog = st.resources.read().map_err(catalog_error)?;
            let bundle = recipe::bundle(&catalog, &pin).map_err(catalog_error)?;
            let loaded = load::execute(st, &ds, p, bundle, None, options()?).map_err(api)?;
            return result(loaded.receipt);
        }
        "catalog_mint_iri" => {
            let a: MintArgs = parse(args)?;
            let ds = t.dataset(Some(&a.dataset))?;
            let reservation = mint::reserve(
                st,
                &ds,
                p,
                &a.resource,
                mint::Request { label: a.label },
                options()?,
            )
            .map_err(api)?;
            return result(reservation);
        }
        _ => unreachable!("catalog tool not registered"),
    };
    if let Outcome::Structured(value) = &out {
        super::tools::evidence_bytes(value, t.cfg().max_bytes, &t.ctx(&[], 0.0))?;
    }
    Ok(out)
}
