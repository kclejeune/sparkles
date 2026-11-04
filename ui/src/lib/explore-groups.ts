// Group nodes of the Explore graph (ROADMAP item 725). Expanding a resource whose fan-out is
// large does not draw hundreds of neighbours. It asks for the number of edges of each
// predicate in each direction, draws the small predicates as before, and draws one "N via
// predicate" group node for each large one. The group opens a page at a time, filters its
// members by a text, splits them by class, and draws the members the person chooses. The
// queries and the paging arithmetic live here so that the page and the tests share them.

import * as api from './api';
import type { Term } from './api';
import { sparqlIri, sparqlString } from './rdf';

/** A predicate with more edges than this, in one direction, becomes a group node. */
export const GROUP_MIN = 25;
/** The edges an expansion draws inline at most, over every small predicate. */
export const INLINE_BUDGET = 100;
/** The members a group opens at a time. */
export const PAGE_SIZE = 100;
/** The classes a group's split lists at most. */
export const CLASS_LIMIT = 30;

const P = `PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX schema: <http://schema.org/>
`;
const LABEL_PATH = 'rdfs:label|skos:prefLabel|foaf:name|dcterms:title|schema:name';
const opts = { send: 5000 };

/** The number of edges of one predicate at a resource, in one direction. */
export type Fan = { predicate: string; incoming: boolean; count: number };

const num = (t: Term | undefined) => (t && t.type !== 'triple' ? Number(t.value) || 0 : 0);
const val = (t: Term | undefined) => (t && t.type !== 'triple' ? t.value : undefined);

/** The edge counts of a resource by predicate, outgoing and incoming. */
export async function fanout(
  ds: string,
  iri: string,
  at?: string,
  signal?: AbortSignal,
): Promise<Fan[]> {
  const node = sparqlIri(iri);
  const o = { ...opts, ...(at ? { at } : {}), signal };
  const [out, inc] = await Promise.all([
    api.select(ds, `SELECT ?p (COUNT(*) AS ?n) WHERE { ${node} ?p ?o } GROUP BY ?p`, o),
    api.select(ds, `SELECT ?p (COUNT(*) AS ?n) WHERE { ?s ?p ${node} } GROUP BY ?p`, o),
  ]);
  const fans: Fan[] = [];
  for (const r of out)
    if (val(r.p)) fans.push({ predicate: val(r.p)!, incoming: false, count: num(r.n) });
  for (const r of inc)
    if (val(r.p)) fans.push({ predicate: val(r.p)!, incoming: true, count: num(r.n) });
  return fans;
}

export type FanPlan = {
  /** The predicates drawn inline, as the expansion always drew them. */
  inline: Fan[];
  /** The predicates that become group nodes, the largest first. */
  groups: Fan[];
};

/**
 * Which predicates draw their edges and which become groups: every predicate over
 * `groupMin` edges is a group, and the small ones are drawn while their total stays within
 * `budget`, the largest of them becoming groups past it.
 */
export function planFanout(
  fans: readonly Fan[],
  groupMin = GROUP_MIN,
  budget = INLINE_BUDGET,
): FanPlan {
  const groups = fans.filter((f) => f.count > groupMin);
  const small = fans.filter((f) => f.count <= groupMin).sort((a, b) => a.count - b.count);
  const inline: Fan[] = [];
  let used = 0;
  for (const f of small) {
    if (used + f.count > budget) groups.push(f);
    else (inline.push(f), (used += f.count));
  }
  groups.sort((a, b) => b.count - a.count || a.predicate.localeCompare(b.predicate));
  return { inline, groups };
}

/** The id of the group node of a predicate at a node. */
export function groupId(owner: string, predicate: string, incoming: boolean): string {
  return `grp:${incoming ? 'in' : 'out'}|${predicate}|${owner}`;
}

export const isGroupId = (id: string) => id.startsWith('grp:');

/** What a group node stands for. */
export type GroupSpec = {
  /** The node id of the resource the group hangs from. */
  owner: string;
  ownerIri: string;
  predicate: string;
  incoming: boolean;
  /** The edges of the predicate at the owner when the group was made. */
  total: number;
};

/** The caption of a group node, such as "1,204 via rdfs:subClassOf". */
export function groupCaption(total: number, predicateLabel: string, incoming = false): string {
  return `${total.toLocaleString('en-US')} via ${predicateLabel}${incoming ? ' (incoming)' : ''}`;
}

/** What a group page lists: a text filter, a class and the page's first member. */
export type GroupQuery = { filter: string; cls: string | null; offset: number };

export const FIRST_PAGE: GroupQuery = { filter: '', cls: null, offset: 0 };

/** A query's graph pattern, over `?m` for the member, with the filter and the class. */
function pattern(
  spec: GroupSpec,
  q: Pick<GroupQuery, 'filter' | 'cls'>,
  labelled: boolean,
): string {
  const owner = sparqlIri(spec.ownerIri);
  const p = sparqlIri(spec.predicate);
  const lines = [spec.incoming ? `?m ${p} ${owner} .` : `${owner} ${p} ?m .`];
  if (q.cls) lines.push(`?m rdf:type ${sparqlIri(q.cls)} .`);
  const text = q.filter.trim().toLowerCase();
  if (labelled || text) lines.push(`OPTIONAL { ?m ${LABEL_PATH} ?l }`);
  if (text) lines.push(`FILTER(CONTAINS(LCASE(STR(COALESCE(?l, ?m))), ${sparqlString(text)}))`);
  return lines.map((l) => `  ${l}`).join('\n');
}

/** One page of the members of a group, ordered by term so that pages do not overlap. */
export function memberQuery(spec: GroupSpec, q: GroupQuery, limit = PAGE_SIZE): string {
  return `${P}SELECT ?m (SAMPLE(?l) AS ?label) WHERE {
${pattern(spec, q, true)}
}
GROUP BY ?m
ORDER BY ?m
LIMIT ${limit}${q.offset ? `\nOFFSET ${q.offset}` : ''}`;
}

/** The members a group's filter and class leave. */
export function countQuery(spec: GroupSpec, q: Pick<GroupQuery, 'filter' | 'cls'>): string {
  return `${P}SELECT (COUNT(DISTINCT ?m) AS ?n) WHERE {
${pattern(spec, q, false)}
}`;
}

/** The classes of the members the filter leaves, most frequent first. */
export function classQuery(spec: GroupSpec, filter: string): string {
  const owner = sparqlIri(spec.ownerIri);
  const p = sparqlIri(spec.predicate);
  const text = filter.trim().toLowerCase();
  return `${P}SELECT ?c (COUNT(DISTINCT ?m) AS ?n) WHERE {
  ${spec.incoming ? `?m ${p} ${owner} .` : `${owner} ${p} ?m .`}
  ?m rdf:type ?c .${
    text
      ? `\n  OPTIONAL { ?m ${LABEL_PATH} ?l }\n  FILTER(CONTAINS(LCASE(STR(COALESCE(?l, ?m))), ${sparqlString(text)}))`
      : ''
  }
}
GROUP BY ?c
ORDER BY DESC(?n)
LIMIT ${CLASS_LIMIT}`;
}

export type Member = { term: Term; label?: string };
export type GroupPage = {
  members: Member[];
  /** The members the filter and the class leave. */
  total: number;
};

/** Read one page of a group and the number of members that match. */
export async function readPage(
  ds: string,
  spec: GroupSpec,
  q: GroupQuery,
  at?: string,
  signal?: AbortSignal,
): Promise<GroupPage> {
  const o = { ...opts, ...(at ? { at } : {}), signal };
  const [rows, count] = await Promise.all([
    api.select(ds, memberQuery(spec, q), o),
    api.select(ds, countQuery(spec, q), o),
  ]);
  return {
    members: rows.filter((r) => r.m).map((r) => ({ term: r.m!, label: val(r.label) })),
    total: num(count[0]?.n),
  };
}

/** The classes of the members of a group that match a filter, with their member counts. */
export async function readClasses(
  ds: string,
  spec: GroupSpec,
  filter: string,
  at?: string,
  signal?: AbortSignal,
): Promise<{ iri: string; count: number }[]> {
  const rows = await api.select(ds, classQuery(spec, filter), {
    ...opts,
    ...(at ? { at } : {}),
    signal,
  });
  return rows.flatMap((r) => (val(r.c) ? [{ iri: val(r.c)!, count: num(r.n) }] : []));
}

/** The position of a page in a list of `total` members read `size` at a time. */
export function pageOf(
  total: number,
  offset: number,
  size = PAGE_SIZE,
): {
  from: number;
  to: number;
  page: number;
  pages: number;
  prev: number | null;
  next: number | null;
} {
  const pages = Math.max(1, Math.ceil(total / size));
  const page = Math.min(pages, Math.floor(offset / size) + 1);
  const from = total ? (page - 1) * size + 1 : 0;
  const to = Math.min(total, page * size);
  return {
    from,
    to,
    page,
    pages,
    prev: page > 1 ? (page - 2) * size : null,
    next: page < pages ? page * size : null,
  };
}
