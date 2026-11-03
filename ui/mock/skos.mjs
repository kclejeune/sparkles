// Mock of the thesaurus editor's server side (docs/specs/U12-ontology-workbench/SPEC.md,
// "SKOS and SKOS-XL thesaurus editor"): the `thes` dataset, a small multilingual thesaurus
// with two schemes, a polyhierarchy, SKOS-XL labels, collections, mappings and one label
// clash; `GET /$/skos/shapes`, which serves the server's own shapes file; and
// `POST /{ds}/shacl` with those shapes, which the mock answers by running SPARQL that
// stands in for each integrity condition of the shapes. The editor's reads are SPARQL and
// its writes are updates, which the mock's oxigraph store runs as they are.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const XL = 'http://www.w3.org/2008/05/skos-xl#';
export const THES = 'http://example.org/thesaurus/';

export const THES_PREFIXES = {
  skos: SKOS,
  skosxl: XL,
  th: THES,
  rdfs: 'http://www.w3.org/2000/01/rdf-schema#',
  rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
};

export const THES_TURTLE = `
@prefix skos: <${SKOS}> . @prefix skosxl: <${XL}> . @prefix th: <${THES}> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

th:animals a skos:ConceptScheme ; skos:prefLabel "Animals"@en , "Animaux"@fr , "Tiere"@de ;
  skos:hasTopConcept th:vertebrates , th:invertebrates ;
  skos:definition "A thesaurus of animals by their kind and their habitat."@en .
th:habitats a skos:ConceptScheme ; skos:prefLabel "Habitats"@en , "Habitats"@fr ;
  skos:hasTopConcept th:water , th:land .

th:vertebrates a skos:Concept ; skos:inScheme th:animals ; skos:topConceptOf th:animals ;
  skos:prefLabel "Vertebrates"@en , "Vertébrés"@fr , "Wirbeltiere"@de ;
  skos:narrower th:mammals , th:birds , th:fish .
th:invertebrates a skos:Concept ; skos:inScheme th:animals ; skos:topConceptOf th:animals ;
  skos:prefLabel "Invertebrates"@en , "Invertébrés"@fr ;
  skos:narrower th:insects .
th:mammals a skos:Concept ; skos:inScheme th:animals ; skos:broader th:vertebrates ;
  skos:prefLabel "Mammals"@en , "Mammifères"@fr , "Säugetiere"@de ;
  skos:altLabel "Mammalia"@en ;
  skos:narrower th:whales , th:bats , th:dogs ;
  skos:definition "Vertebrates that nurse their young with milk."@en .
th:birds a skos:Concept ; skos:inScheme th:animals ; skos:broader th:vertebrates ;
  skos:prefLabel "Birds"@en , "Oiseaux"@fr ; skos:altLabel "Aves"@en ;
  skos:narrower th:penguins , th:owls ;
  skos:related th:flight .
th:fish a skos:Concept ; skos:inScheme th:animals ; skos:broader th:vertebrates ;
  skos:prefLabel "Fish"@en , "Poissons"@fr .
th:insects a skos:Concept ; skos:inScheme th:animals ; skos:broader th:invertebrates ;
  skos:prefLabel "Insects"@en , "Insectes"@fr ; skos:hiddenLabel "insekts"@en .
th:whales a skos:Concept ; skos:inScheme th:animals ;
  skos:broader th:mammals , th:marine ;
  skos:prefLabel "Whales"@en , "Baleines"@fr ; skos:altLabel "Cetaceans"@en ;
  skos:exactMatch <http://www.wikidata.org/entity/Q1865281> .
th:bats a skos:Concept ; skos:inScheme th:animals ; skos:broader th:mammals ;
  skos:prefLabel "Bats"@en , "Chauves-souris"@fr ; skos:related th:flight .
th:dogs a skos:Concept ; skos:inScheme th:animals ; skos:broader th:mammals ;
  skos:prefLabel "Dogs"@en ;
  skosxl:prefLabel th:dogs-xl-fr ; skosxl:altLabel th:dogs-xl-alt .
th:dogs-xl-fr a skosxl:Label ; skosxl:literalForm "Chiens"@fr .
th:dogs-xl-alt a skosxl:Label ; skosxl:literalForm "Canines"@en .
th:penguins a skos:Concept ; skos:inScheme th:animals ;
  skos:broader th:birds , th:marine ;
  skos:prefLabel "Penguins"@en , "Manchots"@fr .
th:owls a skos:Concept ; skos:inScheme th:animals ; skos:broader th:birds ;
  skos:prefLabel "Owls"@en , "Chouettes"@fr ;
  skos:altLabel "Owls"@en .
th:marine a skos:Concept ; skos:inScheme th:animals ; skos:broader th:vertebrates ;
  skos:prefLabel "Marine animals"@en , "Animaux marins"@fr ;
  skos:narrower th:whales , th:penguins .
th:flight a skos:Concept ; skos:inScheme th:animals ;
  skos:prefLabel "Flying animals"@en ; skos:related th:birds , th:bats .

th:water a skos:Concept ; skos:inScheme th:habitats ; skos:topConceptOf th:habitats ;
  skos:prefLabel "Water"@en , "Eau"@fr ; skos:narrower th:ocean .
th:ocean a skos:Concept ; skos:inScheme th:habitats ; skos:broader th:water ;
  skos:prefLabel "Ocean"@en , "Océan"@fr ; skos:closeMatch th:marine .
th:land a skos:Concept ; skos:inScheme th:habitats ; skos:topConceptOf th:habitats ;
  skos:prefLabel "Land"@en , "Terre"@fr .

th:pets a skos:Collection ; skos:prefLabel "Pets"@en , "Animaux de compagnie"@fr ;
  skos:member th:dogs , th:fish .
th:bySize a skos:OrderedCollection ; skos:prefLabel "Mammals by size"@en ;
  skos:memberList ( th:whales th:dogs th:bats ) .

<urn:unrelated:s> <urn:unrelated:p> "kept as it is" .
`;

const here = dirname(fileURLToPath(import.meta.url));
/** The server's own shapes file, which the mock serves as it is. */
const SHAPES = readFileSync(join(here, '../../crates/sparkles/src/skos/shapes.ttl'), 'utf8');
const P = `PREFIX skos: <${SKOS}> PREFIX skosxl: <${XL}> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>`;
const H = '(skos:broader|^skos:narrower|skos:broaderTransitive|^skos:narrowerTransitive)+';
const XLF = (k) => `skosxl:${k}Label/skosxl:literalForm`;

/**
 * Each integrity condition of the shapes as a query of the focus nodes (`?this`) and
 * values (`?value`) that break it, with the shapes' message.
 */
const CHECKS = [
  {
    message: 'S9: skos:ConceptScheme is disjoint with skos:Concept',
    q: `SELECT ?this ?value WHERE { ?this a skos:Concept , skos:ConceptScheme }`,
  },
  {
    message:
      'S14: a resource has no more than one preferred label per language tag, SKOS-XL labels included',
    q: `SELECT DISTINCT ?this ?value WHERE { ?this skos:prefLabel|${XLF('pref')} ?value , ?o
        FILTER(!sameTerm(?value, ?o) && LANG(?value) != "" && LCASE(LANG(?value)) = LCASE(LANG(?o))) }`,
  },
  {
    message: 'S13: preferred, alternative and hidden labels are pairwise disjoint',
    q: `SELECT DISTINCT ?this ?value WHERE {
        { ?this skos:prefLabel ?value . { ?this skos:altLabel ?value } UNION { ?this skos:hiddenLabel ?value } }
        UNION { ?this skos:altLabel ?value ; skos:hiddenLabel ?value } }`,
  },
  {
    message:
      'S13 with S55 to S57: preferred, alternative and hidden labels are pairwise disjoint, SKOS-XL labels included',
    q: `SELECT DISTINCT ?this ?value WHERE {
        { ?this ${XLF('pref')} ?value . { ?this skos:altLabel|skos:hiddenLabel|${XLF('alt')}|${XLF('hidden')} ?value } }
        UNION { ?this ${XLF('alt')} ?value . { ?this skos:prefLabel|skos:hiddenLabel|${XLF('hidden')} ?value } }
        UNION { ?this ${XLF('hidden')} ?value . { ?this skos:prefLabel|skos:altLabel ?value } } }`,
  },
  {
    message: 'S27: skos:related is disjoint with skos:broaderTransitive',
    q: `SELECT DISTINCT ?this ?value WHERE { ?this skos:related|^skos:related ?value .
        { ?this ${H} ?value } UNION { ?value ${H} ?this } }`,
  },
  {
    message:
      'S37: skos:Collection is disjoint with skos:Concept, and the semantic relations link concepts',
    q: `SELECT DISTINCT ?this ?value WHERE { ?this a skos:Collection .
        { ?this a skos:Concept } UNION { ?x skos:broader|skos:narrower|skos:related ?this }
        UNION { ?this skos:broader|skos:narrower|skos:related ?x } }`,
  },
  {
    message: 'S46: skos:exactMatch is disjoint with skos:broadMatch and skos:relatedMatch',
    q: `SELECT DISTINCT ?this ?value WHERE { ?this skos:exactMatch|^skos:exactMatch ?value .
        { ?this skos:broadMatch|^skos:narrowMatch ?value } UNION { ?value skos:broadMatch|^skos:narrowMatch ?this }
        UNION { ?this skos:relatedMatch|^skos:relatedMatch ?value } }`,
  },
  {
    message: 'S52: a SKOS-XL label has exactly one literal form',
    q: `SELECT ?this (COUNT(?f) AS ?n) WHERE { { ?this a skosxl:Label } UNION { ?x skosxl:prefLabel|skosxl:altLabel|skosxl:hiddenLabel ?this }
        OPTIONAL { ?this skosxl:literalForm ?f } } GROUP BY ?this HAVING (COUNT(DISTINCT ?f) != 1)`,
  },
];

const termJson = (t) =>
  t.termType === 'NamedNode'
    ? { type: 'uri', value: t.value }
    : t.termType === 'BlankNode'
      ? { type: 'bnode', value: t.value }
      : {
          type: 'literal',
          value: t.value,
          ...(t.language ? { 'xml:lang': t.language } : {}),
        };

/** Whether a `/{ds}/shacl` body is the built-in SKOS shapes. */
export const isSkosShapes = (body) => String(body ?? '').includes('urn:x-sparkles:skos:');

/** The compact report of the built-in SKOS shapes over a dataset, as the server answers it. */
export function skosReport(ds) {
  const results = [];
  for (const c of CHECKS) {
    for (const b of ds.store.query(`${P} ${c.q}`, { use_default_graph_as_union: true })) {
      const focus = b.get('this');
      if (!focus) continue;
      const value = b.get('value');
      results.push({
        focusNode: termJson(focus),
        resultPath: null,
        value: value ? termJson(value) : null,
        sourceShape: { type: 'uri', value: 'urn:x-sparkles:skos:LabelledShape' },
        sourceConstraintComponent: {
          type: 'uri',
          value: 'http://www.w3.org/ns/shacl#SPARQLConstraintComponent',
        },
        severity: { type: 'uri', value: 'http://www.w3.org/ns/shacl#Violation' },
        messages: [c.message],
      });
    }
  }
  return { conforms: !results.length, results };
}

/** `GET /$/skos/shapes`; true when it answered. */
export function handleSkos(req, res, seg, { send }) {
  if (seg[0] === '$' && seg[1] === 'skos' && seg[2] === 'shapes' && req.method === 'GET') {
    send(res, 200, SHAPES, 'text/turtle; charset=utf-8');
    return true;
  }
  return false;
}
