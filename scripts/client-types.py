#!/usr/bin/env python3
"""Generate the Rust client's admin response models from checked-in OpenAPI.

This deliberately supports only the schema shapes needed by ROOTS. Unknown shapes
fail generation instead of silently becoming Value. String enums stay strings for
forward compatibility. Numeric/string constraints are documentation, not validators.
"""

import argparse
import difflib
import json
import hashlib
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "crates/sparkles-client/src/types/generated.rs"
ROOTS = (
    "SchemaSummary", "DatasetStats", "FusekiStats", "StoredQuery",
    "StoredQueryList", "BackupList", "BackupFiles",
    "SnapshotList", "NamedSnapshot", "SnapshotRequest", "BranchList", "Branch", "BranchCreate", "Diff",
)


def pascal(name):
    return "".join(word[0].upper() + word[1:] for word in re.split(r"[^a-zA-Z0-9]", name) if word)


def snake(name):
    name = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", name)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
    name = re.sub(r"[^a-zA-Z0-9_]", "_", name).lower()
    if name[0].isdigit():
        name = "field_" + name
    # Raw identifiers cannot express self, Self, super or crate.
    if name in {"self", "super", "crate"}:
        return name + "_"
    if name in {"as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern", "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static", "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield"}:
        return "r#" + name
    return name


class Generator:
    def __init__(self, schemas):
        self.schemas = schemas
        self.definitions = {}
        self.shapes = {}

    def model(self, name, schema):
        if name in self.shapes:
            if self.shapes[name] != schema:
                raise ValueError(f"generated type name collision: {name}")
            return name
        allowed = {"type", "properties", "required", "additionalProperties", "oneOf", "anyOf", "description", "externalDocs", "title", "deprecated", "examples"}
        if unknown := schema.keys() - allowed:
            raise ValueError(f"{name}: unsupported named model keywords {sorted(unknown)}")
        self.shapes[name] = schema
        self.definitions[name] = ""  # Reserve before following references.
        self.definitions[name] = self.definition(name, schema)
        return name

    def rust_type(self, schema, name):
        if not isinstance(schema, dict):
            raise ValueError(f"{name}: expected a schema object")
        supported = {"$ref", "type", "properties", "required", "items", "additionalProperties", "oneOf", "anyOf", "description", "externalDocs", "enum", "const", "format", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "minItems", "maxItems", "minLength", "maxLength", "pattern", "default", "examples", "title", "deprecated", "readOnly", "writeOnly"}
        if unknown := schema.keys() - supported:
            raise ValueError(f"{name}: unsupported schema keywords {sorted(unknown)}")
        if any(key in schema for key in ("allOf", "not", "if", "then", "else", "patternProperties", "prefixItems")):
            raise ValueError(f"{name}: unsupported schema composition")
        if "$ref" in schema:
            ref = schema["$ref"]
            if not ref.startswith("#/components/schemas/") or len(schema) != 1:
                raise ValueError(f"{name}: unsupported reference {schema}")
            target = ref.rsplit("/", 1)[1]
            return self.model(target, self.schemas[target])
        for key in ("oneOf", "anyOf"):
            if key in schema:
                choices = schema[key]
                nonnull = [choice for choice in choices if choice.get("type") != "null"]
                if len(nonnull) == 1 and len(choices) == 2:
                    return f"Option<{self.rust_type(nonnull[0], name)}>"
                return self.model(name, schema)
        kind = schema.get("type")
        if isinstance(kind, list):
            if len(kind) == 2 and set(kind) in ({"string", "integer"}, {"boolean", "string"}):
                return self.model(name, schema)
            if len(kind) != 2 or "null" not in kind:
                raise ValueError(f"{name}: unsupported type union {kind}")
            child = dict(schema, type=next(value for value in kind if value != "null"))
            return f"Option<{self.rust_type(child, name)}>"
        if kind is None and "const" in schema:
            value = schema["const"]
            kind = "boolean" if isinstance(value, bool) else "integer" if isinstance(value, int) else "string" if isinstance(value, str) else None
        if kind in {"string", "integer", "number", "boolean", "null"}:
            return {"string": "String", "integer": "i128", "number": "f64", "boolean": "bool", "null": "()"}[kind]
        if kind == "array":
            return f"Vec<{self.rust_type(schema['items'], name + 'Item')}>"
        if kind == "object":
            if schema.get("properties"):
                return self.model(name, schema)
            additional = schema.get("additionalProperties", True)
            if isinstance(additional, dict):
                return f"std::collections::BTreeMap<String, {self.rust_type(additional, name + 'Value')}>"
            if additional is True:
                return "serde_json::Map<String, serde_json::Value>"
            raise ValueError(f"{name}: unsupported closed map")
        raise ValueError(f"{name}: unsupported schema {schema}")

    def definition(self, name, schema):
        lines = [f"/// OpenAPI model `{name}`. Generated members retain unknown fields.", "#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]"]
        kinds = schema.get("type")
        if isinstance(kinds, list) and set(kinds) in ({"string", "integer"}, {"boolean", "string"}):
            lines += ["#[serde(untagged)]", f"pub enum {name} {{"]
            for kind in kinds:
                if kind == "integer":
                    lines.append('    Integer(#[serde(deserialize_with = "deserialize_integer")] i128),')
                else:
                    lines.append(f"    {kind.title()}({dict(string='String', boolean='bool')[kind]}),")
            return "\n".join(lines + ["}"])
        choices = schema.get("anyOf", schema.get("oneOf"))
        if choices:
            # Prefer specific alternatives so a general page cannot swallow a dataset
            # listing into its extra map before the typed dataset variant is tried.
            ordered = sorted(enumerate(choices, 1), key=lambda item: -len(item[1].get("required", [])))
            lines += ["#[serde(untagged)]", f"pub enum {name} {{"]
            for index, choice in ordered:
                variant = f"Alternative{index}"
                lines.append(f"    {variant}({self.rust_type(choice, name + variant)}),")
            return "\n".join(lines + ["}"])
        if schema.get("type") != "object" or not schema.get("properties"):
            raise ValueError(f"{name}: named models must be objects or alternatives")
        lines.append(f"pub struct {name} {{")
        required = set(schema.get("required", []))
        used = {"extra"}
        for key, child in sorted(schema["properties"].items()):
            field = snake(key)
            if field in used:
                raise ValueError(f"{name}: field name collision: {key}")
            used.add(field)
            ty = self.rust_type(child, name + pascal(key))
            if key not in required:
                if not ty.startswith("Option<"):
                    ty = f"Option<{ty}>"
                lines.append('    #[serde(default, skip_serializing_if = "Option::is_none")]')
            if ty == "i128":
                lines.append('    #[serde(deserialize_with = "deserialize_integer")]')
            elif ty == "Option<i128>":
                lines.append('    #[serde(deserialize_with = "deserialize_optional_integer")]')
            elif key in required and ty.startswith("Option<"):
                lines.append('    #[serde(deserialize_with = "deserialize_required_nullable")]')
            if field.removeprefix("r#") != key:
                lines.append(f"    #[serde(rename = {json.dumps(key)})]")
            lines.append(f"    pub {field}: {ty},")
        lines += ["    #[serde(flatten)]", "    pub extra: serde_json::Map<String, serde_json::Value>,", "}"]
        return "\n".join(lines)

    def generate(self):
        for name in ROOTS:
            self.model(name, self.schemas[name])
        selected = {name: self.schemas[name] for name in self.shapes if name in self.schemas}
        digest = hashlib.sha256(json.dumps(selected, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        source = "// Generated by scripts/client-types.py. Run mise run client:types.\n"
        source += f"// Selected OpenAPI schemas SHA-256: {digest}\n"
        source += "// String enums remain strings; open objects remain JSON maps.\n"
        source += "// i128 retains every signed and unsigned 64-bit JSON integer.\n\n"
        source += "\n\n".join(self.definitions[name] for name in sorted(self.definitions)) + "\n"
        source += r"""
// A nullable member can still be required. A custom deserializer keeps Serde
// from silently supplying None for a missing Option field in an untagged branch.
fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer)
}

// Serde's untagged enum buffer lacks deserialize_i128. Reading Number through
// deserialize_any keeps nested integers usable in untagged models without copying
// the response body. Floats are refused even when mathematically integral.
fn integer(number: serde_json::Number) -> Result<i128, &'static str> {
    number.as_i64().map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
        .ok_or("expected a signed or unsigned 64-bit JSON integer")
}

fn deserialize_integer<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i128, D::Error> {
    let number = <serde_json::Number as serde::Deserialize>::deserialize(deserializer)?;
    integer(number).map_err(serde::de::Error::custom)
}

fn deserialize_optional_integer<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<i128>, D::Error> {
    let number = <Option<serde_json::Number> as serde::Deserialize>::deserialize(deserializer)?;
    number.map(integer).transpose().map_err(serde::de::Error::custom)
}
"""
        result = subprocess.run(["rustfmt", "--edition", "2024", "--emit", "stdout"], input=source, text=True, capture_output=True, check=True)
        return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    schemas = json.loads((ROOT / "docs/openapi.json").read_text())["components"]["schemas"]
    generated = Generator(schemas).generate()
    if args.check:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != generated:
            print("Rust admin models are stale. Run mise run client:types.")
            print("".join(difflib.unified_diff(current.splitlines(True), generated.splitlines(True), fromfile=str(OUTPUT), tofile="regenerated")))
            return 1
    else:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_text(generated)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
