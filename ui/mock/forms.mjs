// Mock of editing forms (docs/specs/U02-shape-driven-forms.md): `GET /$/forms/{ds}`,
// `POST /$/forms/{ds}/check` and `POST /$/forms/{ds}/save`.
//
// The real server reads SHACL shapes. The mock has fixed form layouts for the people and
// the companies of the `world` dataset (mock/world.mjs), by their classes, and answers a
// form without fields for any other resource. Layouts for SKOS concepts and collections
// show nested blank-node children (SKOS-XL labels), `rdf:List` members and value options. Its check applies the change to a copy of the store and
// checks the layout's constraints, including `sh:class` of a value, which the browser
// cannot check. A save checks the expected values, commits the change and, on a protected
// `main`, goes to the session branch `edits.anonymous.{date}`.

import { randomUUID } from 'node:crypto';
import ox from 'oxigraph';
import { WORLD } from './world.mjs';
import { branchOf, MAIN } from './branches.mjs';

const SH = 'http://www.w3.org/ns/shacl#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const LANG_STRING = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString';
const LABEL = 'http://www.w3.org/2000/01/rdf-schema#label';
const S = `${WORLD}shapes/`;
const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const XL = 'http://www.w3.org/2008/05/skos-xl#';
const RDF = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';

const lit = (value, datatype) => ({ type: 'literal', value, ...(datatype ? { datatype } : {}) });

const IDENTITY = { iri: `${S}Identity`, label: 'Identity', order: 1 };
const WORK = { iri: `${S}Work`, label: 'Work', order: 2 };
const NETWORK = { iri: `${S}Network`, label: 'Network', order: 3 };

/** The form layouts by class: a node shape, its groups and its fields. */
const LAYOUTS = new Map([
  [
    `${WORLD}Person`,
    {
      shape: `${S}PersonShape`,
      groups: [IDENTITY, WORK, NETWORK],
      fields: [
        {
          path: LABEL,
          label: 'Name',
          description: 'The name people know them by, at most one per language.',
          group: IDENTITY.iri,
          order: 1,
          widget: 'text-lang',
          constraints: {
            minCount: 1,
            datatype: LANG_STRING,
            languageIn: ['en', 'fr', 'es', 'ar'],
            uniqueLang: true,
          },
        },
        {
          path: `${WORLD}email`,
          label: 'Email',
          group: IDENTITY.iri,
          order: 2,
          widget: 'text',
          constraints: {
            maxCount: 1,
            datatype: `${XSD}string`,
            pattern: [{ pattern: '^[^@\\s]+@[^@\\s]+\\.[a-z]+$' }],
          },
        },
        {
          path: `${WORLD}age`,
          label: 'Age',
          group: IDENTITY.iri,
          order: 3,
          widget: 'number',
          constraints: {
            maxCount: 1,
            datatype: `${XSD}integer`,
            minInclusive: lit('18', `${XSD}integer`),
            maxInclusive: lit('100', `${XSD}integer`),
          },
        },
        {
          path: `${WORLD}livesIn`,
          label: 'Lives in',
          group: IDENTITY.iri,
          order: 4,
          widget: 'entity',
          constraints: { maxCount: 1, class: [`${WORLD}City`], nodeKind: `${SH}IRI` },
        },
        {
          path: `${WORLD}worksFor`,
          label: 'Works for',
          group: WORK.iri,
          order: 1,
          widget: 'entity',
          constraints: {
            minCount: 1,
            maxCount: 1,
            class: [`${WORLD}Company`],
            nodeKind: `${SH}IRI`,
          },
        },
        {
          path: `${WORLD}jobTitle`,
          label: 'Job title',
          group: WORK.iri,
          order: 2,
          widget: 'text-lang',
          constraints: { datatype: LANG_STRING, uniqueLang: true },
        },
        {
          path: `${WORLD}joined`,
          label: 'Joined',
          group: WORK.iri,
          order: 3,
          widget: 'date',
          constraints: { maxCount: 1, datatype: `${XSD}date` },
        },
        {
          path: `${WORLD}status`,
          label: 'Status',
          group: WORK.iri,
          order: 4,
          widget: 'radio',
          constraints: {
            maxCount: 1,
            in: [lit('Active'), lit('On leave'), lit('Former')],
          },
        },
        {
          path: `${WORLD}notes`,
          label: 'Notes',
          group: NETWORK.iri,
          order: 2,
          widget: 'text',
          editor: 'http://datashapes.org/dash#TextAreaEditor',
          constraints: { datatype: `${XSD}string`, maxCount: 1 },
        },
        {
          path: `${WORLD}knows`,
          label: 'Knows',
          description: 'People they work with directly.',
          group: NETWORK.iri,
          order: 1,
          widget: 'entity',
          constraints: { class: [`${WORLD}Person`], nodeKind: `${SH}IRI` },
        },
      ],
    },
  ],
  [
    `${WORLD}Company`,
    {
      shape: `${S}CompanyShape`,
      groups: [],
      fields: [
        {
          path: LABEL,
          label: 'Name',
          group: null,
          order: 1,
          widget: 'text-lang',
          constraints: { minCount: 1, datatype: LANG_STRING, uniqueLang: true },
        },
        {
          path: `${WORLD}headquarters`,
          label: 'Head office',
          group: null,
          order: 2,
          widget: 'entity',
          constraints: { minCount: 1, maxCount: 1, class: [`${WORLD}City`] },
        },
        {
          path: `${WORLD}hasOffice`,
          label: 'Offices',
          group: null,
          order: 3,
          widget: 'entity',
          constraints: { class: [`${WORLD}City`] },
        },
        {
          path: `${WORLD}partnerOf`,
          label: 'Partner of',
          group: null,
          order: 4,
          widget: 'entity',
          constraints: { class: [`${WORLD}Company`] },
        },
        {
          path: `${WORLD}sector`,
          label: 'Sector',
          group: null,
          order: 5,
          widget: 'select',
          constraints: {
            maxCount: 1,
            in: ['Air', 'Rail', 'Road', 'Sea', 'Storage', 'Customs', 'Couriers'].map((v) => lit(v)),
          },
        },
      ],
    },
  ],
]);

const CONCEPT = { class: [`${SKOS}Concept`], nodeKind: `${SH}IRI` };
LAYOUTS.set(`${SKOS}Concept`, {
  shape: `${S}ConceptShape`,
  groups: [],
  fields: [
    {
      path: `${SKOS}prefLabel`,
      label: 'Preferred label',
      group: null,
      order: 1,
      widget: 'text-lang',
      constraints: { minCount: 1, datatype: LANG_STRING, uniqueLang: true },
    },
    {
      path: `${SKOS}broader`,
      label: 'Broader',
      group: null,
      order: 2,
      widget: 'entity',
      constraints: CONCEPT,
    },
    {
      path: `${XL}prefLabel`,
      label: 'XL labels',
      group: null,
      order: 3,
      widget: 'nested',
      constraints: { node: [`<${S}XLabel>`], nodeKind: `${SH}BlankNode` },
    },
  ],
});
LAYOUTS.set(`${SKOS}Collection`, {
  shape: `${S}CollectionShape`,
  groups: [],
  fields: [
    {
      path: `${SKOS}member`,
      label: 'Members',
      group: null,
      order: 1,
      widget: 'term',
      constraints: {
        or: {
          kind: 'or',
          options: [
            { shape: '_:c', label: 'Concept', widget: 'entity', constraints: CONCEPT },
            {
              shape: '_:k',
              label: 'Collection',
              widget: 'entity',
              constraints: { class: [`${SKOS}Collection`], nodeKind: `${SH}IRI` },
            },
          ],
        },
      },
    },
    {
      path: `${SKOS}memberList`,
      label: 'Ordered members',
      group: null,
      order: 2,
      widget: 'list',
      constraints: {
        maxCount: 1,
        list: { memberShapes: ['_:m'], member: CONCEPT, memberWidget: 'entity', unique: true },
      },
    },
  ],
});
// The layouts of the built-in SKOS shapes (`shapes=skos`, crates/sparkles/src/skos/
// shapes.ttl): the notes and documentation of every SKOS resource and the members of
// collections, in the shapes' groups.
const SKS = 'urn:x-sparkles:skos:';
const NOTES = { iri: `${SKS}Notes`, label: 'Notes', order: 7 };
const DOCS = { iri: `${SKS}Documentation`, label: 'Documentation', order: 8 };
const MEMBERS = { iri: `${SKS}Members`, label: 'Members', order: 6 };
const LABELS = { iri: `${SKS}Labels`, label: 'Labels', order: 1 };
const note = (local, label, group, order, area = true) => ({
  path: `${SKOS}${local}`,
  label,
  group: group.iri,
  order,
  widget: area ? 'textarea' : 'text',
  ...(area ? { editor: 'http://datashapes.org/dash#TextAreaEditor' } : {}),
  constraints: { nodeKind: `${SH}Literal` },
});
const SKOS_FIELDS = [
  {
    path: `${SKOS}prefLabel`,
    label: 'Preferred label',
    group: LABELS.iri,
    order: 1,
    widget: 'text-lang',
    constraints: { nodeKind: `${SH}Literal`, uniqueLang: true },
  },
  note('note', 'Note', NOTES, 1),
  note('definition', 'Definition', NOTES, 2),
  note('scopeNote', 'Scope note', NOTES, 3),
  note('example', 'Example', NOTES, 4, false),
  note('historyNote', 'History note', DOCS, 1),
  note('editorialNote', 'Editorial note', DOCS, 2),
  note('changeNote', 'Change note', DOCS, 3),
];
const skosLayout = (shape, extra = [], groups = []) => ({
  shape: `${SKS}${shape}`,
  groups: [LABELS, ...groups, NOTES, DOCS],
  fields: [...SKOS_FIELDS, ...extra],
});
const SKOS_LAYOUTS = new Map([
  [`${SKOS}Concept`, skosLayout('ConceptShape')],
  [`${SKOS}ConceptScheme`, skosLayout('ConceptSchemeShape')],
  [
    `${SKOS}Collection`,
    skosLayout(
      'CollectionShape',
      [{ ...LAYOUTS.get(`${SKOS}Collection`).fields[0], group: MEMBERS.iri }],
      [MEMBERS],
    ),
  ],
  [
    `${SKOS}OrderedCollection`,
    skosLayout(
      'OrderedCollectionShape',
      [{ ...LAYOUTS.get(`${SKOS}Collection`).fields[1], group: MEMBERS.iri }],
      [MEMBERS],
    ),
  ],
]);

/**
 * The layouts a form reads: the built-in SKOS ones with `shapes=skos`, none for a dataset
 * without shapes of its own (the `thes` dataset), and the mock's fixed ones otherwise.
 */
const layoutMap = (ds, shapes) =>
  shapes === 'skos' ? SKOS_LAYOUTS : ds.noFormShapes ? new Map() : LAYOUTS;

/** The fields of nested children, by the shapes a field's `constraints.node` names. */
const NODE_LAYOUTS = new Map([
  [
    `<${S}XLabel>`,
    {
      shape: `${S}XLabel`,
      fields: [
        {
          path: `${XL}literalForm`,
          label: 'Literal form',
          group: null,
          order: 1,
          widget: 'text-lang',
          constraints: { minCount: 1, maxCount: 1, datatype: LANG_STRING },
        },
      ],
    },
  ],
]);

const idOf = (path) => `<${path}>`;
const named = (v) => ox.namedNode(v);

function termJson(t) {
  if (t.termType === 'NamedNode') return { type: 'uri', value: t.value };
  if (t.termType === 'BlankNode') return { type: 'bnode', value: t.value };
  const o = { type: 'literal', value: t.value };
  if (t.language) o['xml:lang'] = t.language;
  else if (t.datatype && t.datatype.value !== `${XSD}string`) o.datatype = t.datatype.value;
  return o;
}

function rdfTerm(j, fresh) {
  if (!j || typeof j !== 'object') throw new Error('a term is an object');
  if (j.type === 'uri') return named(j.value);
  if (j.type === 'bnode') {
    if (fresh?.has(j.value)) return fresh.get(j.value);
    return ox.blankNode(j.value);
  }
  if (j.type === 'literal') {
    if (j['xml:lang']) return ox.literal(j.value, j['xml:lang']);
    return ox.literal(j.value, j.datatype ? named(j.datatype) : undefined);
  }
  throw new Error(`a change names IRIs and literals, not ${j.type}`);
}

const same = (a, b) => String(a) === String(b);

function labelOf(store, t) {
  if (t.termType !== 'NamedNode') return undefined;
  const q = store.match(t, named(LABEL), null, null)[0];
  return q?.object.value;
}

/** The node shapes of the focus: the layouts of its classes. */
function layoutsOf(store, focus, map = LAYOUTS) {
  const types = store.match(focus, named(RDF_TYPE), null, null).map((q) => q.object.value);
  return { types, layouts: types.map((t) => map.get(t)).filter(Boolean) };
}

/** The values of a field: the objects of the focus and the predicate, in every graph. */
const valuesOf = (store, focus, path) => store.match(focus, named(path), null, null);

/** The members of the `rdf:List` that `head` starts, or null when it is not one. */
function listOf(store, head) {
  const out = [];
  const seen = new Set();
  let cell = head;
  while (!(cell.termType === 'NamedNode' && cell.value === `${RDF}nil`)) {
    if (cell.termType !== 'BlankNode' || seen.has(cell.value)) return null;
    seen.add(cell.value);
    const first = store.match(cell, named(`${RDF}first`), null, null);
    const rest = store.match(cell, named(`${RDF}rest`), null, null);
    if (first.length !== 1 || rest.length !== 1) return null;
    out.push(first[0].object);
    cell = rest[0].object;
  }
  return out;
}

/** The form fields of a layout's fields at `focus`. */
function fieldsOf(store, focus, layoutFields, shape, editable, empty = false) {
  return layoutFields.map((f) => ({
    id: idOf(f.path),
    path: f.path,
    inverse: false,
    editable,
    ...(editable ? {} : { reason: 'state' }),
    label: f.label,
    ...(f.description ? { description: f.description } : {}),
    group: f.group,
    order: f.order,
    widget: f.widget,
    ...(f.editor ? { editor: f.editor } : {}),
    constraints: f.constraints,
    sources: [shape],
    values: empty
      ? []
      : valuesOf(store, focus, f.path).map((q) => ({
          term: termJson(q.object),
          graph: q.graph.termType === 'DefaultGraph' ? null : q.graph.value,
          ...(labelOf(store, q.object) ? { label: labelOf(store, q.object) } : {}),
          ...(f.constraints.list
            ? {
                items: (listOf(store, q.object) ?? []).map((t) => ({
                  term: termJson(t),
                  graph: null,
                })),
              }
            : {}),
        })),
  }));
}

/** The nested children of the fields, breadth first, and the templates of new ones. */
function nestedOf(store, fields, editable) {
  const nodes = [];
  const seen = new Map();
  const queue = [{ parent: 'root', fields }];
  while (queue.length) {
    const { parent, fields: fs } = queue.shift();
    for (const f of fs) {
      const l = NODE_LAYOUTS.get(f.constraints.node?.[0]);
      if (!l) continue;
      for (const v of f.values) {
        if (v.term.type !== 'bnode') continue;
        const anchor = { parent, field: f.id, graph: v.graph, inverse: false };
        if (seen.has(v.term.value)) {
          seen.get(v.term.value).anchors.push(anchor);
          continue;
        }
        const node = {
          id: `n${nodes.length}`,
          term: v.term,
          anchors: [anchor],
          types: [],
          groups: [],
          other: [],
          graph: null,
          fields: fieldsOf(store, ox.blankNode(v.term.value), l.fields, l.shape, editable),
        };
        seen.set(v.term.value, node);
        nodes.push(node);
        queue.push({ parent: node.id, fields: node.fields });
      }
    }
  }
  const templates = [...NODE_LAYOUTS].map(([shape, l]) => ({
    node: [shape],
    shapes: { nodeShapes: [shape.slice(1, -1)], source: 'graphs', unknownHints: [] },
    types: [],
    groups: [],
    other: [],
    graph: null,
    fields: fieldsOf(store, null, l.fields, l.shape, editable, true),
  }));
  return { nestedNodes: nodes, templates };
}

function model(ds, focusIri, opts) {
  const focus = named(focusIri);
  const store = ds.store;
  const map = opts.map ?? LAYOUTS;
  const { types, layouts } = opts.class
    ? { types: [opts.class], layouts: [map.get(opts.class)].filter(Boolean) }
    : layoutsOf(store, focus, map);
  const fields = [];
  const groups = [];
  const covered = new Set([RDF_TYPE]);
  for (const l of layouts) {
    for (const g of l.groups) if (!groups.some((x) => x.iri === g.iri)) groups.push(g);
    for (const f of l.fields) {
      covered.add(f.path);
      if (fields.some((x) => x.path === f.path)) continue;
      fields.push({
        id: idOf(f.path),
        path: f.path,
        inverse: false,
        editable: opts.editable,
        ...(opts.editable ? {} : { reason: 'state' }),
        label: f.label,
        ...(f.description ? { description: f.description } : {}),
        group: f.group,
        order: f.order,
        widget: f.widget,
        ...(f.editor ? { editor: f.editor } : {}),
        constraints: f.constraints,
        sources: [l.shape],
        values: valuesOf(store, focus, f.path).map((q) => ({
          term: termJson(q.object),
          graph: q.graph.termType === 'DefaultGraph' ? null : q.graph.value,
          ...(labelOf(store, q.object) ? { label: labelOf(store, q.object) } : {}),
          // the mock's stand-in for a graph the caller cannot write
          ...(q.graph.value.includes('readonly') ? { readOnly: true } : {}),
          ...(f.constraints.list
            ? {
                items: (listOf(store, q.object) ?? []).map((t) => ({
                  term: termJson(t),
                  graph: null,
                  ...(labelOf(store, t) ? { label: labelOf(store, t) } : {}),
                })),
              }
            : {}),
        })),
      });
    }
  }
  const other = new Map();
  for (const q of store.match(focus, null, null, null)) {
    if (covered.has(q.predicate.value)) continue;
    const list = other.get(q.predicate.value) ?? [];
    list.push({
      term: termJson(q.object),
      graph: q.graph.termType === 'DefaultGraph' ? null : q.graph.value,
    });
    other.set(q.predicate.value, list);
  }
  return {
    ...(opts.nested ? nestedOf(store, fields, opts.editable) : {}),
    formFormat: 1,
    focus: opts.class ? null : focusIri,
    types,
    commit: opts.head,
    branch: ds.branchName ?? MAIN,
    editable: opts.editable,
    shapes: {
      source: opts.map === SKOS_LAYOUTS ? 'skos' : 'guard',
      nodeShapes: layouts.map((l) => l.shape),
      unknownHints: [],
    },
    groups,
    fields,
    other: [...other].map(([predicate, values]) => ({ predicate, values })),
    graph: null,
    closed: false,
  };
}

const mint = () => `urn:uuid:${randomUUID()}`;

const today = () => new Date().toISOString().slice(0, 10).replaceAll('-', '');

/** Where a save of a form opened on `branch` goes. */
function saveTarget(main, branch) {
  if (branch && branch !== MAIN) return { to: 'branch', branch, exists: true };
  if (!main.protected) return { to: 'main', branch: MAIN, exists: true };
  const name = `edits.anonymous.${today()}`;
  return { to: 'branch', branch: name, exists: main.branchSet.has(name) };
}

/** The quads of the blank nodes `roots` that nothing else refers to once `gone` go. */
function subtree(store, roots, gone) {
  const out = [];
  const keys = new Set(gone.map(String));
  const work = [...roots];
  const done = new Set();
  while (work.length) {
    const b = work.shift();
    if (done.has(b.value)) continue;
    done.add(b.value);
    if (store.match(null, null, b, null).some((q) => !keys.has(String(q)))) continue;
    for (const q of store.match(b, null, null, null)) {
      if (keys.has(String(q))) continue;
      keys.add(String(q));
      out.push(q);
      if (q.object.termType === 'BlankNode') work.push(q.object);
    }
  }
  return out;
}

/** The update of a change: the quads to delete and to insert, or an error. */
function plan(ds, body) {
  const focus = named(body.focus);
  const map = layoutMap(ds, body.shapes);
  const { layouts } = body.class
    ? { layouts: [map.get(body.class)].filter(Boolean) }
    : layoutsOf(ds.store, focus, map);
  const fields = new Map();
  for (const l of layouts) for (const f of l.fields) fields.set(idOf(f.path), f);
  const del = [];
  const ins = [];
  const changed = [];
  const removed = [];
  // new children get fresh blank nodes for the labels the client chose
  const fresh = new Map();
  for (const e of body.nested ?? []) {
    const parentChanges =
      e.parent == null
        ? body.changes
        : body.nested.find((x) => x.node.value === e.parent.value)?.changes;
    const added = (parentChanges ?? []).some(
      (c) =>
        c.field === e.field &&
        (c.add ?? []).some((t) => t.type === 'bnode' && t.value === e.node.value),
    );
    if (added) fresh.set(e.node.value, ox.blankNode());
  }
  const apply = (node, fieldMap, changes) => {
    for (const c of changes ?? []) {
      const f = fieldMap.get(c.field);
      if (!f) return { error: [400, { error: `no field ${c.field} in this form` }] };
      try {
        for (const t of c.remove ?? []) {
          const v = rdfTerm(t, fresh);
          for (const q of ds.store.match(node, named(f.path), v, null)) del.push(q);
          if (v.termType === 'BlankNode') removed.push(v);
        }
        if (c.list) {
          for (const q of valuesOf(ds.store, node, f.path)) {
            del.push(q);
            if (q.object.termType === 'BlankNode') removed.push(q.object);
          }
          const cells = c.list.map(() => ox.blankNode());
          const nil = named(`${RDF}nil`);
          ins.push(ox.quad(node, named(f.path), cells[0] ?? nil, ox.defaultGraph()));
          c.list.forEach((t, k) => {
            ins.push(ox.quad(cells[k], named(`${RDF}first`), rdfTerm(t), ox.defaultGraph()));
            ins.push(
              ox.quad(cells[k], named(`${RDF}rest`), cells[k + 1] ?? nil, ox.defaultGraph()),
            );
          });
        }
        for (const t of c.add ?? [])
          ins.push(ox.quad(node, named(f.path), rdfTerm(t, fresh), ox.defaultGraph()));
      } catch (e) {
        return { error: [400, { error: String(e.message ?? e) }] };
      }
      changed.push({ node, f, nested: node !== focus });
    }
    return null;
  };
  const err = apply(focus, fields, body.changes);
  if (err) return err;
  for (const e of body.nested ?? []) {
    // the parent field: the root's or a nested layout's
    const pf = [...fields.values(), ...[...NODE_LAYOUTS.values()].flatMap((l) => l.fields)].find(
      (f) => idOf(f.path) === e.field,
    );
    const l = NODE_LAYOUTS.get(pf?.constraints.node?.[0]);
    if (!l) return { error: [400, { error: `nested: field ${e.field} declares no sh:node` }] };
    const node = fresh.get(e.node.value) ?? ox.blankNode(e.node.value);
    const err2 = apply(node, new Map(l.fields.map((f) => [idOf(f.path), f])), e.changes);
    if (err2) return err2;
  }
  del.push(...subtree(ds.store, removed, del));
  if (body.class) ins.push(ox.quad(focus, named(RDF_TYPE), named(body.class), ox.defaultGraph()));
  if (!del.length && !ins.length) return { error: [400, { error: 'the change changes nothing' }] };
  return { focus, del, ins, changed, layouts };
}

const quadJson = (op) => (q) => ({
  op,
  subject: String(q.subject),
  predicate: String(q.predicate),
  object: String(q.object),
  graph: q.graph.termType === 'DefaultGraph' ? null : String(q.graph),
});

const result = (focus, f, value, component, message) => ({
  focusNode: termJson(focus),
  resultPath: { type: 'uri', value: f.path },
  value: value ? termJson(value) : null,
  sourceShape: { type: 'bnode', value: 'b0' },
  sourceConstraintComponent: { type: 'uri', value: `${SH}${component}ConstraintComponent` },
  severity: { type: 'uri', value: `${SH}Violation` },
  messages: [message],
  field: idOf(f.path),
});

/** The layout's constraints of the focus on `store`, as validation results. */
function validate(store, focus, layouts) {
  const out = [];
  for (const l of layouts)
    for (const f of l.fields) {
      const c = f.constraints;
      const vs = valuesOf(store, focus, f.path).map((q) => q.object);
      if (c.minCount !== undefined && vs.length < c.minCount)
        out.push(result(focus, f, null, 'MinCount', `Less than ${c.minCount} values`));
      if (c.maxCount !== undefined && vs.length > c.maxCount)
        out.push(result(focus, f, null, 'MaxCount', `More than ${c.maxCount} values`));
      for (const v of vs) {
        for (const cls of c.class ?? [])
          if (!store.match(v, named(RDF_TYPE), named(cls), null).length)
            out.push(result(focus, f, v, 'Class', `Value does not have class <${cls}>`));
        for (const p of c.pattern ?? [])
          if (!new RegExp(p.pattern, 'u').test(v.value))
            out.push(result(focus, f, v, 'Pattern', `Value does not match ${p.pattern}`));
        if (c.minInclusive && Number(v.value) < Number(c.minInclusive.value))
          out.push(
            result(focus, f, v, 'MinInclusive', `Value is less than ${c.minInclusive.value}`),
          );
        if (c.maxInclusive && Number(v.value) > Number(c.maxInclusive.value))
          out.push(
            result(focus, f, v, 'MaxInclusive', `Value is greater than ${c.maxInclusive.value}`),
          );
        const options = c.or?.options ?? [];
        if (
          options.length &&
          !options.some((o) =>
            (o.constraints.class ?? []).every(
              (cls) => store.match(v, named(RDF_TYPE), named(cls), null).length,
            ),
          )
        )
          out.push(result(focus, f, v, 'Or', 'Value does not conform to any of the options'));
        if (c.in && !c.in.some((t) => same(rdfTerm(t), v)))
          out.push(result(focus, f, v, 'In', 'Value is not one of the allowed values'));
      }
    }
  return out;
}

/** The store after the change, as a copy. */
function applied(ds, p) {
  const st = new ox.Store(ds.store.match());
  for (const q of p.del) st.delete(q);
  for (const q of p.ins) st.add(q);
  return st;
}

/** The fields whose stored values differ from the ones the form loaded. */
function conflicts(ds, p, body) {
  const out = [];
  for (const { node, f, nested } of p.changed) {
    const entry = nested ? (body.nested ?? []).find((e) => e.node.value === node.value) : null;
    if (nested && !entry?.expect) continue;
    const expect = nested ? entry.expect : body.expect;
    const vs = valuesOf(ds.store, node, f.path).map((q) => q.object);
    const now = f.constraints.list ? vs.flatMap((h) => listOf(ds.store, h) ?? [h]) : vs;
    const want = (expect?.[idOf(f.path)] ?? []).map((t) => rdfTerm(t));
    const a = now.map(String);
    const b = want.map(String);
    const differ = f.constraints.list
      ? a.length !== b.length || a.some((x, k) => x !== b[k])
      : new Set(a).size !== new Set(b).size || a.some((x) => !b.includes(x));
    if (differ)
      out.push({
        id: idOf(f.path),
        ...(nested ? { node: termJson(node) } : {}),
        now: now.map(termJson),
      });
  }
  return out;
}

export async function handleForms(req, res, url, seg, ctx) {
  if (seg[0] !== '$' || seg[1] !== 'forms' || !seg[2]) return false;
  const { datasets, send, fail, readBody } = ctx;
  const name = decodeURIComponent(seg[2]);
  const ds = datasets.get(name);
  if (!ds) return (fail(res, 404, `no such dataset: /${name}`), true);
  const head = (b) => ctx.headCommit(b).seq;
  if (req.method === 'GET' && seg.length === 3) {
    const cls = url.searchParams.get('class')?.trim() || null;
    const focus = cls ? mint() : url.searchParams.get('focus')?.trim().replace(/^<|>$/g, '');
    if (!focus) return (fail(res, 400, "missing 'focus': the IRI of a resource"), true);
    const main = ctx.mainOf(name);
    const opened = ds.branchName ?? MAIN;
    const save = saveTarget(main, opened);
    const at = url.searchParams.get('at');
    const nested = ['true', '1'].includes(url.searchParams.get('nested') ?? '');
    const m = model(ds, focus, {
      head: head(ds),
      editable: !at,
      class: cls,
      nested,
      map: layoutMap(ds, url.searchParams.get('shapes')),
    });
    send(res, 200, {
      ...m,
      save: { ...save, mainProtected: !!main.protected },
      guard: main.guard ? { language: 'shacl', mode: main.guard.mode ?? 'reject' } : null,
    });
    return true;
  }
  if (req.method !== 'POST' || (seg[3] !== 'check' && seg[3] !== 'save') || seg.length !== 4)
    return false;
  let body;
  try {
    body = JSON.parse(String(await readBody(req)) || '{}');
  } catch (e) {
    return (fail(res, 400, `invalid JSON: ${e.message}`), true);
  }
  if (typeof body.class === 'string' && body.class) body.focus = mint();
  if (typeof body.focus !== 'string' || !Array.isArray(body.changes))
    return (fail(res, 400, 'focus and changes are required'), true);
  const main = ctx.mainOf(name);
  const save = saveTarget(main, body.branch ?? ds.branchName ?? MAIN);
  let target = branchOf(main, save.branch);
  if (!target && seg[3] === 'check') target = main;
  if (!target) {
    const made = ctx.createBranch(main, { name: save.branch, note: 'Edits made through forms' });
    if (made.error) return (send(res, ...made.error), true);
    target = made.branch;
  }
  const p = plan(target, body);
  if (p.error) return (send(res, ...p.error), true);
  if (seg[3] === 'check') {
    const after = applied(target, p);
    const results = validate(after, p.focus, p.layouts);
    const changes = [...p.del.map(quadJson('-')), ...p.ins.map(quadJson('+'))];
    const rejected =
      !!main.guard && (main.guard.mode ?? 'reject') === 'reject' && results.length > 0;
    send(res, 200, {
      branch: save.branch,
      conflicts: conflicts(target, p, body),
      preview: {
        dryRun: true,
        dataset: name,
        committed: false,
        wouldCommit: !rejected,
        outcome: rejected ? 'rejected' : 'commit',
        head: head(target),
        commit: { seq: head(target) + 1, inserted: p.ins.length, deleted: p.del.length },
        changes: { total: changes.length, limit: 1000, truncated: false, quads: changes },
      },
      update: '',
      validation: {
        checked: true,
        withheld: false,
        timedOut: false,
        conforms: results.length === 0,
        results,
      },
    });
    return true;
  }
  const clash = conflicts(target, p, body);
  if (clash.length) {
    send(res, 409, {
      error: 'another commit changed these fields since the form loaded them',
      code: 'form-conflict',
      branch: save.branch,
      fields: clash,
    });
    return true;
  }
  const before = ctx.quadSet(target);
  for (const q of p.del) target.store.delete(q);
  for (const q of p.ins) target.store.add(q);
  const receipt = ctx.commitWrite(target, 'update', before, {
    message: typeof body.message === 'string' && body.message ? body.message : undefined,
  });
  send(res, 200, {
    ...receipt,
    focus: body.focus,
    branch: save.branch,
    branchCreated: !save.exists,
  });
  return true;
}
