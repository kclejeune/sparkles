//! The `forms` settings of a dataset (spec U02 §3.3), which a build without the `shacl`
//! feature reads too.

use oxrdf::NamedNode;
use serde::{Deserialize, Serialize};

/// Where a form's shapes come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapesSource {
    /// the shapes of the dataset's SHACL write guard
    #[default]
    Guard,
    /// the shapes graphs the settings list
    Graphs,
    /// shapes drafted from the data (C02), which nothing enforces
    Drafted,
}

impl ShapesSource {
    pub fn name(self) -> &'static str {
        match self {
            ShapesSource::Guard => "guard",
            ShapesSource::Graphs => "graphs",
            ShapesSource::Drafted => "drafted",
        }
    }

    pub fn parse(s: &str) -> Option<ShapesSource> {
        Some(match s {
            "guard" => ShapesSource::Guard,
            "graphs" => ShapesSource::Graphs,
            "drafted" => ShapesSource::Drafted,
            _ => return None,
        })
    }
}

/// Where saves go (spec U02 §2.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveTo {
    /// a session branch while `main` is protected, `main` otherwise
    #[default]
    Auto,
    Main,
    Branch,
}

fn default_template() -> String {
    "edits.{user}.{date}".into()
}

fn default_target() -> String {
    "type".into()
}

fn default_new_iri() -> String {
    "urn:uuid".into()
}

/// The `forms` settings of a dataset (spec U02 §3.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FormSettings {
    /// where the shapes come from
    #[serde(default)]
    pub shapes: ShapesSource,
    /// the shapes graphs of `shapes: "graphs"`
    #[serde(default)]
    pub graphs: Vec<String>,
    #[serde(default)]
    pub save_to: SaveTo,
    /// the name of a session branch: `{user}` and `{date}` (`yyyymmdd`) are replaced
    #[serde(default = "default_template")]
    pub branch_template: String,
    /// where new values go: `type` (the graph of the resource's `rdf:type` triple),
    /// `default`, or a graph IRI
    #[serde(default = "default_target")]
    pub target_graph: String,
    /// how new instances get their IRI: `urn:uuid`, or a template with `{uuid}`
    #[serde(default = "default_new_iri")]
    pub new_iri: String,
}

impl Default for FormSettings {
    fn default() -> FormSettings {
        FormSettings {
            shapes: ShapesSource::Guard,
            graphs: Vec::new(),
            save_to: SaveTo::Auto,
            branch_template: default_template(),
            target_graph: default_target(),
            new_iri: default_new_iri(),
        }
    }
}

impl FormSettings {
    /// Check the values (the C19 settings kind runs this).
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.shapes == ShapesSource::Graphs && self.graphs.is_empty() {
            return Err("shapes \"graphs\" needs graphs: the shapes graphs to read".into());
        }
        for g in &self.graphs {
            NamedNode::new(g.as_str()).map_err(|_| format!("graphs: {g:?} is not an IRI"))?;
        }
        let t = &self.branch_template;
        if t.is_empty() || t.len() > 64 {
            return Err("branchTemplate: 1 to 64 characters".into());
        }
        let sample = t.replace("{user}", "u").replace("{date}", "20260101");
        if !sample
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            || !sample.starts_with(|c: char| c.is_ascii_alphanumeric())
        {
            return Err(format!(
                "branchTemplate: {t:?} does not make a branch name; use letters, digits, '.', '_', '-', {{user}} and {{date}}"
            ));
        }
        match self.target_graph.as_str() {
            "type" | "default" => {}
            g => {
                NamedNode::new(g).map_err(|_| {
                    format!("targetGraph: \"type\", \"default\" or a graph IRI, not {g:?}")
                })?;
            }
        }
        if self.new_iri != "urn:uuid"
            && !(self.new_iri.contains("{uuid}")
                && NamedNode::new(self.new_iri.replace("{uuid}", "x")).is_ok())
        {
            return Err(format!(
                "newIri: \"urn:uuid\" or an IRI template with {{uuid}}, not {:?}",
                self.new_iri
            ));
        }
        Ok(())
    }

    /// A new IRI for a new instance, by `newIri`: `urn:uuid:` and a random UUID, or the
    /// template with `{uuid}` replaced by one.
    pub fn mint_iri(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        if self.new_iri == "urn:uuid" {
            format!("urn:uuid:{id}")
        } else {
            self.new_iri.replace("{uuid}", &id)
        }
    }

    /// The session branch's name for `user` on `date` (`yyyymmdd`), with characters a
    /// branch name cannot hold replaced by `-`.
    pub fn session_branch(&self, user: &str, date: &str) -> String {
        let user: String = user
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let name = self
            .branch_template
            .replace("{user}", if user.is_empty() { "anonymous" } else { &user })
            .replace("{date}", date);
        name.chars().take(64).collect()
    }
}
