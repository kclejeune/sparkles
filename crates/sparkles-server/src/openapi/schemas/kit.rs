//! Small builders the schema modules share.

use super::*;

pub(super) fn int() -> J {
    json!({ "type": "integer" })
}
pub(super) fn num() -> J {
    json!({ "type": "number" })
}
pub(super) fn string() -> J {
    json!({ "type": "string" })
}
pub(super) fn boolean() -> J {
    json!({ "type": "boolean" })
}
pub(super) fn strings() -> J {
    array(string())
}
/// A schema or `null`.
pub(super) fn or_null(v: J) -> J {
    json!({ "oneOf": [v, { "type": "null" }] })
}
/// A link to `docs/API.md` at `anchor`, and a description.
pub(super) fn doc(mut v: J, description: &str, anchor: &str) -> J {
    v["description"] = description.into();
    v["externalDocs"] = json!({ "url": api_doc(anchor) });
    v
}
/// An object with exactly these members, of which `required` must be present.
pub(super) fn closed(required: &[&str], props: J) -> J {
    let mut o = obj(required, props);
    o["additionalProperties"] = J::Bool(false);
    o
}
/// A free-form object, with a description.
pub(super) fn any_object(description: &str) -> J {
    json!({ "type": "object", "additionalProperties": true, "description": description })
}
