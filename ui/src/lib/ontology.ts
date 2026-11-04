// The ontology editor (docs/specs/U12-ontology-workbench/SPEC.md, "Ontology editor UI"):
// the client of `/$/ontology/{ds}` (docs/API.md, "Ontology"), the trees and frames built
// from its axioms, the Manchester-style display of class expressions, the operations an
// edit makes with their inverses for the session's undo stack, the IRI policy for new
// entities and the SPARQL of a rename and of header edits.
import { ApiError, receiptOf, request, type Receipt } from './api';
import { splitTarget } from './branches';
import { localName, shorten, sparqlIri, sparqlString, WELL_KNOWN, type PrefixMap } from './rdf';
import type { WritePreview } from './validation-overlay';

const enc = encodeURIComponent;
const OWL = WELL_KNOWN.owl;
const RDFS = WELL_KNOWN.rdfs;
const SKOS = WELL_KNOWN.skos;
const XSD = WELL_KNOWN.xsd;

export const RDFS_LABEL = RDFS + 'label';
export const RDFS_COMMENT = RDFS + 'comment';
export const SKOS_PREF_LABEL = SKOS + 'prefLabel';
export const OWL_THING = OWL + 'Thing';
export const OWL_NOTHING = OWL + 'Nothing';
export const OWL_DEPRECATED = OWL + 'deprecated';

// --- the JSON forms ------------------------------------------------------------------

export type EntityKind =
  | 'Class'
  | 'ObjectProperty'
  | 'DataProperty'
  | 'AnnotationProperty'
  | 'NamedIndividual'
  | 'Datatype';

/** The kinds the editor has a tree for, in tab order. */
export const TREE_KINDS: EntityKind[] = [
  'Class',
  'ObjectProperty',
  'DataProperty',
  'AnnotationProperty',
  'NamedIndividual',
];

export const KIND_LABEL: Record<EntityKind, string> = {
  Class: 'Classes',
  ObjectProperty: 'Object properties',
  DataProperty: 'Data properties',
  AnnotationProperty: 'Annotation properties',
  NamedIndividual: 'Individuals',
  Datatype: 'Datatypes',
};

export const KIND_SINGULAR: Record<EntityKind, string> = {
  Class: 'class',
  ObjectProperty: 'object property',
  DataProperty: 'data property',
  AnnotationProperty: 'annotation property',
  NamedIndividual: 'individual',
  Datatype: 'datatype',
};

/** A literal in the JSON form: `{"@value", "@language"?, "@type"?}`. */
export type Lit = { '@value': string; '@language'?: string; '@type'?: string };
/** An annotation value: an IRI, an anonymous individual (`_:label`) or a literal. */
export type AnnValue = string | Lit;
export type Annotation = { property: string; value: AnnValue; annotations?: Annotation[] };
/** An axiom in its JSON form (`sparkles_owl::json`). */
export type AxiomJson = { type: string; annotations?: Annotation[]; [member: string]: unknown };
/** An axiom as listings answer it. */
export type AxiomItem = { type: string; ofn: string; axiom: AxiomJson; graph?: string };
/** What an edit sends: a JSON form, or a string in functional-style syntax. */
export type AxiomInput = AxiomJson | string;

/** The graphs the editor reads: one graph (null for the default graph) and its imports. */
export type Selection = { graph: string | null; imports: boolean };

export type OntologySummary = {
  dataset: string;
  commit: number;
  mode: string;
  graphs: string[];
  imports: { iri: string; graph: string | null }[];
  warnings?: string[];
  header: {
    iri: string | null;
    versionIri: string | null;
    imports: string[];
    annotations: string[];
  };
  axioms: Record<string, number>;
  axiomCount: number;
  entities: Record<string, number>;
  triples: number;
  unmapped: number;
  diagnostics: {
    warnings: number;
    errors: number;
    list: { severity: string; code: string; message: string }[];
  };
};

export type EntityReport = {
  iri: string;
  kinds: EntityKind[];
  labels: Lit[];
  commit: number;
};

export type EditAnswer = {
  added: number;
  present: number;
  removed: number;
  absent: string[];
  declared: string[];
  addedAxioms: AxiomItem[];
  removedAxioms: AxiomItem[];
  axiomsTruncated: boolean;
  inserted: number;
  deleted: number;
  kept: number;
  committed: boolean;
  receipt?: Receipt;
};

export type TaxonomyNode = {
  node: number;
  classes: string[];
  parents: number[];
  parentClasses: string[];
  children: number;
};

export type Classification = {
  commit: number;
  consistent: boolean;
  complete: boolean;
  classes: number;
  nodeCount: number;
  unsatisfiableCount: number;
  unsatisfiable: string[];
  excludedCount: number;
  excluded: (AxiomItem & { rule?: string; message?: string; partial?: string })[];
  nodes: TaxonomyNode[];
  stats: { incremental: boolean; saturationMs: number };
};

// --- requests ------------------------------------------------------------------------

function selParams(
  sel: Selection,
  more: Record<string, string | number | boolean | undefined> = {},
) {
  const p = new URLSearchParams();
  p.set('graph', sel.graph ?? 'default');
  if (!sel.imports) p.set('imports', 'false');
  for (const [k, v] of Object.entries(more)) if (v !== undefined) p.set(k, String(v));
  return p;
}

const selBody = (sel: Selection) => ({ graph: sel.graph ?? 'default', imports: sel.imports });

async function readJson<T>(res: Response): Promise<T> {
  return (await res.json()) as T;
}

/** `GET /$/ontology/{ds}`: the summary and the entity tag an edit's `If-Match` names. */
export async function ontologySummary(
  target: string,
  sel: Selection,
  signal?: AbortSignal,
): Promise<{ summary: OntologySummary; etag: string | null }> {
  const res = await request(`/$/ontology/${enc(target)}?${selParams(sel)}`, {
    headers: { Accept: 'application/json' },
    cache: 'no-store',
    signal,
  });
  return { summary: await readJson<OntologySummary>(res), etag: res.headers.get('ETag') };
}

/** The most items the editor reads of one listing, in pages of 10,000. */
export const READ_CAP = 50_000;

/** Every entity of `kinds`, read in pages. */
export async function ontologyEntities(
  target: string,
  sel: Selection,
  kinds: EntityKind[],
  signal?: AbortSignal,
): Promise<{ entities: { kind: EntityKind; iri: string }[]; truncated: boolean }> {
  const out: { kind: EntityKind; iri: string }[] = [];
  let offset = 0;
  for (;;) {
    const p = selParams(sel, { kind: kinds.join(','), limit: 10_000, offset });
    const res = await request(`/$/ontology/${enc(target)}/entities?${p}`, {
      headers: { Accept: 'application/json' },
      cache: 'no-store',
      signal,
    });
    const j = await readJson<{
      entities: { kind: EntityKind; iri: string }[];
      next: number | null;
    }>(res);
    out.push(...j.entities);
    if (j.next == null) return { entities: out, truncated: false };
    if (out.length >= READ_CAP) return { entities: out, truncated: true };
    offset = j.next;
  }
}

/** Every axiom of `types` (or about `about`), read in pages. */
export async function ontologyAxioms(
  target: string,
  sel: Selection,
  q: { types?: string[]; about?: string; assertions?: boolean; cap?: number },
  signal?: AbortSignal,
): Promise<{ axioms: AxiomItem[]; total: number; truncated: boolean }> {
  const cap = q.cap ?? READ_CAP;
  const out: AxiomItem[] = [];
  let offset = 0;
  for (;;) {
    const p = selParams(sel, {
      type: q.types?.join(','),
      about: q.about,
      assertions: q.assertions || undefined,
      limit: Math.min(10_000, cap),
      offset,
    });
    const res = await request(`/$/ontology/${enc(target)}/axioms?${p}`, {
      headers: { Accept: 'application/json' },
      cache: 'no-store',
      signal,
    });
    const j = await readJson<{ axioms: AxiomItem[]; total: number; next: number | null }>(res);
    out.push(...j.axioms);
    if (j.next == null) return { axioms: out, total: j.total, truncated: false };
    if (out.length >= cap) return { axioms: out, total: j.total, truncated: true };
    offset = j.next;
  }
}

/** Jena's name for the stored default graph, which a query reads even under a union default. */
const DEFAULT_GRAPH_IRI = 'urn:x-arq:DefaultGraph';

/**
 * The SPARQL of every label in the graph that `sel` reads: `rdfs:label`, then
 * `skos:prefLabel`. With no graph selected it names the stored default graph, as the ontology
 * routes read it, so a server with `--union-default-graph` does not add other graphs' labels.
 */
export function labelsQuery(sel: Selection): string {
  const pattern = `VALUES ?p { ${sparqlIri(RDFS_LABEL)} ${sparqlIri(SKOS_PREF_LABEL)} } ?s ?p ?l FILTER(isIRI(?s) && isLiteral(?l))`;
  return `SELECT ?s ?p ?l WHERE { GRAPH ${sparqlIri(sel.graph ?? DEFAULT_GRAPH_IRI)} { ${pattern} } }`;
}

type SparqlBinding = Record<string, { type: string; value: string; 'xml:lang'?: string }>;

/** The labels of SPARQL JSON result rows of `labelsQuery`, `rdfs:label` first. */
export function labelsOfBindings(rows: SparqlBinding[]): Map<string, Lit[]> {
  const out = new Map<string, Lit[]>();
  for (const r of rows) {
    const s = r.s?.value;
    const l = r.l;
    if (!s || !l) continue;
    const lit: Lit = { '@value': l.value };
    if (l['xml:lang']) lit['@language'] = l['xml:lang'];
    const list = out.get(s) ?? [];
    if (r.p?.value === RDFS_LABEL) list.unshift(lit);
    else list.push(lit);
    out.set(s, list);
  }
  return out;
}

/**
 * Every label of the graph that `sel` reads on `target`'s branch, in one SPARQL query. The
 * editor fills the labels its capped axiom listing left out with these, so a large
 * ontology's trees show labels rather than local names. Imported ontologies are not read.
 */
export async function ontologyLabels(
  target: string,
  sel: Selection,
  signal?: AbortSignal,
): Promise<Map<string, Lit[]>> {
  const res = await request(`/${enc(target)}/sparql`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    body: labelsQuery(sel),
    signal,
  });
  const j = await readJson<{ results: { bindings: SparqlBinding[] } }>(res);
  return labelsOfBindings(j.results.bindings);
}

/** The RDF type that declares an entity of each kind, which a read of a kind's labels follows. */
export const KIND_TYPE: Record<EntityKind, string> = {
  Class: OWL + 'Class',
  ObjectProperty: OWL + 'ObjectProperty',
  DataProperty: OWL + 'DatatypeProperty',
  AnnotationProperty: OWL + 'AnnotationProperty',
  NamedIndividual: OWL + 'NamedIndividual',
  Datatype: RDFS + 'Datatype',
};

/** The SPARQL of the labels of `iris` alone, in the graph `sel` reads, as `labelsQuery` reads them. */
export function labelsForQuery(sel: Selection, iris: readonly string[]): string {
  const pattern = `VALUES ?s { ${iris.map(sparqlIri).join(' ')} } VALUES ?p { ${sparqlIri(RDFS_LABEL)} ${sparqlIri(SKOS_PREF_LABEL)} } ?s ?p ?l FILTER(isLiteral(?l))`;
  return `SELECT ?s ?p ?l WHERE { GRAPH ${sparqlIri(sel.graph ?? DEFAULT_GRAPH_IRI)} { ${pattern} } }`;
}

/** The SPARQL of the labels of the entities declared with one of `types`. */
export function typedLabelsQuery(sel: Selection, types: readonly string[]): string {
  const pattern = `VALUES ?t { ${types.map(sparqlIri).join(' ')} } ?s a ?t . VALUES ?p { ${sparqlIri(RDFS_LABEL)} ${sparqlIri(SKOS_PREF_LABEL)} } ?s ?p ?l FILTER(isIRI(?s) && isLiteral(?l))`;
  return `SELECT ?s ?p ?l WHERE { GRAPH ${sparqlIri(sel.graph ?? DEFAULT_GRAPH_IRI)} { ${pattern} } }`;
}

/**
 * The SPARQL of the labels of the individuals: the subjects with a type that is not one of
 * the OWL entity types, so that an individual which only its class assertions name has its
 * label read with the declared ones.
 */
export function individualLabelsQuery(sel: Selection): string {
  const schema = [
    ...Object.values(KIND_TYPE),
    OWL + 'Ontology',
    OWL + 'Restriction',
    OWL + 'Axiom',
    OWL + 'AllDisjointClasses',
    OWL + 'AllDifferent',
    RDFS + 'Class',
    WELL_KNOWN.rdf + 'Property',
  ];
  const pattern = `?s a ?t FILTER(?t NOT IN (${schema.map(sparqlIri).join(', ')})) VALUES ?p { ${sparqlIri(RDFS_LABEL)} ${sparqlIri(SKOS_PREF_LABEL)} } ?s ?p ?l FILTER(isIRI(?s) && isLiteral(?l))`;
  return `SELECT DISTINCT ?s ?p ?l WHERE { GRAPH ${sparqlIri(sel.graph ?? DEFAULT_GRAPH_IRI)} { ${pattern} } }`;
}

async function labelRows(target: string, query: string, signal?: AbortSignal) {
  const res = await request(`/${enc(target)}/sparql`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    body: query,
    signal,
  });
  const j = await readJson<{ results: { bindings: SparqlBinding[] } }>(res);
  return labelsOfBindings(j.results.bindings);
}

/** The labels of `iris`, in one query; the editor reads the labels it shows as they are asked for. */
export function ontologyLabelsFor(
  target: string,
  sel: Selection,
  iris: readonly string[],
  signal?: AbortSignal,
): Promise<Map<string, Lit[]>> {
  return iris.length
    ? labelRows(target, labelsForQuery(sel, iris), signal)
    : Promise.resolve(new Map());
}

/** The labels of the individuals, declared or named only by their class assertions, in one query. */
export function ontologyIndividualLabels(
  target: string,
  sel: Selection,
  signal?: AbortSignal,
): Promise<Map<string, Lit[]>> {
  return labelRows(target, individualLabelsQuery(sel), signal);
}

/** The labels of every entity of `kinds` that the graph declares, in one query. */
export function ontologyKindLabels(
  target: string,
  sel: Selection,
  kinds: readonly EntityKind[],
  signal?: AbortSignal,
): Promise<Map<string, Lit[]>> {
  return labelRows(
    target,
    typedLabelsQuery(
      sel,
      kinds.map((k) => KIND_TYPE[k]),
    ),
    signal,
  );
}

/** `labels` with the subjects of `more` that it has no label for. */
export function withMissingLabels(
  labels: Map<string, Lit[]>,
  more: Map<string, Lit[]>,
): Map<string, Lit[]> {
  const out = new Map(labels);
  for (const [s, l] of more) if (!out.get(s)?.length) out.set(s, l);
  return out;
}

/** `GET /$/ontology/{ds}/entity`: the kinds and labels of an entity; null when unknown. */
export async function ontologyEntity(
  target: string,
  sel: Selection,
  iri: string,
  signal?: AbortSignal,
): Promise<EntityReport | null> {
  try {
    const res = await request(`/$/ontology/${enc(target)}/entity?${selParams(sel, { iri })}`, {
      headers: { Accept: 'application/json' },
      cache: 'no-store',
      signal,
    });
    return await readJson<EntityReport>(res);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}

/** `POST /$/ontology/{ds}/classify` with every node of the taxonomy, read in pages. */
export async function classifyOntology(
  target: string,
  sel: Selection,
  signal?: AbortSignal,
): Promise<Classification> {
  let offset = 0;
  let first: Classification | null = null;
  for (;;) {
    const res = await request(`/$/ontology/${enc(target)}/classify`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ ...selBody(sel), nodes: true, limit: 10_000, offset }),
      signal,
    });
    const j = await readJson<Classification & { next?: number }>(res);
    if (!first) first = j;
    else first.nodes.push(...j.nodes);
    if (j.next == null || first.nodes.length >= READ_CAP) return first;
    offset = j.next;
  }
}

/** What `POST /$/ontology/{ds}/parse` reads Manchester text as. */
export type ParseAs = 'classExpression' | 'dataRange' | 'individual' | 'literal' | 'axiom';

/** Manchester text that does not parse: the message, the span in UTF-16 code units and the
 * entities an ambiguous name could stand for. */
export type ManchesterProblem = {
  error: string;
  reason: 'syntax' | 'unknown' | 'ambiguous' | 'kind';
  start: number;
  end: number;
  candidates: { iri: string; kind: string; label: string | null }[];
};

export type Parsed = { value: unknown; ofn: string; manchester: string | null };

const isProblem = (b: unknown): b is ManchesterProblem =>
  !!b && typeof b === 'object' && (b as { code?: unknown }).code === 'invalid-manchester';

/**
 * `POST /$/ontology/{ds}/parse`: Manchester text read against the ontology, or the
 * problem with its span when it does not parse.
 */
export async function parseManchester(
  target: string,
  sel: Selection,
  text: string,
  as: ParseAs = 'classExpression',
  signal?: AbortSignal,
): Promise<{ ok: true; parsed: Parsed } | { ok: false; problem: ManchesterProblem }> {
  try {
    const res = await request(`/$/ontology/${enc(target)}/parse`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ ...selBody(sel), text, as }),
      signal,
    });
    return { ok: true, parsed: await readJson<Parsed>(res) };
  } catch (e) {
    if (e instanceof ApiError && isProblem(e.body)) return { ok: false, problem: e.body };
    throw e;
  }
}

/** The answers a DL query can ask for, in the order the tab shows them. */
export const DL_ANSWERS = [
  ['directSuperclasses', 'Direct superclasses'],
  ['superclasses', 'Superclasses'],
  ['equivalents', 'Equivalent classes'],
  ['directSubclasses', 'Direct subclasses'],
  ['subclasses', 'Subclasses'],
  ['instances', 'Instances'],
] as const;

export type DlAnswer = (typeof DL_ANSWERS)[number][0];
export type DlReasoning = 'auto' | 'el' | 'rl';

export type DlGap = {
  message: string;
  rule: string | null;
  partial: boolean;
  type?: string;
  ofn?: string;
  manchester?: string | null;
  axiom?: AxiomJson;
};

export type DlQueryResult = {
  commit: number;
  expression: { manchester: string; ofn: string; json: unknown };
  reasoning: {
    engine: 'el' | 'rl';
    title: string;
    why: string;
    complete: boolean;
    instancesComplete: boolean | null;
    gapCount: number;
    gaps: DlGap[];
    notes: string[];
  };
  satisfiable: boolean | null;
  consistent: boolean | null;
  incremental: boolean;
  elapsedMs: number;
  counts: Partial<Record<DlAnswer, number>>;
} & Partial<Record<DlAnswer, string[]>>;

/**
 * `POST /$/ontology/{ds}/dl-query`: a DL query of a Manchester class expression, or the
 * problem with its span when the expression does not parse.
 */
export async function dlQuery(
  target: string,
  sel: Selection,
  q: { expression: string; answers: DlAnswer[]; reasoning: DlReasoning; limit?: number },
  signal?: AbortSignal,
): Promise<{ ok: true; result: DlQueryResult } | { ok: false; problem: ManchesterProblem }> {
  try {
    const res = await request(`/$/ontology/${enc(target)}/dl-query`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ ...selBody(sel), ...q }),
      signal,
    });
    return { ok: true, result: await readJson<DlQueryResult>(res) };
  } catch (e) {
    if (e instanceof ApiError && isProblem(e.body)) return { ok: false, problem: e.body };
    throw e;
  }
}

/** The options of a write: a dry run, the tag of the state the editor shows, a message. */
export type WriteOpts = {
  dryRun?: boolean;
  etag?: string | null;
  message?: string;
  /** The head commit the editor shows, which a revert expects the branch to be at. */
  head?: number | null;
};

/** The preview of a dry run, which a `412`, `422` or `507` carries as its body. */
async function previewOr<T>(go: () => Promise<Response>, dry: boolean): Promise<T | WritePreview> {
  try {
    const res = await go();
    return (await res.json()) as T | WritePreview;
  } catch (e) {
    if (dry && e instanceof ApiError && e.body && (e.body as { dryRun?: unknown }).dryRun === true)
      return e.body as unknown as WritePreview;
    throw e;
  }
}

/**
 * A commit message as the `Sparkles-Commit-Message` header carries it: as it is when it is
 * printable ASCII, else percent-encoded UTF-8 after `utf-8''` (docs/API.md), since a
 * header value cannot hold other characters.
 */
export function commitMessageHeader(message: string): string {
  return /^[\x20-\x7e]*$/.test(message) ? message : `utf-8''${encodeURIComponent(message)}`;
}

function writeHeaders(o: WriteOpts, json: boolean): Record<string, string> {
  const h: Record<string, string> = { Accept: 'application/json' };
  if (json) h['Content-Type'] = 'application/json';
  if (o.etag) h['If-Match'] = o.etag;
  if (o.message) h['Sparkles-Commit-Message'] = commitMessageHeader(o.message);
  return h;
}

const writeQuery = (o: WriteOpts) => (o.dryRun ? 'dryRun=true&changes=50' : 'receipt=true');

function withReceipt(j: EditAnswer): EditAnswer {
  const r = receiptOf(j as unknown as Record<string, unknown>);
  return r ? { ...j, receipt: r } : j;
}

/** `POST /$/ontology/{ds}/axioms`: an edit, or its preview with `dryRun`. */
export async function editAxioms(
  target: string,
  sel: Selection,
  edit: { add?: AxiomInput[]; remove?: AxiomInput[]; declare?: boolean },
  o: WriteOpts = {},
): Promise<EditAnswer | WritePreview> {
  const body = {
    ...selBody(sel),
    add: edit.add ?? [],
    remove: edit.remove ?? [],
    ...(edit.declare === false ? { declare: false } : {}),
  };
  const r = await previewOr<EditAnswer>(
    () =>
      request(`/$/ontology/${enc(target)}/axioms?${writeQuery(o)}`, {
        method: 'POST',
        headers: writeHeaders(o, true),
        body: JSON.stringify(body),
      }),
    !!o.dryRun,
  );
  return isPreview(r) ? r : withReceipt(r);
}

/** `DELETE /$/ontology/{ds}/entity`: an entity removal, or its preview. */
export async function removeEntity(
  target: string,
  sel: Selection,
  iri: string,
  o: WriteOpts = {},
): Promise<EditAnswer | WritePreview> {
  const p = selParams(sel, { iri });
  const r = await previewOr<EditAnswer>(
    () =>
      request(`/$/ontology/${enc(target)}/entity?${p}&${writeQuery(o)}`, {
        method: 'DELETE',
        headers: writeHeaders(o, false),
      }),
    !!o.dryRun,
  );
  return isPreview(r) ? r : withReceipt(r);
}

export type UpdateAnswer = { inserted: number; deleted: number; receipt?: Receipt };

/**
 * `POST /$/revert/{ds}?branch=&commit=`: the revert of one commit of the target's branch,
 * or its preview. A revert is a three-way merge of the commit's parent into the head, so it
 * restores exactly the quads, blank nodes included, that the commit changed. `o.head` is the
 * head the editor shows, and a head that moved since answers `409` `head-moved`.
 */
export async function revertCommit(
  target: string,
  commit: number,
  o: WriteOpts = {},
): Promise<UpdateAnswer | WritePreview> {
  const { name, branch } = splitTarget(target);
  const p = new URLSearchParams({ branch, commit: String(commit) });
  const body = {
    ...(o.dryRun ? { dryRun: true, changes: 50 } : {}),
    ...(o.head != null ? { expect: { target: o.head } } : {}),
    ...(o.message ? { message: o.message } : {}),
  };
  const r = await previewOr<Record<string, unknown>>(
    () =>
      request(`/$/revert/${enc(name)}?${p}`, {
        method: 'POST',
        headers: { Accept: 'application/json', 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }),
    !!o.dryRun,
  );
  if (isPreview(r)) return r;
  // the head no longer holds the commit's changes, for instance because another writer
  // reverted it: the server answers `upToDate` with no commit, for a dry run too
  if (o.dryRun && r.upToDate === true)
    return {
      dryRun: true,
      wouldCommit: false,
      outcome: 'no-change',
      head: Number((r.target as { seq?: number } | undefined)?.seq ?? o.head ?? 0),
    };
  const c = r.commit as Receipt['commit'] | null | undefined;
  const changes = (r.changes ?? {}) as { inserted?: number; deleted?: number };
  return {
    inserted: Number(changes.inserted ?? 0),
    deleted: Number(changes.deleted ?? 0),
    receipt: c ? { dataset: name, datasetId: '', committed: true, commit: c } : undefined,
  };
}

/** `POST /{ds}/update`: a SPARQL update, or its preview. */
export async function runUpdate(
  target: string,
  sparql: string,
  o: WriteOpts = {},
): Promise<UpdateAnswer | WritePreview> {
  const r = await previewOr<UpdateAnswer & Record<string, unknown>>(
    () =>
      request(`/${enc(target)}/update?${writeQuery(o)}`, {
        method: 'POST',
        headers: {
          ...writeHeaders(o, false),
          'Content-Type': 'application/sparql-update',
        },
        body: sparql,
      }),
    !!o.dryRun,
  );
  if (isPreview(r)) return r;
  const receipt = receiptOf(r);
  return { inserted: Number(r.inserted ?? 0), deleted: Number(r.deleted ?? 0), receipt };
}

export function isPreview(x: unknown): x is WritePreview {
  return !!x && typeof x === 'object' && (x as { dryRun?: unknown }).dryRun === true;
}

// --- labels --------------------------------------------------------------------------

/** The annotation properties whose literals name an entity, best first. */
export const LABEL_PROPERTIES = [RDFS_LABEL, SKOS_PREF_LABEL];

export const isLit = (v: unknown): v is Lit =>
  !!v && typeof v === 'object' && typeof (v as Lit)['@value'] === 'string';

/** The label literals of each subject among annotation assertions. */
export function labelsOf(axioms: Iterable<AxiomItem | AxiomJson>): Map<string, Lit[]> {
  const out = new Map<string, Lit[]>();
  for (const it of axioms) {
    const a =
      'axiom' in it && typeof it.axiom === 'object' ? (it.axiom as AxiomJson) : (it as AxiomJson);
    if (a.type !== 'AnnotationAssertion') continue;
    const prop = a.property as string;
    const rank = LABEL_PROPERTIES.indexOf(prop);
    if (rank < 0 || typeof a.subject !== 'string' || !isLit(a.value)) continue;
    const l = out.get(a.subject) ?? [];
    // rdfs:label first, then skos:prefLabel
    if (rank === 0) l.unshift(a.value);
    else l.push(a.value);
    out.set(a.subject, l);
  }
  return out;
}

/**
 * The label to show among `lits`: the first in one of the `langs` (in order, matching a
 * primary subtag too), then one without a language, then English, then the first.
 */
export function pickLabel(
  lits: Lit[] | undefined,
  langs: readonly string[] = ['en'],
): string | null {
  if (!lits?.length) return null;
  const lower = (s?: string) => (s ?? '').toLowerCase();
  for (const want of langs.map(lower)) {
    const hit =
      lits.find((l) => lower(l['@language']) === want) ??
      lits.find((l) => lower(l['@language']).split('-')[0] === want.split('-')[0]);
    if (hit) return hit['@value'];
  }
  return (
    lits.find((l) => !l['@language'])?.['@value'] ??
    lits.find((l) => lower(l['@language']).startsWith('en'))?.['@value'] ??
    lits[0]['@value']
  );
}

/** A short name for an IRI without a label: a prefixed name or the local name. */
export function iriName(iri: string, prefixes: PrefixMap): string {
  return shorten(iri, prefixes) ?? localName(iri);
}

/** The display of an entity with `labels`. */
export function entityName(
  iri: string,
  labels: Map<string, Lit[]>,
  prefixes: PrefixMap,
  langs?: readonly string[],
): string {
  return pickLabel(labels.get(iri), langs) ?? iriName(iri, prefixes);
}

// --- trees ---------------------------------------------------------------------------

/** The top entity of each kind's hierarchy, whose subentities are roots. */
const TOPS = new Set([OWL_THING, OWL + 'topObjectProperty', OWL + 'topDataProperty']);

export type Tree = {
  roots: string[];
  children: Map<string, string[]>;
  parents: Map<string, string[]>;
  nodes: Set<string>;
};

/** The named child–parent pairs of a kind's hierarchy among `axioms`. */
export function hierarchyEdges(
  kind: EntityKind,
  axioms: Iterable<AxiomItem | AxiomJson>,
): [string, string][] {
  const want: Record<string, string> = {
    Class: 'SubClassOf',
    ObjectProperty: 'SubObjectPropertyOf',
    DataProperty: 'SubDataPropertyOf',
    AnnotationProperty: 'SubAnnotationPropertyOf',
    NamedIndividual: 'ClassAssertion',
  };
  const t = want[kind];
  const out: [string, string][] = [];
  for (const it of axioms) {
    const a =
      'axiom' in it && typeof it.axiom === 'object' ? (it.axiom as AxiomJson) : (it as AxiomJson);
    if (a.type !== t) continue;
    const [c, p] =
      kind === 'Class'
        ? [a.subClass, a.superClass]
        : kind === 'NamedIndividual'
          ? [a.individual, a.class]
          : [a.subProperty, a.superProperty];
    if (typeof c === 'string' && typeof p === 'string' && !c.startsWith('_:')) out.push([c, p]);
  }
  return out;
}

/**
 * A tree of `nodes` by the `edges` (child, parent). A node is a root when it has no
 * parent among the nodes (a parent outside them, such as owl:Thing, does not count). A
 * cycle leaves its members without a root: the first member of each such group by IRI
 * becomes one, so that every node is reachable.
 */
export function buildTree(nodes: Iterable<string>, edges: Iterable<[string, string]>): Tree {
  const set = new Set(nodes);
  const children = new Map<string, string[]>();
  const parents = new Map<string, string[]>();
  for (const [c, p] of edges) {
    if (!set.has(c) || !set.has(p) || c === p || TOPS.has(p)) continue;
    const ps = parents.get(c) ?? [];
    if (ps.includes(p)) continue;
    ps.push(p);
    parents.set(c, ps);
    const cs = children.get(p) ?? [];
    cs.push(c);
    children.set(p, cs);
  }
  const roots = [...set].filter((n) => !parents.get(n)?.length && !TOPS.has(n)).sort();
  // nodes only a cycle reaches
  const seen = new Set<string>();
  const walk = (start: string) => {
    const stack = [start];
    while (stack.length) {
      const n = stack.pop()!;
      if (seen.has(n)) continue;
      seen.add(n);
      for (const c of children.get(n) ?? []) stack.push(c);
    }
  };
  roots.forEach(walk);
  for (const n of [...set].sort()) {
    if (!seen.has(n) && !TOPS.has(n)) {
      roots.push(n);
      walk(n);
    }
  }
  return { roots, children, parents, nodes: set };
}

/** The nodes a filter keeps: those whose name matches, and their ancestors. */
export function filterTree(
  tree: Tree,
  name: (iri: string) => string,
  text: string,
): Set<string> | null {
  const q = text.trim().toLowerCase();
  if (!q) return null;
  const keep = new Set<string>();
  const up = (n: string) => {
    const stack = [n];
    while (stack.length) {
      const x = stack.pop()!;
      if (keep.has(x)) continue;
      keep.add(x);
      for (const p of tree.parents.get(x) ?? []) stack.push(p);
    }
  };
  for (const n of tree.nodes) {
    if (name(n).toLowerCase().includes(q) || n.toLowerCase().includes(q)) up(n);
  }
  return keep;
}

/** The ancestors of `iri`, so that a tree can open the path to it. */
export function ancestorsOf(tree: Tree, iri: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>([iri]);
  const stack = [...(tree.parents.get(iri) ?? [])];
  while (stack.length) {
    const p = stack.pop()!;
    if (seen.has(p)) continue;
    seen.add(p);
    out.push(p);
    stack.push(...(tree.parents.get(p) ?? []));
  }
  return out;
}

/** The inferred class tree of a classification, by node: `n:<node>` ids. */
export function taxonomyTree(c: Classification): {
  tree: Tree;
  classesOf: Map<string, string[]>;
  nodeOf: Map<string, string>;
} {
  const classesOf = new Map<string, string[]>();
  const nodeOf = new Map<string, string>();
  const ids: string[] = [];
  const edges: [string, string][] = [];
  for (const n of c.nodes) {
    // node 0 holds owl:Thing, node 1 owl:Nothing and the unsatisfiable classes
    if (n.node === 0) continue;
    const named = n.classes.filter((x) => x !== OWL_THING);
    if (n.node === 1 && named.every((x) => x === OWL_NOTHING)) continue;
    const id = `n:${n.node}`;
    ids.push(id);
    classesOf.set(id, named);
    for (const x of named) nodeOf.set(x, id);
    for (const p of n.parents) if (p !== 0) edges.push([id, `n:${p}`]);
  }
  return { tree: buildTree(ids, edges), classesOf, nodeOf };
}

// --- the frame -----------------------------------------------------------------------

/** One row of a frame: an axiom about the entity, shown by the part that is not it. */
export type FrameRow = { item: AxiomItem; shown: unknown[] };
export type FrameSection = { id: string; title: string; rows: FrameRow[] };

const others = (xs: unknown, iri: string): unknown[] =>
  Array.isArray(xs) ? xs.filter((x) => x !== iri) : [];

/** The section ids of each kind's frame, in order, with their titles. */
export const SECTIONS: Record<EntityKind, [string, string][]> = {
  Class: [
    ['sub', 'SubClass Of'],
    ['equivalent', 'Equivalent To'],
    ['disjoint', 'Disjoint With'],
    ['disjointUnion', 'Disjoint Union Of'],
    ['superOf', 'Superclass Of'],
    ['keys', 'Keys'],
    ['instances', 'Instances'],
  ],
  ObjectProperty: [
    ['domains', 'Domains'],
    ['ranges', 'Ranges'],
    ['superProperties', 'SubProperty Of'],
    ['subProperties', 'SuperProperty Of'],
    ['equivalent', 'Equivalent To'],
    ['disjoint', 'Disjoint With'],
    ['inverses', 'Inverse Of'],
    ['chains', 'Property Chains'],
  ],
  DataProperty: [
    ['domains', 'Domains'],
    ['ranges', 'Ranges'],
    ['superProperties', 'SubProperty Of'],
    ['subProperties', 'SuperProperty Of'],
    ['equivalent', 'Equivalent To'],
    ['disjoint', 'Disjoint With'],
  ],
  AnnotationProperty: [
    ['superProperties', 'SubProperty Of'],
    ['subProperties', 'SuperProperty Of'],
    ['domains', 'Domains'],
    ['ranges', 'Ranges'],
  ],
  NamedIndividual: [
    ['types', 'Types'],
    ['same', 'Same Individual As'],
    ['different', 'Different Individuals'],
    ['objectAssertions', 'Object Property Assertions'],
    ['dataAssertions', 'Data Property Assertions'],
    ['negative', 'Negative Property Assertions'],
  ],
  Datatype: [['definitions', 'Definitions']],
};

/** The characteristics an object property can have, and the axiom of each. */
export const OBJECT_CHARACTERISTICS: [string, string][] = [
  ['FunctionalObjectProperty', 'Functional'],
  ['InverseFunctionalObjectProperty', 'Inverse functional'],
  ['TransitiveObjectProperty', 'Transitive'],
  ['SymmetricObjectProperty', 'Symmetric'],
  ['AsymmetricObjectProperty', 'Asymmetric'],
  ['ReflexiveObjectProperty', 'Reflexive'],
  ['IrreflexiveObjectProperty', 'Irreflexive'],
];

/** The section an axiom about `iri` belongs to in a frame of `kind`, and what it shows. */
export function sectionOf(kind: EntityKind, iri: string, a: AxiomJson): [string, unknown[]] | null {
  const t = a.type;
  switch (kind) {
    case 'Class':
      if (t === 'SubClassOf' && a.subClass === iri) return ['sub', [a.superClass]];
      if (t === 'SubClassOf' && a.superClass === iri) return ['superOf', [a.subClass]];
      if (t === 'EquivalentClasses' && (a.classes as unknown[]).includes(iri))
        return ['equivalent', others(a.classes, iri)];
      if (t === 'DisjointClasses' && (a.classes as unknown[]).includes(iri))
        return ['disjoint', others(a.classes, iri)];
      if (t === 'DisjointUnion' && a.class === iri)
        return ['disjointUnion', a.classes as unknown[]];
      if (t === 'HasKey' && a.class === iri)
        return [
          'keys',
          [
            ...((a.objectProperties as unknown[]) ?? []),
            ...((a.dataProperties as unknown[]) ?? []),
          ],
        ];
      if (t === 'ClassAssertion' && a.class === iri) return ['instances', [a.individual]];
      return null;
    case 'ObjectProperty':
    case 'DataProperty': {
      const p = a.property;
      if ((t === 'ObjectPropertyDomain' || t === 'DataPropertyDomain') && p === iri)
        return ['domains', [a.domain]];
      if ((t === 'ObjectPropertyRange' || t === 'DataPropertyRange') && p === iri)
        return ['ranges', [a.range]];
      if (
        (t === 'SubObjectPropertyOf' || t === 'SubDataPropertyOf') &&
        a.chain &&
        a.superProperty === iri
      )
        return ['chains', [{ type: 'Chain', chain: a.chain }]];
      if ((t === 'SubObjectPropertyOf' || t === 'SubDataPropertyOf') && a.subProperty === iri)
        return ['superProperties', [a.superProperty]];
      if (
        (t === 'SubObjectPropertyOf' || t === 'SubDataPropertyOf') &&
        a.superProperty === iri &&
        a.subProperty
      )
        return ['subProperties', [a.subProperty]];
      if (
        (t === 'EquivalentObjectProperties' || t === 'EquivalentDataProperties') &&
        (a.properties as unknown[]).includes(iri)
      )
        return ['equivalent', others(a.properties, iri)];
      if (
        (t === 'DisjointObjectProperties' || t === 'DisjointDataProperties') &&
        (a.properties as unknown[]).includes(iri)
      )
        return ['disjoint', others(a.properties, iri)];
      if (t === 'InverseObjectProperties' && (a.first === iri || a.second === iri))
        return ['inverses', [a.first === iri ? a.second : a.first]];
      return null;
    }
    case 'AnnotationProperty':
      if (t === 'SubAnnotationPropertyOf' && a.subProperty === iri)
        return ['superProperties', [a.superProperty]];
      if (t === 'SubAnnotationPropertyOf' && a.superProperty === iri)
        return ['subProperties', [a.subProperty]];
      if (t === 'AnnotationPropertyDomain' && a.property === iri) return ['domains', [a.domain]];
      if (t === 'AnnotationPropertyRange' && a.property === iri) return ['ranges', [a.range]];
      return null;
    case 'NamedIndividual':
      if (t === 'ClassAssertion' && a.individual === iri) return ['types', [a.class]];
      if (t === 'SameIndividual' && (a.individuals as unknown[]).includes(iri))
        return ['same', others(a.individuals, iri)];
      if (t === 'DifferentIndividuals' && (a.individuals as unknown[]).includes(iri))
        return ['different', others(a.individuals, iri)];
      if (t === 'ObjectPropertyAssertion' && a.source === iri)
        return ['objectAssertions', [a.property, a.target]];
      if (t === 'DataPropertyAssertion' && a.source === iri)
        return ['dataAssertions', [a.property, a.value]];
      if (
        (t === 'NegativeObjectPropertyAssertion' || t === 'NegativeDataPropertyAssertion') &&
        a.source === iri
      )
        return ['negative', [a.property, a.target ?? a.value]];
      return null;
    case 'Datatype':
      if (t === 'DatatypeDefinition' && a.datatype === iri) return ['definitions', [a.range]];
      return null;
  }
}

/** The characteristic axioms of a property that the axioms hold. */
export function characteristicsOf(iri: string, axioms: AxiomItem[]): Map<string, AxiomItem> {
  const out = new Map<string, AxiomItem>();
  for (const it of axioms) {
    const a = it.axiom;
    if (
      a.property === iri &&
      (OBJECT_CHARACTERISTICS.some(([t]) => t === a.type) || a.type === 'FunctionalDataProperty')
    )
      out.set(a.type, it);
  }
  return out;
}

/** The frame of an entity of `kind` from the axioms about it. */
export function buildFrame(
  kind: EntityKind,
  iri: string,
  axioms: AxiomItem[],
): {
  annotations: AxiomItem[];
  sections: FrameSection[];
  declarations: AxiomItem[];
  other: AxiomItem[];
} {
  const sections = new Map<string, FrameSection>(
    SECTIONS[kind].map(([id, title]) => [id, { id, title, rows: [] }]),
  );
  const annotations: AxiomItem[] = [];
  const declarations: AxiomItem[] = [];
  const other: AxiomItem[] = [];
  const characteristic = new Set([
    ...OBJECT_CHARACTERISTICS.map(([t]) => t),
    'FunctionalDataProperty',
  ]);
  for (const it of axioms) {
    const a = it.axiom;
    if (a.type === 'AnnotationAssertion' && a.subject === iri) {
      annotations.push(it);
      continue;
    }
    if (a.type === 'Declaration') {
      if (a.iri === iri) declarations.push(it);
      else other.push(it);
      continue;
    }
    if (characteristic.has(a.type) && a.property === iri) continue;
    const hit = sectionOf(kind, iri, a);
    if (hit && sections.has(hit[0])) sections.get(hit[0])!.rows.push({ item: it, shown: hit[1] });
    else other.push(it);
  }
  return { annotations, sections: [...sections.values()], declarations, other };
}

// --- display -------------------------------------------------------------------------

const FACETS: Record<string, string> = {
  minInclusive: '>=',
  maxInclusive: '<=',
  minExclusive: '>',
  maxExclusive: '<',
};

/**
 * A literal as Manchester syntax writes it: an integer, a decimal with digits on both
 * sides of its point and a boolean bare, as the server's reader reads them back, and
 * other literals quoted with their language tag or datatype.
 */
export function litText(l: Lit, prefixes: PrefixMap): string {
  const v = JSON.stringify(l['@value']);
  if (l['@language']) return `${v}@${l['@language']}`;
  const t = l['@type'];
  const raw = l['@value'];
  if (!t || t === XSD + 'string') return v;
  if (t === XSD + 'integer' && /^[+-]?\d+$/.test(raw)) return raw;
  if (t === XSD + 'decimal' && /^[+-]?\d+\.\d+$/.test(raw)) return raw;
  if (t === XSD + 'boolean' && (raw === 'true' || raw === 'false')) return raw;
  return `${v}^^${shorten(t, prefixes) ?? `<${t}>`}`;
}

/** The words of Manchester syntax that a name written bare would be read as. */
export const MANCHESTER_KEYWORDS = [
  'and',
  'or',
  'not',
  'that',
  'some',
  'only',
  'value',
  'Self',
  'min',
  'max',
  'exactly',
  'inverse',
  'true',
  'false',
  'o',
  'SubClassOf',
  'EquivalentTo',
  'DisjointWith',
  'SubPropertyOf',
  'InverseOf',
  'Domain',
  'Range',
  'Type',
  'Types',
  'SameAs',
  'DifferentFrom',
  'SubPropertyChain',
  'Functional',
  'InverseFunctional',
  'Reflexive',
  'Irreflexive',
  'Symmetric',
  'Asymmetric',
  'Transitive',
];

/**
 * A name as Manchester syntax writes it: bare when the server's reader reads it back as
 * one word, and otherwise in single quotes with `'` and `\` escaped. A name with a
 * space, a bracket, a quote, `^` or `@`, one written as a number, a keyword, and one
 * that starts as an anonymous individual are quoted. A colon is left bare, since a
 * prefixed name holds one.
 */
export function manchesterName(s: string): string {
  // a full IRI in angle brackets is a name as it is
  if (/^<[^\s<>]*>$/.test(s)) return s;
  const bare =
    s !== '' &&
    !/[\s()[\]{},'"<>^@]/.test(s) &&
    !/^([0-9]|[+\-.][0-9.])/.test(s) &&
    !MANCHESTER_KEYWORDS.includes(s) &&
    !s.startsWith('_:');
  return bare ? s : `'${s.replace(/['\\]/g, (c) => `\\${c}`)}'`;
}

/**
 * One piece of a Manchester rendering: a keyword, a name with the IRI it stands for, a
 * literal, punctuation or a space. Joining the texts gives [`manchester`]'s string, and
 * the browser renders each name as a link.
 */
export type ManchesterToken = {
  text: string;
  role: 'kw' | 'name' | 'lit' | 'punct' | 'space' | 'bnode';
  iri?: string;
};

/**
 * A class expression, property expression, data range, individual or literal of the
 * JSON form as Manchester tokens (W3C OWL 2 Manchester Syntax, second edition), with
 * entity names from `name`. Nested expressions are parenthesized.
 */
export function manchesterTokens(
  e: unknown,
  rawName: (iri: string) => string,
  prefixes: PrefixMap,
  nested = false,
): ManchesterToken[] {
  const out: ManchesterToken[] = [];
  const kw = (text: string) => out.push({ text, role: 'kw' });
  const sp = () => out.push({ text: ' ', role: 'space' });
  const punct = (text: string) => out.push({ text, role: 'punct' });
  const name = (iri: string) => out.push({ text: manchesterName(rawName(iri)), role: 'name', iri });
  const m = (v: unknown) => out.push(...manchesterTokens(v, rawName, prefixes, true));
  const wrap = (body: () => void) => {
    if (nested) punct('(');
    body();
    if (nested) punct(')');
  };
  const list = (k: string, sep: string, x: Record<string, unknown>) =>
    ((x[k] as unknown[]) ?? []).forEach((v, i) => {
      if (i > 0) {
        if (sep === ',') {
          punct(',');
          sp();
        } else {
          sp();
          kw(sep);
          sp();
        }
      }
      m(v);
    });
  if (typeof e === 'string') {
    if (e.startsWith('_:')) out.push({ text: e, role: 'bnode' });
    else name(e);
    return out;
  }
  if (isLit(e)) {
    out.push({ text: litText(e, prefixes), role: 'lit' });
    return out;
  }
  if (!e || typeof e !== 'object') {
    out.push({ text: String(e), role: 'lit' });
    return out;
  }
  const x = e as Record<string, unknown>;
  if (typeof x.inverseOf === 'string') {
    kw('inverse');
    sp();
    name(x.inverseOf);
    return out;
  }
  const restriction = (prop: unknown, word: string, filler: unknown) =>
    wrap(() => {
      m(prop);
      sp();
      kw(word);
      if (filler !== undefined) {
        sp();
        m(filler);
      }
    });
  const card = (word: string) => {
    const filler = x.class ?? x.range;
    wrap(() => {
      m(x.property);
      sp();
      kw(word);
      sp();
      out.push({ text: String(x.cardinality), role: 'lit' });
      if (filler) {
        sp();
        m(filler);
      }
    });
  };
  switch (x.type) {
    case 'Class':
    case 'Datatype':
      name(String(x.iri));
      return out;
    case 'ObjectIntersectionOf':
      wrap(() => list('classes', 'and', x));
      return out;
    case 'ObjectUnionOf':
      wrap(() => list('classes', 'or', x));
      return out;
    case 'ObjectComplementOf':
      kw('not');
      sp();
      m(x.class);
      return out;
    case 'ObjectOneOf':
      punct('{');
      list('individuals', ',', x);
      punct('}');
      return out;
    case 'ObjectSomeValuesFrom':
      restriction(x.property, 'some', x.class);
      return out;
    case 'ObjectAllValuesFrom':
      restriction(x.property, 'only', x.class);
      return out;
    case 'ObjectHasValue':
      restriction(x.property, 'value', x.individual);
      return out;
    case 'ObjectHasSelf':
      restriction(x.property, 'Self', undefined);
      return out;
    case 'ObjectMinCardinality':
    case 'DataMinCardinality':
      card('min');
      return out;
    case 'ObjectMaxCardinality':
    case 'DataMaxCardinality':
      card('max');
      return out;
    case 'ObjectExactCardinality':
    case 'DataExactCardinality':
      card('exactly');
      return out;
    case 'DataSomeValuesFrom':
      restriction(x.property ?? (x.properties as unknown[])?.[0], 'some', x.range);
      return out;
    case 'DataAllValuesFrom':
      restriction(x.property ?? (x.properties as unknown[])?.[0], 'only', x.range);
      return out;
    case 'DataHasValue':
      restriction(x.property, 'value', x.value);
      return out;
    case 'DataIntersectionOf':
      wrap(() => list('ranges', 'and', x));
      return out;
    case 'DataUnionOf':
      wrap(() => list('ranges', 'or', x));
      return out;
    case 'DataComplementOf':
      kw('not');
      sp();
      m(x.range);
      return out;
    case 'DataOneOf':
      punct('{');
      list('values', ',', x);
      punct('}');
      return out;
    case 'DatatypeRestriction': {
      m(x.datatype);
      punct('[');
      ((x.restrictions as { facet: string; value: unknown }[]) ?? []).forEach((f, i) => {
        if (i > 0) {
          punct(',');
          sp();
        }
        const local = f.facet.slice(f.facet.lastIndexOf('#') + 1);
        kw(FACETS[local] ?? local);
        sp();
        m(f.value);
      });
      punct(']');
      return out;
    }
    case 'Chain':
      ((x.chain as unknown[]) ?? []).forEach((v, i) => {
        if (i > 0) {
          sp();
          kw('o');
          sp();
        }
        m(v);
      });
      return out;
  }
  out.push({ text: JSON.stringify(e), role: 'lit' });
  return out;
}

/**
 * A class expression, property expression, data range, individual or literal of the
 * JSON form in Manchester syntax (W3C OWL 2 Manchester Syntax, second edition), with
 * entity names from `name`. Nested expressions are parenthesized.
 */
export function manchester(
  e: unknown,
  rawName: (iri: string) => string,
  prefixes: PrefixMap,
  nested = false,
): string {
  return manchesterTokens(e, rawName, prefixes, nested)
    .map((t) => t.text)
    .join('');
}

// --- operations and their inverses ---------------------------------------------------

/**
 * An edit the editor makes, which the preview dialog runs as a dry run and then commits.
 * Undo and redo are reverts of the commit they take back.
 */
export type Op =
  | { kind: 'axioms'; add: AxiomInput[]; remove: AxiomInput[]; declare?: boolean }
  | { kind: 'removeEntity'; iri: string }
  | { kind: 'update'; sparql: string }
  | { kind: 'revert'; commit: number };

/** An entry of the session's undo stack: a commit and what it did. */
export type Done = { label: string; commit: number };

const isDone = (d: unknown): d is Done =>
  !!d &&
  typeof d === 'object' &&
  typeof (d as Done).label === 'string' &&
  typeof (d as Done).commit === 'number';

/**
 * The session's undo and redo stacks, kept per dataset and branch in session storage, so
 * that a reload keeps them for the tab.
 */
export class UndoStack {
  done: Done[] = [];
  undone: Done[] = [];
  constructor(readonly key: string | null = null) {
    this.load();
  }

  private load() {
    if (!this.key) return;
    try {
      const raw = sessionStorage.getItem(this.key);
      if (!raw) return;
      const j = JSON.parse(raw) as { done?: Done[]; undone?: Done[] };
      // entries of an older form, without a commit, cannot be reverted
      this.done = Array.isArray(j.done) ? j.done.filter(isDone) : [];
      this.undone = Array.isArray(j.undone) ? j.undone.filter(isDone) : [];
    } catch {
      /* storage unavailable: a stack for this page only */
    }
  }

  private save() {
    if (!this.key) return;
    try {
      sessionStorage.setItem(this.key, JSON.stringify({ done: this.done, undone: this.undone }));
    } catch {
      /* storage unavailable */
    }
  }

  /** A new edit: it can be undone, and nothing can be redone any more. */
  push(d: Done) {
    this.done.push(d);
    this.undone = [];
    this.save();
  }

  /** After the top entry was reverted by commit `revert`, which a redo reverts in turn. */
  undid(revert: number | null) {
    const top = this.done.pop();
    if (top && revert != null) this.undone.push({ label: top.label, commit: revert });
    this.save();
  }

  /** After the top of the redo stack was reverted by commit `revert`. */
  redid(revert: number | null) {
    const top = this.undone.pop();
    if (top && revert != null) this.done.push({ label: top.label, commit: revert });
    this.save();
  }

  clear() {
    this.done = [];
    this.undone = [];
    this.save();
  }
}

// --- the IRI policy for new entities -------------------------------------------------

export type IriMode = 'label' | 'uuid' | 'sequence';
export type IriPolicy = { namespace: string; mode: IriMode; prefix: string; digits: number };

/** The namespace of new entities: the ontology IRI with `#`, or as is when it ends in `#` or `/`. */
export function defaultNamespace(ontologyIri: string | null): string {
  if (!ontologyIri) return 'http://example.org/ontology#';
  return /[#/]$/.test(ontologyIri) ? ontologyIri : `${ontologyIri}#`;
}

export function defaultPolicy(ontologyIri: string | null): IriPolicy {
  return { namespace: defaultNamespace(ontologyIri), mode: 'label', prefix: 'ID_', digits: 7 };
}

const policyKey = (ds: string) => `sparkles.onto.iri.${ds}`;

export function loadPolicy(ds: string, ontologyIri: string | null): IriPolicy {
  const d = defaultPolicy(ontologyIri);
  try {
    const raw = localStorage.getItem(policyKey(ds));
    if (!raw) return d;
    const j = JSON.parse(raw) as Partial<IriPolicy>;
    return {
      namespace: typeof j.namespace === 'string' && j.namespace ? j.namespace : d.namespace,
      mode: j.mode === 'uuid' || j.mode === 'sequence' ? j.mode : 'label',
      prefix: typeof j.prefix === 'string' ? j.prefix : d.prefix,
      digits: Number.isInteger(j.digits) && j.digits! > 0 && j.digits! <= 12 ? j.digits! : d.digits,
    };
  } catch {
    return d;
  }
}

export function savePolicy(ds: string, p: IriPolicy) {
  try {
    localStorage.setItem(policyKey(ds), JSON.stringify(p));
  } catch {
    /* storage unavailable */
  }
}

/**
 * A local name from a label: its words joined in CamelCase for classes, datatypes and
 * individuals, and in camelCase for properties, keeping letters and digits of any script.
 */
export function localFromLabel(label: string, kind: EntityKind): string {
  const words = label
    .normalize('NFC')
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean);
  if (!words.length) return '';
  const cap = (w: string) => w.charAt(0).toUpperCase() + w.slice(1);
  const lowerFirst = kind.endsWith('Property');
  let s = words
    .map((w, i) => (i === 0 && lowerFirst ? w.charAt(0).toLowerCase() + w.slice(1) : cap(w)))
    .join('');
  // a local name that starts with a digit is not a valid prefixed name
  if (/^\p{N}/u.test(s)) s = `_${s}`;
  return s;
}

/**
 * A new IRI under `policy` that `taken` does not hold: the label's local name (with a
 * number appended on a collision), a UUID, or the next number of the sequence after the
 * highest one in use.
 */
export function mintIri(
  policy: IriPolicy,
  label: string,
  kind: EntityKind,
  taken: ReadonlySet<string>,
  uuid: () => string = () => crypto.randomUUID(),
): string {
  const ns = policy.namespace;
  if (policy.mode === 'uuid') {
    for (;;) {
      const iri = ns + uuid();
      if (!taken.has(iri)) return iri;
    }
  }
  if (policy.mode === 'sequence') {
    const re = new RegExp(`^${escapeRe(ns + policy.prefix)}(\\d+)$`);
    let max = 0;
    for (const t of taken) {
      const m = re.exec(t);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return ns + policy.prefix + String(max + 1).padStart(policy.digits, '0');
  }
  const base = localFromLabel(label, kind) || 'Entity';
  if (!taken.has(ns + base)) return ns + base;
  for (let i = 2; ; i++) if (!taken.has(`${ns}${base}${i}`)) return `${ns}${base}${i}`;
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** Whether a string is an absolute IRI the editor can write. */
export function validIri(s: string): boolean {
  return /^[A-Za-z][A-Za-z0-9+.-]*:[^\s<>"{}|\\^`]*$/.test(s) && !s.startsWith('_:');
}

/** A BCP 47 language tag as RDF takes it (empty for no tag). */
export function validLang(s: string): boolean {
  return s === '' || /^[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*$/.test(s);
}

// --- SPARQL of renames and header edits ----------------------------------------------

const inGraph = (graph: string | null, body: string) =>
  graph ? `GRAPH ${sparqlIri(graph)} { ${body} }` : body;

/**
 * The update that renames `from` to `to` in one graph: every triple with `from` as its
 * subject, predicate or object is replaced by one with `to`. It renames nothing when the
 * graph already uses `to`, so a use that lands after the editor checked stops it.
 * `sameTerm` compares terms of any kind without errors, so no triple is deleted without
 * its replacement.
 */
export function renameUpdate(from: string, to: string, graph: string | null): string {
  const f = sparqlIri(from);
  const t = sparqlIri(to);
  const sub = (v: string) => `BIND(IF(sameTerm(?${v}, ${f}), ${t}, ?${v}) AS ?${v}2)`;
  const where = [
    `{ ${f} ?p ?o BIND(${f} AS ?s) }`,
    `UNION { ?s ${f} ?o BIND(${f} AS ?p) }`,
    `UNION { ?s ?p ${f} BIND(${f} AS ?o) }`,
  ].join(' ');
  const used = `{ ${t} ?a ?b } UNION { ?a ${t} ?b } UNION { ?a ?b ${t} }`;
  return [
    `DELETE { ${inGraph(graph, '?s ?p ?o')} }`,
    `INSERT { ${inGraph(graph, '?s2 ?p2 ?o2')} }`,
    `WHERE {`,
    `  ${inGraph(graph, where)}`,
    `  FILTER NOT EXISTS { ${inGraph(graph, used)} }`,
    `  ${sub('s')} ${sub('p')} ${sub('o')}`,
    `}`,
  ].join('\n');
}

/** A term of a header triple: an IRI, or a literal. */
export type HeaderValue = { iri: string } | Lit;

export function termSparql(v: HeaderValue): string {
  if ('iri' in v) return sparqlIri(v.iri);
  const s = sparqlString(v['@value']);
  if (v['@language']) return `${s}@${v['@language']}`;
  if (v['@type'] && v['@type'] !== XSD + 'string') return `${s}^^${sparqlIri(v['@type'])}`;
  return s;
}

export type HeaderTriple = { s: string; p: string; o: HeaderValue };

const tripleSparql = (t: HeaderTriple) =>
  `${sparqlIri(t.s)} ${sparqlIri(t.p)} ${termSparql(t.o)} .`;

/** The update that deletes and inserts ground triples in one graph, and its inverse. */
export function dataUpdate(
  remove: HeaderTriple[],
  add: HeaderTriple[],
  graph: string | null,
): { sparql: string; inverse: string } {
  const part = (op: 'DELETE' | 'INSERT', ts: HeaderTriple[]) =>
    ts.length ? `${op} DATA { ${inGraph(graph, ts.map(tripleSparql).join(' '))} }` : '';
  const go = (r: HeaderTriple[], a: HeaderTriple[]) =>
    [part('DELETE', r), part('INSERT', a)].filter(Boolean).join(' ;\n');
  return { sparql: go(remove, add), inverse: go(add, remove) };
}

/** The query of the header node's triples in one graph. */
export function headerQuery(ontologyIri: string, graph: string | null): string {
  return `SELECT ?p ?o WHERE { ${inGraph(graph, `${sparqlIri(ontologyIri)} ?p ?o`)} }`;
}

/** The query of the graphs that hold an ontology header. */
export const ONTOLOGY_GRAPHS = `SELECT DISTINCT ?g WHERE { GRAPH ?g { ?o a <${OWL}Ontology> } } ORDER BY ?g LIMIT 200`;

/** The ASK of whether any triple of the graph uses an IRI. */
export function usedQuery(iri: string, graph: string | null): string {
  const t = sparqlIri(iri);
  return `ASK { ${inGraph(graph, `{ ${t} ?a ?b } UNION { ?a ${t} ?b } UNION { ?a ?b ${t} }`)} }`;
}

// --- building axioms -----------------------------------------------------------------

/** A declaration of an entity. */
export const declaration = (kind: EntityKind, iri: string): AxiomJson => ({
  type: 'Declaration',
  kind,
  iri,
});

/** An annotation assertion. */
export const annotationAssertion = (
  property: string,
  subject: string,
  value: AnnValue,
): AxiomJson => ({
  type: 'AnnotationAssertion',
  property,
  subject,
  value,
});

/** The axiom that puts a new entity of `kind` under `parent`. */
export function underParent(kind: EntityKind, iri: string, parent: string): AxiomJson | null {
  switch (kind) {
    case 'Class':
      return { type: 'SubClassOf', subClass: iri, superClass: parent };
    case 'ObjectProperty':
      return { type: 'SubObjectPropertyOf', subProperty: iri, superProperty: parent };
    case 'DataProperty':
      return { type: 'SubDataPropertyOf', subProperty: iri, superProperty: parent };
    case 'AnnotationProperty':
      return { type: 'SubAnnotationPropertyOf', subProperty: iri, superProperty: parent };
    case 'NamedIndividual':
      return { type: 'ClassAssertion', class: parent, individual: iri };
    default:
      return null;
  }
}

/** The kind of the entities a frame section's rows name, for its entity picker. */
export function sectionTarget(kind: EntityKind, section: string): EntityKind | null {
  if (kind === 'Class')
    return section === 'instances'
      ? 'NamedIndividual'
      : section === 'keys'
        ? 'ObjectProperty'
        : 'Class';
  if (kind === 'ObjectProperty')
    return section === 'domains' || section === 'ranges'
      ? 'Class'
      : section === 'chains'
        ? null
        : 'ObjectProperty';
  if (kind === 'DataProperty')
    return section === 'domains' ? 'Class' : section === 'ranges' ? 'Datatype' : 'DataProperty';
  if (kind === 'AnnotationProperty')
    return section === 'domains' || section === 'ranges' ? null : 'AnnotationProperty';
  if (kind === 'NamedIndividual')
    return section === 'types'
      ? 'Class'
      : section === 'same' || section === 'different'
        ? 'NamedIndividual'
        : null;
  return null;
}

/** The sections whose rows an add form builds from one named entity or expression. */
export function sectionAxiom(
  kind: EntityKind,
  section: string,
  iri: string,
  value: AxiomInput,
): AxiomInput | null {
  // a functional-style class expression is sent as a functional-style axiom
  const ofn = typeof value === 'string' && !validIri(value) ? value : null;
  const v = ofn ?? value;
  const wrap = (t: string, json: AxiomJson, order: 'self-first' | 'value-first' = 'self-first') =>
    ofn ? (order === 'self-first' ? `${t}(<${iri}> ${ofn})` : `${t}(${ofn} <${iri}>)`) : json;
  switch (`${kind}.${section}`) {
    case 'Class.sub':
      return wrap('SubClassOf', { type: 'SubClassOf', subClass: iri, superClass: v });
    case 'Class.superOf':
      return wrap(
        'SubClassOf',
        { type: 'SubClassOf', subClass: v, superClass: iri },
        'value-first',
      );
    case 'Class.equivalent':
      return wrap('EquivalentClasses', { type: 'EquivalentClasses', classes: [iri, v] });
    case 'Class.disjoint':
      return wrap('DisjointClasses', { type: 'DisjointClasses', classes: [iri, v] });
    case 'Class.instances':
      return ofn ? null : { type: 'ClassAssertion', class: iri, individual: v };
    case 'ObjectProperty.domains':
      return wrap('ObjectPropertyDomain', {
        type: 'ObjectPropertyDomain',
        property: iri,
        domain: v,
      });
    case 'ObjectProperty.ranges':
      return wrap('ObjectPropertyRange', { type: 'ObjectPropertyRange', property: iri, range: v });
    case 'ObjectProperty.superProperties':
      return ofn ? null : { type: 'SubObjectPropertyOf', subProperty: iri, superProperty: v };
    case 'ObjectProperty.subProperties':
      return ofn ? null : { type: 'SubObjectPropertyOf', subProperty: v, superProperty: iri };
    case 'ObjectProperty.equivalent':
      return ofn ? null : { type: 'EquivalentObjectProperties', properties: [iri, v] };
    case 'ObjectProperty.disjoint':
      return ofn ? null : { type: 'DisjointObjectProperties', properties: [iri, v] };
    case 'ObjectProperty.inverses':
      return ofn ? null : { type: 'InverseObjectProperties', first: iri, second: v };
    case 'DataProperty.domains':
      return wrap('DataPropertyDomain', { type: 'DataPropertyDomain', property: iri, domain: v });
    case 'DataProperty.ranges':
      return wrap('DataPropertyRange', { type: 'DataPropertyRange', property: iri, range: v });
    case 'DataProperty.superProperties':
      return ofn ? null : { type: 'SubDataPropertyOf', subProperty: iri, superProperty: v };
    case 'DataProperty.subProperties':
      return ofn ? null : { type: 'SubDataPropertyOf', subProperty: v, superProperty: iri };
    case 'DataProperty.equivalent':
      return ofn ? null : { type: 'EquivalentDataProperties', properties: [iri, v] };
    case 'DataProperty.disjoint':
      return ofn ? null : { type: 'DisjointDataProperties', properties: [iri, v] };
    case 'AnnotationProperty.superProperties':
      return ofn ? null : { type: 'SubAnnotationPropertyOf', subProperty: iri, superProperty: v };
    case 'AnnotationProperty.subProperties':
      return ofn ? null : { type: 'SubAnnotationPropertyOf', subProperty: v, superProperty: iri };
    case 'AnnotationProperty.domains':
      return ofn ? null : { type: 'AnnotationPropertyDomain', property: iri, domain: v };
    case 'AnnotationProperty.ranges':
      return ofn ? null : { type: 'AnnotationPropertyRange', property: iri, range: v };
    case 'NamedIndividual.types':
      return ofn
        ? `ClassAssertion(${ofn} <${iri}>)`
        : { type: 'ClassAssertion', class: v, individual: iri };
    case 'NamedIndividual.same':
      return ofn ? null : { type: 'SameIndividual', individuals: [iri, v] };
    case 'NamedIndividual.different':
      return ofn ? null : { type: 'DifferentIndividuals', individuals: [iri, v] };
  }
  return null;
}

/** An axiom with other annotations: the same structure, the given annotations. */
export function withAnnotations(a: AxiomJson, annotations: Annotation[]): AxiomJson {
  const { annotations: _old, ...rest } = a;
  void _old;
  return annotations.length ? { ...rest, annotations } : { ...rest };
}

/** The property assertion axiom of an individual. */
export function propertyAssertion(
  kind: 'ObjectProperty' | 'DataProperty',
  property: string,
  source: string,
  value: string | Lit,
): AxiomJson {
  return kind === 'ObjectProperty'
    ? { type: 'ObjectPropertyAssertion', property, source, target: value }
    : { type: 'DataPropertyAssertion', property, source, value };
}

/** The common annotation properties an annotation editor offers first. */
export const COMMON_ANNOTATION_PROPERTIES = [
  RDFS_LABEL,
  RDFS_COMMENT,
  SKOS_PREF_LABEL,
  SKOS + 'altLabel',
  SKOS + 'definition',
  RDFS + 'seeAlso',
  RDFS + 'isDefinedBy',
  OWL_DEPRECATED,
  OWL + 'versionInfo',
];

/** The datatypes a literal editor offers. */
export const LITERAL_TYPES = [
  XSD + 'string',
  XSD + 'boolean',
  XSD + 'integer',
  XSD + 'decimal',
  XSD + 'double',
  XSD + 'date',
  XSD + 'dateTime',
  XSD + 'anyURI',
];

/** The literal of a value typed into the editor. */
export function literalOf(value: string, lang: string, datatype: string): Lit {
  if (lang) return { '@value': value, '@language': lang };
  if (datatype && datatype !== XSD + 'string') return { '@value': value, '@type': datatype };
  return { '@value': value };
}

/** The session branch's name for a user on a day: `{prefix}.{user}.{yyyymmdd}`. */
export function sessionBranchName(
  user: string | null | undefined,
  now = new Date(),
  prefix = 'onto',
): string {
  const u =
    (user ?? 'anonymous').replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^[-._]+/, '') || 'user';
  const d = now.toISOString().slice(0, 10).replace(/-/g, '');
  return `${prefix}.${u}.${d}`.slice(0, 64);
}

// --- running operations ----------------------------------------------------------------

export type OpAnswer = EditAnswer | UpdateAnswer;

/** Run an operation, or its dry run with `o.dryRun`. */
export function runOp(
  target: string,
  sel: Selection,
  op: Op,
  o: WriteOpts = {},
): Promise<OpAnswer | WritePreview> {
  switch (op.kind) {
    case 'axioms':
      return editAxioms(target, sel, op, o);
    case 'removeEntity':
      return removeEntity(target, sel, op.iri, o);
    case 'update':
      return runUpdate(target, op.sparql, o);
    case 'revert':
      return revertCommit(target, op.commit, o);
  }
}

/** The commit an answer made, or null when it made none. */
export function committedSeq(a: OpAnswer): number | null {
  return a.receipt?.committed ? a.receipt.commit.seq : null;
}

/** Whether an answer changed anything. */
export function changedAnything(a: OpAnswer): boolean {
  if (a.receipt) return a.receipt.committed;
  if ('addedAxioms' in a) return a.addedAxioms.length + a.removedAxioms.length > 0;
  return a.inserted + a.deleted > 0;
}

/**
 * An axiom as the preview lists it: Manchester-like text with the names `name` gives, and
 * the number of its annotations. A string in functional-style syntax is shown as it is.
 */
export function describeAxiom(
  a: AxiomInput,
  name: (iri: string) => string,
  prefixes: PrefixMap,
): string {
  if (typeof a === 'string') return a;
  const sh = (x: unknown) => manchester(x, name, prefixes);
  const anns = a.annotations?.length
    ? ` [${a.annotations.length} annotation${a.annotations.length > 1 ? 's' : ''}]`
    : '';
  switch (a.type) {
    case 'Declaration':
      return `Declaration: ${KIND_SINGULAR[a.kind as EntityKind] ?? a.kind} ${sh(a.iri)}`;
    case 'SubClassOf':
      return `${sh(a.subClass)} SubClassOf ${sh(a.superClass)}${anns}`;
    case 'AnnotationAssertion':
      return `${sh(a.subject)} ${sh(a.property)} ${sh(a.value)}${anns}`;
    case 'ClassAssertion':
      return `${sh(a.individual)} Type ${sh(a.class)}${anns}`;
    default: {
      const parts = Object.entries(a)
        .filter(([k]) => k !== 'type' && k !== 'annotations')
        .map(([, v]) => (Array.isArray(v) ? v.map(sh).join(', ') : sh(v)));
      return `${a.type}(${parts.join(' ')})${anns}`;
    }
  }
}

/** The axioms an operation adds and removes, for the preview's list. */
export function opAxioms(op: Op): { add: AxiomInput[]; remove: AxiomInput[] } {
  return op.kind === 'axioms' ? { add: op.add, remove: op.remove } : { add: [], remove: [] };
}

/** The sentence of a dry run's outcome. */
export const OUTCOME: Record<string, string> = {
  commit: 'The edit would make one commit.',
  'no-change': 'The edit would change nothing.',
  rejected: 'The write-time guard would reject the edit.',
  'storage-refused': 'The storage limit would refuse the edit.',
  'precondition-failed': 'Another commit landed since the editor read the ontology.',
};

/** An IRI as an editor field holds it without loss: a prefixed name, or `<iri>`. */
export function editableIri(iri: string, prefixes: PrefixMap): string {
  return shorten(iri, prefixes) ?? `<${iri}>`;
}

/**
 * `list` with the annotation equal to `original` replaced by `row`, keeping its own
 * annotations, or null when `list` no longer holds `original`. An editor finds the row it
 * edits by its value, since a refresh can insert or remove rows before it.
 */
export function replaceAnnotation(
  list: Annotation[],
  original: Annotation,
  row: Annotation,
): Annotation[] | null {
  const key = annotationKey(original);
  const i = list.findIndex((a) => annotationKey(a) === key);
  if (i < 0) return null;
  const next = [...list];
  next[i] = { ...row, ...(list[i].annotations ? { annotations: list[i].annotations } : {}) };
  return next;
}

/** A structural key of an annotation, its own annotations included. */
export function annotationKey(a: Annotation): string {
  return axiomKey({ type: 'Annotation', ...a } as unknown as AxiomJson);
}

/** A key of an axiom that ignores the order of members and of set operands. */
export function axiomKey(a: AxiomInput): string {
  if (typeof a === 'string') return a;
  const sets = new Set(['classes', 'properties', 'individuals', 'annotations', 'ranges', 'values']);
  const canon = (v: unknown, member = ''): unknown => {
    if (Array.isArray(v)) {
      const xs = v.map((x) => canon(x));
      return sets.has(member) ? xs.map((x) => JSON.stringify(x)).sort() : xs;
    }
    if (v && typeof v === 'object')
      return Object.fromEntries(
        Object.keys(v as object)
          .sort()
          .map((k) => [k, canon((v as Record<string, unknown>)[k], k)]),
      );
    return v;
  };
  return JSON.stringify(canon(a));
}

/** The edit that turns the axioms `before` into `after`: what to remove and add. */
export function diffAxioms(
  before: AxiomJson[],
  after: AxiomJson[],
): { add: AxiomJson[]; remove: AxiomJson[] } {
  const b = new Map(before.map((a) => [axiomKey(a), a]));
  const n = new Map(after.map((a) => [axiomKey(a), a]));
  return {
    remove: [...b].filter(([k]) => !n.has(k)).map(([, a]) => a),
    add: [...n].filter(([k]) => !b.has(k)).map(([, a]) => a),
  };
}

/**
 * What a typed value names: an entity among `options` by its shown name, an IRI in angle
 * brackets, a prefixed name, an absolute IRI, or else a functional-style expression when
 * it has a parenthesis. Null when it is none of these.
 */
/**
 * The IRI that `text` names: an option's name, a full IRI in angle brackets, a prefixed
 * name or an absolute IRI. Null for anything else, such as an expression.
 */
export function resolveName(
  text: string,
  options: { iri: string; name: string }[],
  prefixes: PrefixMap,
): string | null {
  const s = text.trim();
  // an option's name may hold spaces, and anything else with a space or a bracket is an
  // expression for the server's reader, such as `ex:p some ex:A`
  const named = options.find((o) => o.name === s);
  if (named) return named.iri;
  if (/[\s()[\]{},]/.test(s)) return null;
  const r = resolveInput(s, options, prefixes);
  return typeof r === 'string' ? r : null;
}

/** Whether text is in functional-style syntax: a constructor such as `ObjectSomeValuesFrom(`. */
export const isFunctional = (s: string) => /^\s*[A-Z][A-Za-z]*\s*\(/.test(s);

export function resolveInput(
  text: string,
  options: { iri: string; name: string }[],
  prefixes: PrefixMap,
): string | { ofn: string } | null {
  const s = text.trim();
  if (!s) return null;
  const hit =
    options.find((o) => o.name === s) ??
    options.find((o) => o.name.toLowerCase() === s.toLowerCase());
  if (hit) return hit.iri;
  if (/^<[^<>\s]+>$/.test(s)) return s.slice(1, -1);
  if (s.includes('(')) return { ofn: s };
  const c = s.indexOf(':');
  if (c >= 0 && prefixes[s.slice(0, c)] !== undefined && !s.slice(c + 1).startsWith('//'))
    return prefixes[s.slice(0, c)] + s.slice(c + 1);
  return validIri(s) && s.includes(':/') ? s : null;
}
