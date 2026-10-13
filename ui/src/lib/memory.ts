// The memory browser (C18 §8.7): recall results grouped for the Memory tab, their
// filters, and the bounded SPARQL 1.2 reifier queries of the Memory page, in the
// vocabulary of C17 §3.2.

import type {
  FactStatus,
  RecallCitation,
  RecallConflict,
  RecallEntity,
  RecallResult,
  RecallSuperseded,
} from './ask-api';

/** One value of a predicate, with the citations of every fact that states it. */
export type FactValue = { o: string; citations: (number | string)[]; status?: FactStatus };

/** The facts of one predicate of an entity. */
export type PredicateGroup = { p: string; values: FactValue[] };

/** An entity's facts grouped by predicate, in order of first appearance. */
export function groupByPredicate(facts: RecallEntity['facts']): PredicateGroup[] {
  const groups = new Map<string, Map<string, FactValue>>();
  for (const f of facts) {
    let values = groups.get(f.p);
    if (!values) groups.set(f.p, (values = new Map()));
    const v = values.get(f.o);
    if (!v) {
      values.set(f.o, { o: f.o, citations: [f.citation], status: f.status });
    } else {
      if (!v.citations.includes(f.citation)) v.citations.push(f.citation);
      // a value stated in a reviewed graph is reviewed
      if (f.status === 'reviewed' || (f.status && !v.status)) v.status = f.status;
    }
  }
  return [...groups].map(([p, values]) => ({ p, values: [...values.values()] }));
}

/** A fact that differs between two commits. */
export type FactChange = {
  kind: 'added' | 'superseded' | 'retracted';
  p: string;
  o: string;
  citations: (number | string)[];
  status?: FactStatus;
};

/**
 * What changed in an entity's facts between an earlier read (`before`) and a later one
 * (`after`), as the time slider's compare mode shows it. A fact only the later read has is
 * added. A fact only the earlier read has is superseded when the later read states another
 * value of the same predicate, and retracted when it states none.
 */
export function factChanges(
  before: RecallEntity['facts'] | undefined,
  after: RecallEntity['facts'] | undefined,
): FactChange[] {
  const keyOf = (f: { p: string; o: string }) => `${f.p}\u0000${f.o}`;
  const was = new Map((before ?? []).map((f) => [keyOf(f), f]));
  const is = new Map((after ?? []).map((f) => [keyOf(f), f]));
  const predicatesNow = new Set((after ?? []).map((f) => f.p));
  const out: FactChange[] = [];
  for (const g of groupByPredicate(after ?? [])) {
    for (const v of g.values) {
      if (!was.has(keyOf({ p: g.p, o: v.o })))
        out.push({ kind: 'added', p: g.p, o: v.o, citations: v.citations, status: v.status });
    }
  }
  for (const g of groupByPredicate(before ?? [])) {
    for (const v of g.values) {
      if (is.has(keyOf({ p: g.p, o: v.o }))) continue;
      out.push({
        kind: predicatesNow.has(g.p) ? 'superseded' : 'retracted',
        p: g.p,
        o: v.o,
        citations: v.citations,
        status: v.status,
      });
    }
  }
  return out;
}

/** The filters of the Memory tab: empty sets mean everything. */
export type MemoryFilter = { graphs: Set<string>; agents: Set<string> };

/** The distinct graphs and agents (`by`) of a result's citations, sorted. */
export function filterChoices(r: Pick<RecallResult, 'citations'>): {
  graphs: string[];
  agents: string[];
} {
  const graphs = new Set<string>();
  const agents = new Set<string>();
  for (const c of r.citations) {
    graphs.add(c.graph);
    if (c.by) agents.add(c.by);
  }
  return { graphs: [...graphs].sort(), agents: [...agents].sort() };
}

const citationPasses = (c: RecallCitation | undefined, f: MemoryFilter) =>
  !!c &&
  (!f.graphs.size || f.graphs.has(c.graph)) &&
  (!f.agents.size || (!!c.by && f.agents.has(c.by)));

/** The facts of an entity whose citation passes the filter. */
export function filterFacts(
  facts: RecallEntity['facts'],
  citations: RecallCitation[],
  f: MemoryFilter,
): RecallEntity['facts'] {
  if (!f.graphs.size && !f.agents.size) return facts;
  const by = new Map(citations.map((c) => [String(c.id), c]));
  return facts.filter((x) => citationPasses(by.get(String(x.citation)), f));
}

/** Superseded or retracted facts of a subject in the filter's graphs, newest first. */
export function historyOf(
  superseded: RecallSuperseded[] | undefined,
  subject: string,
  f: MemoryFilter,
): RecallSuperseded[] {
  return (superseded ?? [])
    .filter((h) => h.s === subject && (!f.graphs.size || f.graphs.has(h.graph)))
    .sort((a, b) => (b.invalidatedAt ?? '').localeCompare(a.invalidatedAt ?? ''));
}

/**
 * What became of a superseded or retracted fact: `superseded on 2026-10-08 by [3]`, with
 * the citation of its replacement when the result cites it, else the replacing reifier.
 */
export function fateLine(h: RecallSuperseded, replacement?: number | string): string {
  const on = h.invalidatedAt ? ` on ${h.invalidatedAt.slice(0, 10)}` : '';
  if (!h.replacedBy) return `retracted${on}`;
  return `superseded${on} by ${replacement != null ? `[${replacement}]` : h.replacedBy}`;
}

/** Conflicts of a subject. */
export const conflictsOf = (conflicts: RecallConflict[], subject: string) =>
  conflicts.filter((c) => c.s === subject);

/** The principal's name of `urn:x-sparkles:principal:agent-7`, or the term as it is. */
export function principalName(by: string): string {
  const m = /^<?urn:x-sparkles:principal:([^>]+)>?$/.exec(by);
  return m ? decodeURIComponent(m[1]) : by;
}

/**
 * The graph prefixes of an agent's graphs: each graph without its last path segment,
 * so `…/agents/agent-7/s1` and `…/agents/agent-7/s2` give `…/agents/agent-7/`.
 */
export function graphPrefixes(graphs: string[]): string[] {
  const out = new Set<string>();
  for (const g of graphs) {
    const m = /^(.*[/#:])[^/#:]+$/.exec(g);
    out.add(m ? m[1] : g);
  }
  return [...out].sort();
}

// --- the Memory page's queries ------------------------------------------------------

const PREFIXES = `PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
`;

/** The principals that wrote facts, with their graphs, fact counts and last write. */
export const AGENTS_QUERY = `${PREFIXES}SELECT ?agent (COUNT(DISTINCT ?r) AS ?facts) (MAX(?time) AS ?last)
  (GROUP_CONCAT(DISTINCT STR(?g); separator=" ") AS ?graphs)
WHERE {
  GRAPH ?g {
    ?act a prov:Activity ; prov:wasAssociatedWith ?agent .
    OPTIONAL { ?act prov:startedAtTime ?time }
    OPTIONAL { ?r prov:wasGeneratedBy ?act ; rdf:reifies ?fact }
  }
}
GROUP BY ?agent
ORDER BY DESC(?facts)
LIMIT 100`;

/** The sources facts were derived from, with fact counts. */
export const SOURCES_QUERY = `${PREFIXES}SELECT ?source (COUNT(DISTINCT ?r) AS ?facts)
WHERE {
  GRAPH ?g { ?r rdf:reifies ?fact ; prov:wasDerivedFrom ?source }
}
GROUP BY ?source
ORDER BY DESC(?facts)
LIMIT 100`;

/** The newest activities with their labels, principals and times. */
export const ACTIVITY_QUERY = `${PREFIXES}SELECT ?act ?label ?time ?agent ?g
WHERE {
  GRAPH ?g {
    ?act a prov:Activity .
    OPTIONAL { ?act rdfs:label ?label }
    OPTIONAL { ?act prov:startedAtTime ?time }
    OPTIONAL { ?act prov:wasAssociatedWith ?agent }
  }
}
ORDER BY DESC(?time)
LIMIT 20`;

/**
 * Branch name prefixes of review work: proposals, ingestion, reviews and scratchpads.
 * Branch names hold no slash, so the server's agent template names proposal branches
 * `proposals.NAME.*`; the slash form of the spec is accepted too.
 */
export const REVIEW_BRANCH = /^(proposals[./]|ingest[./]|review[./]|scratch)/;

export const isReviewBranch = (name: string) => REVIEW_BRANCH.test(name);
