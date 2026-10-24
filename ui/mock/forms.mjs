// Mock of editing forms (docs/specs/U02-shape-driven-forms.md): `GET /$/forms/{ds}`,
// `POST /$/forms/{ds}/check` and `POST /$/forms/{ds}/save`.
//
// The real server reads SHACL shapes. The mock has fixed form layouts for the people and
// the companies of the `world` dataset (mock/world.mjs), by their classes, and answers a
// form without fields for any other resource. Its check applies the change to a copy of the store and
// checks the layout's constraints, including `sh:class` of a value, which the browser
// cannot check. A save checks the expected values, commits the change and, on a protected
// `main`, goes to the session branch `edits.anonymous.{date}`.

import ox from 'oxigraph';
import { WORLD } from './world.mjs';
import { branchOf, MAIN } from './branches.mjs';

const SH = 'http://www.w3.org/ns/shacl#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const LANG_STRING = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString';
const LABEL = 'http://www.w3.org/2000/01/rdf-schema#label';
const S = `${WORLD}shapes/`;

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

function rdfTerm(j) {
  if (!j || typeof j !== 'object') throw new Error('a term is an object');
  if (j.type === 'uri') return named(j.value);
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
function layoutsOf(store, focus) {
  const types = store.match(focus, named(RDF_TYPE), null, null).map((q) => q.object.value);
  return { types, layouts: types.map((t) => LAYOUTS.get(t)).filter(Boolean) };
}

/** The values of a field: the objects of the focus and the predicate, in every graph. */
const valuesOf = (store, focus, path) => store.match(focus, named(path), null, null);

function model(ds, focusIri, opts) {
  const focus = named(focusIri);
  const store = ds.store;
  const { types, layouts } = layoutsOf(store, focus);
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
        constraints: f.constraints,
        sources: [l.shape],
        values: valuesOf(store, focus, f.path).map((q) => ({
          term: termJson(q.object),
          graph: q.graph.termType === 'DefaultGraph' ? null : q.graph.value,
          ...(labelOf(store, q.object) ? { label: labelOf(store, q.object) } : {}),
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
    formFormat: 1,
    focus: focusIri,
    types,
    commit: opts.head,
    branch: ds.branchName ?? MAIN,
    editable: opts.editable,
    shapes: {
      source: 'guard',
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

const today = () => new Date().toISOString().slice(0, 10).replaceAll('-', '');

/** Where a save of a form opened on `branch` goes. */
function saveTarget(main, branch) {
  if (branch && branch !== MAIN) return { to: 'branch', branch, exists: true };
  if (!main.protected) return { to: 'main', branch: MAIN, exists: true };
  const name = `edits.anonymous.${today()}`;
  return { to: 'branch', branch: name, exists: main.branchSet.has(name) };
}

/** The update of a change: the quads to delete and to insert, or an error. */
function plan(ds, body) {
  const focus = named(body.focus);
  const { layouts } = layoutsOf(ds.store, focus);
  const fields = new Map();
  for (const l of layouts) for (const f of l.fields) fields.set(idOf(f.path), f);
  const del = [];
  const ins = [];
  const changed = [];
  for (const c of body.changes ?? []) {
    const f = fields.get(c.field);
    if (!f) return { error: [400, { error: `no field ${c.field} in this form` }] };
    try {
      for (const t of c.remove ?? [])
        for (const q of ds.store.match(focus, named(f.path), rdfTerm(t), null)) del.push(q);
      for (const t of c.add ?? [])
        ins.push(ox.quad(focus, named(f.path), rdfTerm(t), ox.defaultGraph()));
    } catch (e) {
      return { error: [400, { error: String(e.message ?? e) }] };
    }
    changed.push(f);
  }
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
function conflicts(ds, p, expect) {
  const out = [];
  for (const f of p.changed) {
    const now = valuesOf(ds.store, p.focus, f.path).map((q) => q.object);
    const want = (expect?.[idOf(f.path)] ?? []).map(rdfTerm);
    const a = new Set(now.map(String));
    const b = new Set(want.map(String));
    if (a.size !== b.size || [...a].some((x) => !b.has(x)))
      out.push({ id: idOf(f.path), now: now.map(termJson) });
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
    const focus = url.searchParams.get('focus')?.trim().replace(/^<|>$/g, '');
    if (!focus) return (fail(res, 400, "missing 'focus': the IRI of a resource"), true);
    if (url.searchParams.get('class'))
      return (
        fail(res, 501, 'forms for new instances (class=) come with phase 2 of the forms'), true
      );
    const main = ctx.mainOf(name);
    const opened = ds.branchName ?? MAIN;
    const save = saveTarget(main, opened);
    const at = url.searchParams.get('at');
    const m = model(ds, focus, { head: head(ds), editable: !at });
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
      conflicts: conflicts(target, p, body.expect),
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
  const clash = conflicts(target, p, body.expect);
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
    branch: save.branch,
    branchCreated: !save.exists,
  });
  return true;
}
