// Query-result labels read only for visible entities, from the result's branch and state.
import { sparqlIri } from './rdf';
import type { Term } from './api';

export function resultLabelsQuery(iris: readonly string[]): string {
  const pattern = '?s ?p ?l FILTER(isLiteral(?l))';
  return `SELECT DISTINCT ?s ?p ?l WHERE { VALUES ?s { ${iris.map(sparqlIri).join(' ')} } VALUES ?p { <http://www.w3.org/2000/01/rdf-schema#label> <http://www.w3.org/2004/02/skos/core#prefLabel> } { ${pattern} } UNION { GRAPH ?g { ${pattern} } } }`;
}

/** Include the entities nested in an RDF triple term. */
export function termIris(term: Term | null | undefined): string[] {
  if (term?.type === 'uri') return [term.value];
  if (term?.type !== 'triple') return [];
  return [term.value.subject, term.value.predicate, term.value.object].flatMap(termIris);
}
