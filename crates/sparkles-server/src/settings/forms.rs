//! The `forms` settings kind of a dataset (spec U02 §3.3): where editing forms read their
//! shapes, where saves go, the name of a session branch, the graph new values go into and
//! the IRIs of new instances, kept in `<db>/forms.json`. The type and its checks are the
//! library's [`sparkles::forms::FormSettings`].

use crate::settings::{Providers, Typed};
pub use sparkles::forms::FormSettings;

/// The file in a dataset's directory that keeps the runtime layer.
pub const FORMS_FILE: &str = "forms.json";

/// The kind's top-level members.
pub const MEMBERS: &[&str] = &[
    "shapes",
    "graphs",
    "saveTo",
    "branchTemplate",
    "targetGraph",
    "newIri",
];

impl Typed for FormSettings {
    fn validate(&self, _: Providers) -> Result<(), String> {
        FormSettings::validate(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_and_members() {
        let d = serde_json::to_value(FormSettings::default()).unwrap();
        assert_eq!(
            d,
            json!({
                "shapes": "guard", "graphs": [], "saveTo": "auto",
                "branchTemplate": "edits.{user}.{date}", "targetGraph": "type", "newIri": "urn:uuid"
            })
        );
        let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
        let mut members = MEMBERS.to_vec();
        members.sort();
        let mut keys = keys;
        keys.sort();
        assert_eq!(keys, members);
    }
}
