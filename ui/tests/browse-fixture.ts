// A small ontology in the style of an OBO Foundry release, for the ontology browser's
// end-to-end tests and README screenshots: labels, definitions with their sources,
// synonyms of several kinds, cross-references, subsets, a part-of relation, a class with
// two parents, siblings, a deprecated class with its replacement, and a graph of its own.
// The namespace CELLO and its wording are made up for the tests.

export const BROWSE_DATASET = 'browse-e2e';
export const OBO = 'http://purl.obolibrary.org/obo/';
export const CELLO = (n: number) => `${OBO}CELLO_${String(n).padStart(7, '0')}`;

const c = (n: number) => `obo:CELLO_${String(n).padStart(7, '0')}`;

/** A class with an OBO identifier, label, namespace and the rest of `more`. */
const cls = (n: number, label: string, more: string) => `${c(n)} a owl:Class ;
  rdfs:label "${label}" ;
  oio:id "CELLO:${String(n).padStart(7, '0')}" ;
  oio:hasOBONamespace "cellular_component" ;
  ${more} .`;

export const BROWSE_TURTLE = `
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix dc: <http://purl.org/dc/elements/1.1/> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix oio: <http://www.geneontology.org/formats/oboInOwl#> .
@prefix obo: <${OBO}> .

<${OBO}cello.owl> a owl:Ontology ;
  dc:title "Cell organelle ontology" ;
  dc:description "A small ontology of the parts of a eukaryotic cell, written for the tests of the Sparkles ontology browser." ;
  owl:versionInfo "2026-11-01" ;
  dcterms:license <https://creativecommons.org/licenses/by/4.0/> ;
  oio:default-namespace "cellular_component" .

obo:IAO_0000115 a owl:AnnotationProperty ; rdfs:label "definition" .
obo:IAO_0100001 a owl:AnnotationProperty ; rdfs:label "term replaced by" .
oio:hasExactSynonym a owl:AnnotationProperty .
oio:hasRelatedSynonym a owl:AnnotationProperty .
oio:hasDbXref a owl:AnnotationProperty .
oio:id a owl:AnnotationProperty .
oio:inSubset a owl:AnnotationProperty .
oio:hasOBONamespace a owl:AnnotationProperty .

obo:BFO_0000050 a owl:ObjectProperty , owl:TransitiveProperty ;
  rdfs:label "part of" ;
  oio:id "BFO:0000050" ;
  obo:IAO_0000115 "A core relation that holds between a part and its whole." .
obo:BFO_0000051 a owl:ObjectProperty ;
  rdfs:label "has part" ;
  owl:inverseOf obo:BFO_0000050 .

${cls(
  1,
  'cellular component',
  `obo:IAO_0000115 "A location in or around a cell that a gene product can occupy." ;
  oio:inSubset "slim_generic"`,
)}
${cls(
  2,
  'organelle',
  `rdfs:subClassOf ${c(1)} ;
  obo:IAO_0000115 "A structure of a cell with a distinct form and function." ;
  oio:hasDbXref "Wikipedia:Organelle"`,
)}
${cls(
  3,
  'membrane-bounded organelle',
  `rdfs:subClassOf ${c(2)} ;
  obo:IAO_0000115 "An organelle that a lipid bilayer encloses."`,
)}
${cls(
  4,
  'intracellular organelle',
  `rdfs:subClassOf ${c(2)} ;
  obo:IAO_0000115 "An organelle found inside a cell."`,
)}
${cls(
  5,
  'mitochondrion',
  `rdfs:subClassOf ${c(3)} , ${c(4)} ,
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom ${c(6)} ] ;
  obo:IAO_0000115 "A self-replicating organelle of the cytoplasm of eukaryotic cells, where most of the cell's respiration takes place." ;
  oio:hasExactSynonym "mitochondria" ;
  oio:hasRelatedSynonym "powerhouse of the cell" ;
  oio:hasDbXref "Wikipedia:Mitochondrion" , "NIF_Subcellular:sao1860313010" ;
  oio:inSubset "slim_generic" , "slim_yeast" ;
  rdfs:comment "Some anaerobic organisms keep mitochondrion-related organelles instead. Annotate those with CELLO:0000011 'mitosome'."`,
)}
${cls(
  6,
  'cytoplasm',
  `rdfs:subClassOf ${c(1)} ;
  obo:IAO_0000115 "The contents of a cell between the plasma membrane and the nucleus."`,
)}
${cls(
  7,
  'mitochondrial envelope',
  `rdfs:subClassOf ${c(1)} ,
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom ${c(5)} ] ;
  obo:IAO_0000115 "The double membrane that surrounds a mitochondrion."`,
)}
${cls(
  8,
  'mitochondrial matrix',
  `rdfs:subClassOf ${c(1)} ,
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom ${c(5)} ] ;
  obo:IAO_0000115 "The space inside the inner membrane of a mitochondrion."`,
)}
${cls(
  9,
  'nucleus',
  `rdfs:subClassOf ${c(3)} ;
  obo:IAO_0000115 "The organelle that holds most of the genetic material of a eukaryotic cell."`,
)}
${cls(
  10,
  'vacuole',
  `rdfs:subClassOf ${c(3)} ;
  obo:IAO_0000115 "A membrane-bounded compartment that stores water, ions and waste."`,
)}
${cls(
  11,
  'mitosome',
  `rdfs:subClassOf ${c(3)} ;
  obo:IAO_0000115 "A small organelle derived from a mitochondrion that has lost its respiratory function."`,
)}
${cls(
  12,
  'obsolete mitochondrial organelle',
  `owl:deprecated true ;
  obo:IAO_0100001 ${c(5)} ;
  rdfs:comment "This term was merged into mitochondrion."`,
)}
${cls(
  13,
  'mitochondrial nucleoid',
  `rdfs:subClassOf ${c(5)} ;
  obo:IAO_0000115 "The region of a mitochondrion that holds its DNA."`,
)}

[] a owl:Axiom ;
  owl:annotatedSource ${c(5)} ;
  owl:annotatedProperty obo:IAO_0000115 ;
  owl:annotatedTarget "A self-replicating organelle of the cytoplasm of eukaryotic cells, where most of the cell's respiration takes place." ;
  oio:hasDbXref "CELLO:curators" .
[] a owl:Axiom ;
  owl:annotatedSource ${c(5)} ;
  owl:annotatedProperty oio:hasExactSynonym ;
  owl:annotatedTarget "mitochondria" ;
  oio:hasDbXref "CELLO:plural" .
`;
