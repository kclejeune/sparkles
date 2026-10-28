#!/usr/bin/env python3
"""X12 HTTP/CLI compatibility checks. Read-only; Python 3.11+, standard library only.

Schema implication is deliberately bounded. Unsupported changed constraints and
ambiguous unions need explicit review. X12 also protects deletion of documented
response values, beyond formal response subtyping.
"""
from __future__ import annotations

import argparse
import copy
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[1]
METHODS = {'get', 'head', 'post', 'put', 'delete', 'patch', 'options', 'trace'}
ANNOTATIONS = {'title', 'description', 'summary', 'examples', 'example', 'default', 'deprecated', 'externalDocs', 'x-sparkles-experimental'}
SCHEMA_KEYS = {'$ref', 'type', 'enum', 'const', 'properties', 'required', 'items', 'additionalProperties', 'allOf', 'anyOf', 'oneOf'} | ANNOTATIONS
# The same context registry drives shaping, validation and reachable signatures.
# Only a schema object interprets keywords. User map names and literal payloads do
# not acquire schema meaning merely because they are spelled default or $ref.
SCHEMA_MAP_KEYS = frozenset({'properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas'})
SCHEMA_CONTEXTS = {
    **dict.fromkeys(SCHEMA_MAP_KEYS, 'schema-map'),
    **dict.fromkeys({'allOf', 'anyOf', 'oneOf', 'prefixItems'}, 'schema-list'),
    **dict.fromkeys({'items', 'additionalProperties', 'unevaluatedProperties', 'unevaluatedItems', 'propertyNames', 'contains', 'not', 'if', 'then', 'else', 'contentSchema', 'additionalItems'}, 'schema'),
    **dict.fromkeys({'dependentRequired', '$vocabulary'}, 'data-map'),
    **dict.fromkeys({'enum', 'const', 'required', 'type', 'discriminator', 'xml'}, 'literal'),
    'dependencies': 'dependency-map',
}
REFERENCE_KEYS = frozenset({'$ref', '$dynamicRef', '$recursiveRef', '$id'})
SCHEMA_DIALECTS = frozenset({'https://json-schema.org/draft/2020-12/schema', 'https://spec.openapis.org/oas/3.1/dialect/base'})

# These contexts locate actual schemas and OpenAPI reference objects. Other object
# fields retain opaque data, unless an explicitly literal field permits any value.
OPENAPI_CONTEXTS = {
    'document': {'paths': 'paths', 'webhooks': 'map:path-item', 'components': 'components'},
    'components': {'schemas': 'map:schema', 'parameters': 'map:parameter', 'headers': 'map:header', 'requestBodies': 'map:request-body', 'responses': 'map:response', 'pathItems': 'map:path-item', 'callbacks': 'map:callback', 'links': 'map:link', 'examples': 'map:example', 'securitySchemes': 'map:security-scheme'},
    'path-item': {**dict.fromkeys(METHODS, 'operation'), 'parameters': 'list:parameter'},
    'operation': {'parameters': 'list:parameter', 'requestBody': 'request-body', 'responses': 'responses', 'callbacks': 'map:callback'},
    'parameter': {'schema': 'schema', 'content': 'map:media', 'examples': 'map:example'},
    'header': {'schema': 'schema', 'content': 'map:media', 'examples': 'map:example'},
    'request-body': {'content': 'map:media'},
    'response': {'content': 'map:media', 'headers': 'map:header', 'links': 'map:link'},
    'media': {'schema': 'schema', 'examples': 'map:example', 'encoding': 'map:encoding'},
    'encoding': {'headers': 'map:header'},
    'link': {}, 'example': {}, 'security-scheme': {},
}
OPENAPI_PATTERNED_CONTEXTS = {'paths': 'path-item', 'responses': 'response', 'callback': 'path-item'}
OPENAPI_REF_CONTEXTS = frozenset({'path-item', 'parameter', 'header', 'request-body', 'response', 'callback', 'link', 'example', 'security-scheme'})
OPENAPI_LITERAL_FIELDS = {
    'document': {'security', 'servers'}, 'operation': {'security', 'servers'},
    'path-item': {'servers'}, 'parameter': {'example'}, 'header': {'example'},
    'media': {'example'}, 'example': {'value'},
    'link': {'parameters', 'requestBody', 'server'},
    'security-scheme': {'flows'},
}


# One inherited description error in the unpublished bootstrap. Only the registered
# OLD bytes can use this rule. Current documents always require a media-type map.
LEGACY_RENAME_SOURCE = '7c533eaa9042a456cbb6352bbe3983a4558e7bf7'
BOOTSTRAP_SOURCE = LEGACY_RENAME_SOURCE
LEGACY_RENAME_SHA = '6b6d4a8e05a59c9ddd812403c21b5a850a208307dec86c212ff9ef23090a4f3a'
LEGACY_RENAME_POINTER = '/paths/~1$~1datasets~1{ds}~1rename/post/responses/200/content'
LEGACY_RENAME_SCHEMA = {
    'type': 'object', 'required': ['name', 'renamedFrom'],
    'properties': {'name': {'type': 'string'}, 'renamedFrom': {'type': 'string'}},
}


class InputError(ValueError):
    """Invalid or unsupported input is not evidence of compatibility."""


@dataclass(frozen=True, order=True)
class Diagnostic:
    surface: str
    identity: str
    pointer: str
    rule: str
    severity: str = 'break'

    def key(self):
        return (self.surface, self.identity, self.pointer, self.rule, self.severity)

    def __str__(self):
        return f'{self.severity}: {self.surface} {self.identity} {self.pointer} [{self.rule}]'


def pointer(at, name):
    return at + '/' + str(name).replace('~', '~0').replace('/', '~1')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)


def opaque_data(value):
    """Keep unknown data verbatim; do not guess reference or annotation meaning."""
    if isinstance(value, dict):
        if REFERENCE_KEYS & value.keys():
            raise InputError('unsupported reference context in opaque structured data')
        for child in value.values():
            opaque_data(child)
    elif isinstance(value, list):
        for child in value:
            opaque_data(child)
    return value


def openapi_patterned_entries(value):
    """Separate extensions from entries in Paths, Responses and Callback objects."""
    if not isinstance(value, dict):
        raise InputError('OpenAPI patterned container must be an object')
    for name, child in value.items():
        if name.startswith('x-'):
            opaque_data(child)
        else:
            yield name, child


def schema_tree(value, context='schema', on_ref=None):
    """Walk explicit contexts once for all schema consumers; never mutate input."""
    if context == 'literal':
        return value
    if context == 'opaque':
        return opaque_data(value)
    if context in {'schema-map', 'data-map', 'dependency-map'}:
        if not isinstance(value, dict):
            raise InputError(f'{context} must be an object')
        if context == 'data-map':
            return value
        return {name: schema_tree(child, 'literal' if context == 'dependency-map' and isinstance(child, list) else 'schema', on_ref) for name, child in value.items()}
    if context == 'schema-list':
        if not isinstance(value, list):
            raise InputError('schema-list must be an array')
        return [schema_tree(child, 'schema', on_ref) for child in value]
    if context != 'schema':
        raise InputError('unsupported schema traversal context')
    if isinstance(value, bool):
        return value
    if not isinstance(value, dict):
        raise InputError('schema must be an object or boolean')
    out = {}
    for key, child in value.items():
        if key in ANNOTATIONS:
            continue
        if key in {'$id', '$dynamicRef', '$recursiveRef'}:
            raise InputError(f'unsupported schema resource or reference scope: {key}')
        if key == '$schema' and (not isinstance(child, str) or child not in SCHEMA_DIALECTS):
            raise InputError('unsupported schema dialect')
        if key == '$ref':
            if not isinstance(child, str):
                raise InputError('schema reference must be a string')
            if on_ref is not None:
                on_ref(child)
            out[key] = child
        else:
            out[key] = schema_tree(child, SCHEMA_CONTEXTS.get(key, 'opaque'), on_ref)
    return out


def shape(value):
    """Discard annotations only at actual schema positions, retaining all data."""
    return schema_tree(value)


def experimental(schema):
    return isinstance(schema, dict) and schema.get('x-sparkles-experimental') is True


class Schemas:
    def __init__(self, doc):
        self.doc = doc
        self.cache = {}
        self.signatures = {}

    def ref(self, ref):
        if not isinstance(ref, str) or not ref.startswith('#/'):
            raise InputError('only local JSON Pointer references are supported')
        value = self.doc
        for part in unquote(ref[2:]).split('/'):
            if re.search(r'~(?![01])', part):
                raise InputError('invalid JSON Pointer escape')
            part = part.replace('~1', '/').replace('~0', '~')
            if not isinstance(value, dict) or part not in value:
                raise InputError(f'unresolved reference {ref}')
            value = value[part]
        return value

    def validate_refs(self, node):
        seen = set()
        def visit(ref):
            target = self.ref(ref)
            if ref not in seen:
                seen.add(ref)
                schema_tree(target, on_ref=visit)
        schema_tree(node, on_ref=visit)

    def validate_document_refs(self):
        seen = set()
        def visit(value, context):
            if context == 'schema':
                self.validate_refs(value)
                return
            if context.startswith('map:'):
                if not isinstance(value, dict):
                    raise InputError('OpenAPI named container must be an object')
                for child in value.values():
                    visit(child, context[4:])
                return
            if context.startswith('list:'):
                if not isinstance(value, list):
                    raise InputError('OpenAPI sequence must be an array')
                for child in value:
                    visit(child, context[5:])
                return
            if not isinstance(value, dict):
                raise InputError(f'OpenAPI {context} must be an object')
            if '$ref' in value:
                if context not in OPENAPI_REF_CONTEXTS:
                    raise InputError(f'unsupported OpenAPI reference context: {context}')
                ref = value['$ref']
                # Resolve before the visited check so reference-only cycles and
                # invalid reference types never become a successful early return.
                resolved = self.resolve_object(value, omit_reference_descriptions=True)
                key = (ref, context)
                if key in seen:
                    return
                seen.add(key)
                value = resolved
            if context in OPENAPI_PATTERNED_CONTEXTS:
                for _, child in openapi_patterned_entries(value):
                    visit(child, OPENAPI_PATTERNED_CONTEXTS[context])
                return
            fields = OPENAPI_CONTEXTS[context]
            for key, child in value.items():
                if context == 'document' and key == 'jsonSchemaDialect' and (not isinstance(child, str) or child not in SCHEMA_DIALECTS):
                    raise InputError('unsupported OpenAPI schema dialect')
                if key in fields:
                    visit(child, fields[key])
                elif key not in OPENAPI_LITERAL_FIELDS.get(context, set()):
                    opaque_data(child)
        visit(self.doc, 'document')

    def signature(self, node):
        # The shared walker distinguishes real refs from map names and literal
        # payloads. Register each target before following it to terminate cycles.
        key = id(node)
        if key in self.signatures:
            return self.signatures[key][1]
        definitions = {}
        def visit(ref):
            if ref not in definitions:
                target = self.ref(ref)
                definitions[ref] = None
                definitions[ref] = schema_tree(target, on_ref=visit)
        root = schema_tree(node, on_ref=visit)
        result = canonical([root, definitions])
        self.signatures[key] = (node, result)
        return result

    def resolve_object(self, node, omit_reference_descriptions=False):
        seen = set()
        while isinstance(node, dict) and '$ref' in node:
            ref = node['$ref']
            target = self.ref(ref)
            if ref in seen:
                raise InputError('reference-only cycle')
            seen.add(ref)
            if not isinstance(target, dict):
                raise InputError('OpenAPI reference target must be an object')
            # Reference descriptions are metadata, even when the referenced value
            # is a named callback map. Preserve the target's own map member names.
            excluded = {'$ref', 'summary', 'description'} if omit_reference_descriptions else {'$ref'}
            node = {**target, **{k: v for k, v in node.items() if k not in excluded}}
        return node

    def schema(self, node, trail=frozenset()):
        """Resolve root refs/allOf while retaining child schemas by identity."""
        if isinstance(node, bool):
            return node
        if not isinstance(node, dict):
            raise InputError('schema must be an object or boolean')
        key = id(node)
        if key in self.cache:
            return self.cache[key][1]
        if key in trail:
            raise InputError('reference-only/allOf cycle cannot be resolved')
        trail = trail | {key}
        parts = []
        if '$ref' in node:
            parts.append(self.schema(self.ref(node['$ref']), trail))
        parts.extend(self.schema(x, trail) for x in node.get('allOf', []))
        result = {k: v for k, v in node.items() if k not in {'$ref', 'allOf'}}
        for part in parts:
            result = self.conjoin(part, result)
        self.cache[key] = (node, result)
        return result

    @staticmethod
    def conjoin(a, b):
        if a is False or b is False:
            return False
        if a is True:
            return b
        if b is True:
            return a
        result = dict(a)
        for k, v in b.items():
            if k not in result or canonical(result[k]) == canonical(v):
                result[k] = v
            elif k == 'properties':
                properties = dict(result[k])
                for name, member in v.items():
                    properties[name] = ({'allOf': [properties[name], member]} if name in properties and canonical(properties[name]) != canonical(member) else member)
                result[k] = properties
            elif k == 'required':
                result[k] = sorted(set(result[k]) | set(v))
            elif k in {'type', 'enum'}:
                left = result[k] if isinstance(result[k], list) else [result[k]]
                right = v if isinstance(v, list) else [v]
                result[k] = [x for x in left if canonical(x) in {canonical(y) for y in right}]
            elif k == 'additionalProperties':
                if result[k] is False or v is False:
                    result[k] = False
                elif result[k] is True:
                    result[k] = v
                elif v is not True:
                    result[k] = {'allOf': [result[k], v]}
            elif k in ANNOTATIONS:
                # An experimental ref target remains experimental through siblings.
                result[k] = result[k] or v if k == 'x-sparkles-experimental' else v
            else:
                result['x-compat-conflict'] = True
        return result


def legacy_rename_record():
    return {
        'source': LEGACY_RENAME_SOURCE,
        'baseline_sha256': LEGACY_RENAME_SHA,
        'pointer': LEGACY_RENAME_POINTER,
        'expected_shape_sha256': hashlib.sha256(canonical(LEGACY_RENAME_SCHEMA).encode()).hexdigest(),
    }


def normalize_baseline_http(document, data):
    record = data['http'].get('legacy_rename_content')
    if record is None:
        return document
    if record != legacy_rename_record() or data['baseline']['kind'] != 'bootstrap' or data['baseline']['source'] != LEGACY_RENAME_SOURCE or data['http']['sha256'] != LEGACY_RENAME_SHA:
        raise InputError('unregistered legacy rename description normalization')
    result = copy.deepcopy(document)
    response = result['paths']['/$/datasets/{ds}/rename']['post']['responses']['200']
    if canonical(response.get('content')) != canonical(LEGACY_RENAME_SCHEMA):
        raise InputError('legacy rename content differs from its exact registered shape')
    response['content'] = {'application/json': {'schema': response['content']}}
    return result


def validate_media_content(resolver):
    def content(value):
        if not isinstance(value, dict):
            raise InputError('OpenAPI content must be a media-type map')
        for media, entry in value.items():
            if not isinstance(media, str) or '/' not in media or not isinstance(entry, dict):
                raise InputError('OpenAPI content requires media-type keys and MediaType objects')
    def body(value):
        obj = resolver.resolve_object(value)
        if 'content' in obj:
            content(obj['content'])
    for _, item in openapi_patterned_entries(resolver.doc.get('paths', {})):
        item = resolver.resolve_object(item)
        for method in METHODS & set(item):
            op = item[method]
            if 'requestBody' in op:
                body(op['requestBody'])
            for _, response in openapi_patterned_entries(op.get('responses', {})):
                body(response)
            for parameter in item.get('parameters', []) + op.get('parameters', []):
                body(parameter)
    for category in ('responses', 'requestBodies', 'parameters', 'headers'):
        for value in resolver.doc.get('components', {}).get(category, {}).values():
            body(value)


class HttpDiff:
    def __init__(self, old, new):
        self.old, self.new = Schemas(old), Schemas(new)
        self.diagnostics = set()
        self.seen = set()
        self.identity = ''

    def add(self, at, rule, severity='break'):
        self.diagnostics.add(Diagnostic('http', self.identity, at, rule, severity))

    def schema(self, old, new, at, direction):
        if experimental(old):
            return
        key = (id(old), id(new), direction)
        if key in self.seen:
            return
        self.seen.add(key)
        try:
            self._schema(old, new, at, direction)
        finally:
            self.seen.remove(key)

    def _schema(self, old, new, at, direction):
        original_old, original_new = old, new
        old, new = self.old.schema(old), self.new.schema(new)
        if isinstance(old, dict) and isinstance(new, dict) and (old.get('x-compat-conflict') or new.get('x-compat-conflict')):
            if self.old.signature(original_old) != self.new.signature(original_new):
                self.add(at, 'composition-review', 'review')
        if experimental(old):
            return
        if isinstance(old, bool) or isinstance(new, bool):
            if (direction == 'request' and old is not False and new is False) or (direction == 'response' and old is False and new is not False):
                self.add(at, 'schema-incompatible')
            elif old != new:
                self.add(at, 'schema-review', 'review')
            return
        ot, nt = old.get('type'), new.get('type')
        def types(t):
            return set(t if isinstance(t, list) else [t]) if t is not None else None
        ot, nt = types(ot), types(nt)
        # integer values are a subset of number values for incoming requests.
        def included(left, right):
            return all(t in right or (t == 'integer' and 'number' in right) for t in left)
        if ot != nt:
            narrowed = nt is not None and (ot is None or not included(ot, nt))
            widened = ot is not None and (nt is None or not included(nt, ot))
            if narrowed or (direction == 'response' and widened):
                self.add(pointer(at, 'type'), 'type-incompatible')
        def finite_values(schema):
            values = None if 'enum' not in schema else {canonical(v) for v in schema['enum']}
            if 'const' in schema:
                constant = {canonical(schema['const'])}
                values = constant if values is None else values & constant
            return values
        # JSON Schema applies enum and const together, including after allOf/refs.
        # None means unbounded, while the empty intersection admits no values.
        oe_values, ne_values = finite_values(old), finite_values(new)
        if oe_values != ne_values:
            removed = ne_values is not None and (oe_values is None or not oe_values <= ne_values)
            added = oe_values is not None and (ne_values is None or not ne_values <= oe_values)
            if removed or (direction == 'response' and added):
                self.add(pointer(at, 'enum'), 'enum-incompatible')
        op, np = old.get('properties', {}), new.get('properties', {})
        for name, previous in op.items():
            if experimental(previous) or experimental(self.old.schema(previous)):
                continue
            location = pointer(pointer(at, 'properties'), name)
            if name not in np:
                self.add(location, 'member-removed')
            else:
                self.schema(previous, np[name], location, direction)
        required_old, required_new = set(old.get('required', [])), set(new.get('required', []))
        if direction == 'request':
            for name in required_new - required_old:
                if not experimental(op.get(name)) and not experimental(self.old.schema(op.get(name, True))):
                    self.add(pointer(pointer(at, 'required'), name), 'request-required')
        else:
            for name in required_old - required_new:
                if not experimental(op.get(name)) and not experimental(self.old.schema(op.get(name, True))):
                    self.add(pointer(pointer(at, 'required'), name), 'response-guarantee-removed')
        if 'items' in old or 'items' in new:
            self.schema(old.get('items', True), new.get('items', True), pointer(at, 'items'), direction)
        oa, na = old.get('additionalProperties', True), new.get('additionalProperties', True)
        if oa != na:
            narrowing = oa is not False and na is False or oa is True and isinstance(na, dict)
            widening = oa is False and na is not False or isinstance(oa, dict) and na is True
            if direction == 'request' and narrowing or direction == 'response' and widening:
                self.add(pointer(at, 'additionalProperties'), 'additional-properties')
        # Equal raw refs and equal inline schemas may reach changed definitions.
        if isinstance(oa, dict) and isinstance(na, dict):
            self.schema(oa, na, pointer(at, 'additionalProperties'), direction)
        for kind in ('anyOf', 'oneOf'):
            a, b = old.get(kind), new.get(kind)
            if kind == 'oneOf' and a is not None and b is not None:
                if sorted(self.old.signature(x) for x in a) != sorted(self.new.signature(x) for x in b):
                    self.add(pointer(at, kind), 'composition-review', 'review')
            if a == b:
                # Equal refs may lead to changed component contents.
                if a:
                    for i, (x, y) in enumerate(zip(a, b)):
                        self.schema(x, y, pointer(pointer(at, kind), i), direction)
                continue
            if a is None or b is None:
                self.add(pointer(at, kind), 'composition-review', 'review')
                continue
            left, right = list(a), list(b)
            for x in a:
                candidates = [y for y in right if shape(x) == shape(y)]
                if candidates:
                    y = candidates[0]
                    left.remove(x)
                    right.remove(y)
                    self.schema(x, y, pointer(pointer(at, kind), a.index(x)), direction)
            # Stable reference identities allow deep changes behind identical refs.
            for x in list(left):
                candidates = [y for y in right if isinstance(x, dict) and isinstance(y, dict) and x.get('$ref') is not None and x.get('$ref') == y.get('$ref')]
                if len(candidates) == 1:
                    y = candidates[0]
                    left.remove(x)
                    right.remove(y)
                    self.schema(x, y, pointer(pointer(at, kind), a.index(x)), direction)
            if kind == 'oneOf' and (left or right):
                self.add(pointer(at, kind), 'composition-review', 'review')
            elif left and right:
                self.add(pointer(at, kind), 'composition-review', 'review')
            elif left and direction == 'request' or right and direction == 'response':
                self.add(pointer(at, kind), 'union-incompatible')
            # Removing a documented output branch still loses covered response values.
            elif left and direction == 'response':
                self.add(pointer(at, kind), 'composition-review', 'review')
        for k in (set(old) | set(new)) - SCHEMA_KEYS:
            # Preserve arbitrary unknown data verbatim, and also follow unchanged
            # references. Wrapping the keyword keeps named schema-map context.
            a = {k: old[k]} if k in old else {}
            b = {k: new[k]} if k in new else {}
            if canonical(a) != canonical(b) or self.old.signature(a) != self.new.signature(b):
                self.add(pointer(at, k), 'constraint-review', 'review')

    def parameters(self, path_item, operation, resolver):
        values = {}
        for entry in path_item.get('parameters', []) + operation.get('parameters', []):
            p = resolver.resolve_object(entry)
            values[(p['in'], p['name'])] = p
        return values

    def content(self, old, new, at, direction):
        oc, nc = old.get('content', {}), new.get('content', {})
        for media, value in oc.items():
            location = pointer(pointer(at, 'content'), media)
            if media not in nc:
                self.add(location, 'content-removed')
            else:
                self.schema(value.get('schema', True), nc[media].get('schema', True), pointer(location, 'schema'), direction)

    def run(self):
        for resolver in (self.old, self.new):
            resolver.validate_document_refs()
            validate_media_content(resolver)
        self.identity = '$document'
        for k in ('security',):
            if self.old.doc.get(k) != self.new.doc.get(k):
                self.add(pointer('', k), 'security-review', 'review')
        if self.old.doc.get('components', {}).get('securitySchemes') != self.new.doc.get('components', {}).get('securitySchemes'):
            self.add('/components/securitySchemes', 'security-review', 'review')
        for path, item in openapi_patterned_entries(self.old.doc.get('paths', {})):
            item = self.old.resolve_object(item)
            new_item = self.new.resolve_object(self.new.doc.get('paths', {}).get(path, {}))
            for method in sorted(METHODS & set(item)):
                old = item[method]
                if experimental(old):
                    continue
                self.identity = method.upper() + ' ' + path
                self.seen = set()
                at = pointer(pointer('/paths', path), method)
                if method not in new_item:
                    self.add(at, 'operation-removed')
                    continue
                new = new_item[method]
                for k in ('security', 'x-sparkles-permission'):
                    if old.get(k) != new.get(k):
                        self.add(pointer(at, k), 'security-review', 'review')
                previous = self.parameters(item, old, self.old)
                current = self.parameters(new_item, new, self.new)
                for key, parameter in previous.items():
                    if experimental(parameter):
                        continue
                    pa = pointer(pointer(at, 'parameters'), ':'.join(key))
                    if key not in current:
                        self.add(pa, 'parameter-removed')
                        continue
                    other = current[key]
                    if not parameter.get('required', False) and other.get('required', False):
                        self.add(pointer(pa, 'required'), 'request-required')
                    self.schema(parameter.get('schema', True), other.get('schema', True), pointer(pa, 'schema'), 'request')
                    if 'content' in parameter or 'content' in other:
                        self.content(parameter, other, pa, 'request')
                    for k in ('style', 'explode', 'allowReserved', 'allowEmptyValue'):
                        if parameter.get(k) != other.get(k):
                            self.add(pointer(pa, k), 'parameter-review', 'review')
                for key in current.keys() - previous.keys():
                    if current[key].get('required', False):
                        self.add(pointer(pointer(at, 'parameters'), ':'.join(key)), 'request-required')
                ob, nb = old.get('requestBody'), new.get('requestBody')
                if ob is not None:
                    ob = self.old.resolve_object(ob)
                    if nb is None:
                        self.add(pointer(at, 'requestBody'), 'body-removed')
                    else:
                        nb = self.new.resolve_object(nb)
                        if nb.get('required', False) and not ob.get('required', False):
                            self.add(pointer(at, 'requestBody'), 'request-required')
                        self.content(ob, nb, pointer(at, 'requestBody'), 'request')
                elif nb is not None and self.new.resolve_object(nb).get('required', False):
                    self.add(pointer(at, 'requestBody'), 'request-required')
                for status, response in openapi_patterned_entries(old.get('responses', {})):
                    ra = pointer(pointer(at, 'responses'), status)
                    if status not in new.get('responses', {}):
                        self.add(ra, 'status-removed')
                    else:
                        self.content(self.old.resolve_object(response), self.new.resolve_object(new['responses'][status]), ra, 'response')
        return sorted(self.diagnostics)


def compare_http(old, new):
    for doc in (old, new):
        if doc.get('openapi') != '3.1.0' or not isinstance(doc.get('paths'), dict):
            raise InputError('expected an OpenAPI 3.1.0 document with paths')
    return HttpDiff(old, new).run()


def validate_cli(tree):
    if type(tree.get('schema_version')) is not int or tree['schema_version'] != 1 or not isinstance(tree.get('command'), dict):
        raise InputError('unsupported CLI contract schema')
    def walk(command):
        ids, indices = set(), set()
        for positional in command.get('positionals', []):
            identity, index = positional.get('id'), positional.get('index')
            if not isinstance(identity, str) or not identity or identity in ids:
                raise InputError('CLI positionals need nonempty distinct argument IDs')
            if type(index) is not int or index < 1 or index in indices:
                raise InputError('CLI positionals need positive distinct indices')
            ids.add(identity)
            indices.add(index)
        for child in command.get('commands', {}).values():
            walk(child)
    walk(tree['command'])


def compare_cli(old, new):
    for tree in (old, new):
        validate_cli(tree)
    if old.get('profile') != new.get('profile'):
        raise InputError('CLI feature profiles differ; compare like artifacts')
    out = set()
    def walk(a, b, path):
        def add(at, rule):
            out.add(Diagnostic('cli', path, at, rule))
        for alias in set(a.get('aliases', [])) - set(b.get('aliases', [])) - {b['name']}:
            add(pointer('/aliases', alias), 'command-alias-removed')
        af, bf = a.get('flags', {}), b.get('flags', {})
        for flag, previous in af.items():
            at = pointer('/flags', flag)
            if flag not in bf:
                add(at, 'flag-removed')
            elif previous != bf[flag]:
                add(at, 'argument-changed')
        for flag in bf.keys() - af.keys():
            if bf[flag].get('required', False):
                add(pointer('/flags', flag), 'argument-changed')
        if a.get('positionals', []) != b.get('positionals', []):
            add('/positionals', 'positional-changed')
        for name, command in a.get('commands', {}).items():
            candidates = [c for n, c in b.get('commands', {}).items() if name == n or name in c.get('aliases', [])]
            if len(candidates) != 1:
                add(pointer('/commands', name), 'command-removed')
            else:
                walk(command, candidates[0], path + ' ' + name)
    walk(old['command'], new['command'], old['command']['name'])
    return sorted(out)


def load_json(path):
    def unique(pairs):
        out = {}
        for k, v in pairs:
            if k in out:
                raise InputError(f'duplicate JSON key {k}')
            out[k] = v
        return out
    def invalid_constant(value):
        raise InputError('non-finite JSON constant ' + value)
    return json.loads(Path(path).read_text(), object_pairs_hook=unique, parse_constant=invalid_constant)


def version(value):
    # Release policy requires final SemVer. Prereleases need a separately reviewed policy.
    if not isinstance(value, str) or not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', value):
        raise InputError('compatibility versions must be final X.Y.Z SemVer')
    return tuple(map(int, value.split('.')))


def safe_path(root, value):
    if not isinstance(value, str) or not value or Path(value).is_absolute() or '..' in Path(value).parts:
        raise InputError('manifest paths must be safe repository-relative paths')
    path = (root / value).resolve()
    if not path.is_relative_to(root.resolve()):
        raise InputError('manifest path escapes repository')
    return path


def read_toml(path):
    value = tomllib.loads(Path(path).read_text())
    if type(value.get('schema_version')) is not int or value['schema_version'] != 1:
        raise InputError('unsupported manifest schema version')
    return value


def inventory(path, root=ROOT):
    data = read_toml(path)
    if set(data) != {'schema_version', 'enforced', 'pending', 'baseline', 'http', 'cli'}:
        raise InputError('unknown or missing inventory fields')
    expected_pending = {'rust', 'bindings', 'mcp', 'metrics', 'cli-json-exit', 'deprecation', 'release-install'}
    if data.get('enforced') != ['http', 'cli'] or not isinstance(data.get('pending'), list) or set(data['pending']) != expected_pending:
        raise InputError('expected explicit Phase 4a enforced and pending surfaces')
    b = data.get('baseline', {})
    if b.get('kind') not in {'bootstrap', 'release'} or not re.fullmatch(r'[0-9a-f]{40}', b.get('source', '')):
        raise InputError('baseline needs a kind and exact source commit')
    if not re.fullmatch(r'[A-Za-z0-9.-]+', b.get('id', '')):
        raise InputError('invalid baseline identity')
    if set(b) != {'id', 'kind', 'source', 'version', 'released', 'profile', 'features'}:
        raise InputError('unknown or missing baseline fields')
    version(b.get('version'))
    if b.get('profile') != 'standard' or not isinstance(b.get('features'), list) or not b['features'] or len(set(b['features'])) != len(b['features']):
        raise InputError('baseline needs an explicit standard feature profile')
    if b['kind'] == 'bootstrap' and b.get('released', True) is not False:
        raise InputError('bootstrap must explicitly declare released=false')
    if b['kind'] == 'release' and b.get('released') is not True:
        raise InputError('release baseline must declare released=true')
    for surface in ('http', 'cli'):
        record = data.get(surface, {})
        expected_fields = {'path', 'sha256'} | ({'legacy_rename_content'} if surface == 'http' and 'legacy_rename_content' in record else set())
        if set(record) != expected_fields:
            raise InputError('unknown or missing surface inventory fields')
        p = safe_path(root, record.get('path'))
        if not re.fullmatch(r'[0-9a-f]{64}', record.get('sha256', '')) or hashlib.sha256(p.read_bytes()).hexdigest() != record['sha256']:
            raise InputError(f'{surface} baseline digest mismatch')
        value = load_json(p)
        if surface == 'http':
            expected = '0.0.0' if b['kind'] == 'bootstrap' else b['version']
            if value.get('info', {}).get('version') != expected:
                raise InputError('OpenAPI baseline version differs from its declared kind/version')
            normalize_baseline_http(value, data)
        else:
            validate_cli(value)
            if value.get('profile') != b['profile']:
                raise InputError('CLI baseline metadata differs from inventory')
    return data


def apply_entries(diagnostics, data, breaks, target, changelog):
    b = data['baseline']
    before, after = version(b['version']), version(target)
    if after < before:
        raise InputError('target version precedes baseline')
    if type(breaks.get('schema_version')) is not int or breaks['schema_version'] != 1 or set(breaks) - {'schema_version', 'entry'}:
        raise InputError('unsupported breaks schema version or fields')
    remaining = set(diagnostics)
    seen = set()
    for entry in breaks.get('entry', []):
        required = {'baseline', 'target', 'surface', 'identity', 'pointer', 'rule', 'severity', 'reason', 'changelog', 'disposition'}
        if set(entry) != required or any(not isinstance(entry[k], str) or not entry[k].strip() for k in required):
            raise InputError('incomplete compatibility entry')
        if entry['baseline'] != b['id'] or entry['target'] != target:
            raise InputError('compatibility entry targets another baseline/version')
        if entry['surface'] not in {'http', 'cli'} or entry['severity'] not in {'break', 'review'}:
            raise InputError('unknown compatibility surface or severity')
        key = tuple(entry[k] for k in ('surface', 'identity', 'pointer', 'rule', 'severity'))
        if key in seen or any('*' in v for v in key):
            raise InputError('duplicate or wildcard compatibility entry')
        seen.add(key)
        diagnostic = Diagnostic(*key)
        if diagnostic not in remaining:
            raise InputError('stale or unmatched compatibility entry')
        # Require a real Compatibility subsection, not a substring in arbitrary prose.
        anchor = entry['changelog']
        if anchor not in {'unreleased-compatibility', target + '-compatibility'}:
            raise InputError('invalid changelog anchor')
        section = '## Unreleased' if anchor == 'unreleased-compatibility' else '## ' + target
        start = changelog.find(section + '\n')
        if start < 0:
            raise InputError('missing changelog release anchor')
        block = changelog[start:].split('\n## ', 1)[0]
        if '\n### Compatibility\n' not in block:
            raise InputError('missing changelog Compatibility heading')
        if b['kind'] == 'bootstrap':
            if entry['disposition'] != 'development' or anchor != 'unreleased-compatibility':
                raise InputError('bootstrap differences require explicit development review')
        elif entry['severity'] == 'review' and entry['disposition'] == 'nonbreaking-review':
            pass  # Exact independent semantic review; never applies to proven breaks.
        elif entry['disposition'] != 'break':
            raise InputError('a proven break cannot be relabeled nonbreaking')
        elif after[0] == before[0] and (after[0] >= 1 or after[1] <= before[1]):
            raise InputError('patch and stable minor releases cannot waive breaks')
        elif after[0] > before[0] and before[0] >= 1:
            # A mere new exception cannot prove an earlier minor announcement.
            raise InputError('major removals require implemented prior-announcement evidence; Phase 4c is pending')
        remaining.remove(diagnostic)
    return sorted(remaining)


def git_run(root, *args, **kwargs):
    # A read/check command must not silently fetch objects in a partial clone.
    return subprocess.run(['git', '-C', str(root), *args], capture_output=True,
                          env={**os.environ, 'GIT_NO_LAZY_FETCH': '1'}, **kwargs)


def check_pinned_source(data, root=ROOT):
    b = data['baseline']
    # Verify HTTP bytes against the claimed commit, not just a PR-editable digest.
    result = git_run(root, 'show', b['source'] + ':docs/openapi.json')
    if result.returncode:
        raise InputError('baseline source commit unavailable; fetch its history explicitly')
    expected = result.stdout
    if b['kind'] == 'release' and result.returncode == 0:
        value = json.loads(expected)
        value['info']['version'] = b['version']
        expected = canonical(value).encode()
        actual = canonical(load_json(safe_path(root, data['http']['path']))).encode()
    else:
        actual = safe_path(root, data['http']['path']).read_bytes()
    if expected != actual:
        raise InputError('HTTP bootstrap bytes do not match baseline source')
    source = git_run(root, 'show', b['source'] + ':Cargo.toml')
    if source.returncode or tomllib.loads(source.stdout.decode())['workspace']['package']['version'] != b['version']:
        raise InputError('baseline package version does not match source')
    features = git_run(root, 'show', b['source'] + ':crates/sparkles-server/Cargo.toml')
    if features.returncode or sorted(tomllib.loads(features.stdout.decode())['features']['default']) != sorted(b['features']):
        raise InputError('baseline feature inventory does not match its source')


def check_base_inventory(data, base_ref, root=ROOT):
    if not base_ref or not re.fullmatch(r'[0-9a-f]{40}', base_ref):
        raise InputError('base revision must be an explicit full commit hash')
    exists = git_run(root, 'cat-file', '-e', base_ref + '^{commit}')
    if exists.returncode:
        raise InputError('base revision unavailable; fetch its history explicitly')
    tree = git_run(root, 'ls-tree', '-z', base_ref, '--', 'docs/api-compatibility.toml')
    if tree.returncode:
        raise InputError('trusted base tree unavailable; fetch its history explicitly')
    if tree.stdout:
        previous = git_run(root, 'show', base_ref + ':docs/api-compatibility.toml')
        if previous.returncode:
            raise InputError('trusted base inventory unavailable; initial trust cannot replace missing proof')
        prior = tomllib.loads(previous.stdout.decode())
        for key in ('baseline', 'http', 'cli'):
            if prior.get(key) != data.get(key):
                raise InputError('baseline inventory changed from trusted base; release baseline updates require the release workflow')
    else:
        if data['baseline']['kind'] != 'bootstrap' or data['baseline']['source'] != BOOTSTRAP_SOURCE or data['http']['sha256'] != LEGACY_RENAME_SHA:
            raise InputError('initial bootstrap must name the exact approved source and digest')
        source = data['baseline']['source']
        for ref in (source, 'HEAD'):
            proof = git_run(root, 'cat-file', '-e', ref + '^{commit}')
            if proof.returncode:
                raise InputError('bootstrap ancestry unavailable; fetch its history explicitly')
        shallow = git_run(root, 'rev-parse', '--is-shallow-repository', text=True)
        if shallow.returncode or shallow.stdout.strip() != 'false':
            raise InputError('bootstrap ancestry requires complete history, not a shallow repository')
        def ancestor(a, b):
            result = git_run(root, 'merge-base', '--is-ancestor', a, b)
            if result.returncode not in (0, 1):
                raise InputError('bootstrap ancestry could not be proved')
            return result.returncode == 0
        if not ancestor(source, 'HEAD'):
            raise InputError('approved bootstrap source is not an ancestor of the current source')
        if not (ancestor(base_ref, source) or ancestor(source, base_ref)):
            raise InputError('trusted base and approved bootstrap source diverge')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--surface', choices=['http', 'cli'])
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--current', type=Path)
    parser.add_argument('--manifest', type=Path, default=ROOT / 'docs/api-compatibility.toml')
    parser.add_argument('--breaks', type=Path, default=ROOT / 'docs/api-breaks.toml')
    parser.add_argument('--version')
    parser.add_argument('--check-manifest', action='store_true')
    parser.add_argument('--base-ref', help='trusted full base commit for CI baseline immutability')
    args = parser.parse_args(argv)
    try:
        data = inventory(args.manifest)
        breaks = read_toml(args.breaks)
        current_version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
        target = args.version or current_version
        if target != current_version:
            raise InputError('target version differs from workspace')
        if args.check_manifest:
            check_pinned_source(data)
        if args.base_ref:
            check_base_inventory(data, args.base_ref)
        for entry in breaks.get('entry', []):
            if entry.get('surface') not in {'http', 'cli'}:
                raise InputError('unknown compatibility entry surface')
        surfaces = [args.surface] if args.surface else ['http']
        diagnostics = []
        for surface in surfaces:
            baseline = args.baseline or safe_path(ROOT, data[surface]['path'])
            if baseline.resolve() != safe_path(ROOT, data[surface]['path']):
                raise InputError('explicit baseline differs from inventory')
            current = args.current or (ROOT / 'docs/openapi.json' if surface == 'http' else None)
            if current is None:
                raise InputError('CLI comparison requires generated --current tree')
            compare = compare_http if surface == 'http' else compare_cli
            previous = load_json(baseline)
            if surface == 'http':
                previous = normalize_baseline_http(previous, data)
            diagnostics.extend(compare(previous, load_json(current)))
        # Entries for another surface are checked by its generated comparison.
        selected = {**breaks, 'entry': [e for e in breaks.get('entry', []) if e.get('surface') in surfaces]}
        diagnostics = apply_entries(diagnostics, data, selected, target, (ROOT / 'CHANGELOG.md').read_text())
        for diagnostic in diagnostics:
            print(diagnostic)
        if diagnostics:
            return 1
        print('API compatibility passed against ' + data['baseline']['kind'] + ' ' + data['baseline']['id'])
        return 0
    except (InputError, OSError, ValueError, KeyError, TypeError) as error:
        print('API compatibility input error: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
