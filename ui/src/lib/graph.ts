import type { Term } from './api';
import type { GEdge, GNode } from './components/GraphView.svelte';
import {
  displayIri,
  literalText,
  localName,
  RDF_TYPE,
  RDFS_LABEL,
  termKey,
  type PrefixMap,
} from './rdf';

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
  const d = displayIri(iri, prefixes);
  return d === iri ? localName(iri) : d;
}

export function kindOf(t: Term): GNode['kind'] {
  return t.type === 'uri' ? 'iri' : t.type;
}

/** Turn triples into graph elements. Literals get one node per (subject, predicate, value). */
export function triplesToGraph(
  triples: Triple[],
  prefixes: PrefixMap,
  opts: { hideTypes?: boolean; hideLiterals?: boolean; maxNodes?: number } = {},
): GraphBuild {
  const max = opts.maxNodes ?? 500;
  const labels = new Map<string, string>();
  for (const [s, p, o] of triples) {
    if (p.type === 'uri' && LABEL_PREDICATES.has(p.value) && o.type === 'literal') {
      const k = termKey(s);
      if (!labels.has(k) || p.value === RDFS_LABEL) labels.set(k, o.value);
    }
  }
  const nodes = new Map<string, GNode>();
  const edges: GEdge[] = [];
  let truncated = false;
  const seenNodeIds = new Set<string>();

  const ensure = (t: Term, id: string): boolean => {
    seenNodeIds.add(id);
    if (nodes.has(id)) return true;
    if (nodes.size >= max) {
      truncated = true;
      return false;
    }
    nodes.set(id, {
      id,
      label: labels.get(id) ?? nodeLabel(t, prefixes),
      kind: kindOf(t),
      title: t.type === 'uri' ? t.value : t.value.toString(),
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
      // Label literals are already shown as the node label.
      if (opts.hideLiterals || labels.get(termKey(s)) === o.value) {
        ensure(s, termKey(s));
        return;
      }
    }
    const sid = termKey(s);
    const oid = o.type === 'literal' ? `lit:${sid}|${termKey(p)}|${termKey(o)}` : termKey(o);
    if (!ensure(s, sid)) return;
    if (!ensure(o, oid)) return;
    edges.push({
      id: `e${i}:${sid}|${termKey(p)}|${oid}`,
      source: sid,
      target: oid,
      label: p.type === 'uri' ? shortLabel(p.value, prefixes) : nodeLabel(p, prefixes),
      title: p.type === 'uri' ? p.value : undefined,
      isType,
    });
  });
  return { nodes: [...nodes.values()], edges, truncated, totalNodes: seenNodeIds.size };
}
