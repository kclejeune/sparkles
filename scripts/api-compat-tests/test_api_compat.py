"""Independent old-client/new-server contracts for the X12 compatibility guard."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'api-compat.py'
spec = importlib.util.spec_from_file_location('api_compat', SCRIPT)
api = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = api
spec.loader.exec_module(api)


def document():
    operation = {
        'requestBody': {'content': {'application/json': {'schema': {'$ref': '#/components/schemas/Input'}}}},
        'responses': {
            '200': {'content': {'application/json': {'schema': {'$ref': '#/components/schemas/DatasetInfo'}}}},
            '400': {'description': 'error'},
        },
    }
    return {
        'openapi': '3.1.0', 'info': {'version': '0.0.0'},
        'paths': {'/books': {
            'parameters': [{'in': 'query', 'name': 'limit', 'schema': {'type': 'integer'}}],
            'post': operation,
        }},
        'components': {'schemas': {
            'Input': {'type': 'object', 'properties': {
                'title': {'type': ['string', 'null']}, 'mode': {'enum': ['a', 'b']},
            }},
            'DatasetInfo': {'type': 'object', 'required': ['name'], 'properties': {
                'name': {'type': 'string'}, 'state': {'enum': ['ready', 'closed']},
                'error': {'properties': {'code': {'enum': ['bad-input', 'conflict']}}},
            }},
        }},
    }


def cli():
    return {'schema_version': 1, 'profile': 'standard', 'command': {
        'name': 'sparkles', 'aliases': [], 'flags': {'--cache': {'required': False, 'action': 'Set', 'arity': [1, 1], 'choices': []}}, 'positionals': [],
        'commands': {'query': {'name': 'query', 'aliases': ['q'], 'flags': {'--format': {'required': False, 'action': 'Set', 'arity': [1, 1], 'choices': ['json', 'text']}}, 'positionals': [{'id': 'input', 'index': 1, 'arity': [1, 1], 'required': True}], 'commands': {}}}}}


class HttpContracts(unittest.TestCase):
    def setUp(self):
        self.old = document()
        self.new = copy.deepcopy(self.old)

    def rules(self):
        return {d.rule for d in api.compare_http(self.old, self.new)}

    def request(self):
        return self.new['components']['schemas']['Input']

    def response(self):
        return self.new['components']['schemas']['DatasetInfo']

    def test_same_document_and_prose_order(self):
        self.new['paths']['/books']['post']['summary'] = 'new words'
        self.new['components']['schemas']['DatasetInfo']['description'] = 'words'
        self.new['components']['schemas']['DatasetInfo']['properties'] = dict(reversed(list(self.response()['properties'].items())))
        self.assertEqual(api.compare_http(self.old, self.new), [])

    def test_x12_a11_removed_referenced_dataset_name(self):
        del self.response()['properties']['name']
        ds = api.compare_http(self.old, self.new)
        self.assertTrue(any(d.rule == 'member-removed' and d.identity == 'POST /books' and d.pointer.endswith('/properties/name') and d.severity == 'break' for d in ds), ds)

    def test_operation_removed(self):
        del self.new['paths']['/books']['post']
        self.assertIn('operation-removed', self.rules())

    def test_status_removed(self):
        del self.new['paths']['/books']['post']['responses']['400']
        self.assertIn('status-removed', self.rules())

    def test_inherited_parameter_removed(self):
        self.new['paths']['/books']['parameters'] = []
        self.assertIn('parameter-removed', self.rules())

    def test_referenced_path_item_is_traversed(self):
        for d in (self.old, self.new):
            d['components']['pathItems'] = {'Books': d['paths']['/books']}
            d['paths']['/books'] = {'$ref': '#/components/pathItems/Books'}
        del self.new['components']['pathItems']['Books']['post']['responses']['400']
        self.assertIn('status-removed', self.rules())

    def test_parameter_content_schema_is_traversed(self):
        for d in (self.old, self.new):
            d['paths']['/books']['parameters'] = [{'in': 'query', 'name': 'filter', 'content': {'application/json': {'schema': {'enum': ['a', 'b']}}}}]
        self.new['paths']['/books']['parameters'][0]['content']['application/json']['schema']['enum'] = ['a']
        self.assertIn('enum-incompatible', self.rules())

    def test_parameter_newly_required(self):
        self.new['paths']['/books']['parameters'][0]['required'] = True
        self.assertIn('request-required', self.rules())

    def test_request_member_required(self):
        self.request()['required'] = ['title']
        self.assertIn('request-required', self.rules())

    def test_new_required_parameter(self):
        self.new['paths']['/books']['post']['parameters'] = [{'in': 'query', 'name': 'new', 'required': True, 'schema': {'type': 'string'}}]
        self.assertIn('request-required', self.rules())

    def test_body_newly_required(self):
        self.new['paths']['/books']['post']['requestBody']['required'] = True
        self.assertIn('request-required', self.rules())

    def test_request_type_narrowing(self):
        self.request()['properties']['title']['type'] = 'string'
        self.assertIn('type-incompatible', self.rules())

    def test_request_enum_narrowing(self):
        self.request()['properties']['mode']['enum'] = ['a']
        self.assertIn('enum-incompatible', self.rules())

    def test_request_enum_widening_is_allowed(self):
        self.request()['properties']['mode']['enum'].append('c')
        self.assertEqual(self.rules(), set())

    def test_enum_boolean_is_not_the_number_one(self):
        self.old['components']['schemas']['Input']['properties']['mode']['enum'] = [True]
        self.request()['properties']['mode']['enum'] = [1]
        self.assertIn('enum-incompatible', self.rules())

    def test_response_enum_widening_breaks_old_consumer(self):
        self.response()['properties']['state']['enum'].append('other')
        self.assertIn('enum-incompatible', self.rules())

    def test_response_enum_narrowing_is_covered_value_removal(self):
        self.response()['properties']['state']['enum'] = ['ready']
        self.assertIn('enum-incompatible', self.rules())

    def test_response_error_code_deletion_is_covered(self):
        self.response()['properties']['error']['properties']['code']['enum'] = ['bad-input']
        ds = api.compare_http(self.old, self.new)
        self.assertTrue(any(d.rule == 'enum-incompatible' and d.pointer.endswith('/properties/error/properties/code/enum') for d in ds), ds)

    def test_response_type_narrowing_is_covered(self):
        self.old['components']['schemas']['DatasetInfo']['properties']['name']['type'] = ['string', 'null']
        self.assertIn('type-incompatible', self.rules())

    def test_response_required_member_becomes_optional(self):
        self.response()['required'] = []
        self.assertIn('response-guarantee-removed', self.rules())

    def test_additions_are_allowed(self):
        self.request()['properties']['new'] = {'type': 'string'}
        self.response()['properties']['new'] = {'type': 'string'}
        self.new['paths']['/other'] = copy.deepcopy(self.new['paths']['/books'])
        self.new['paths']['/books']['post']['responses']['201'] = {'description': 'new'}
        self.assertEqual(self.rules(), set())

    def test_array_item_type(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Input']['properties']['list'] = {'type': 'array', 'items': {'type': 'string'}}
        self.request()['properties']['list']['items']['type'] = 'integer'
        self.assertIn('type-incompatible', self.rules())

    def test_ref_sibling_constraint(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Word'] = {'type': 'string'}
            d['components']['schemas']['Input']['properties']['word'] = {'$ref': '#/components/schemas/Word', 'enum': ['x', 'y']}
        self.request()['properties']['word']['enum'] = ['x']
        self.assertIn('enum-incompatible', self.rules())

    def test_json_pointer_escaping(self):
        for d in (self.old, self.new):
            d['components']['schemas']['a/b~c'] = {'type': 'string'}
            d['components']['schemas']['Input']['properties']['word'] = {'$ref': '#/components/schemas/a~1b~0c'}
        self.new['components']['schemas']['a/b~c']['type'] = 'integer'
        self.assertIn('type-incompatible', self.rules())

    def test_recursive_refs_retain_other_changes(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Input']['properties']['child'] = {'$ref': '#/components/schemas/Input'}
        self.request()['properties']['mode']['enum'] = ['a']
        self.assertIn('enum-incompatible', self.rules())
        self.assertLess(len(api.compare_http(self.old, self.new)), 10)

    def test_allof_referenced_object(self):
        for d in (self.old, self.new):
            s = d['components']['schemas']['DatasetInfo']
            d['components']['schemas']['DatasetInfo'] = {'allOf': [s, {'properties': {'extra': {'type': 'integer'}}}]}
        del self.new['components']['schemas']['DatasetInfo']['allOf'][0]['properties']['name']
        self.assertIn('member-removed', self.rules())

    def test_oneof_change_is_not_assumed_safe(self):
        self.old['components']['schemas']['Input']['properties']['union'] = {'oneOf': [{'type': 'string'}, {'type': 'integer'}]}
        self.new['components']['schemas']['Input']['properties']['union'] = {'oneOf': [{'type': 'string'}, {'type': 'number'}]}
        self.assertIn('composition-review', self.rules())
        self.assertTrue(any(d.severity == 'review' for d in api.compare_http(self.old, self.new)))

    def test_anyof_request_addition_is_safe(self):
        self.old['components']['schemas']['Input']['properties']['union'] = {'anyOf': [{'type': 'string'}]}
        self.new['components']['schemas']['Input']['properties']['union'] = {'anyOf': [{'type': 'string'}, {'type': 'integer'}]}
        self.assertEqual(self.rules(), set())

    def test_oneof_ref_target_addition_can_change_exclusivity(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Left'] = {'type': 'object', 'properties': {'a': {'type': 'string'}}, 'additionalProperties': False}
            d['components']['schemas']['Right'] = {'type': 'object', 'properties': {'b': {'type': 'string'}}, 'additionalProperties': False}
            d['components']['schemas']['Input']['properties']['union'] = {'oneOf': [{'$ref': '#/components/schemas/Left'}, {'$ref': '#/components/schemas/Right'}]}
        self.new['components']['schemas']['Left']['properties']['b'] = {'type': 'string'}
        self.assertIn('composition-review', self.rules())

    def test_oneof_fingerprint_keeps_annotation_named_user_properties(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Left'] = {'type': 'object', 'properties': {'a': {'type': 'string'}}, 'additionalProperties': False}
            d['components']['schemas']['Right'] = {'type': 'object', 'properties': {'default': {'type': 'string'}}, 'additionalProperties': False}
            d['components']['schemas']['Input']['properties']['union'] = {'oneOf': [{'$ref': '#/components/schemas/Left'}, {'$ref': '#/components/schemas/Right'}]}
        self.new['components']['schemas']['Left']['properties']['default'] = {'type': 'string'}
        self.assertIn('composition-review', self.rules())

    def test_shape_does_not_strip_enum_object_data_keys(self):
        self.assertNotEqual(api.shape({'enum': [{'description': 'a'}]}), api.shape({'enum': [{'description': 'b'}]}))

    def test_changed_conflicting_allof_is_not_silently_flattened(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Input']['properties']['word'] = {'allOf': [{'type': 'string', 'minLength': 1}, {'minLength': 3}]}
        self.request()['properties']['word']['allOf'][1]['minLength'] = 4
        self.assertIn('composition-review', self.rules())

    def test_removed_experimental_reference_target_is_excluded(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Experimental'] = {'type': 'string', 'x-sparkles-experimental': True}
            d['components']['schemas']['DatasetInfo']['properties']['experimental'] = {'$ref': '#/components/schemas/Experimental'}
        del self.response()['properties']['experimental']
        self.assertEqual(self.rules(), set())

    def test_ref_named_enum_data_is_not_a_schema_reference(self):
        for d in (self.old, self.new):
            d['components']['schemas']['Input']['properties']['data'] = {'enum': [{'$ref': 'literal data'}]}
        self.assertEqual(self.rules(), set())

    def test_schema_property_named_const_still_validates_its_reference(self):
        self.request()['properties']['const'] = {'$ref': 'invalid-reference'}
        with self.assertRaises(api.InputError):
            api.compare_http(self.old, self.new)

    def test_unknown_constraint_change_needs_review(self):
        self.request()['properties']['title']['pattern'] = '^safe$'
        self.assertIn('constraint-review', self.rules())

    def test_request_additional_properties_narrowing(self):
        self.request()['additionalProperties'] = False
        self.assertIn('additional-properties', self.rules())

    def test_only_baseline_experimental_subtree_excluded(self):
        self.old['components']['schemas']['DatasetInfo']['properties']['name']['x-sparkles-experimental'] = True
        del self.response()['properties']['name']
        self.response()['properties']['state']['enum'].append('other')
        ds = api.compare_http(self.old, self.new)
        self.assertFalse(any(d.rule == 'member-removed' for d in ds), ds)
        self.assertTrue(any(d.rule == 'enum-incompatible' for d in ds), ds)

    def test_new_experimental_marker_does_not_hide_break(self):
        self.response()['x-sparkles-experimental'] = True
        del self.response()['properties']['name']
        self.assertIn('member-removed', self.rules())

    def test_security_changes_need_review(self):
        self.new['paths']['/books']['post']['security'] = [{'token': []}]
        self.assertIn('security-review', self.rules())

    def test_invalid_reference_is_configuration_failure(self):
        self.request()['properties']['title'] = {'$ref': 'https://invalid.example/schema'}
        with self.assertRaises(api.InputError):
            api.compare_http(self.old, self.new)

    def test_deterministic_sorted_diagnostics(self):
        del self.response()['properties']['name']
        self.request()['required'] = ['title']
        a = api.compare_http(self.old, self.new)
        b = api.compare_http(json.loads(json.dumps(self.old, sort_keys=True)), json.loads(json.dumps(self.new, sort_keys=True)))
        self.assertEqual(a, b)
        self.assertEqual(a, sorted(a))


class ReviewSchemaRegressions(unittest.TestCase):
    """Concrete valid-before/invalid-after controls, independent of diff internals."""

    def compare(self, before, after, direction='request', form='direct', targets=None):
        old, new = document(), document()
        for doc, schema in ((old, before), (new, after)):
            if form == 'allOf':
                schema = {'allOf': [{k: v} for k, v in schema.items()]}
            elif form == 'ref-sibling':
                doc['components']['schemas']['Finite'] = {'enum': schema['enum']}
                schema = {'$ref': '#/components/schemas/Finite', **{k: v for k, v in schema.items() if k != 'enum'}}
            operation = doc['paths']['/books']['post']
            body = operation['requestBody'] if direction == 'request' else operation['responses']['200']
            body['content']['application/json']['schema'] = schema
        if targets is not None:
            old['components']['schemas'].update(targets[0])
            new['components']['schemas'].update(targets[1])
        return api.compare_http(old, new)

    def assert_enum_break(self, before, after):
        for direction in ('request', 'response'):
            for form in ('direct', 'allOf', 'ref-sibling'):
                with self.subTest(direction=direction, form=form):
                    ds = self.compare(before, after, direction, form)
                    self.assertTrue(any(d.rule == 'enum-incompatible' and d.severity == 'break' for d in ds), ds)

    def test_added_const_alongside_enum_rejects_previously_valid_b(self):
        self.assert_enum_break({'enum': ['a', 'b']}, {'enum': ['a', 'b'], 'const': 'a'})

    def test_changed_const_alongside_enum_rejects_previously_valid_a(self):
        self.assert_enum_break({'enum': ['a', 'b'], 'const': 'a'}, {'enum': ['a', 'b'], 'const': 'b'})

    def test_const_conjunction_preserves_boolean_number_distinction(self):
        self.assert_enum_break({'enum': [True, 1], 'const': True}, {'enum': [True, 1], 'const': 1})

    def test_const_conjunction_preserves_object_data(self):
        a, b = {'description': 'a'}, {'description': 'b'}
        self.assert_enum_break({'enum': [a, b], 'const': a}, {'enum': [a, b], 'const': b})

    def test_removed_const_widens_requests_but_changes_covered_response_values(self):
        before, after = {'enum': ['a', 'b'], 'const': 'a'}, {'enum': ['a', 'b']}
        self.assertEqual(self.compare(before, after), [])
        self.assertTrue(any(d.rule == 'enum-incompatible' for d in self.compare(before, after, 'response')))

    def test_enum_changes_outside_const_leave_same_allowed_value(self):
        for direction in ('request', 'response'):
            self.assertEqual(self.compare({'enum': ['a', 'b'], 'const': 'a'}, {'enum': ['a', 'b', 'c'], 'const': 'a'}, direction), [])

    def test_empty_intersection_is_not_treated_as_unconstrained(self):
        before, after = {'enum': ['a'], 'const': 'a'}, {'enum': ['a'], 'const': 'b'}
        self.assert_enum_break(before, after)

    def test_boolean_numeric_constraints_in_allof_are_not_equal(self):
        ds = self.compare({'allOf': [{'enum': [True, 1]}, {'enum': [True]}]}, {'allOf': [{'enum': [True]}, {'enum': [1]}]})
        self.assertTrue(any(d.rule == 'enum-incompatible' for d in ds), ds)

    def test_conflicting_const_allof_change_requires_review(self):
        ds = self.compare({'allOf': [{'const': 'a'}, {'const': 'b'}]}, {'allOf': [{'const': 'a'}, {'const': 'c'}]})
        self.assertTrue(any(d.rule == 'composition-review' and d.severity == 'review' for d in ds), ds)

    def test_unknown_map_ref_target_narrowing_requires_review(self):
        schema = {'type': 'object', 'patternProperties': {'^x': {'$ref': '#/components/schemas/Word'}}}
        ds = self.compare(schema, schema, targets=({'Word': {'type': 'string'}}, {'Word': {'type': 'integer'}}))
        self.assertTrue(any(d.rule == 'constraint-review' and d.pointer.endswith('/patternProperties') and d.severity == 'review' for d in ds), ds)

    def test_unknown_single_schema_ref_target_narrowing_requires_review(self):
        schema = {'type': 'string', 'not': {'$ref': '#/components/schemas/Word'}}
        ds = self.compare(schema, schema, targets=({'Word': {'const': 'a'}}, {'Word': {'enum': ['a', 'b']}}))
        self.assertTrue(any(d.rule == 'constraint-review' and d.pointer.endswith('/not') and d.severity == 'review' for d in ds), ds)

    def test_unknown_nested_recursive_target_change_requires_review(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        old = {'Word': {'properties': {'child': {'$ref': '#/components/schemas/Word'}, 'value': {'$ref': '#/components/schemas/Leaf'}}}, 'Leaf': {'enum': ['a', 'b']}}
        new = copy.deepcopy(old)
        new['Leaf']['enum'] = ['a']
        ds = self.compare(schema, schema, targets=(old, new))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)
        self.assertLess(len(ds), 5)

    def test_unknown_unchanged_recursive_target_is_quiet(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        target = {'Word': {'properties': {'child': {'$ref': '#/components/schemas/Word'}}}}
        self.assertEqual(self.compare(schema, schema, targets=(target, copy.deepcopy(target))), [])

    def test_unknown_referenced_annotations_are_quiet(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        self.assertEqual(self.compare(schema, schema, targets=({'Word': {'type': 'string', 'description': 'old'}}, {'Word': {'type': 'string', 'description': 'new'}})), [])

    def test_unknown_map_preserves_annotation_named_schema_keys(self):
        for name in ('description', 'default', '$ref'):
            with self.subTest(name=name):
                schema = {'patternProperties': {name: {'$ref': '#/components/schemas/Word'}}}
                ds = self.compare(schema, schema, targets=({'Word': {'type': 'string'}}, {'Word': {'type': 'integer'}}))
                self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)

    def test_unknown_literal_ref_data_is_not_dereferenced(self):
        schema = {'not': {'const': {'$ref': 'literal', 'description': 'a'}}}
        self.assertEqual(self.compare(schema, copy.deepcopy(schema)), [])
        changed = {'not': {'const': {'$ref': 'literal', 'description': 'b'}}}
        self.assertTrue(any(d.rule == 'constraint-review' for d in self.compare(schema, changed)))

    def test_unknown_changed_user_data_is_not_removed_as_annotation(self):
        self.assertTrue(any(d.rule == 'constraint-review' for d in self.compare({'unknown-validator': {'description': 'a'}}, {'unknown-validator': {'description': 'b'}})))

    def test_unknown_missing_target_fails_input(self):
        with self.assertRaises(api.InputError):
            self.compare({'not': {'$ref': '#/components/schemas/Missing'}}, {})

    def dependent_compare(self, schema, targets, direction='request'):
        try:
            return self.compare(schema, copy.deepcopy(schema), direction, targets=targets)
        except (api.InputError, TypeError) as error:
            self.fail(f'Valid named schema members were interpreted as validator keywords: {error}')

    def test_dependent_schema_narrowing_review_for_reserved_member_names(self):
        # Presence of default activates Word against the object: {default: null,
        # foo: "a"} is accepted before and rejected after foo becomes integer.
        for name in ('default', 'description', 'enum', 'const', '$ref', 'examples'):
            for direction in ('request', 'response'):
                with self.subTest(name=name, direction=direction):
                    schema = {'type': 'object', 'dependentSchemas': {name: {'$ref': '#/components/schemas/Word'}}}
                    ds = self.dependent_compare(schema, ({'Word': {'properties': {'foo': {'type': 'string'}}}}, {'Word': {'properties': {'foo': {'type': 'integer'}}}}), direction)
                    self.assertTrue(any(d.rule == 'constraint-review' and d.pointer.endswith('/dependentSchemas') and d.severity == 'review' for d in ds), ds)

    def test_dependent_schema_unchanged_target_is_quiet_for_reserved_names(self):
        for name in ('default', 'description', 'enum', 'const', '$ref', 'examples'):
            with self.subTest(name=name):
                schema = {'dependentSchemas': {name: {'$ref': '#/components/schemas/Word'}}}
                self.assertEqual(self.dependent_compare(schema, ({'Word': {'type': 'object'}}, {'Word': {'type': 'object'}})), [])

    def test_dependent_schema_missing_ref_is_not_hidden_by_reserved_names(self):
        for name in ('default', 'description', 'enum', 'const', '$ref', 'examples'):
            with self.subTest(name=name):
                schema = {'dependentSchemas': {name: {'$ref': '#/components/schemas/Missing'}}}
                with self.assertRaises(api.InputError):
                    self.compare(schema, copy.deepcopy(schema))

    def test_dependent_schema_recursive_target_change_requires_review(self):
        schema = {'dependentSchemas': {'enum': {'$ref': '#/components/schemas/Word'}}}
        before = {'Word': {'properties': {'next': {'$ref': '#/components/schemas/Word'}, 'foo': {'enum': ['a', 'b']}}}}
        after = copy.deepcopy(before)
        after['Word']['properties']['foo']['enum'] = ['a']
        ds = self.dependent_compare(schema, (before, after))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)
        self.assertLess(len(ds), 5)

    def test_nested_dependent_schema_target_change_requires_review(self):
        schema = {'dependentSchemas': {'default': {'dependentSchemas': {'const': {'$ref': '#/components/schemas/Word'}}}}}
        ds = self.dependent_compare(schema, ({'Word': {'required': []}}, {'Word': {'required': ['foo']}}))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)

    def test_dependent_schema_recursive_unchanged_target_is_quiet(self):
        schema = {'dependentSchemas': {'const': {'$ref': '#/components/schemas/Word'}}}
        target = {'Word': {'properties': {'next': {'$ref': '#/components/schemas/Word'}}}}
        self.assertEqual(self.dependent_compare(schema, (target, copy.deepcopy(target))), [])

    def test_dependent_schema_annotation_only_target_change_is_quiet(self):
        for name in ('default', 'enum', 'const', '$ref'):
            with self.subTest(name=name):
                schema = {'dependentSchemas': {name: {'$ref': '#/components/schemas/Word'}}}
                self.assertEqual(self.dependent_compare(schema, ({'Word': {'type': 'object', 'description': 'old'}}, {'Word': {'type': 'object', 'description': 'new'}})), [])

    def test_dependent_schema_child_literal_refs_are_data(self):
        for name in ('default', 'enum', 'const', '$ref'):
            for keyword in ('const', 'enum', 'default', 'examples', 'example'):
                with self.subTest(name=name, keyword=keyword):
                    literal = {'$ref': 'literal-data', 'description': 'data', 'enum': 'also data'}
                    schema = {'dependentSchemas': {name: {keyword: [literal] if keyword in ('enum', 'examples') else literal}}}
                    self.assertEqual(self.dependent_compare(schema, ({}, {})), [])

    def test_named_schema_map_shapes_keep_user_names_and_literal_data(self):
        for keyword in ('properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas'):
            for name in ('default', 'description', 'enum', 'const', '$ref'):
                with self.subTest(keyword=keyword, name=name):
                    literal = {'$ref': 'literal-data', 'description': 'data'}
                    schema = {keyword: {name: {'const': literal, 'description': 'annotation'}}}
                    self.assertEqual(api.shape(schema), {keyword: {name: {'const': literal}}})

    def test_named_schema_map_signatures_follow_targets_under_reserved_names(self):
        for keyword in ('properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas'):
            for name in ('default', 'description', 'enum', 'const', '$ref'):
                with self.subTest(keyword=keyword, name=name):
                    schema = {keyword: {name: {'$ref': '#/components/schemas/Word'}}}
                    old, new = document(), document()
                    old['components']['schemas']['Word'] = {'type': 'string'}
                    new['components']['schemas']['Word'] = {'type': 'integer'}
                    try:
                        a, b = api.Schemas(old).signature(schema), api.Schemas(new).signature(schema)
                    except (api.InputError, TypeError) as error:
                        self.fail(f'Valid schema-map member was treated as a reference keyword: {error}')
                    self.assertNotEqual(a, b)

    def test_named_schema_map_ref_validation_checks_each_child_schema(self):
        for keyword in ('properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas'):
            for name in ('default', 'description', 'enum', 'const', '$ref'):
                with self.subTest(keyword=keyword, name=name):
                    schema = {keyword: {name: {'$ref': '#/components/schemas/Missing'}}}
                    with self.assertRaises(api.InputError):
                        api.Schemas(document()).validate_refs(schema)

    def test_dependent_schema_child_literal_change_is_preserved(self):
        old = {'dependentSchemas': {'default': {'const': {'description': 'a'}}}}
        new = {'dependentSchemas': {'default': {'const': {'description': 'b'}}}}
        self.assertTrue(any(d.rule == 'constraint-review' for d in self.compare(old, new)))
        self.assertNotEqual(api.shape(old), api.shape(new))


class TraversalContexts(unittest.TestCase):
    reserved = ('default', 'description', 'title', 'enum', 'const', '$ref')

    def compare(self, before, after=None, targets=None):
        return ReviewSchemaRegressions.compare(self, before, copy.deepcopy(before) if after is None else after, targets=targets)

    def valid_compare(self, before, after=None, targets=None):
        try:
            return self.compare(before, after, targets)
        except (api.InputError, TypeError) as error:
            self.fail(f'Valid literal or named data was treated as a schema: {error}')

    def test_dependent_required_narrowing_behind_unchanged_refs(self):
        # {x: {default: null, a: 1}} loses validity when dependent b is required.
        for outer in ('patternProperties', 'not'):
            for name in self.reserved:
                with self.subTest(outer=outer, name=name):
                    ref = {'$ref': '#/components/schemas/Word'}
                    schema = {outer: {'^x': ref} if outer == 'patternProperties' else ref}
                    ds = self.valid_compare(schema, targets=({'Word': {'dependentRequired': {name: ['a']}}}, {'Word': {'dependentRequired': {name: ['a', 'b']}}}))
                    self.assertTrue(any(d.rule == 'constraint-review' and d.severity == 'review' for d in ds), ds)

    def test_dependent_required_removal_remains_unproven(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        ds = self.valid_compare(schema, targets=({'Word': {'dependentRequired': {'default': ['a', 'b']}}}, {'Word': {'dependentRequired': {'default': ['a']}}}))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)

    def test_identical_named_data_maps_and_reference_looking_strings_are_literal(self):
        for name in self.reserved:
            with self.subTest(name=name):
                data = {'dependentRequired': {name: ['$ref', '#/components/schemas/Missing', 'description']}}
                self.assertEqual(self.valid_compare(data), [])
                self.assertEqual(api.shape(data), data)

    def test_nested_recursive_target_data_map_change_is_not_lost(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        old = {'Word': {'properties': {'next': {'$ref': '#/components/schemas/Word'}, 'child': {'$ref': '#/components/schemas/Leaf'}}}, 'Leaf': {'dependentRequired': {'description': ['a']}}}
        new = copy.deepcopy(old)
        new['Leaf']['dependentRequired']['description'].append('b')
        ds = self.valid_compare(schema, targets=(old, new))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)
        self.assertLess(len(ds), 5)

    def test_recursive_target_annotations_are_ignored_only_in_schema_positions(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        old = {'Word': {'description': 'old', 'properties': {'next': {'$ref': '#/components/schemas/Word'}}, 'dependentRequired': {'description': ['a']}}}
        new = copy.deepcopy(old)
        new['Word']['description'] = 'new'
        self.assertEqual(self.valid_compare(schema, targets=(old, new)), [])

    def test_mixed_dependency_map_schema_and_literal_values_remain_distinct(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        old = {'Word': {'dependencies': {'default': {'$ref': '#/components/schemas/Leaf'}, '$ref': ['a']}}, 'Leaf': {'type': 'string'}}
        new = copy.deepcopy(old)
        new['Leaf']['type'] = 'integer'
        ds = self.valid_compare(schema, targets=(old, new))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)
        self.assertEqual(self.valid_compare(schema, targets=(old, copy.deepcopy(old))), [])

    def test_missing_ref_in_mixed_dependency_schema_still_fails(self):
        with self.assertRaises(api.InputError):
            self.compare({'dependencies': {'default': {'$ref': '#/components/schemas/Missing'}, '$ref': ['literal']}})

    def test_all_known_single_schema_positions_follow_changed_targets(self):
        for key in ('not', 'if', 'then', 'else', 'contains', 'propertyNames', 'unevaluatedProperties', 'unevaluatedItems', 'contentSchema', 'additionalItems'):
            with self.subTest(key=key):
                schema = {key: {'$ref': '#/components/schemas/Word'}}
                old = {'Word': {'dependentRequired': {'default': ['a']}}}
                new = {'Word': {'dependentRequired': {'default': ['a', 'b']}}}
                ds = self.valid_compare(schema, targets=(old, new))
                self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)

    def test_prefix_item_schema_array_context_follows_refs(self):
        schema = {'prefixItems': [{'$ref': '#/components/schemas/Word'}]}
        ds = self.valid_compare(schema, targets=({'Word': {'dependentRequired': {'title': ['a']}}}, {'Word': {'dependentRequired': {'title': ['a', 'b']}}}))
        self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)

    def test_unknown_structured_data_preserves_all_annotation_named_members(self):
        schema = {'not': {'$ref': '#/components/schemas/Word'}}
        for name in self.reserved[:-1]:
            with self.subTest(name=name):
                old = {'Word': {'unknown-validator': {name: {'description': 'a'}}}}
                new = {'Word': {'unknown-validator': {name: {'description': 'b'}}}}
                ds = self.valid_compare(schema, targets=(old, new))
                self.assertTrue(any(d.rule == 'constraint-review' for d in ds), ds)

    def test_unknown_reference_shaped_structure_explicitly_refuses_ambiguity(self):
        for value in ({'$ref': '#/components/schemas/Input'}, {'literal': {'$ref': 'data'}}, [{'$dynamicRef': '#word'}]):
            with self.subTest(value=value):
                with self.assertRaisesRegex(api.InputError, 'unsupported.*context'):
                    self.compare({'unknown-validator': value})

    def test_nonlocal_resource_scopes_and_dynamic_refs_are_explicitly_unsupported(self):
        for key, value in (('$id', 'https://example.invalid/resource'), ('$dynamicRef', '#word'), ('$recursiveRef', '#'), ('$schema', 'https://example.invalid/dialect')):
            with self.subTest(key=key):
                with self.assertRaisesRegex(api.InputError, 'unsupported'):
                    self.compare({key: value})

    def test_resource_and_ref_looking_keys_in_literals_are_data(self):
        payload = {'$id': 'literal', '$ref': 'literal', '$dynamicRef': 'literal', 'description': {'default': 'data'}}
        for key in ('enum', 'const', 'default', 'example', 'examples'):
            with self.subTest(key=key):
                schema = {key: [payload] if key in ('enum', 'examples') else payload}
                self.assertEqual(self.valid_compare(schema), [])

    def test_literal_discriminator_mapping_names_are_not_reference_objects(self):
        schema = {'discriminator': {'propertyName': 'kind', 'mapping': {'$ref': '#/components/schemas/Input', 'default': 'literal'}}}
        self.assertEqual(self.valid_compare(schema), [])
        self.assertEqual(api.shape(schema), schema)

    def test_data_map_vocabulary_is_retained_without_schema_interpretation(self):
        schema = {'$vocabulary': {'https://example.invalid/vocab': True}}
        self.assertEqual(self.valid_compare(schema), [])
        self.assertEqual(api.shape(schema), schema)

    def test_named_schema_map_child_with_data_map_is_not_reclassified(self):
        for name in self.reserved:
            with self.subTest(name=name):
                schema = {'dependentSchemas': {name: {'dependentRequired': {'$ref': ['default']}}}}
                self.assertEqual(self.valid_compare(schema), [])
                self.assertEqual(api.shape(schema), schema)

    def test_reference_validation_follows_target_then_terminates_cycles(self):
        d = document()
        d['components']['schemas']['Word'] = {'properties': {'self': {'$ref': '#/components/schemas/Word'}, 'missing': {'$ref': '#/components/schemas/Missing'}}}
        with self.assertRaises(api.InputError):
            api.Schemas(d).validate_refs({'$ref': '#/components/schemas/Word'})
        del d['components']['schemas']['Word']['properties']['missing']
        api.Schemas(d).validate_refs({'$ref': '#/components/schemas/Word'})

    def test_malformed_schema_containers_fail_as_input_errors(self):
        for schema in ({'properties': []}, {'dependentSchemas': {'name': 7}}, {'prefixItems': {}}, {'not': []}):
            with self.subTest(schema=schema):
                with self.assertRaises(api.InputError):
                    self.compare(schema)

    def test_openapi_example_values_and_link_parameters_are_literal(self):
        old = document()
        old['components']['examples'] = {'default': {'value': {'$ref': 'literal data', 'schema': {'$ref': 'also data'}}}}
        old['components']['links'] = {'$ref': {'operationId': 'read', 'parameters': {'$ref': '#/components/schemas/Missing'}, 'requestBody': {'schema': {'$ref': 'literal'}}}}
        media = old['paths']['/books']['post']['responses']['200']['content']['application/json']
        media['examples'] = {'description': {'$ref': '#/components/examples/default'}}
        try:
            self.assertEqual(api.compare_http(old, copy.deepcopy(old)), [])
        except api.InputError as error:
            self.fail(f'OpenAPI literal data was treated as a reference: {error}')

    def test_openapi_true_example_reference_is_validated(self):
        old = document()
        old['components']['examples'] = {'default': {'$ref': '#/components/examples/Missing'}}
        with self.assertRaises(api.InputError):
            api.compare_http(old, copy.deepcopy(old))

    def test_openapi_named_header_data_and_schema_ref_contexts(self):
        old = document()
        old['paths']['/books']['post']['responses']['200']['headers'] = {'$ref': {'schema': {'enum': [{'$ref': 'literal'}]}}}
        try:
            self.assertEqual(api.compare_http(old, copy.deepcopy(old)), [])
        except api.InputError as error:
            self.fail(f'Header map key was treated as a reference: {error}')
        old['paths']['/books']['post']['responses']['200']['headers']['$ref']['schema'] = {'$ref': '#/components/schemas/Missing'}
        with self.assertRaises(api.InputError):
            api.compare_http(old, copy.deepcopy(old))

    def test_openapi_unknown_reference_context_is_explicitly_unsupported(self):
        old = document()
        old['x-custom'] = {'description': {'$ref': '#/components/schemas/Input'}}
        with self.assertRaisesRegex(api.InputError, 'unsupported.*context'):
            api.compare_http(old, copy.deepcopy(old))

    def test_openapi_unknown_dialect_is_not_assumed_to_be_default(self):
        old = document()
        old['jsonSchemaDialect'] = 'https://example.invalid/dialect'
        with self.assertRaisesRegex(api.InputError, 'unsupported'):
            api.compare_http(old, copy.deepcopy(old))

    def test_invalid_reference_and_dialect_types_raise_input_errors(self):
        for value in ([], {}, 1, None):
            with self.subTest(value=value):
                old = document()
                old['components']['responses'] = {'Probe': {'$ref': value}}
                with self.assertRaises(api.InputError):
                    api.compare_http(old, copy.deepcopy(old))
                with self.assertRaises(api.InputError):
                    self.compare({'$schema': value})
                old = document()
                old['jsonSchemaDialect'] = value
                with self.assertRaises(api.InputError):
                    api.compare_http(old, copy.deepcopy(old))

    def test_openapi_reference_only_cycles_and_nonobjects_fail(self):
        for targets in ({'A': {'$ref': '#/components/responses/B'}, 'B': {'$ref': '#/components/responses/A'}}, {'A': False}):
            with self.subTest(targets=targets):
                old = document()
                old['components']['responses'] = {'Probe': {'$ref': '#/components/responses/A'}, **targets}
                with self.assertRaises(api.InputError):
                    api.compare_http(old, copy.deepcopy(old))

    def test_openapi_reference_object_contexts_keep_valid_data_and_check_missing_targets(self):
        targets = {
            'parameters': {'in': 'query', 'name': 'p', 'schema': {'$ref': '#/components/schemas/Input'}},
            'headers': {'schema': {'enum': [{'$ref': 'literal'}]}},
            'requestBodies': {'content': {'application/json': {'schema': {'$ref': '#/components/schemas/Input'}}}},
            'responses': {'description': 'ok'},
            'pathItems': {'get': {'responses': {'200': {'description': 'ok'}}}},
            'callbacks': {'{$request.query.url}': {'get': {'responses': {'200': {'description': 'ok'}}}}},
            'links': {'operationId': 'read', 'parameters': {'$ref': 'literal'}},
            'examples': {'value': {'$ref': 'literal'}},
            'securitySchemes': {'type': 'apiKey', 'name': 'key', 'in': 'header'},
        }
        for category, target in targets.items():
            with self.subTest(category=category):
                old = document()
                old['components'][category] = {'Alias': {'$ref': f'#/components/{category}/Target'}, 'Target': target}
                try:
                    self.assertEqual(api.compare_http(old, copy.deepcopy(old)), [])
                except api.InputError as error:
                    self.fail(f'Valid OpenAPI {category} reference rejected: {error}')
                del old['components'][category]['Target']
                with self.assertRaises(api.InputError):
                    api.compare_http(old, copy.deepcopy(old))

    def test_recursive_openapi_callback_is_finite_and_checks_other_edges(self):
        old = document()
        operation = {'callbacks': {'again': {'$ref': '#/components/callbacks/C'}}, 'responses': {'200': {'description': 'ok'}}}
        old['components']['callbacks'] = {'C': {'{$request.query.url}': {'post': operation}}}
        self.assertEqual(api.compare_http(old, copy.deepcopy(old)), [])
        operation['requestBody'] = {'$ref': '#/components/requestBodies/Missing'}
        with self.assertRaises(api.InputError):
            api.compare_http(old, copy.deepcopy(old))

    def test_openapi_oauth_scope_names_are_literal_data(self):
        old = document()
        old['components']['securitySchemes'] = {'oauth': {'type': 'oauth2', 'flows': {'clientCredentials': {'tokenUrl': 'https://example.invalid/token', 'scopes': {'$ref': 'read', 'default': 'write'}}}}}
        old['security'] = [{'oauth': ['$ref', 'default']}]
        try:
            self.assertEqual(api.compare_http(old, copy.deepcopy(old)), [])
        except api.InputError as error:
            self.fail(f'Valid OAuth scope names were treated as references: {error}')

    def test_callback_reference_descriptions_do_not_become_map_entries(self):
        old = document()
        old['components']['responses'] = {'R': {'description': 'ok'}}
        old['components']['callbacks'] = {
            'Alias': {'$ref': '#/components/callbacks/Inner', 'summary': 'reference metadata', 'description': 'outer'},
            'Inner': {'$ref': '#/components/callbacks/C', 'description': 'inner'},
            'C': {'description': {'post': {'responses': {'200': {'$ref': '#/components/responses/R'}}}}},
        }
        try:
            self.assertEqual(api.compare_http(old, copy.deepcopy(old)), [])
        except api.InputError as error:
            self.fail(f'Reference descriptions were interpreted as callback map entries: {error}')
        del old['components']['responses']['R']
        with self.assertRaises(api.InputError):
            api.compare_http(old, copy.deepcopy(old))


class CliContracts(unittest.TestCase):
    def setUp(self):
        self.old = cli()
        self.new = copy.deepcopy(self.old)

    def rules(self):
        return {d.rule for d in api.compare_cli(self.old, self.new)}

    def test_same_and_addition(self):
        self.new['command']['flags']['--new'] = {'required': False}
        self.assertEqual(self.rules(), set())

    def test_removed_global_flag(self):
        del self.new['command']['flags']['--cache']
        self.assertIn('flag-removed', self.rules())

    def test_removed_subcommand(self):
        self.new['command']['commands'] = {}
        self.assertIn('command-removed', self.rules())

    def test_removed_visible_alias(self):
        self.new['command']['commands']['query']['aliases'] = []
        self.assertIn('command-alias-removed', self.rules())

    def test_alias_retains_old_command_spelling(self):
        q = self.new['command']['commands'].pop('query')
        q['name'] = 'select'
        q['aliases'].append('query')
        self.new['command']['commands']['select'] = q
        self.assertEqual(self.rules(), set())

    def test_flag_contract_change(self):
        self.new['command']['flags']['--cache']['arity'] = [0, 1]
        self.assertIn('argument-changed', self.rules())

    def test_positional_order_contract(self):
        self.new['command']['commands']['query']['positionals'][0]['index'] = 2
        self.assertIn('positional-changed', self.rules())

    def test_same_shaped_positional_id_reorder(self):
        first = self.old['command']['commands']['query']['positionals'][0]
        self.old['command']['commands']['query']['positionals'].append({**first, 'id': 'output', 'index': 2})
        self.new = copy.deepcopy(self.old)
        positions = self.new['command']['commands']['query']['positionals']
        positions[0]['id'], positions[1]['id'] = positions[1]['id'], positions[0]['id']
        self.assertIn('positional-changed', self.rules())

    def test_lossy_positional_identity_is_refused(self):
        for tree in (self.old, self.new):
            del tree['command']['commands']['query']['positionals'][0]['id']
        with self.assertRaises(api.InputError):
            self.rules()

    def test_invalid_or_duplicate_positional_identity_is_refused(self):
        for value in ('', 1, 'input'):
            with self.subTest(value=value):
                self.new = copy.deepcopy(self.old)
                positions = self.new['command']['commands']['query']['positionals']
                positions.append({**positions[0], 'id': value, 'index': 2})
                with self.assertRaises(api.InputError):
                    self.rules()

    def test_invalid_or_duplicate_positional_index_is_refused(self):
        for value in (0, True, 1):
            with self.subTest(value=value):
                self.new = copy.deepcopy(self.old)
                positions = self.new['command']['commands']['query']['positionals']
                positions.append({**positions[0], 'id': 'output', 'index': value})
                with self.assertRaises(api.InputError):
                    self.rules()

    def test_feature_profile_mismatch_is_not_removal(self):
        self.new['profile'] = 'stripped'
        with self.assertRaises(api.InputError):
            api.compare_cli(self.old, self.new)

    def test_unknown_tree_version_refused(self):
        self.new['schema_version'] = 99
        with self.assertRaises(api.InputError):
            api.compare_cli(self.old, self.new)


class ExceptionPolicy(unittest.TestCase):
    def setUp(self):
        self.d = api.Diagnostic('http', 'POST /books', '/properties/name', 'member-removed')
        self.data = {'baseline': {'id': 'v0.1.0', 'kind': 'release', 'version': '0.1.0'}}
        self.entry = dict(zip(('surface', 'identity', 'pointer', 'rule', 'severity'), self.d.key()))
        self.entry.update(baseline='v0.1.0', target='0.2.0', reason='Required by the next minor contract', changelog='0.2.0-compatibility', disposition='break')
        self.log = '## 0.2.0\n\n### Compatibility\n\nA documented break.\n'

    def apply(self, target='0.2.0'):
        return api.apply_entries([self.d], self.data, {'schema_version': 1, 'entry': [self.entry]}, target, self.log)

    def test_declared_preone_minor_break(self):
        self.assertEqual(self.apply(), [])

    def test_reviewed_uncertain_minor_break_uses_same_version_rules(self):
        self.d = api.Diagnostic('http', 'POST /books', '/properties/name', 'composition-review', 'review')
        self.entry.update(rule=self.d.rule, severity='review')
        self.assertEqual(self.apply(), [])
        self.entry.update(target='0.1.1', changelog='0.1.1-compatibility')
        self.log = self.log.replace('0.2.0', '0.1.1')
        with self.assertRaises(api.InputError):
            self.apply('0.1.1')

    def test_patch_cannot_waive_break(self):
        self.entry.update(target='0.1.1', changelog='0.1.1-compatibility')
        self.log = self.log.replace('0.2.0', '0.1.1')
        with self.assertRaises(api.InputError):
            self.apply('0.1.1')

    def test_stable_minor_cannot_waive_break(self):
        self.data['baseline']['version'] = '1.1.0'
        self.entry.update(target='1.2.0', changelog='1.2.0-compatibility')
        self.log = self.log.replace('0.2.0', '1.2.0')
        with self.assertRaises(api.InputError):
            self.apply('1.2.0')

    def test_proven_break_cannot_be_called_nonbreaking(self):
        self.entry['disposition'] = 'nonbreaking-review'
        with self.assertRaises(api.InputError):
            self.apply()

    def test_stale_wrong_pointer_rule_baseline_target_and_wildcards_fail(self):
        for key, value in [('pointer', '/wrong'), ('rule', 'wrong'), ('baseline', 'v0.0.1'), ('target', '0.3.0'), ('identity', '*')]:
            with self.subTest(key=key):
                old = self.entry[key]
                self.entry[key] = value
                with self.assertRaises(api.InputError):
                    self.apply()
                self.entry[key] = old

    def test_duplicate_entry_fails(self):
        with self.assertRaises(api.InputError):
            api.apply_entries([self.d], self.data, {'schema_version': 1, 'entry': [self.entry, self.entry]}, '0.2.0', self.log)

    def test_changelog_anchor_and_compatibility_heading_are_required(self):
        for log in ('## 0.2.0\n### Changed\nAnything', '## 0.3.0\n### Compatibility\nAnything'):
            with self.subTest(log=log), self.assertRaises(api.InputError):
                api.apply_entries([self.d], self.data, {'schema_version': 1, 'entry': [self.entry]}, '0.2.0', log)

    def test_bootstrap_allows_explicit_equal_version_development_review(self):
        self.data['baseline']['kind'] = 'bootstrap'
        self.entry.update(target='0.1.0', disposition='development', changelog='unreleased-compatibility')
        self.log = '## Unreleased\n\n### Compatibility\n\nA reviewed development change.\n'
        self.assertEqual(self.apply('0.1.0'), [])
        self.data['baseline']['kind'] = 'release'
        with self.assertRaises(api.InputError):
            self.apply('0.1.0')

    def test_invalid_and_downgrade_versions(self):
        for value in ('01.2.0', '0.2', '0.2.0-pre', '0.0.9'):
            with self.subTest(value=value), self.assertRaises(api.InputError):
                self.apply(value)

    def test_unknown_break_schema_refused(self):
        with self.assertRaises(api.InputError):
            api.apply_entries([], self.data, {'schema_version': 99}, '0.2.0', self.log)


class InventoryIntegrity(unittest.TestCase):
    def setUp(self):
        import tempfile
        import hashlib
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'docs').mkdir()
        self.path = self.root / 'docs/api-compatibility.toml'
        for surface, value in [('http', document()), ('cli', cli())]:
            (self.root / f'docs/{surface}.json').write_text(json.dumps(value))
        self.text = '''schema_version = 1
enforced = ["http", "cli"]
pending = ["rust", "bindings", "mcp", "metrics", "cli-json-exit", "deprecation", "release-install"]
[baseline]
id = "bootstrap-abcdef"
kind = "bootstrap"
source = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
version = "0.1.0"
released = false
profile = "standard"
features = ["auth", "mcp"]
'''
        for surface in ('http', 'cli'):
            digest = hashlib.sha256((self.root / f'docs/{surface}.json').read_bytes()).hexdigest()
            self.text += f'\n[{surface}]\npath = "docs/{surface}.json"\nsha256 = "{digest}"\n'
        self.path.write_text(self.text)

    def check(self):
        return api.inventory(self.path, self.root)

    def test_inventory_valid(self):
        self.assertEqual(self.check()['baseline']['kind'], 'bootstrap')

    def test_inventory_missing_digest_or_file_fails(self):
        (self.root / 'docs/http.json').unlink()
        with self.assertRaises(OSError):
            self.check()

    def test_inventory_damage_fails(self):
        (self.root / 'docs/http.json').write_text('{}')
        with self.assertRaises(api.InputError):
            self.check()

    def test_inventory_refuses_lossy_cli_even_with_matching_digest(self):
        import hashlib
        path = self.root / 'docs/cli.json'
        previous_digest = hashlib.sha256(path.read_bytes()).hexdigest()
        tree = json.loads(path.read_text())
        del tree['command']['commands']['query']['positionals'][0]['id']
        path.write_text(json.dumps(tree))
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        self.path.write_text(self.text.replace(previous_digest, digest))
        with self.assertRaisesRegex(api.InputError, 'argument IDs'):
            self.check()

    def test_path_traversal_and_unknown_schema_and_no_surface_fail(self):
        for old, new in [('docs/http.json', '../http.json'), ('schema_version = 1', 'schema_version = 9'), ('["http", "cli"]', '[]')]:
            with self.subTest(new=new):
                self.path.write_text(self.text.replace(old, new))
                with self.assertRaises(api.InputError):
                    self.check()

    def test_bootstrap_cannot_pretend_it_shipped(self):
        self.path.write_text(self.text.replace('released = false', 'released = true'))
        with self.assertRaises(api.InputError):
            self.check()

    def test_duplicate_toml_identity_fails(self):
        self.path.write_text(self.text + '\n[baseline]\nid = "other"\n')
        with self.assertRaises(ValueError):
            self.check()

    def test_released_http_version_must_match(self):
        self.path.write_text(self.text.replace('kind = "bootstrap"', 'kind = "release"').replace('released = false', 'released = true'))
        with self.assertRaises(api.InputError):
            self.check()

    def test_duplicate_json_keys_fail(self):
        file = self.root / 'duplicate.json'
        file.write_text('{"a":1,"a":2}')
        with self.assertRaises(api.InputError):
            api.load_json(file)

    def test_symlink_escape_fails(self):
        (self.root / 'docs/escape').symlink_to(self.root.parent, target_is_directory=True)
        with self.assertRaises(api.InputError):
            api.safe_path(self.root, 'docs/escape/outside.json')

    def test_trusted_base_prevents_silent_baseline_replacement(self):
        import subprocess
        def git(*args):
            return subprocess.check_output(['git', '-C', str(self.root), *args], stderr=subprocess.DEVNULL).decode().strip()
        git('init', '-q')
        git('add', 'docs')
        git('-c', 'commit.gpgsign=false', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'baseline')
        base = git('rev-parse', 'HEAD')
        data = self.check()
        api.check_base_inventory(data, base, self.root)
        data['baseline']['kind'] = 'release'
        with self.assertRaises(api.InputError):
            api.check_base_inventory(data, base, self.root)


class LegacyRenameContract(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parents[2]
        self.old = api.load_json(root / 'docs/openapi-bootstrap-7c533eaa9042.json')
        self.data = {
            'baseline': {'kind': 'bootstrap', 'source': api.LEGACY_RENAME_SOURCE},
            'http': {'sha256': api.LEGACY_RENAME_SHA, 'legacy_rename_content': api.legacy_rename_record()},
        }

    def test_exact_old_rule_preserves_members_requiredness_and_other_document_fields(self):
        new = api.normalize_baseline_http(self.old, self.data)
        old_schema = self.old['paths']['/$/datasets/{ds}/rename']['post']['responses']['200']['content']
        new_schema = new['paths']['/$/datasets/{ds}/rename']['post']['responses']['200']['content']['application/json']['schema']
        self.assertEqual(old_schema, new_schema)
        restored = copy.deepcopy(new)
        restored['paths']['/$/datasets/{ds}/rename']['post']['responses']['200']['content'] = old_schema
        self.assertEqual(self.old, restored)
        self.assertEqual(api.compare_http(new, copy.deepcopy(new)), [])

    def test_current_malformed_content_never_uses_old_rule(self):
        normalized = api.normalize_baseline_http(self.old, self.data)
        with self.assertRaises(api.InputError):
            api.compare_http(normalized, self.old)

    def test_extra_old_annotation_or_different_shape_cannot_use_rule(self):
        for key, value in [('description', 'unregistered annotation'), ('type', 'array')]:
            old = copy.deepcopy(self.old)
            old['paths']['/$/datasets/{ds}/rename']['post']['responses']['200']['content'][key] = value
            with self.subTest(key=key), self.assertRaises(api.InputError):
                api.normalize_baseline_http(old, self.data)

    def test_source_digest_pointer_and_shape_registration_are_exact(self):
        for key in ('source', 'baseline_sha256', 'pointer', 'expected_shape_sha256'):
            data = copy.deepcopy(self.data)
            data['http']['legacy_rename_content'][key] = 'unregistered'
            with self.subTest(key=key), self.assertRaises(api.InputError):
                api.normalize_baseline_http(self.old, data)

    def test_another_malformed_location_is_refused(self):
        new = api.normalize_baseline_http(self.old, self.data)
        new['paths']['/other'] = {'get': {'responses': {'200': {'content': {'type': 'string'}}}}}
        with self.assertRaises(api.InputError):
            api.compare_http(api.normalize_baseline_http(self.old, self.data), new)

    def test_corrected_response_members_still_cannot_disappear(self):
        old = api.normalize_baseline_http(self.old, self.data)
        new = copy.deepcopy(old)
        del new['paths']['/$/datasets/{ds}/rename']['post']['responses']['200']['content']['application/json']['schema']['properties']['renamedFrom']
        ds = api.compare_http(old, new)
        self.assertTrue(any(d.identity == 'POST /$/datasets/{ds}/rename' and d.rule == 'member-removed' and d.pointer.endswith('/properties/renamedFrom') for d in ds), ds)


class BootstrapTrust(unittest.TestCase):
    def setUp(self):
        import tempfile
        import subprocess
        from unittest.mock import patch
        self.subprocess = subprocess
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'origin'
        self.root.mkdir()
        self.git('init', '-q')
        self.base = self.commit('base')
        self.source = self.commit('approved source')
        self.tip = self.commit('current source')
        self.data = {'baseline': {'kind': 'bootstrap', 'source': self.source}, 'http': {'sha256': api.LEGACY_RENAME_SHA}}
        self.patch = patch.object(api, 'BOOTSTRAP_SOURCE', self.source)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def git(self, *args, root=None):
        return self.subprocess.check_output(['git', '-C', str(root or self.root), *args], stderr=self.subprocess.DEVNULL).decode().strip()

    def commit(self, text):
        (self.root / 'marker').write_text(text)
        self.git('add', 'marker')
        self.git('-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', text)
        return self.git('rev-parse', 'HEAD')

    def test_initial_batch_base_can_precede_approved_source(self):
        api.check_base_inventory(self.data, self.base, self.root)

    def test_initial_merge_base_can_follow_approved_source(self):
        api.check_base_inventory(self.data, self.tip, self.root)

    def test_initial_anchor_cannot_move_to_pr_selected_commit(self):
        self.data['baseline']['source'] = self.tip
        with self.assertRaises(api.InputError):
            api.check_base_inventory(self.data, self.base, self.root)

    def test_initial_digest_cannot_change_with_anchor(self):
        self.data['http']['sha256'] = 'b' * 64
        with self.assertRaises(api.InputError):
            api.check_base_inventory(self.data, self.base, self.root)

    def test_approved_source_must_be_in_current_history(self):
        self.git('checkout', '-q', '--detach', self.base)
        with self.assertRaises(api.InputError):
            api.check_base_inventory(self.data, self.base, self.root)

    def test_diverged_trusted_base_fails(self):
        self.git('checkout', '-q', '--detach', self.base)
        other = self.commit('diverged')
        self.git('checkout', '-q', '--detach', self.tip)
        with self.assertRaises(api.InputError):
            api.check_base_inventory(self.data, other, self.root)

    def test_missing_history_fails(self):
        with self.assertRaises(api.InputError):
            api.check_base_inventory(self.data, 'b' * 40, self.root)

    def test_shallow_history_is_not_successful_proof(self):
        shallow = self.root.parent / 'shallow'
        self.subprocess.run(['git', 'clone', '-q', '--depth=1', self.root.as_uri(), str(shallow)], check=True, stderr=self.subprocess.DEVNULL)
        self.assertEqual(self.git('rev-parse', '--is-shallow-repository', root=shallow), 'true')
        with self.assertRaises(api.InputError):
            api.check_base_inventory(self.data, self.tip, shallow)


if __name__ == "__main__":
    unittest.main()
