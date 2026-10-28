#!/usr/bin/env python3
"""Deterministic unit checks for the bounded Rust admin model generator."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("client_types", Path(__file__).with_name("client-types.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class GeneratorTests(unittest.TestCase):
    def setUp(self):
        self.generator = module.Generator({})

    def test_null_and_optional_shapes(self):
        self.assertEqual(self.generator.rust_type({"type": ["null", "integer"]}, "Count"), "Option<i128>")
        self.assertEqual(self.generator.rust_type({"oneOf": [{"type": "null"}, {"type": "string"}]}, "Name"), "Option<String>")

    def test_extensible_maps_and_strings(self):
        self.assertEqual(self.generator.rust_type({"type": "string", "enum": ["one"]}, "Kind"), "String")
        self.assertEqual(self.generator.rust_type({"type": "object"}, "Open"), "serde_json::Map<String, serde_json::Value>")
        self.assertEqual(self.generator.rust_type({"type": "object", "additionalProperties": {"type": "boolean"}}, "Map"), "std::collections::BTreeMap<String, bool>")

    def test_unsupported_shapes_fail_closed(self):
        for schema in [{}, {"type": ["string", "integer"]}, {"allOf": []}, {"type": "string", "dependentSchemas": {}}]:
            with self.subTest(schema=schema), self.assertRaises(ValueError):
                self.generator.rust_type(schema, "Unsupported")
        with self.assertRaises(ValueError):
            self.generator.model("Unsupported", {"type": "object", "properties": {"name": {"type": "string"}}, "not": {}})

    def test_field_collisions_fail_closed(self):
        with self.assertRaises(ValueError):
            self.generator.model("Collision", {"type": "object", "properties": {"fooBar": {"type": "string"}, "foo_bar": {"type": "integer"}}})

    def test_specific_union_variant_is_tried_first(self):
        base = {"type": "object", "properties": {"items": {"type": "array", "items": {"type": "string"}}}, "required": ["items"]}
        specific = {"type": "object", "properties": dict(base["properties"], dataset={"type": "string"}), "required": ["items", "dataset"]}
        self.generator.model("Listing", {"anyOf": [base, specific]})
        output = self.generator.definitions["Listing"]
        self.assertLess(output.index("Alternative2("), output.index("Alternative1("))

    def test_integer_fields_use_untagged_compatible_decoders(self):
        self.generator.model("Counts", {"type": "object", "properties": {"count": {"type": "integer"}, "optional": {"type": "integer"}}, "required": ["count"]})
        output = self.generator.definitions["Counts"]
        self.assertIn('deserialize_with = "deserialize_integer"', output)
        self.assertIn('deserialize_with = "deserialize_optional_integer"', output)
        self.assertIn('default, skip_serializing_if = "Option::is_none"', output)

    def test_required_nullable_members_cannot_disappear_into_union_extras(self):
        self.generator.model("RequiredNullable", {"type": "object", "properties": {
            "class": {"type": ["string", "null"]},
            "optional": {"type": ["string", "null"]},
        }, "required": ["class"]})
        output = self.generator.definitions["RequiredNullable"]
        self.assertIn('    #[serde(deserialize_with = "deserialize_required_nullable")]\n    pub class: Option<String>,', output)
        self.assertIn('    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub optional: Option<String>,', output)

    def test_keyword_names_are_valid_identifiers(self):
        self.assertEqual(module.snake("type"), "r#type")
        self.assertEqual(module.snake("datasetId"), "dataset_id")
        self.assertEqual(module.snake("self"), "self_")


if __name__ == "__main__":
    unittest.main()
