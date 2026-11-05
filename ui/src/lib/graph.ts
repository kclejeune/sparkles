import type { Term } from './api';
import { iriForm } from './iri-form.svelte';
import { preferredLabel, type Label } from './rendering';
import type { GEdge, GNode } from './components/GraphView.svelte';
import type { Mark } from './rowdiff';
import { displayEntity, literalText, RDF_TYPE, RDFS_LABEL, termKey, type PrefixMap } from './rdf';

export type Triple = [Term, Term, Term];

export type GraphBuild = { nodes: GNode[]; edges: GEdge[]; truncated: boolean; totalNodes: number };

const LABEL_PREDICATES = new Set([
  RDFS_LABEL,
  'http://www.w3.org/2004/02/skos/core#prefLabel',
  'http://xmlns.com/foaf/0.1/name',
  'http://schema.org/name',
  'http://purl.org/dc/terms/title',
]);

export function nodeLabel(t: Term, prefixes: PrefixMap): string {
  if (t.type === 'uri') return shortLabel(t.value, prefixes);
  if (t.type === 'bnode') return `_:${t.value}`;
  if (t.type === 'literal') {
    const s = literalText(t);
    return s.length > 60 ? s.slice(0, 57) + '…' : s;
  }
  return '<<triple>>';
}

export function shortLabel(iri: string, prefixes: PrefixMap): string {
  if (iri.startsWith('urn:var:')) return '?' + iri.slice(8);
  return displayEntity(iri, prefixes);
}

export function kindOf(t: Term): GNode['kind'] {
  return t.type === 'uri' ? 'iri' : t.type;
}

/**
 * Turn triples into graph elements. Literals get one node per (subject, predicate, value).
 * With `marks` (parallel to `triples`) each edge carries the change of its triple between
 * two states, and a node is added or removed when every triple it appears in is.
 */
export function triplesToGraph(
  triples: Triple[],
  prefixes: PrefixMap,
  opts: { hideTypes?: boolean; hideLiterals?: boolean; maxNodes?: number; marks?: Mark[] } = {},
): GraphBuild {
  const max = opts.maxNodes ?? 500;
  const labels = new Map<string, string>();
  const candidates = new Map<string, Label[]>();
  for (const [s, p, o] of triples) {
    if (p.type === 'uri' && LABEL_PREDICATES.has(p.value) && o.type === 'literal') {
      const k = termKey(s);
      const all = candidates.get(k) ?? [];
      const label = { '@value': o.value, '@language': o['xml:lang'] };
      if (p.value === RDFS_LABEL) all.unshift(label);
      else all.push(label);
      candidates.set(k, all);
    }
  }
  for (const [id, all] of candidates) {
    const label = preferredLabel(all, iriForm.languages);
    if (label) labels.set(id, label);
  }
  const nodes = new Map<string, GNode>();
  const edges: GEdge[] = [];
  let truncated = false;
  const seenNodeIds = new Set<string>();
  // per node: how many of its triples were added, removed or kept
  const seen = new Map<string, { added: number; removed: number; kept: number }>();
  const count = (id: string, m: Mark | undefined) => {
    if (!m) return;
    const c = seen.get(id) ?? { added: 0, removed: 0, kept: 0 };
    c[m === 'added' ? 'added' : m === 'removed' ? 'removed' : 'kept']++;
    seen.set(id, c);
  };
  const caption = (t: Term, id: string): string =>
    t.type === 'uri'
      ? displayEntity(t.value, prefixes, candidates.get(id))
      : (labels.get(id) ?? nodeLabel(t, prefixes));

  const ensure = (t: Term, id: string): boolean => {
    seenNodeIds.add(id);
    if (nodes.has(id)) return true;
    if (nodes.size >= max) {
      truncated = true;
      return false;
    }
    nodes.set(id, {
      id,
      label: caption(t, id),
      kind: kindOf(t),
      title: t.type === 'uri' ? t.value : t.value.toString(),
      term: t,
    });
    return true;
  };

  triples.forEach(([s, p, o], i) => {
    const isType = p.type === 'uri' && p.value === RDF_TYPE;
    if (opts.hideTypes && isType) return;
    if (
      o.type === 'literal' &&
      (opts.hideLiterals ||
        (p.type === 'uri' && LABEL_PREDICATES.has(p.value) && labels.has(termKey(s))))
    ) {
      // A label statement is redundant only when the caption actually displays it.
      if (opts.hideLiterals || caption(s, termKey(s)) === o.value) {
        ensure(s, termKey(s));
        count(termKey(s), opts.marks?.[i]);
        return;
      }
    }
    const sid = termKey(s);
    const oid = o.type === 'literal' ? `lit:${sid}|${termKey(p)}|${termKey(o)}` : termKey(o);
    if (!ensure(s, sid)) return;
    if (!ensure(o, oid)) return;
    const mark = opts.marks?.[i];
    count(sid, mark);
    count(oid, mark);
    edges.push({
      id: `e${i}:${sid}|${termKey(p)}|${oid}`,
      source: sid,
      target: oid,
      label: p.type === 'uri' ? shortLabel(p.value, prefixes) : nodeLabel(p, prefixes),
      title: p.type === 'uri' ? p.value : undefined,
      isType,
      triple: [s, p, o],
      ...(mark ? { change: mark } : {}),
    });
  });
  if (opts.marks) {
    for (const [id, c] of seen) {
      const n = nodes.get(id);
      if (!n) continue;
      n.change = c.kept || (c.added && c.removed) ? 'kept' : c.added ? 'added' : 'removed';
    }
  }
  return { nodes: [...nodes.values()], edges, truncated, totalNodes: seenNodeIds.size };
}
