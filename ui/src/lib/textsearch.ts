// Full-text search (`text:query`, docs/API.md "Full-text search"): query building and
// result shaping for the Explore page's Search tab.
import * as api from './api';
import type { Term } from './api';
import { sparqlIri } from './rdf';

export const TEXT_NS = 'http://jena.apache.org/text#';
/** The graph slot of a hit in the default graph. */
export const DEFAULT_GRAPH = 'urn:x-arq:DefaultGraph';

/**
 * A SPARQL string literal (`"…"`) holding exactly `s`. Backslashes are doubled, so the
 * text query syntax's own escapes (`\:`) reach the search engine unchanged.
 */
export function sparqlLiteral(s: string): string {
  const body = s.replace(/[\\"\n\r\t\b\f]/g, (c) => {
    switch (c) {
      case '\n':
        return '\\n';
      case '\r':
        return '\\r';
      case '\t':
        return '\\t';
      case '\b':
        return '\\b';
      case '\f':
        return '\\f';
      default:
        return `\\${c}`;
    }
  });
  return `"${body}"`;
}

/**
 * The text query as the search engine should see it. Everything the user typed passes
 * through (terms, "phrases"~slop, AND/OR, +required/-excluded, parentheses, prefix
 * phrases), except that a `:` not already escaped becomes `\:`. The index has a single
 * text field, so `field:term` syntax would only fail ("Field does not exist"); escaping
 * lets IRIs, times and prefixed names be searched as typed.
 */
export function escapeTextQuery(q: string): string {
  let out = '';
  for (let i = 0; i < q.length; i++) {
    const c = q[i];
    if (c === '\\' && i + 1 < q.length) {
      // keep an existing escape pair as it is
      out += c + q[i + 1];
      i++;
    } else if (c === ':') {
      out += '\\:';
    } else {
      out += c;
    }
  }
  return out;
}

export type TextSearchOptions = {
  /** Only search literals of these predicates (default: every indexed predicate). */
  predicates?: string[];
  /** Language tag of the literals to search (e.g. `en`). */
  lang?: string;
  /** Hits to return (the search's own top-n, before any join). */
  limit?: number;
  /**
   * Search the named graphs as well as the default graph (the search runs within the
   * active graph, so named graphs need their own `GRAPH ?g` call).
   */
  namedGraphs?: boolean;
};

/** A ranked `text:query` SELECT returning subject, score, literal, graph and predicate. */
export function buildTextQuery(text: string, opts: TextSearchOptions = {}): string {
  const limit = Math.max(1, Math.floor(opts.limit ?? 50));
  const args = [
    ...(opts.predicates ?? []).map(sparqlIri),
    sparqlLiteral(escapeTextQuery(text.trim())),
    String(limit),
    ...(opts.lang?.trim() ? [sparqlLiteral(`lang:${opts.lang.trim()}`)] : []),
  ];
  const call = `(?s ?score ?literal ?graph ?predicate) text:query (${args.join(' ')}) .`;
  const where = opts.namedGraphs
    ? `  { ${call} }
  UNION
  { GRAPH ?g { ${call} } }`
    : `  ${call}`;
  return `PREFIX text: <${TEXT_NS}>
SELECT ?s ?score ?literal ?graph ?predicate WHERE {
${where}
}
ORDER BY DESC(?score)
LIMIT ${limit}`;
}

export type TextHit = {
  subject: Term;
  score: number;
  literal?: Term;
  /** Named graph of the match (undefined in the default graph). */
  graph?: string;
  predicate?: string;
};

/** Rows of the query from `buildTextQuery` as hits, best first. */
export function textHits(rows: Record<string, Term | undefined>[]): TextHit[] {
  const hits: TextHit[] = [];
  for (const r of rows) {
    if (!r.s) continue;
    const g = r.graph?.type === 'uri' ? r.graph.value : undefined;
    hits.push({
      subject: r.s,
      score: r.score?.type === 'literal' ? Number(r.score.value) : NaN,
      literal: r.literal,
      graph: g && g !== DEFAULT_GRAPH ? g : undefined,
      predicate: r.predicate?.type === 'uri' ? r.predicate.value : undefined,
    });
  }
  return hits.sort((a, b) => (b.score || 0) - (a.score || 0));
}

/** Run a text search. */
export async function textSearch(
  ds: string,
  text: string,
  opts: TextSearchOptions & { signal?: AbortSignal } = {},
): Promise<TextHit[]> {
  const rows = await api.select(ds, buildTextQuery(text, opts), { signal: opts.signal });
  return textHits(rows);
}

/**
 * Predicates typed one per line (or separated by spaces/commas) as full IRIs, `<IRIs>`
 * or prefixed names. Returns the IRIs in order without duplicates, and the entries that
 * are neither.
 */
export function parsePredicateList(
  text: string,
  prefixes: Record<string, string>,
): { iris: string[]; bad: string[] } {
  const iris: string[] = [];
  const bad: string[] = [];
  for (const raw of text.split(/[\s,]+/)) {
    const t = raw.trim();
    if (!t) continue;
    let iri: string | null = null;
    const angled = /^<([^<>\s]+)>$/.exec(t);
    const pn = /^([A-Za-z][\w.-]*)?:([^\s<>]*)$/.exec(t);
    if (angled) iri = angled[1];
    else if (pn && prefixes[pn[1] ?? ''] != null) iri = prefixes[pn[1] ?? ''] + pn[2];
    else if (/^[a-z][a-z0-9+.-]*:[^\s<>"]+$/i.test(t)) iri = t;
    if (!iri || !/^[a-z][a-z0-9+.-]*:/i.test(iri)) bad.push(t);
    else if (!iris.includes(iri)) iris.push(iri);
  }
  return { iris, bad };
}

/** Predicates that usually hold text, offered first when configuring the index. */
export const TEXT_PREDICATES = [
  'http://www.w3.org/2000/01/rdf-schema#label',
  'http://www.w3.org/2000/01/rdf-schema#comment',
  'http://www.w3.org/2004/02/skos/core#prefLabel',
  'http://www.w3.org/2004/02/skos/core#altLabel',
  'http://www.w3.org/2004/02/skos/core#definition',
  'http://purl.org/dc/terms/title',
  'http://purl.org/dc/terms/description',
  'http://purl.org/dc/elements/1.1/title',
  'http://purl.org/dc/elements/1.1/description',
  'http://xmlns.com/foaf/0.1/name',
  'http://schema.org/name',
  'http://schema.org/description',
];

/** A plain-words explanation of a failed text search, by status. */
export function textErrorHint(e: unknown): { title: string; hint?: string } {
  if (!(e instanceof api.ApiError)) return { title: api.errorMessage(e) };
  switch (e.status) {
    case 400:
      return /no full-text index|not text-indexed|not enabled/i.test(e.message)
        ? {
            title: e.message,
            hint: 'Enable full-text search (or add the predicate) in the Full-text search panel of the dataset page.',
          }
        : {
            title: e.message,
            hint: 'Check the query syntax: quotes for phrases, AND/OR, +required, -excluded, parentheses.',
          };
    case 503:
      return {
        title: 'The full-text index is being rebuilt or is behind the data.',
        hint: `Text queries are refused rather than answered from a stale index. Try again when the rebuild has finished. (${e.message})`,
      };
    case 501:
      return { title: 'This server was built without full-text search.' };
    case 507:
      return {
        title: e.message,
        hint: 'Too many hits without a limit: lower the number of results or narrow the query.',
      };
    default:
      return { title: api.errorMessage(e) };
  }
}
