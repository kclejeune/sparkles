//! The `draft_shapes` tool: SHACL shapes or a ShEx schema drafted from a dataset's data
//! (`sparkles::schema::draft`), with the counts of what each constraint would exclude.

use super::Outcome;
use super::errors::ToolError;
use super::render::Prefixes;
use super::tools::{Tools, bounded, dataset_prefixes, parse, parse_iri};
use crate::http::INFERRED_GRAPH;
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::schema::draft::{
    DEFAULT_MAX_COUNT, DEFAULT_MAX_IN, DraftOptions, TRACKED_VALUES, default_base,
};
use sparkles::schema::{GraphSelection, SchemaOptions, draft_shapes};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DraftArgs {
    dataset: Option<String>,
    graph: Option<String>,
    reasoning: Option<bool>,
    language: Option<Language>,
    shapes_format: Option<ShapesFormat>,
    support: Option<f64>,
    classes: Option<Vec<String>>,
    min_instances: Option<u64>,
    max_in: Option<u64>,
    max_count: Option<u64>,
    closed: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Language {
    Shacl,
    Shex,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ShapesFormat {
    Turtle,
    Shaclc,
}

impl Tools<'_> {
    pub(super) fn draft_shapes(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: DraftArgs = parse(args)?;
        if a.language == Some(Language::Shex) && a.shapes_format.is_some() {
            return Err(ToolError::bad_argument(
                "shapesFormat applies to SHACL drafts, not ShEx",
            ));
        }
        let support = a.support.unwrap_or(1.0);
        if !(support > 0.0 && support <= 1.0) {
            return Err(ToolError::bad_argument("support must be in (0, 1]"));
        }
        let max_in = bounded(
            "maxIn",
            a.max_in,
            DEFAULT_MAX_IN as u64,
            0,
            TRACKED_VALUES as u64,
        )? as usize;
        let max_count = bounded("maxCount", a.max_count, DEFAULT_MAX_COUNT, 0, u64::MAX >> 1)?;
        let min_instances = bounded("minInstances", a.min_instances, 1, 1, u64::MAX >> 1)?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        self.info_endpoint(&ds.name)?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let graph = match a.graph.as_deref().map(str::trim) {
            None | Some("default") => GraphSelection::Default,
            Some("union") => GraphSelection::Union,
            Some(g) => match parse_iri(g, &prefix_map, false)? {
                Term::NamedNode(n) => {
                    GraphSelection::parse(n.as_str()).map_err(ToolError::bad_argument)?
                }
                _ => {
                    return Err(ToolError::bad_argument(
                        "graph must be default, union or a graph IRI",
                    ));
                }
            },
        };
        let mut classes = Vec::new();
        for c in a.classes.unwrap_or_default() {
            match parse_iri(&c, &prefix_map, false)? {
                Term::NamedNode(n) => classes.push(n.into_string()),
                _ => return Err(ToolError::bad_argument("classes must be IRIs")),
            }
        }
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?;
        let opts = DraftOptions {
            schema: SchemaOptions {
                graph,
                inferred_graph: Some(INFERRED_GRAPH.to_string()),
                // drafts leave inferences out unless asked, as write-time validation does
                include_inferred: a.reasoning == Some(true) && ds.reasoning.read().is_some(),
                deadline: Some(self.call.arrived + timeout),
                cancel: Some(self.call.cancel.clone()),
                max_entries: self.server.state.schema_max_entries,
                graphs: self
                    .call
                    .principal
                    .view(&ds.name, crate::auth::Endpoint::Info),
                ..Default::default()
            },
            dataset: ds.name.clone(),
            support,
            classes,
            min_instances,
            max_in,
            max_count,
            closed: a.closed.unwrap_or(false),
            base: default_base(&ds.name),
            prefixes: prefix_map.into_iter().collect(),
        };
        let draft = draft_shapes(&snap, &opts).map_err(|e| ctx.schema(e))?;
        let language = a.language.unwrap_or(Language::Shacl);
        let shapes: Vec<Value> = draft
            .shapes
            .iter()
            .map(|s| {
                let excluding: Vec<Value> = s
                    .properties
                    .iter()
                    .flat_map(|p| {
                        p.constraints
                            .iter()
                            .filter(|c| c.excluded > 0)
                            .map(move |c| json!({"path": p.path, "component": c.component, "excluded": c.excluded}))
                    })
                    .collect();
                json!({
                    "class": s.class,
                    "shape": s.shape,
                    "instances": s.instances,
                    "properties": s.properties.len(),
                    "constraints": s.properties.iter().map(|p| p.constraints.len()).sum::<usize>(),
                    "excluding": excluding,
                })
            })
            .collect();
        let mut out = json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "graph": draft.selection.graph,
            "support": support,
            "language": if language == Language::Shacl { "shacl" } else { "shex" },
            "totals": draft.totals,
            "shapes": shapes,
        });
        match language {
            Language::Shacl => {
                let format = a.shapes_format.unwrap_or(ShapesFormat::Turtle);
                out["shapesFormat"] = json!(match format {
                    ShapesFormat::Turtle => "turtle",
                    ShapesFormat::Shaclc => "shaclc",
                });
                out["shacl"] = Value::String(match format {
                    ShapesFormat::Turtle => draft.shacl,
                    ShapesFormat::Shaclc => draft.shaclc,
                });
            }
            Language::Shex => {
                out["shex"] = Value::String(draft.shex);
                out["shapeMap"] = Value::String(draft.shape_map);
            }
        }
        Ok(Outcome::Structured(out))
    }
}
