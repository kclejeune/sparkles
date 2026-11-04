// Mock of the ontology routes (docs/API.md, "Ontology") for the ontology editor
// (docs/specs/U12-ontology-workbench/SPEC.md): the summary, entities, entity, axioms,
// edits with dry runs and If-Match, entity removal and a small classification, and the
// change feed of `/{ds}/changes` as server-sent events.
//
// The real server reads the OWL 2 mapping to RDF graphs in full. The mock reads the
// structures its editor writes: declarations, named and restriction class expressions,
// the hierarchy, domains, ranges, characteristics, inverses, chains, assertions,
// annotation assertions and annotated axioms through `owl:Axiom` nodes, and keeps every
// other triple as unmapped. Edits write the same structures with fresh blank nodes. Its
// classification takes the told hierarchy and the classes defined as an intersection of
// named classes and existential restrictions, which is enough for the editor's inferred
// tree. Functional-style axioms are parsed for the forms the editor sends.

import ox from 'oxigraph';
import { linesOf } from './branches.mjs';
import { stateLines } from './history.mjs';

const RDF = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const OWL = 'http://www.w3.org/2002/07/owl#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const DCT = 'http://purl.org/dc/terms/';
const TYPE = RDF + 'type';

/** The `uni` dataset: a small university ontology in the default graph. */
export const UNI_PREFIXES = {
  rdf: RDF,
  rdfs: RDFS,
  owl: OWL,
  xsd: XSD,
  skos: SKOS,
  dcterms: DCT,
  uni: 'http://example.org/uni#',
};

export const UNI_TURTLE = `
@prefix rdf: <${RDF}> . @prefix rdfs: <${RDFS}> . @prefix owl: <${OWL}> .
@prefix xsd: <${XSD}> . @prefix skos: <${SKOS}> . @prefix dcterms: <${DCT}> .
@prefix uni: <http://example.org/uni#> .

<http://example.org/uni> a owl:Ontology ;
  owl:versionIRI <http://example.org/uni/1.2> ;
  rdfs:label "University ontology"@en , "Ontologie universitaire"@fr ;
  dcterms:creator "Sparkles mock" ; owl:versionInfo "1.2" .

skos:definition a owl:AnnotationProperty .
uni:editorialNote a owl:AnnotationProperty ; rdfs:label "editorial note"@en ; rdfs:subPropertyOf rdfs:comment .

uni:Agent a owl:Class ; rdfs:label "Agent"@en .
uni:Person a owl:Class ; rdfs:subClassOf uni:Agent ; rdfs:label "Person"@en , "Personne"@fr ;
  skos:definition "A human being who takes part in university life."@en ,
    "Un être humain qui prend part à la vie universitaire."@fr ;
  rdfs:comment "Staff and students are both persons."@en ,
    "A person is counted once however many roles they hold: someone who teaches a course while enrolled in another is one Person who is both an Employee and a Student. Departments and universities are agents too, but they are organizations, which are disjoint from persons in this ontology."@en ;
  skos:scopeNote "Use for people in any role; prefer Student or Employee when the role is known."@en ;
  skos:example "Ada Lovelace, a professor who teaches Logic 101."@en ;
  skos:historyNote "Split from Agent in version 1.1, when organizations got a class of their own."@en ;
  skos:editorialNote "Check whether visiting scholars need a subclass."@en .
uni:Student a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Student"@en , "Étudiant"@fr .
uni:GraduateStudent a owl:Class ; rdfs:subClassOf uni:Student ; rdfs:label "Graduate student"@en .
uni:Employee a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Employee"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ;
  rdfs:label "Professor"@en , "Professeur"@fr ;
  rdfs:comment "An academic who leads research and teaching in a department."@en .
uni:Teacher a owl:Class ; rdfs:label "Teacher"@en ;
  owl:equivalentClass [ owl:intersectionOf ( uni:Person [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ) ] .
uni:Organization a owl:Class ; rdfs:subClassOf uni:Agent ; rdfs:label "Organization"@en .
uni:University a owl:Class ; rdfs:subClassOf uni:Organization ; rdfs:label "University"@en .
uni:Department a owl:Class ; rdfs:subClassOf uni:Organization ; rdfs:label "Department"@en .
uni:Course a owl:Class ; rdfs:label "Course"@en , "Cours"@fr ; owl:disjointWith uni:Person .
uni:GraduateCourse a owl:Class ; rdfs:subClassOf uni:Course ; rdfs:label "Graduate course"@en .

uni:memberOf a owl:ObjectProperty ; rdfs:label "member of"@en ; rdfs:domain uni:Person ; rdfs:range uni:Organization .
uni:worksFor a owl:ObjectProperty ; rdfs:subPropertyOf uni:memberOf ; rdfs:label "works for"@en ; rdfs:domain uni:Employee .
uni:headOf a owl:ObjectProperty , owl:FunctionalProperty ; rdfs:subPropertyOf uni:worksFor ; rdfs:label "head of"@en ; rdfs:range uni:Department .
uni:teaches a owl:ObjectProperty ; rdfs:label "teaches"@en ; rdfs:domain uni:Employee ; rdfs:range uni:Course ; owl:inverseOf uni:taughtBy .
uni:taughtBy a owl:ObjectProperty ; rdfs:label "taught by"@en .
uni:enrolledIn a owl:ObjectProperty ; rdfs:label "enrolled in"@en ; rdfs:domain uni:Student ; rdfs:range uni:Course .
uni:advisor a owl:ObjectProperty ; rdfs:label "advisor"@en ; rdfs:domain uni:GraduateStudent ; rdfs:range uni:Professor .
uni:subOrganizationOf a owl:ObjectProperty , owl:TransitiveProperty ; rdfs:label "sub-organization of"@en .

uni:name a owl:DatatypeProperty ; rdfs:label "name"@en ; rdfs:range xsd:string .
uni:email a owl:DatatypeProperty ; rdfs:label "email"@en ; rdfs:domain uni:Person ; rdfs:range xsd:string .
uni:credits a owl:DatatypeProperty , owl:FunctionalProperty ; rdfs:label "credits"@en ; rdfs:domain uni:Course ; rdfs:range xsd:integer .

uni:ada a owl:NamedIndividual , uni:Professor ; rdfs:label "Ada Lovelace"@en ; uni:teaches uni:logic101 ; uni:email "ada@example.org" .
uni:alan a owl:NamedIndividual , uni:GraduateStudent ; rdfs:label "Alan Turing"@en ; uni:advisor uni:ada ; uni:enrolledIn uni:logic101 .
uni:logic101 a owl:NamedIndividual , uni:Course ; rdfs:label "Logic 101"@en ; uni:credits 6 .
uni:cs a owl:NamedIndividual , uni:Department ; rdfs:label "Computer science"@en ; uni:subOrganizationOf uni:sparklesU .
uni:sparklesU a owl:NamedIndividual , uni:University ; rdfs:label "Sparkles University"@en .

[] a owl:Axiom ; owl:annotatedSource uni:GraduateStudent ; owl:annotatedProperty rdfs:subClassOf ;
  owl:annotatedTarget uni:Student ; rdfs:comment "Every graduate student is a student."@en .
`;

/** The `cells` dataset: a few cell types in the manner of an OBO cell ontology, with a rich
 * term for the Browse page's layout: a sourced definition, a description of several
 * paragraphs, a comment, sourced and typed synonyms, and part-of restrictions. */
export const CELLS_PREFIXES = {
  rdf: RDF,
  rdfs: RDFS,
  owl: OWL,
  xsd: XSD,
  dcterms: DCT,
  skos: SKOS,
  obo: 'http://purl.obolibrary.org/obo/',
  oboInOwl: 'http://www.geneontology.org/formats/oboInOwl#',
};

export const CELLS_TURTLE = `
@prefix rdf: <${RDF}> . @prefix rdfs: <${RDFS}> . @prefix owl: <${OWL}> .
@prefix xsd: <${XSD}> . @prefix dcterms: <${DCT}> . @prefix skos: <${SKOS}> .
@prefix obo: <http://purl.obolibrary.org/obo/> .
@prefix oboInOwl: <http://www.geneontology.org/formats/oboInOwl#> .
@prefix cl: <http://purl.obolibrary.org/obo/cl#> .

<http://purl.obolibrary.org/obo/cells.owl> a owl:Ontology ;
  rdfs:label "Cell types"@en ; owl:versionInfo "2026-10-01" .

obo:IAO_0000115 a owl:AnnotationProperty ; rdfs:label "definition"@en .
oboInOwl:hasExactSynonym a owl:AnnotationProperty . oboInOwl:hasRelatedSynonym a owl:AnnotationProperty .
oboInOwl:hasDbXref a owl:AnnotationProperty . oboInOwl:hasSynonymType a owl:AnnotationProperty .
cl:markerCount a owl:DatatypeProperty ; rdfs:label "marker count"@en .
cl:firstSeen a owl:DatatypeProperty ; rdfs:label "first seen"@en .
cl:PLURAL a owl:AnnotationProperty ; rdfs:label "plural"@en .
cl:ABBREVIATION a owl:AnnotationProperty .
obo:BFO_0000050 a owl:ObjectProperty ; rdfs:label "part of"@en .

obo:CL_0000000 a owl:Class ; rdfs:label "cell"@en .
obo:CL_0000125 a owl:Class ; rdfs:subClassOf obo:CL_0000000 ; rdfs:label "glial cell"@en .
obo:CL_0000235 a owl:Class ; rdfs:subClassOf obo:CL_0000000 ; rdfs:label "macrophage"@en .
obo:UBERON_0001017 a owl:Class ; rdfs:label "central nervous system"@en .
obo:UBERON_0000955 a owl:Class ; rdfs:label "brain"@en .

obo:CL_0000129 a owl:Class ;
  rdfs:label "microglial cell"@en ;
  oboInOwl:id "CL:0000129" ;
  oboInOwl:hasOBONamespace "cell" ;
  rdfs:subClassOf obo:CL_0000125 , obo:CL_0000235 ,
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom obo:UBERON_0001017 ] ;
  obo:IAO_0000115 "A resident immune cell of the central nervous system that descends from embryonic yolk-sac progenitors, surveys its surroundings with fine motile processes and clears debris and dying cells."@en ;
  dcterms:description """Microglia settle in the developing brain early, before most other glia appear.

They tile the tissue so that each cell watches its own territory, and their processes extend and retract over minutes.
In a resting state they prune synapses and support the cells around them.

After an injury they change shape, move towards the damage and take up what is left of dying cells.

Their numbers are kept by local division rather than by cells arriving from the blood.

Note: this text was written for a test fixture and is not taken from a published ontology."""@en ;
  rdfs:comment "Cells of this type are easy to confuse with macrophages that line the meninges and the blood vessels of the brain; those border macrophages differ in origin, place and the markers they express, and are not covered by this class."@en ;
  oboInOwl:hasExactSynonym "brain-resident macrophage"@en , "hortega cell"@en , "microgliocyte"@en ;
  oboInOwl:hasRelatedSynonym "microglia"@en , "MG.cns"@en .

[] a owl:Axiom ; owl:annotatedSource obo:CL_0000129 ; owl:annotatedProperty obo:IAO_0000115 ;
  owl:annotatedTarget "A resident immune cell of the central nervous system that descends from embryonic yolk-sac progenitors, surveys its surroundings with fine motile processes and clears debris and dying cells."@en ;
  oboInOwl:hasDbXref "PMID:00000001" , "GOC:fixture" .
[] a owl:Axiom ; owl:annotatedSource obo:CL_0000129 ; owl:annotatedProperty oboInOwl:hasExactSynonym ;
  owl:annotatedTarget "brain-resident macrophage"@en ; oboInOwl:hasDbXref "PMID:00000002" .
[] a owl:Axiom ; owl:annotatedSource obo:CL_0000129 ; owl:annotatedProperty oboInOwl:hasExactSynonym ;
  owl:annotatedTarget "hortega cell"@en ; oboInOwl:hasDbXref "FMA:00001" .
[] a owl:Axiom ; owl:annotatedSource obo:CL_0000129 ; owl:annotatedProperty oboInOwl:hasRelatedSynonym ;
  owl:annotatedTarget "microglia"@en ; oboInOwl:hasSynonymType cl:PLURAL .
[] a owl:Axiom ; owl:annotatedSource obo:CL_0000129 ; owl:annotatedProperty oboInOwl:hasRelatedSynonym ;
  owl:annotatedTarget "MG.cns"@en ; oboInOwl:hasSynonymType cl:ABBREVIATION ; oboInOwl:hasDbXref "FIXTURE:panel" .

# A heavy term for the layout review: several definitions from different
# predicates, comments and notes of every kind, a logical definition, several restrictions, a
# disjointness, many cross-references and entities that point at it. The text is invented.
skos:definition a owl:AnnotationProperty . skos:scopeNote a owl:AnnotationProperty .
skos:note a owl:AnnotationProperty . skos:example a owl:AnnotationProperty .
skos:historyNote a owl:AnnotationProperty . skos:editorialNote a owl:AnnotationProperty .
skos:changeNote a owl:AnnotationProperty .
obo:RO_0002202 a owl:ObjectProperty ; rdfs:label "develops from"@en .
obo:RO_0002215 a owl:ObjectProperty ; rdfs:label "capable of"@en .
obo:RO_0002100 a owl:ObjectProperty ; rdfs:label "has soma location"@en .
obo:CL_0000576 a owl:Class ; rdfs:subClassOf obo:CL_0000000 ; rdfs:label "monocyte"@en .
obo:UBERON_0002360 a owl:Class ; rdfs:label "meninx"@en .
obo:UBERON_0001891 a owl:Class ; rdfs:label "midbrain"@en .
obo:GO_0006909 a owl:Class ; rdfs:label "phagocytosis"@en .
obo:CL_0009001 a owl:Class ;
  rdfs:label "border-associated macrophage"@en , "macrophage associé aux frontières"@fr ;
  oboInOwl:id "CL:0009001" ;
  oboInOwl:hasOBONamespace "cell" ;
  rdfs:subClassOf obo:CL_0000235 ,
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom obo:UBERON_0002360 ] ,
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom obo:UBERON_0001017 ] ,
    [ a owl:Restriction ; owl:onProperty obo:RO_0002202 ; owl:someValuesFrom obo:CL_0000576 ] ,
    [ a owl:Restriction ; owl:onProperty obo:RO_0002215 ; owl:someValuesFrom obo:GO_0006909 ] ,
    [ a owl:Restriction ; owl:onProperty obo:RO_0002100 ; owl:someValuesFrom obo:UBERON_0001891 ] ;
  owl:equivalentClass [ owl:intersectionOf ( obo:CL_0000235
    [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom obo:UBERON_0002360 ] ) ] ;
  owl:disjointWith obo:CL_0000129 , obo:CL_0000125 ;
  obo:IAO_0000115 "A macrophage that sits in the meninges, the choroid plexus or around the blood vessels of the brain, where it samples the cerebrospinal fluid and the blood and takes up what they carry in."@en ,
    "Un macrophage des méninges, du plexus choroïde ou des parois des vaisseaux du cerveau qui prélève le liquide céphalorachidien et le sang."@fr ;
  skos:definition "A tissue macrophage of the barriers of the central nervous system, derived from embryonic progenitors and in part replaced by blood monocytes with age."@en ;
  dcterms:description """Border-associated macrophages form a family of cells at the interfaces of the brain and the rest of the body.

They are found in the meninges, in the choroid plexus and in the perivascular space. Each site has its own set of markers and its own rate of turnover.
They take up antigens from the fluid around them and pass signals to the T cells that patrol the same spaces.

During development they settle before birth, and in some sites they are slowly replaced by cells that come from the bone marrow.

Note: this text was written for a test fixture and is not taken from a published ontology."""@en ;
  rdfs:comment "Do not confuse with microglial cells, which live inside the tissue and not at its borders."@en ,
    "The three sites differ enough that some sources split the class by site; this ontology keeps one class and records the site in the part of restriction."@en ,
    "Markers vary by species. The mouse markers are the best known and do not all carry over to humans."@en ;
  skos:scopeNote "Use for macrophages of the meninges, the choroid plexus and the perivascular space. Use microglial cell for cells inside the parenchyma."@en ;
  skos:note "This class was added together with its three subclasses."@en ;
  skos:example "A CD206-positive macrophage in the dura mater of a mouse."@en ;
  skos:historyNote "Added in version 2026-10-01 after a request from a neuroimmunology group."@en ;
  skos:editorialNote "Check the definition against the new review of brain macrophages."@en ;
  skos:changeNote "The French label was added in the same release."@en ;
  oboInOwl:hasExactSynonym "BAM"@en , "CNS border macrophage"@en , "central nervous system border macrophage"@en ;
  oboInOwl:hasRelatedSynonym "meningeal macrophage"@en , "perivascular macrophage"@en , "choroid plexus macrophage"@en ;
  oboInOwl:hasDbXref "PMID:00000011" , "PMID:00000012" , "FMA:00002" , "MESH:D000001" , "NCIT:C00001" , "Wikipedia:Border-associated_macrophage" ,
    obo:CL_0009006 , <https://en.wikipedia.org/wiki/Foam_cell> ;
  dcterms:created "2026-10-01"^^xsd:date ;
  cl:reviewedOn "2026-10-02T09:30:00Z"^^xsd:dateTime ;
  cl:reviewCount "5"^^xsd:integer ;
  cl:reviewNote "a plain string"^^xsd:string ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty cl:markerCount ; owl:hasValue "3"^^xsd:integer ] ,
    [ a owl:Restriction ; owl:onProperty cl:firstSeen ; owl:hasValue "2026-10-01"^^xsd:date ] ;
  rdfs:seeAlso <http://example.org/reviews/brain-macrophages> .
obo:CL_0009002 a owl:Class ; rdfs:subClassOf obo:CL_0009001 ; rdfs:label "meningeal macrophage"@en .
obo:CL_0009003 a owl:Class ; rdfs:subClassOf obo:CL_0009001 ; rdfs:label "choroid plexus macrophage"@en .
obo:CL_0009004 a owl:Class ; rdfs:subClassOf obo:CL_0009001 ; rdfs:label "perivascular macrophage"@en .
obo:CL_0009005 a owl:Class ; rdfs:subClassOf obo:CL_0009004 ; rdfs:label "pial perivascular macrophage"@en .
obo:CL_0009006 a owl:Class ; rdfs:label "antigen-presenting border cell"@en ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty obo:RO_0002202 ; owl:someValuesFrom obo:CL_0009001 ] .
obo:CL_0009007 a owl:Class ; rdfs:label "dural dendritic cell"@en ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty obo:RO_0002202 ; owl:someValuesFrom obo:CL_0009001 ] .
obo:CL_0009008 a owl:Class ; rdfs:label "leptomeningeal cell"@en ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty obo:BFO_0000050 ; owl:someValuesFrom obo:CL_0009001 ] .
obo:CL_0009009 a owl:Class ; rdfs:label "choroid plexus epithelial cell"@en ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty obo:RO_0002215 ; owl:someValuesFrom obo:CL_0009001 ] .
`;

// --- terms ---------------------------------------------------------------------------

const nn = (v) => ox.namedNode(v);
const key = (q) => `${q.subject} ${q.predicate} ${q.object} ${q.graph}`;
const isIri = (t) => t?.termType === 'NamedNode';
const isBlank = (t) => t?.termType === 'BlankNode';

function litJson(t) {
  const o = { '@value': t.value };
  if (t.language) o['@language'] = t.language;
  else if (t.datatype && t.datatype.value !== XSD + 'string') o['@type'] = t.datatype.value;
  return o;
}

function litTerm(v) {
  if (typeof v === 'string') return ox.literal(v);
  if (v['@language']) return ox.literal(String(v['@value']), v['@language']);
  if (v['@type']) return ox.literal(String(v['@value']), nn(v['@type']));
  return ox.literal(String(v['@value']));
}

const isLit = (v) => v && typeof v === 'object' && '@value' in v;

/** A stable key of a JSON axiom or expression: members sorted, sets sorted. */
const SETS = new Set([
  'classes',
  'properties',
  'individuals',
  'ranges',
  'values',
  'objectProperties',
  'dataProperties',
]);
function canon(v, member = '') {
  if (Array.isArray(v)) {
    const xs = v.map((x) => canon(x));
    return SETS.has(member) ? [...xs].sort() : xs;
  }
  if (v && typeof v === 'object') {
    const ks = Object.keys(v).sort();
    const parts = ks.map((k) => `${JSON.stringify(k)}:${JSON.stringify(canon(v[k], k))}`);
    if (member === 'annotations') return `{${parts.join(',')}}`;
    return `{${parts.join(',')}}`;
  }
  return v;
}
const axiomKey = (a) => {
  const { annotations, ...rest } = a;
  const anns = (annotations ?? []).map((x) => JSON.stringify(canon(x))).sort();
  return JSON.stringify(canon(rest)) + '|' + anns.join(',');
};

/** Whether a JSON value mentions an IRI. */
function mentions(v, iri) {
  if (typeof v === 'string') return v === iri;
  if (Array.isArray(v)) return v.some((x) => mentions(x, iri));
  if (v && typeof v === 'object') return Object.values(v).some((x) => mentions(x, iri));
  return false;
}

// --- reading -------------------------------------------------------------------------

const DECLARED = {
  [OWL + 'Class']: 'Class',
  [RDFS + 'Class']: 'Class',
  [OWL + 'ObjectProperty']: 'ObjectProperty',
  [OWL + 'DatatypeProperty']: 'DataProperty',
  [OWL + 'AnnotationProperty']: 'AnnotationProperty',
  [OWL + 'NamedIndividual']: 'NamedIndividual',
  [RDFS + 'Datatype']: 'Datatype',
};
const KIND_TYPE = {
  Class: OWL + 'Class',
  ObjectProperty: OWL + 'ObjectProperty',
  DataProperty: OWL + 'DatatypeProperty',
  AnnotationProperty: OWL + 'AnnotationProperty',
  NamedIndividual: OWL + 'NamedIndividual',
  Datatype: RDFS + 'Datatype',
};
const CHARACTERISTIC = {
  [OWL + 'FunctionalProperty']: 'Functional',
  [OWL + 'InverseFunctionalProperty']: 'InverseFunctionalObjectProperty',
  [OWL + 'TransitiveProperty']: 'TransitiveObjectProperty',
  [OWL + 'SymmetricProperty']: 'SymmetricObjectProperty',
  [OWL + 'AsymmetricProperty']: 'AsymmetricObjectProperty',
  [OWL + 'ReflexiveProperty']: 'ReflexiveObjectProperty',
  [OWL + 'IrreflexiveProperty']: 'IrreflexiveObjectProperty',
};
const CHARACTERISTIC_TYPE = Object.fromEntries(
  Object.entries(CHARACTERISTIC)
    .filter(([, v]) => v !== 'Functional')
    .map(([k, v]) => [v, k]),
);
const BUILTIN_ANNOTATIONS = new Set([
  RDFS + 'label',
  RDFS + 'comment',
  RDFS + 'seeAlso',
  RDFS + 'isDefinedBy',
  OWL + 'versionInfo',
  OWL + 'deprecated',
  OWL + 'priorVersion',
  SKOS + 'prefLabel',
  SKOS + 'altLabel',
  SKOS + 'definition',
  SKOS + 'note',
  DCT + 'creator',
  DCT + 'title',
  DCT + 'description',
]);
const BUILTIN_CLASSES = new Set([OWL + 'Thing', OWL + 'Nothing']);
/**
 * A tree for the Workbench's layout tests (ROADMAP item 633): a chain 20 levels deep with
 * long labels, and a class with more children than one level of the hierarchy route reads.
 */
export const DEEP_PREFIXES = {
  rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
  rdfs: 'http://www.w3.org/2000/01/rdf-schema#',
  owl: 'http://www.w3.org/2002/07/owl#',
  deep: 'http://example.org/deep#',
};
export const DEEP_CHAIN = 20;
export const DEEP_WIDE = 1050;
export function deepTurtle() {
  const out = [
    '@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .',
    '@prefix owl: <http://www.w3.org/2002/07/owl#> .',
    '@prefix deep: <http://example.org/deep#> .',
    '<http://example.org/deep> a owl:Ontology ; rdfs:label "Deep ontology"@en .',
  ];
  for (let i = 0; i < DEEP_CHAIN; i++) {
    const sup = i ? ` ; rdfs:subClassOf deep:C${i - 1}` : '';
    out.push(
      `deep:C${i} a owl:Class${sup} ; rdfs:label "level ${i} of a deep chain of nested anatomical structures"@en .`,
    );
  }
  out.push('deep:Wide a owl:Class ; rdfs:label "wide parent"@en .');
  for (let i = 0; i < DEEP_WIDE; i++) {
    const n = String(i).padStart(4, '0');
    out.push(
      `deep:W${n} a owl:Class ; rdfs:subClassOf deep:Wide ; rdfs:label "wide child ${n}"@en .`,
    );
  }
  return out.join('\n') + '\n';
}

export const ASSERTION_TYPES = new Set([
  'ClassAssertion',
  'ObjectPropertyAssertion',
  'DataPropertyAssertion',
  'NegativeObjectPropertyAssertion',
  'NegativeDataPropertyAssertion',
  'SameIndividual',
  'DifferentIndividuals',
]);

const graphTerm = (g) => (!g || g === 'default' ? ox.defaultGraph() : nn(g));

/** The ontology of one graph of `ds`, read as the mock's mapping reads it. */
export function readOntology(ds, graph) {
  const g = graphTerm(graph);
  const quads = ds.store.match(null, null, null, g);
  const bySubject = new Map();
  for (const q of quads) {
    const k = q.subject.toString();
    if (!bySubject.has(k)) bySubject.set(k, []);
    bySubject.get(k).push(q);
  }
  const consumed = new Set();
  const kinds = new Map();
  const addKind = (iri, k) => {
    if (!kinds.has(iri)) kinds.set(iri, new Set());
    kinds.get(iri).add(k);
  };
  const axioms = [];
  const push = (json, triples, main) => {
    for (const t of triples) consumed.add(key(t));
    axioms.push({ json, triples: new Set(triples.map(key)), quads: triples, main });
  };
  // the header
  const header = { iri: null, versionIri: null, imports: [], annotations: [], node: null };
  const ontQ = quads.find((q) => q.predicate.value === TYPE && q.object.value === OWL + 'Ontology');
  if (ontQ) {
    header.node = ontQ.subject;
    header.iri = isIri(ontQ.subject) ? ontQ.subject.value : null;
    for (const q of bySubject.get(ontQ.subject.toString()) ?? []) {
      consumed.add(key(q));
      if (q.predicate.value === TYPE) continue;
      if (q.predicate.value === OWL + 'versionIRI') header.versionIri = q.object.value;
      else if (q.predicate.value === OWL + 'imports') header.imports.push(q.object.value);
      else header.annotations.push({ property: q.predicate.value, value: q.object });
    }
  }
  // declarations and characteristics
  for (const q of quads) {
    if (q.predicate.value !== TYPE || !isIri(q.subject)) continue;
    const k = DECLARED[q.object.value];
    if (k) {
      addKind(q.subject.value, k);
      push({ type: 'Declaration', kind: k, iri: q.subject.value }, [q], q);
    }
  }
  const kindOf = (iri) => kinds.get(iri) ?? new Set();
  const isData = (iri) => kindOf(iri).has('DataProperty');
  const isAnnotation = (iri) =>
    kindOf(iri).has('AnnotationProperty') || BUILTIN_ANNOTATIONS.has(iri);
  for (const q of quads) {
    if (q.predicate.value !== TYPE || !isIri(q.subject)) continue;
    const c = CHARACTERISTIC[q.object.value];
    if (!c) continue;
    const t =
      c === 'Functional'
        ? isData(q.subject.value)
          ? 'FunctionalDataProperty'
          : 'FunctionalObjectProperty'
        : c;
    push({ type: t, property: q.subject.value }, [q], q);
  }
  // blank node structures
  const listOf = (head, used) => {
    const out = [];
    let cur = head;
    let guard = 0;
    while (cur && !(isIri(cur) && cur.value === RDF + 'nil') && guard++ < 1000) {
      const cell = bySubject.get(cur.toString()) ?? [];
      const first = cell.find((q) => q.predicate.value === RDF + 'first');
      const rest = cell.find((q) => q.predicate.value === RDF + 'rest');
      if (!first || !rest) return null;
      used.push(first, rest);
      out.push(first.object);
      cur = rest.object;
    }
    return out;
  };
  const ce = (t, used, depth = 0) => {
    if (isIri(t)) return t.value;
    if (!isBlank(t) || depth > 64) return null;
    const qs = bySubject.get(t.toString()) ?? [];
    const one = (p) => qs.find((q) => q.predicate.value === p);
    const list = (p, f) => {
      const q = one(p);
      if (!q) return null;
      used.push(q);
      const items = listOf(q.object, used);
      return items ? items.map(f) : null;
    };
    const sub = (x) => ce(x, used, depth + 1);
    for (const [p, type] of [
      [OWL + 'intersectionOf', 'ObjectIntersectionOf'],
      [OWL + 'unionOf', 'ObjectUnionOf'],
    ]) {
      const xs = list(p, sub);
      if (xs) {
        const tq = one(TYPE);
        if (tq) used.push(tq);
        return { type, classes: xs };
      }
    }
    if (one(OWL + 'complementOf')) {
      const q = one(OWL + 'complementOf');
      used.push(q);
      return { type: 'ObjectComplementOf', class: sub(q.object) };
    }
    if (one(OWL + 'oneOf')) {
      const xs = list(OWL + 'oneOf', (x) => x.value);
      return { type: 'ObjectOneOf', individuals: xs };
    }
    const on = one(OWL + 'onProperty');
    if (!on) return null;
    used.push(on);
    const tq = one(TYPE);
    if (tq) used.push(tq);
    const p = on.object.value;
    const data = isData(p);
    const take = (pred) => {
      const q = one(pred);
      if (q) used.push(q);
      return q;
    };
    const filler = (q) => (data ? q.object.value : sub(q.object));
    let q;
    if ((q = take(OWL + 'someValuesFrom')))
      return data
        ? { type: 'DataSomeValuesFrom', property: p, range: q.object.value }
        : { type: 'ObjectSomeValuesFrom', property: p, class: filler(q) };
    if ((q = take(OWL + 'allValuesFrom')))
      return data
        ? { type: 'DataAllValuesFrom', property: p, range: q.object.value }
        : { type: 'ObjectAllValuesFrom', property: p, class: filler(q) };
    if ((q = take(OWL + 'hasValue')))
      return data
        ? { type: 'DataHasValue', property: p, value: litJson(q.object) }
        : { type: 'ObjectHasValue', property: p, individual: q.object.value };
    for (const [pred, word, qualified] of [
      [OWL + 'minCardinality', 'Min', false],
      [OWL + 'maxCardinality', 'Max', false],
      [OWL + 'cardinality', 'Exact', false],
      [OWL + 'minQualifiedCardinality', 'Min', true],
      [OWL + 'maxQualifiedCardinality', 'Max', true],
      [OWL + 'qualifiedCardinality', 'Exact', true],
    ]) {
      if ((q = take(pred))) {
        const out = {
          type: `${data ? 'Data' : 'Object'}${word}Cardinality`,
          property: p,
          cardinality: Number(q.object.value),
        };
        if (qualified) {
          const f = take(OWL + 'onClass') ?? take(OWL + 'onDataRange');
          if (f) out[data ? 'range' : 'class'] = data ? f.object.value : sub(f.object);
        }
        return out;
      }
    }
    return null;
  };
  const mainAxiom = (q) => {
    const s = q.subject;
    const p = q.predicate.value;
    const o = q.object;
    const used = [q];
    const both = () => [ce(s, used), ce(o, used)];
    if (p === RDFS + 'subClassOf') {
      const [a, b] = both();
      return a && b ? [{ type: 'SubClassOf', subClass: a, superClass: b }, used] : null;
    }
    if (p === OWL + 'equivalentClass') {
      const [a, b] = both();
      return a && b ? [{ type: 'EquivalentClasses', classes: [a, b] }, used] : null;
    }
    if (p === OWL + 'disjointWith') {
      const [a, b] = both();
      return a && b ? [{ type: 'DisjointClasses', classes: [a, b] }, used] : null;
    }
    if (!isIri(s)) return null;
    const sk = kindOf(s.value);
    if (p === RDFS + 'subPropertyOf' && isIri(o)) {
      const t = sk.has('DataProperty')
        ? 'SubDataPropertyOf'
        : sk.has('ObjectProperty')
          ? 'SubObjectPropertyOf'
          : 'SubAnnotationPropertyOf';
      return [{ type: t, subProperty: s.value, superProperty: o.value }, used];
    }
    if (p === OWL + 'propertyChainAxiom') {
      const items = listOf(o, used);
      return items
        ? [
            {
              type: 'SubObjectPropertyOf',
              chain: items.map((x) => x.value),
              superProperty: s.value,
            },
            used,
          ]
        : null;
    }
    if (p === RDFS + 'domain' || p === RDFS + 'range') {
      const which = p === RDFS + 'domain' ? 'domain' : 'range';
      if (sk.has('AnnotationProperty') && isIri(o))
        return [
          {
            type: which === 'domain' ? 'AnnotationPropertyDomain' : 'AnnotationPropertyRange',
            property: s.value,
            [which]: o.value,
          },
          used,
        ];
      if (sk.has('DataProperty'))
        return [
          which === 'domain'
            ? { type: 'DataPropertyDomain', property: s.value, domain: ce(o, used) }
            : { type: 'DataPropertyRange', property: s.value, range: o.value },
          used,
        ];
      const c = ce(o, used);
      return c
        ? [
            {
              type: which === 'domain' ? 'ObjectPropertyDomain' : 'ObjectPropertyRange',
              property: s.value,
              [which]: c,
            },
            used,
          ]
        : null;
    }
    if (p === OWL + 'inverseOf' && isIri(o))
      return [{ type: 'InverseObjectProperties', first: s.value, second: o.value }, used];
    if (p === OWL + 'equivalentProperty' && isIri(o))
      return [
        {
          type: isData(s.value) ? 'EquivalentDataProperties' : 'EquivalentObjectProperties',
          properties: [s.value, o.value],
        },
        used,
      ];
    if (p === OWL + 'propertyDisjointWith' && isIri(o))
      return [
        {
          type: isData(s.value) ? 'DisjointDataProperties' : 'DisjointObjectProperties',
          properties: [s.value, o.value],
        },
        used,
      ];
    if (p === TYPE) {
      if (DECLARED[o.value] || CHARACTERISTIC[o.value] || o.value.startsWith(OWL)) return null;
      const c = ce(o, used);
      return c ? [{ type: 'ClassAssertion', class: c, individual: s.value }, used] : null;
    }
    if (p === OWL + 'sameAs' && isIri(o))
      return [{ type: 'SameIndividual', individuals: [s.value, o.value] }, used];
    if (p === OWL + 'differentFrom' && isIri(o))
      return [{ type: 'DifferentIndividuals', individuals: [s.value, o.value] }, used];
    if (isAnnotation(p))
      return [
        {
          type: 'AnnotationAssertion',
          property: p,
          subject: s.value,
          value: isIri(o) ? o.value : litJson(o),
        },
        used,
      ];
    if (kindOf(p).has('ObjectProperty') && isIri(o))
      return [
        { type: 'ObjectPropertyAssertion', property: p, source: s.value, target: o.value },
        used,
      ];
    if (kindOf(p).has('DataProperty') && o.termType === 'Literal')
      return [
        { type: 'DataPropertyAssertion', property: p, source: s.value, value: litJson(o) },
        used,
      ];
    return null;
  };
  // the reified annotations of axioms: `owl:Axiom` nodes, by their main triple
  const reified = new Map();
  for (const q of quads) {
    if (q.predicate.value !== TYPE || q.object.value !== OWL + 'Axiom' || !isBlank(q.subject))
      continue;
    const qs = bySubject.get(q.subject.toString()) ?? [];
    const get = (p) => qs.find((x) => x.predicate.value === p)?.object;
    const [s, p, o] = [
      get(OWL + 'annotatedSource'),
      get(OWL + 'annotatedProperty'),
      get(OWL + 'annotatedTarget'),
    ];
    if (!s || !p || !o) continue;
    const anns = qs
      .filter(
        (x) =>
          ![
            TYPE,
            OWL + 'annotatedSource',
            OWL + 'annotatedProperty',
            OWL + 'annotatedTarget',
          ].includes(x.predicate.value),
      )
      .map((x) => ({
        property: x.predicate.value,
        value: isIri(x.object) ? x.object.value : litJson(x.object),
      }));
    const mk = `${s} ${p} ${o}`;
    if (!reified.has(mk)) reified.set(mk, []);
    reified.get(mk).push({ quads: qs, anns });
  }
  const structural = new Set();
  for (const q of quads) {
    // a blank node that something points at is part of a structure, not a main subject
    if (isBlank(q.object)) structural.add(q.object.toString());
  }
  for (const q of quads) {
    if (consumed.has(key(q))) continue;
    if (isBlank(q.subject) && (structural.has(q.subject.toString()) || reifiedNode(q, bySubject)))
      continue;
    const hit = mainAxiom(q);
    if (!hit) continue;
    const [json, used] = hit;
    const mk = `${q.subject} ${q.predicate} ${q.object}`;
    const annotated = reified.get(mk) ?? [];
    // an axiom with annotations is its reified form, which holds the main triple too
    if (!annotated.length) push({ ...json }, used, q);
    for (const r of annotated)
      push({ ...json, annotations: r.anns }, [q, ...used.slice(1), ...r.quads], q);
  }
  // entities used without a declaration, typed by their use
  for (const a of axioms) {
    const j = a.json;
    if (j.type === 'ClassAssertion' && typeof j.class === 'string') {
      addKind(j.class, 'Class');
      if (!kinds.has(j.individual)) addKind(j.individual, 'NamedIndividual');
    }
    if (j.type === 'SubClassOf')
      for (const x of [j.subClass, j.superClass])
        if (typeof x === 'string' && !BUILTIN_CLASSES.has(x) && !kinds.has(x)) addKind(x, 'Class');
  }
  const unmapped = quads.filter((q) => !consumed.has(key(q))).length;
  return { graph: graph ?? 'default', header, axioms, kinds, quads: quads.length, unmapped };
}

function reifiedNode(q, bySubject) {
  return (bySubject.get(q.subject.toString()) ?? []).some(
    (x) => x.predicate.value === TYPE && x.object.value === OWL + 'Axiom',
  );
}

// --- writing -------------------------------------------------------------------------

/** The triples of a JSON axiom by the forward mapping, with fresh blank nodes. */
function axiomTriples(a, g) {
  const out = [];
  const t = (s, p, o) => out.push(ox.quad(s, typeof p === 'string' ? nn(p) : p, o, g));
  const term = (x) =>
    typeof x === 'string' ? (x.startsWith('_:') ? ox.blankNode(x.slice(2)) : nn(x)) : litTerm(x);
  const list = (xs) => {
    let head = nn(RDF + 'nil');
    for (const x of [...xs].reverse()) {
      const cell = ox.blankNode();
      t(cell, RDF + 'first', x);
      t(cell, RDF + 'rest', head);
      head = cell;
    }
    return head;
  };
  const ce = (e) => {
    if (typeof e === 'string') return nn(e);
    const b = ox.blankNode();
    const restriction = (prop) => {
      t(b, TYPE, nn(OWL + 'Restriction'));
      t(b, OWL + 'onProperty', nn(prop));
    };
    switch (e.type) {
      case 'ObjectIntersectionOf':
      case 'ObjectUnionOf':
        t(b, TYPE, nn(OWL + 'Class'));
        t(
          b,
          e.type === 'ObjectIntersectionOf' ? OWL + 'intersectionOf' : OWL + 'unionOf',
          list(e.classes.map(ce)),
        );
        return b;
      case 'ObjectComplementOf':
        t(b, TYPE, nn(OWL + 'Class'));
        t(b, OWL + 'complementOf', ce(e.class));
        return b;
      case 'ObjectOneOf':
        t(b, TYPE, nn(OWL + 'Class'));
        t(b, OWL + 'oneOf', list(e.individuals.map(nn)));
        return b;
      case 'ObjectSomeValuesFrom':
      case 'ObjectAllValuesFrom':
        restriction(e.property);
        t(
          b,
          e.type === 'ObjectSomeValuesFrom' ? OWL + 'someValuesFrom' : OWL + 'allValuesFrom',
          ce(e.class),
        );
        return b;
      case 'DataSomeValuesFrom':
      case 'DataAllValuesFrom':
        restriction(e.property);
        t(
          b,
          e.type === 'DataSomeValuesFrom' ? OWL + 'someValuesFrom' : OWL + 'allValuesFrom',
          nn(e.range),
        );
        return b;
      case 'ObjectHasValue':
        restriction(e.property);
        t(b, OWL + 'hasValue', nn(e.individual));
        return b;
      case 'DataHasValue':
        restriction(e.property);
        t(b, OWL + 'hasValue', litTerm(e.value));
        return b;
      default: {
        const m = /^(Object|Data)(Min|Max|Exact)Cardinality$/.exec(e.type ?? '');
        if (!m) throw new Error(`the mock cannot write ${e.type}`);
        restriction(e.property);
        const filler = e.class ?? e.range;
        const pred = { Min: 'minCardinality', Max: 'maxCardinality', Exact: 'cardinality' }[m[2]];
        const q = {
          Min: 'minQualifiedCardinality',
          Max: 'maxQualifiedCardinality',
          Exact: 'qualifiedCardinality',
        }[m[2]];
        t(
          b,
          OWL + (filler ? q : pred),
          ox.literal(String(e.cardinality), nn(XSD + 'nonNegativeInteger')),
        );
        if (filler)
          t(
            b,
            m[1] === 'Object' ? OWL + 'onClass' : OWL + 'onDataRange',
            m[1] === 'Object' ? ce(filler) : nn(filler),
          );
        return b;
      }
    }
  };
  let main = null;
  const m = (s, p, o) => {
    main = [s, nn(p), o];
    t(s, p, o);
  };
  switch (a.type) {
    case 'Declaration':
      m(nn(a.iri), TYPE, nn(KIND_TYPE[a.kind]));
      break;
    case 'SubClassOf':
      m(ce(a.subClass), RDFS + 'subClassOf', ce(a.superClass));
      break;
    case 'EquivalentClasses':
    case 'DisjointClasses': {
      const [x, y] = a.classes;
      if (a.classes.length !== 2)
        throw new Error('the mock writes equivalences and disjointness of two classes');
      m(
        ce(x),
        a.type === 'EquivalentClasses' ? OWL + 'equivalentClass' : OWL + 'disjointWith',
        ce(y),
      );
      break;
    }
    case 'SubObjectPropertyOf':
    case 'SubDataPropertyOf':
    case 'SubAnnotationPropertyOf':
      if (a.chain) m(nn(a.superProperty), OWL + 'propertyChainAxiom', list(a.chain.map(nn)));
      else m(nn(a.subProperty), RDFS + 'subPropertyOf', nn(a.superProperty));
      break;
    case 'ObjectPropertyDomain':
    case 'DataPropertyDomain':
      m(nn(a.property), RDFS + 'domain', ce(a.domain));
      break;
    case 'ObjectPropertyRange':
      m(nn(a.property), RDFS + 'range', ce(a.range));
      break;
    case 'DataPropertyRange':
    case 'AnnotationPropertyRange':
      m(nn(a.property), RDFS + 'range', nn(a.range));
      break;
    case 'AnnotationPropertyDomain':
      m(nn(a.property), RDFS + 'domain', nn(a.domain));
      break;
    case 'InverseObjectProperties':
      m(nn(a.first), OWL + 'inverseOf', nn(a.second));
      break;
    case 'EquivalentObjectProperties':
    case 'EquivalentDataProperties':
      m(nn(a.properties[0]), OWL + 'equivalentProperty', nn(a.properties[1]));
      break;
    case 'DisjointObjectProperties':
    case 'DisjointDataProperties':
      m(nn(a.properties[0]), OWL + 'propertyDisjointWith', nn(a.properties[1]));
      break;
    case 'FunctionalObjectProperty':
    case 'FunctionalDataProperty':
      m(nn(a.property), TYPE, nn(OWL + 'FunctionalProperty'));
      break;
    case 'ClassAssertion':
      m(term(a.individual), TYPE, ce(a.class));
      break;
    case 'ObjectPropertyAssertion':
      m(term(a.source), a.property, term(a.target));
      break;
    case 'DataPropertyAssertion':
      m(term(a.source), a.property, litTerm(a.value));
      break;
    case 'SameIndividual':
      m(nn(a.individuals[0]), OWL + 'sameAs', nn(a.individuals[1]));
      break;
    case 'DifferentIndividuals':
      m(nn(a.individuals[0]), OWL + 'differentFrom', nn(a.individuals[1]));
      break;
    case 'AnnotationAssertion':
      m(term(a.subject), a.property, term(a.value));
      break;
    default:
      if (CHARACTERISTIC_TYPE[a.type]) {
        m(nn(a.property), TYPE, nn(CHARACTERISTIC_TYPE[a.type]));
        break;
      }
      throw new Error(`the mock cannot write ${a.type}`);
  }
  if (a.annotations?.length) {
    const x = ox.blankNode();
    t(x, TYPE, nn(OWL + 'Axiom'));
    t(x, OWL + 'annotatedSource', main[0]);
    t(x, OWL + 'annotatedProperty', main[1]);
    t(x, OWL + 'annotatedTarget', main[2]);
    for (const an of a.annotations) t(x, an.property, term(an.value));
  }
  return out;
}

/** The entities an axiom uses, with the kind its position gives them. */
function signature(a) {
  const out = [];
  const cls = (e) => {
    if (typeof e === 'string') return void out.push(['Class', e]);
    if (!e || typeof e !== 'object') return;
    if (e.property && typeof e.property === 'string')
      out.push([e.type?.startsWith('Data') ? 'DataProperty' : 'ObjectProperty', e.property]);
    for (const c of e.classes ?? []) cls(c);
    if (e.class) cls(e.class);
  };
  const t = a.type;
  if (['SubClassOf'].includes(t)) [a.subClass, a.superClass].forEach(cls);
  if (['EquivalentClasses', 'DisjointClasses'].includes(t)) a.classes.forEach(cls);
  if (t === 'ClassAssertion') cls(a.class);
  if (
    [
      'SubObjectPropertyOf',
      'InverseObjectProperties',
      'EquivalentObjectProperties',
      'DisjointObjectProperties',
    ].includes(t)
  )
    for (const p of [
      a.subProperty,
      a.superProperty,
      a.first,
      a.second,
      ...(a.properties ?? []),
      ...(a.chain ?? []),
    ])
      if (typeof p === 'string') out.push(['ObjectProperty', p]);
  if (['SubDataPropertyOf', 'EquivalentDataProperties', 'DisjointDataProperties'].includes(t))
    for (const p of [a.subProperty, a.superProperty, ...(a.properties ?? [])])
      if (typeof p === 'string') out.push(['DataProperty', p]);
  if (t === 'SubAnnotationPropertyOf')
    out.push(['AnnotationProperty', a.subProperty], ['AnnotationProperty', a.superProperty]);
  if (t.startsWith('ObjectProperty')) {
    out.push(['ObjectProperty', a.property]);
    cls(a.domain ?? a.range);
  }
  if (t.startsWith('DataProperty') && t !== 'DataPropertyAssertion') {
    out.push(['DataProperty', a.property]);
    if (a.domain) cls(a.domain);
  }
  if (t === 'ObjectPropertyAssertion') out.push(['ObjectProperty', a.property]);
  if (t === 'DataPropertyAssertion') out.push(['DataProperty', a.property]);
  // an annotation assertion declares its property, as the server's edits do, so that the
  // triple reads back as an annotation assertion that a later edit can remove
  if (t === 'AnnotationAssertion') out.push(['AnnotationProperty', a.property]);
  return out.filter(
    ([, iri]) =>
      typeof iri === 'string' &&
      !iri.startsWith(OWL) &&
      !iri.startsWith(XSD) &&
      !iri.startsWith(RDFS),
  );
}

// --- functional-style syntax -----------------------------------------------------------

function tokenize(s) {
  const out = [];
  const re =
    /\s*(?:(<[^>]*>)|("(?:[^"\\]|\\.)*"(?:@[A-Za-z0-9-]+|\^\^(?:<[^>]*>|[A-Za-z][\w.-]*:[\w.-]*))?)|([()])|([^\s()"<>]+))/gy;
  let m;
  let at = 0;
  while (at < s.trimEnd().length && (m = re.exec(s)) && m[0].length) {
    out.push(m[1] ?? m[2] ?? m[3] ?? m[4]);
    at = re.lastIndex;
  }
  if (at < s.trimEnd().length) throw new Error(`cannot read the axiom at ${at}`);
  return out;
}

function parseOfn(s, prefixes) {
  const toks = tokenize(s);
  let i = 0;
  const name = (tok) => {
    if (tok.startsWith('<')) return tok.slice(1, -1);
    const c = tok.indexOf(':');
    const ns = c >= 0 ? prefixes[tok.slice(0, c)] : undefined;
    if (ns === undefined) throw new Error(`unknown prefix in ${tok}`);
    return ns + tok.slice(c + 1);
  };
  const lit = (tok) => {
    const m = /^"((?:[^"\\]|\\.)*)"(?:@([A-Za-z0-9-]+)|\^\^(.+))?$/.exec(tok);
    const value = JSON.parse(`"${m[1]}"`);
    if (m[2]) return { '@value': value, '@language': m[2] };
    if (m[3]) return { '@value': value, '@type': name(m[3]) };
    return { '@value': value };
  };
  const node = () => {
    const tok = toks[i++];
    if (tok === undefined) throw new Error('the axiom ends early');
    if (tok.startsWith('"')) return lit(tok);
    if (/^\d+$/.test(tok)) return Number(tok);
    if (toks[i] === '(') {
      i++;
      const args = [];
      while (toks[i] !== ')') {
        if (i >= toks.length) throw new Error('a parenthesis is not closed');
        args.push(node());
      }
      i++;
      return { f: tok, args };
    }
    return name(tok);
  };
  const tree = node();
  if (i !== toks.length) throw new Error('text follows the axiom');
  const expr = (n) => {
    if (typeof n === 'string' || typeof n === 'number' || isLit(n)) return n;
    const a = n.args;
    switch (n.f) {
      case 'ObjectInverseOf':
        return { inverseOf: a[0] };
      case 'ObjectIntersectionOf':
      case 'ObjectUnionOf':
        return { type: n.f, classes: a.map(expr) };
      case 'ObjectComplementOf':
        return { type: n.f, class: expr(a[0]) };
      case 'ObjectOneOf':
        return { type: n.f, individuals: a };
      case 'ObjectSomeValuesFrom':
      case 'ObjectAllValuesFrom':
        return { type: n.f, property: expr(a[0]), class: expr(a[1]) };
      case 'ObjectHasValue':
        return { type: n.f, property: expr(a[0]), individual: a[1] };
      case 'DataSomeValuesFrom':
      case 'DataAllValuesFrom':
        return { type: n.f, property: a[0], range: a[1] };
      case 'DataHasValue':
        return { type: n.f, property: a[0], value: a[1] };
      case 'ObjectMinCardinality':
      case 'ObjectMaxCardinality':
      case 'ObjectExactCardinality':
        return {
          type: n.f,
          cardinality: a[0],
          property: expr(a[1]),
          ...(a[2] ? { class: expr(a[2]) } : {}),
        };
      case 'DataMinCardinality':
      case 'DataMaxCardinality':
      case 'DataExactCardinality':
        return { type: n.f, cardinality: a[0], property: a[1], ...(a[2] ? { range: a[2] } : {}) };
    }
    throw new Error(`the mock does not read ${n.f}`);
  };
  const a = tree.args;
  switch (tree.f) {
    case 'SubClassOf':
      return { type: 'SubClassOf', subClass: expr(a[0]), superClass: expr(a[1]) };
    case 'EquivalentClasses':
    case 'DisjointClasses':
      return { type: tree.f, classes: a.map(expr) };
    case 'ClassAssertion':
      return { type: 'ClassAssertion', class: expr(a[0]), individual: a[1] };
    case 'ObjectPropertyDomain':
    case 'DataPropertyDomain':
      return { type: tree.f, property: a[0], domain: expr(a[1]) };
    case 'ObjectPropertyRange':
      return { type: tree.f, property: a[0], range: expr(a[1]) };
    case 'DataPropertyRange':
      return { type: tree.f, property: a[0], range: a[1] };
  }
  throw new Error(`the mock does not read ${tree?.f ?? 'this'} axiom`);
}

// --- functional-style rendering --------------------------------------------------------

function renderer(prefixes) {
  const entries = Object.entries(prefixes).sort((a, b) => b[1].length - a[1].length);
  const iri = (v) => {
    if (v.startsWith('_:')) return v;
    for (const [p, ns] of entries) {
      if (v.startsWith(ns) && /^[A-Za-z_][\w.-]*$|^$/.test(v.slice(ns.length)))
        return `${p}:${v.slice(ns.length)}`;
    }
    return `<${v}>`;
  };
  const lit = (l) => {
    const v = JSON.stringify(l['@value']);
    if (l['@language']) return `${v}@${l['@language']}`;
    if (l['@type']) return `${v}^^${iri(l['@type'])}`;
    return v;
  };
  const val = (x) =>
    typeof x === 'string'
      ? iri(x)
      : isLit(x)
        ? lit(x)
        : x?.inverseOf
          ? `ObjectInverseOf(${iri(x.inverseOf)})`
          : e(x);
  const e = (x) => {
    if (typeof x === 'string' || isLit(x) || x?.inverseOf) return val(x);
    const f = x.type;
    const args = [];
    if (/Cardinality$/.test(f)) args.push(String(x.cardinality));
    for (const k of [
      'property',
      'class',
      'classes',
      'individual',
      'individuals',
      'range',
      'value',
    ]) {
      if (x[k] === undefined) continue;
      if (Array.isArray(x[k])) args.push(...x[k].map(val));
      else args.push(val(x[k]));
    }
    return `${f}(${args.join(' ')})`;
  };
  const anns = (a) =>
    (a.annotations ?? []).map((x) => `Annotation(${iri(x.property)} ${val(x.value)}) `).join('');
  const axiom = (a) => {
    const t = a.type;
    const parts = (() => {
      switch (t) {
        case 'Declaration':
          return [`${a.kind}(${iri(a.iri)})`];
        case 'SubClassOf':
          return [val(a.subClass), val(a.superClass)];
        case 'SubObjectPropertyOf':
        case 'SubDataPropertyOf':
        case 'SubAnnotationPropertyOf':
          return [
            a.chain ? `ObjectPropertyChain(${a.chain.map(val).join(' ')})` : val(a.subProperty),
            val(a.superProperty),
          ];
        case 'InverseObjectProperties':
          return [val(a.first), val(a.second)];
        case 'ClassAssertion':
          return [val(a.class), val(a.individual)];
        case 'ObjectPropertyAssertion':
          return [val(a.property), val(a.source), val(a.target)];
        case 'DataPropertyAssertion':
          return [val(a.property), val(a.source), val(a.value)];
        case 'AnnotationAssertion':
          return [val(a.property), val(a.subject), val(a.value)];
        default:
          return [
            a.property,
            a.domain,
            a.range,
            ...(a.classes ?? []),
            ...(a.properties ?? []),
            ...(a.individuals ?? []),
          ]
            .filter((x) => x !== undefined)
            .map(val);
      }
    })();
    return `${t}(${anns(a)}${parts.join(' ')})`;
  };
  return { axiom, entity: (k, i) => `${k}(${iri(i)})`, iri, val };
}

// --- classification --------------------------------------------------------------------

function classify(o) {
  const classes = new Set([...o.kinds].filter(([, k]) => k.has('Class')).map(([i]) => i));
  const told = new Map([...classes].map((c) => [c, new Set()]));
  const some = new Map([...classes].map((c) => [c, []]));
  const defs = [];
  const equiv = [];
  for (const a of o.axioms) {
    const j = a.json;
    if (j.type === 'SubClassOf' && typeof j.subClass === 'string' && told.has(j.subClass)) {
      if (typeof j.superClass === 'string') told.get(j.subClass).add(j.superClass);
      else if (j.superClass.type === 'ObjectSomeValuesFrom')
        some.get(j.subClass).push(j.superClass);
    }
    if (j.type === 'EquivalentClasses') {
      const named = j.classes.filter((c) => typeof c === 'string');
      const exprs = j.classes.filter((c) => typeof c !== 'string');
      if (named.length >= 2) equiv.push(named);
      for (const d of named)
        for (const x of exprs)
          if (x.type === 'ObjectIntersectionOf') defs.push({ cls: d, parts: x.classes });
    }
  }
  for (const [a, b] of equiv.flatMap((ns) => ns.slice(1).map((n) => [ns[0], n]))) {
    told.get(a)?.add(b);
    told.get(b)?.add(a);
  }
  const closure = (c, seen = new Set()) => {
    for (const s of told.get(c) ?? [])
      if (!seen.has(s)) {
        seen.add(s);
        closure(s, seen);
      }
    return seen;
  };
  // a class is under a definition when its told superclasses and existentials meet every part
  let changed = true;
  while (changed) {
    changed = false;
    for (const c of classes) {
      const sup = closure(c);
      sup.add(c);
      const exist = [...sup].flatMap((x) => some.get(x) ?? []);
      for (const d of defs) {
        if (d.cls === c || told.get(c).has(d.cls)) continue;
        const ok = d.parts.every((p) =>
          typeof p === 'string'
            ? sup.has(p)
            : p.type === 'ObjectSomeValuesFrom' &&
              exist.some(
                (x) =>
                  x.property === p.property &&
                  (x.class === p.class || closure(x.class).has(p.class)),
              ),
        );
        if (ok) {
          told.get(c).add(d.cls);
          changed = true;
        }
      }
    }
  }
  // equivalence nodes: classes that reach each other
  const nodeOf = new Map();
  const nodes = [
    { node: 0, classes: [OWL + 'Thing'], parents: [], children: 0 },
    { node: 1, classes: [OWL + 'Nothing'], parents: [], children: 0 },
  ];
  for (const c of [...classes].sort()) {
    if (nodeOf.has(c)) continue;
    const sup = closure(c);
    const same = [c, ...[...sup].filter((x) => x !== c && closure(x).has(c))].sort();
    const n = { node: nodes.length, classes: same, parents: [], children: 0 };
    nodes.push(n);
    for (const x of same) nodeOf.set(x, n.node);
  }
  for (const n of nodes.slice(2)) {
    const c = n.classes[0];
    const sup = [...closure(c)].filter((x) => nodeOf.has(x) && nodeOf.get(x) !== n.node);
    const supNodes = [...new Set(sup.map((x) => nodeOf.get(x)))];
    const direct = supNodes.filter(
      (s) =>
        !supNodes.some((t) => t !== s && closure(nodes[t].classes[0]).has(nodes[s].classes[0])),
    );
    n.parents = direct.length ? direct : [0];
    for (const p of n.parents) nodes[p].children++;
  }
  return { nodes, classes: classes.size };
}

// --- Manchester syntax and the DL query --------------------------------------------------

const DL_QUERY = 'urn:x-sparkles:dl-query';

class ManchesterError extends Error {
  constructor(reason, message, start, end, candidates = []) {
    super(message);
    this.reason = reason;
    this.start = start;
    this.end = end;
    this.candidates = candidates;
  }
  json() {
    return {
      error: this.message,
      code: 'invalid-manchester',
      reason: this.reason,
      start: this.start,
      end: this.end,
      candidates: this.candidates,
    };
  }
}

/** The names of the ontology: labels, local names and prefixed names, each to its IRIs. */
function vocabulary(o, prefixes) {
  const names = new Map();
  const add = (n, iri) => {
    const k = n.toLowerCase();
    if (!names.has(k)) names.set(k, new Set());
    names.get(k).add(iri);
  };
  const label = new Map();
  for (const [iri] of o.kinds) {
    const local = iri.replace(/^.*[#/]/, '');
    if (local) add(local, iri);
  }
  for (const a of o.axioms)
    if (
      a.json.type === 'AnnotationAssertion' &&
      a.json.property === RDFS + 'label' &&
      typeof a.json.subject === 'string' &&
      isLit(a.json.value)
    ) {
      add(a.json.value['@value'], a.json.subject);
      // the label a name shows: an English one before the others
      const en = /^en\b/i.test(a.json.value['@language'] ?? '');
      if (!label.has(a.json.subject) || (en && !label.get(a.json.subject).en))
        label.set(a.json.subject, { text: a.json.value['@value'], en });
    }
  const expand = (n) => {
    const i = n.indexOf(':');
    if (i < 0) return null;
    const ns = prefixes[n.slice(0, i)];
    return ns === undefined ? null : ns + n.slice(i + 1);
  };
  return {
    names,
    label: new Map([...label].map(([k, l]) => [k, l.text])),
    expand,
    kinds: o.kinds,
  };
}

/** Manchester tokens with their spans: names, quoted names, IRIs and punctuation. */
function mTokens(text) {
  const out = [];
  const re = /\s+|<[^>\s]*>|'(?:[^'\\]|\\.)*'?|[(){},]|[^\s(){},'<>]+/gy;
  let at = 0;
  while (at < text.length) {
    re.lastIndex = at;
    const m = re.exec(text);
    if (!m) throw new ManchesterError('syntax', 'unexpected character', at, at + 1);
    const t = m[0];
    at = m.index + t.length;
    if (/^\s/.test(t)) continue;
    if (t.startsWith("'") && (t.length < 2 || !t.endsWith("'")))
      throw new ManchesterError('syntax', 'a quoted name is not closed', m.index, text.length);
    out.push({ t, start: m.index, end: at });
  }
  return out;
}

const M_KEYWORDS = new Set(['and', 'or', 'not', 'that', 'some', 'only', 'value']);
const AXIOM_WORDS = { SubClassOf: 1, EquivalentTo: 1, DisjointWith: 1 };

/** A reader of the class expressions and class axioms the mock understands. */
function manchesterReader(text, v) {
  const toks = mTokens(text);
  let i = 0;
  const peek = () => toks[i];
  const end = () => (toks.length ? toks[toks.length - 1].end : 0);
  const want = (what) => {
    const t = toks[i];
    if (!t) throw new ManchesterError('syntax', `expected ${what} at the end`, end(), end());
    return t;
  };
  const resolve = (tok, kind) => {
    const raw = tok.t;
    let iris;
    if (raw.startsWith('<')) iris = [raw.slice(1, -1)];
    else {
      const name = raw.startsWith("'") ? raw.slice(1, -1).replace(/\\(.)/g, '$1') : raw;
      const ex = raw.startsWith("'") ? null : v.expand(name);
      iris = ex && v.kinds.has(ex) ? [ex] : [...(v.names.get(name.toLowerCase()) ?? [])];
      iris = iris.filter((x) => v.kinds.get(x)?.has(kind));
      if (!iris.length)
        throw new ManchesterError(
          'unknown',
          `no ${kind === 'Class' ? 'class' : 'object property'} is named ${raw}`,
          tok.start,
          tok.end,
        );
      if (iris.length > 1)
        throw new ManchesterError(
          'ambiguous',
          `${raw} names ${iris.length} entities`,
          tok.start,
          tok.end,
          iris.map((x) => ({ iri: x, kind, label: v.label.get(x) ?? null })),
        );
    }
    return iris[0];
  };
  const primary = () => {
    const t = want('a class expression');
    if (t.t === 'not') {
      i++;
      return { type: 'ObjectComplementOf', class: primary() };
    }
    if (t.t === '(') {
      i++;
      const e = union();
      const c = want('a closing bracket');
      if (c.t !== ')') throw new ManchesterError('syntax', 'expected )', c.start, c.end);
      i++;
      return e;
    }
    if (M_KEYWORDS.has(t.t) || '(){},'.includes(t.t) || t.t in AXIOM_WORDS)
      throw new ManchesterError(
        'syntax',
        `expected a class expression, not ${t.t}`,
        t.start,
        t.end,
      );
    i++;
    const next = peek();
    if (next && (next.t === 'some' || next.t === 'only' || next.t === 'value')) {
      const property = resolve(t, 'ObjectProperty');
      i++;
      if (next.t === 'value') {
        const ind = want('an individual');
        i++;
        return { type: 'ObjectHasValue', property, individual: resolve(ind, 'NamedIndividual') };
      }
      return {
        type: next.t === 'some' ? 'ObjectSomeValuesFrom' : 'ObjectAllValuesFrom',
        property,
        class: primary(),
      };
    }
    return resolve(t, 'Class');
  };
  const conj = () => {
    const parts = [primary()];
    while (peek() && (peek().t === 'and' || peek().t === 'that')) {
      i++;
      parts.push(primary());
    }
    return parts.length > 1 ? { type: 'ObjectIntersectionOf', classes: parts } : parts[0];
  };
  const union = () => {
    const parts = [conj()];
    while (peek()?.t === 'or') {
      i++;
      parts.push(conj());
    }
    return parts.length > 1 ? { type: 'ObjectUnionOf', classes: parts } : parts[0];
  };
  const done = () => {
    const t = peek();
    if (t) throw new ManchesterError('syntax', `unexpected ${t.t}`, t.start, t.end);
  };
  return {
    classExpression() {
      const e = union();
      done();
      return e;
    },
    axiom() {
      const left = union();
      const k = want('SubClassOf, EquivalentTo or DisjointWith');
      if (!(k.t in AXIOM_WORDS))
        throw new ManchesterError(
          'syntax',
          `expected an axiom keyword, not ${k.t}`,
          k.start,
          k.end,
        );
      i++;
      const right = union();
      done();
      if (k.t === 'SubClassOf') return { type: 'SubClassOf', subClass: left, superClass: right };
      return {
        type: k.t === 'EquivalentTo' ? 'EquivalentClasses' : 'DisjointClasses',
        classes: [left, right],
      };
    },
  };
}

/** A class expression in Manchester syntax, with labels where they name one entity. */
function manchesterOf(x, v, r) {
  const name = (iri) => {
    const l = v.label.get(iri);
    if (l && v.names.get(l.toLowerCase())?.size === 1)
      return /^[A-Za-z_][\w-]*$/.test(l) && !M_KEYWORDS.has(l) ? l : `'${l.replace(/'/g, "\\'")}'`;
    return r.iri(iri);
  };
  const go = (e, top) => {
    if (typeof e === 'string') return name(e);
    const wrap = (s) => (top ? s : `(${s})`);
    switch (e.type) {
      case 'ObjectIntersectionOf':
        return wrap(e.classes.map((c) => go(c, false)).join(' and '));
      case 'ObjectUnionOf':
        return wrap(e.classes.map((c) => go(c, false)).join(' or '));
      case 'ObjectComplementOf':
        return `not ${go(e.class, false)}`;
      case 'ObjectSomeValuesFrom':
        return wrap(`${name(e.property)} some ${go(e.class, false)}`);
      case 'ObjectAllValuesFrom':
        return wrap(`${name(e.property)} only ${go(e.class, false)}`);
      case 'ObjectHasValue':
        return wrap(`${name(e.property)} value ${name(e.individual)}`);
      default:
        return r.val(e);
    }
  };
  return go(x, true);
}

/** The mock's DL query: the expression classified as a fresh class with the told axioms. */
function dlQueryOf(o, expr, answers) {
  const kinds = new Map(o.kinds);
  kinds.set(DL_QUERY, new Set(['Class']));
  const parts = expr.type === 'ObjectIntersectionOf' ? expr.classes : [expr];
  const extra = [
    {
      json: {
        type: 'EquivalentClasses',
        classes: [DL_QUERY, { type: 'ObjectIntersectionOf', classes: parts }],
      },
    },
    ...parts.map((p) => ({ json: { type: 'SubClassOf', subClass: DL_QUERY, superClass: p } })),
  ];
  // the mock's classifier reads no superclasses from a definition, so its parts are added
  for (const a of o.axioms)
    if (a.json.type === 'EquivalentClasses')
      for (const d of a.json.classes.filter((x) => typeof x === 'string'))
        for (const x of a.json.classes)
          if (x?.type === 'ObjectIntersectionOf')
            for (const p of x.classes)
              extra.push({ json: { type: 'SubClassOf', subClass: d, superClass: p } });
  if (typeof expr === 'string')
    extra.push({ json: { type: 'EquivalentClasses', classes: [DL_QUERY, expr] } });
  const c = classify({ kinds, axioms: [...o.axioms, ...extra] });
  const nodes = c.nodes;
  const at = nodes.findIndex((n) => n.classes.includes(DL_QUERY));
  const named = (ns) =>
    [...new Set(ns.flatMap((n) => nodes[n].classes))].filter((x) => x !== DL_QUERY).sort();
  const up = (n, seen = new Set()) => {
    for (const p of nodes[n].parents)
      if (!seen.has(p)) {
        seen.add(p);
        up(p, seen);
      }
    return seen;
  };
  const below = (n) => nodes.map((m) => m.node).filter((m) => m > 1 && up(m).has(n));
  const subs = below(at);
  const directSubs = subs.filter((m) => nodes[m].parents.includes(at));
  const sups = [...up(at)];
  const out = {
    equivalents: named([at]),
    directSuperclasses: named(nodes[at].parents),
    superclasses: named(sups),
    // as the server's answers, these leave out owl:Nothing
    directSubclasses: named(directSubs),
    subclasses: named(subs),
  };
  const classesOf = new Set([at, ...subs].flatMap((n) => nodes[n].classes));
  out.instances = o.axioms
    .filter(
      (a) =>
        a.json.type === 'ClassAssertion' &&
        typeof a.json.class === 'string' &&
        classesOf.has(a.json.class),
    )
    .map((a) => a.json.individual)
    .filter((x, k, all) => all.indexOf(x) === k)
    .sort();
  const body = { counts: {} };
  for (const k of answers)
    if (out[k]) {
      body[k] = out[k];
      body.counts[k] = out[k].length;
    }
  return body;
}

// --- the routes ------------------------------------------------------------------------

const etag = (ds, seq) => `W/"${ds.branchId ?? ds.id}:${seq}:owl"`;

/** The `Sparkles-Commit-Message` of a request, decoded from `utf-8''` percent-encoding. */
function commitMessageOf(req) {
  const raw = req.headers['sparkles-commit-message'];
  if (!raw) return undefined;
  return /^utf-8''/i.test(raw) ? decodeURIComponent(raw.slice(7)) : raw;
}

function quadJson(op) {
  return (q) => ({
    op,
    subject: q.subject.toString(),
    predicate: q.predicate.toString(),
    object: q.object.toString(),
    graph: q.graph.termType === 'DefaultGraph' ? null : q.graph.toString(),
  });
}

/** `/$/ontology/{ds}…` and `/{ds}/changes`; true when it answered. */
export async function handleOntology(req, res, url, seg, ctx) {
  if (seg.length >= 2 && seg[0] !== '$' && seg[1] === 'changes')
    return changes(req, res, url, seg, ctx);
  if (seg[0] === '$' && seg[1] === 'revert' && seg[2] && req.method === 'POST')
    return revert(req, res, url, seg, ctx);
  if (seg[0] !== '$' || seg[1] !== 'ontology' || !seg[2]) return false;
  const { datasets, send, fail, readBody, headCommit, quadSet, commitWrite } = ctx;
  const ds = datasets.get(seg[2]);
  if (!ds) return (fail(res, 404, `no such dataset: /${seg[2]}`), true);
  const p = url.searchParams;
  const sub = seg[3] ?? '';
  const graph = p.get('graph') ?? 'default';
  const r = renderer(ds.prefixes ?? {});
  const head = () => headCommit(ds).seq;
  const item = (a) => ({ type: a.json.type, ofn: r.axiom(a.json), axiom: a.json, graph });
  const opened = (o) => ({
    dataset: ds.name,
    commit: head(),
    mode: 'lenient',
    graphs: [graph],
    imports: [],
    warnings: [],
  });
  const terminology = (o) => o.axioms.filter((a) => !ASSERTION_TYPES.has(a.json.type));

  if (req.method === 'GET' && sub === '') {
    const o = readOntology(ds, graph);
    const counts = {};
    for (const a of terminology(o)) counts[a.json.type] = (counts[a.json.type] ?? 0) + 1;
    const ents = {};
    for (const [, ks] of o.kinds) for (const k of ks) ents[k] = (ents[k] ?? 0) + 1;
    const body = {
      ...opened(o),
      header: {
        iri: o.header.iri,
        versionIri: o.header.versionIri,
        imports: o.header.imports,
        annotations: o.header.annotations.map(
          (x) =>
            `Annotation(${r.iri(x.property)} ${r.val(isIri(x.value) ? x.value.value : litJson(x.value))})`,
        ),
      },
      axioms: counts,
      axiomCount: Object.values(counts).reduce((a, b) => a + b, 0),
      entities: ents,
      triples: o.quads,
      unmapped: o.unmapped,
      diagnostics: { warnings: 0, errors: 0, list: [] },
      cached: false,
    };
    send(res, 200, body, 'application/json', { ETag: etag(ds, head()) });
    return true;
  }
  if (req.method === 'GET' && sub === 'search') {
    send(res, 200, searchEntities(ds, graph, p));
    return true;
  }
  if (req.method === 'GET' && sub === 'hierarchy') {
    const b = browseIndex(ds, graph);
    const kind = p.get('kind') ?? 'class';
    const iri = p.get('iri');
    const limit = Math.min(Number(p.get('limit') ?? 100), 1000);
    const offset = Number(p.get('offset') ?? 0);
    const tree = b.trees[kind];
    if (!tree) return (fail(res, 400, `unknown hierarchy kind: ${kind}`), true);
    if (iri && !b.kinds.has(iri))
      return (fail(res, 404, `the graphs do not mention <${iri}>`, { code: 'not-found' }), true);
    const node = (i) => ({
      ...b.name(i),
      children: (tree.children.get(i) ?? []).length,
      parents: (tree.parents.get(i) ?? []).length,
    });
    const order = (a, c) => (a.label ?? a.iri).localeCompare(c.label ?? c.iri);
    const all = (iri ? (tree.children.get(iri) ?? []) : tree.roots)
      .map(node)
      .sort((a, c) => Number(a.deprecated) - Number(c.deprecated) || order(a, c));
    const page = all.slice(offset, offset + limit);
    const body = {
      dataset: ds.name,
      commit: head(),
      graphs: [graph],
      imports: [],
      warnings: [],
      kind,
      node: iri ? node(iri) : null,
      children: page,
      total: all.length,
      offset,
      limit,
      next: offset + page.length < all.length ? offset + page.length : null,
      order: 'label',
    };
    if (!iri) {
      body.roots = 'computed';
      body.deprecatedRoots = all.filter((n) => n.deprecated).length;
    }
    if (iri && ['true', '1'].includes(p.get('ancestors') ?? '')) {
      const nodes = [];
      const edges = [];
      const seen = new Set([iri]);
      const todo = [iri];
      while (todo.length) {
        const c = todo.shift();
        for (const par of tree.parents.get(c) ?? []) {
          edges.push([c, par]);
          if (seen.has(par)) continue;
          seen.add(par);
          nodes.push(node(par));
          todo.push(par);
        }
      }
      body.ancestors = { nodes, edges, truncated: false };
    }
    send(res, 200, body, 'application/json', { ETag: etag(ds, head()) });
    return true;
  }
  if (req.method === 'GET' && sub === 'term') {
    const b = browseIndex(ds, graph);
    const iri = p.get('iri');
    if (!iri || !b.mentioned.has(iri))
      return (fail(res, 404, `the graphs do not mention <${iri}>`, { code: 'not-found' }), true);
    const own = b.quads.filter(
      (q) => q.subject.termType === 'NamedNode' && q.subject.value === iri,
    );
    const format = p.get('format');
    if (format && format !== 'json') {
      const types = {
        ttl: 'text/turtle',
        nt: 'application/n-triples',
        rdfxml: 'application/rdf+xml',
        jsonld: 'application/ld+json',
        ofn: 'text/owl-functional',
      };
      if (!types[format]) return (fail(res, 400, `unknown format: ${format}`), true);
      const text = own.map((q) => `${q.subject} ${q.predicate} ${q.object} .`).join('\n');
      const ext = { ttl: 'ttl', nt: 'nt', rdfxml: 'rdf', jsonld: 'jsonld', ofn: 'ofn' }[format];
      send(res, 200, text + '\n', types[format], {
        'Content-Disposition': `attachment; filename="${iri.replace(/^.*[#/]/, '') || 'term'}.${ext}"`,
      });
      return true;
    }
    const o = readOntology(ds, graph);
    const used = new Set([iri]);
    const note = (v) => {
      if (typeof v === 'string') used.add(v);
      else if (Array.isArray(v)) v.forEach(note);
      else if (v && typeof v === 'object' && !isLit(v)) Object.values(v).forEach(note);
    };
    const statements = own
      .filter((q) => q.object.termType !== 'BlankNode')
      .map((q) => {
        const value = q.object.termType === 'Literal' ? litJson(q.object) : q.object.value;
        note(q.predicate.value);
        note(value);
        // the annotations of the reified axiom that states the triple, as the server gives
        const stated = o.axioms.find(
          (x) =>
            x.json.type === 'AnnotationAssertion' &&
            x.json.subject === iri &&
            x.json.property === q.predicate.value &&
            JSON.stringify(x.json.value) === JSON.stringify(value),
        );
        const annotations = (stated?.json.annotations ?? []).map((x) => ({
          property: x.property,
          value: x.value,
        }));
        return { property: q.predicate.value, value, graph, annotations };
      });
    // as on the server, an axiom is about the entity when one of its triples has it as subject
    const about = (a) => a.quads.some((q) => isIri(q.subject) && q.subject.value === iri);
    const axioms = [];
    const mentioning = [];
    // as on the server, a mention is an axiom whose blank nodes hold the entity; a triple
    // of a named subject that points at it is an incoming statement instead
    const viaBlank = (a) => a.quads.some((q) => isBlank(q.subject) && q.object.value === iri);
    for (const a of o.axioms) {
      if (a.json.type === 'AnnotationAssertion' || !mentions(a.json, iri)) continue;
      if (about(a)) axioms.push(a);
      else if (viaBlank(a)) mentioning.push(a);
      else continue;
      note(a.json);
    }
    const groups = new Map();
    for (const q of b.quads) {
      if (q.object.termType !== 'NamedNode' || q.object.value !== iri) continue;
      if (q.subject.termType !== 'NamedNode' || q.subject.value === iri) continue;
      const g = groups.get(q.predicate.value) ?? [];
      if (!g.includes(q.subject.value)) g.push(q.subject.value);
      groups.set(q.predicate.value, g);
    }
    const cap = Number(p.get('incoming') ?? 20);
    const incoming = [...groups].map(([property, subjects]) => {
      note(property);
      subjects.slice(0, cap).forEach(note);
      return {
        property,
        count: subjects.length,
        more: subjects.length > cap,
        subjects: subjects.slice(0, cap),
      };
    });
    const users = [
      ...new Set(
        b.quads
          .filter((q) => q.predicate.value === iri && q.subject.termType === 'NamedNode')
          .map((q) => q.subject.value),
      ),
    ];
    users.slice(0, cap).forEach(note);
    const names = {};
    for (const i of used) {
      if (i === iri || !b.mentioned.has(i)) continue;
      const { iri: _, ...rest } = b.name(i);
      names[i] = rest;
    }
    send(
      res,
      200,
      {
        dataset: ds.name,
        commit: head(),
        graphs: [graph],
        imports: [],
        warnings: [],
        ...b.name(iri),
        types: own
          .filter((q) => q.predicate.value === TYPE && isIri(q.object))
          .map((q) => q.object.value),
        ontologies: [{ graph, ontology: o.header.iri }],
        statements,
        statementsTruncated: false,
        xlLabels: [],
        axioms: axioms.map(item),
        mentions: { axioms: mentioning.map(item), total: mentioning.length, truncated: false },
        incoming,
        usage: users.length
          ? {
              property: iri,
              count: users.length,
              more: users.length > cap,
              subjects: users.slice(0, cap),
            }
          : null,
        names,
      },
      'application/json',
      { ETag: etag(ds, head()) },
    );
    return true;
  }
  if (req.method === 'GET' && sub === 'entities') {
    const o = readOntology(ds, graph);
    const want = p.get('kind')?.split(',');
    const all = [];
    for (const [iri, ks] of [...o.kinds].sort((a, b) => (a[0] < b[0] ? -1 : 1)))
      for (const k of ks) if (!want || want.includes(k)) all.push({ kind: k, iri });
    const limit = Number(p.get('limit') ?? 100);
    const offset = Number(p.get('offset') ?? 0);
    const page = all.slice(offset, offset + limit);
    send(res, 200, {
      ...opened(o),
      entities: page,
      total: all.length,
      next: offset + limit < all.length ? offset + limit : null,
    });
    return true;
  }
  if (req.method === 'GET' && sub === 'axioms') {
    const o = readOntology(ds, graph);
    const types = p.get('type')?.split(',');
    const about = p.get('about');
    const assertions = ['true', '1', ''].includes(String(p.get('assertions') ?? 'x'));
    const list = o.axioms.filter(
      (a) =>
        (assertions || !ASSERTION_TYPES.has(a.json.type)) &&
        (!types || types.includes(a.json.type)) &&
        (!about || mentions(a.json, about)),
    );
    if (about && !list.length && !o.kinds.has(about))
      return (
        fail(res, 404, `the ontology neither declares nor uses <${about}>`, { code: 'not-found' }),
        true
      );
    const limit = Number(p.get('limit') ?? 100);
    const offset = Number(p.get('offset') ?? 0);
    const page = list.slice(offset, offset + limit).map(item);
    send(res, 200, {
      ...opened(o),
      axioms: page,
      total: list.length,
      offset,
      next: offset + limit < list.length ? offset + limit : null,
    });
    return true;
  }
  if (req.method === 'GET' && sub === 'entity') {
    const o = readOntology(ds, graph);
    const iri = p.get('iri');
    if (!iri || !o.kinds.has(iri))
      return (
        fail(res, 404, `the ontology neither declares nor uses <${iri}>`, { code: 'not-found' }),
        true
      );
    const labels = o.axioms
      .filter(
        (a) =>
          a.json.type === 'AnnotationAssertion' &&
          a.json.subject === iri &&
          a.json.property === RDFS + 'label',
      )
      .map((a) => a.json.value);
    send(res, 200, {
      ...opened(o),
      iri,
      kinds: [...o.kinds.get(iri)],
      labels,
      annotations: [],
      assertions: false,
      truncated: false,
    });
    return true;
  }
  if (req.method === 'POST' && (sub === 'parse' || sub === 'dl-query')) {
    let body;
    try {
      body = JSON.parse((await readBody(req)) || '{}');
    } catch {
      return (fail(res, 400, 'invalid JSON'), true);
    }
    const o = readOntology(ds, body.graph ?? graph);
    const v = vocabulary(o, ds.prefixes ?? {});
    const text = sub === 'parse' ? (body.text ?? '') : (body.expression ?? '');
    const as = sub === 'parse' ? (body.as ?? 'classExpression') : 'classExpression';
    let value;
    try {
      if (as !== 'classExpression' && as !== 'axiom')
        throw new ManchesterError('syntax', `the mock reads no ${as}`, 0, text.length);
      value = manchesterReader(text, v)[as]();
    } catch (e) {
      if (e instanceof ManchesterError) return (send(res, 400, e.json()), true);
      throw e;
    }
    if (sub === 'parse') {
      const manchester = as === 'axiom' ? null : manchesterOf(value, v, r);
      send(res, 200, {
        ...opened(o),
        as,
        value,
        ofn: as === 'axiom' ? r.axiom(value) : r.val(value),
        manchester,
      });
      return true;
    }
    const t0 = performance.now();
    const answers = body.answers ?? [
      'equivalents',
      'directSuperclasses',
      'superclasses',
      'directSubclasses',
      'subclasses',
    ];
    const el = body.reasoning !== 'rl';
    send(res, 200, {
      ...opened(o),
      expression: { manchester: manchesterOf(value, v, r), ofn: r.val(value), json: value },
      reasoning: el
        ? {
            engine: 'el',
            title: 'OWL 2 EL classifier',
            why: 'the expression and the ontology are inside OWL 2 EL',
            complete: true,
            instancesComplete: true,
            gapCount: 0,
            gaps: [],
            notes: [],
          }
        : {
            engine: 'rl',
            title: 'OWL 2 RL rules by demand reasoning',
            why: 'the caller asked for the OWL 2 RL rules',
            complete: false,
            instancesComplete: true,
            gapCount: 0,
            gaps: [],
            notes: [
              'the OWL 2 RL rules derive subsumptions between classes only in part, so the class answers can miss entailed ones',
            ],
          },
      satisfiable: true,
      consistent: true,
      incremental: el,
      elapsedMs: Math.max(0.1, performance.now() - t0),
      ...dlQueryOf(o, value, answers),
    });
    return true;
  }
  if (req.method === 'POST' && sub === 'classify') {
    const body = JSON.parse((await readBody(req)) || '{}');
    const o = readOntology(ds, body.graph ?? graph);
    const c = classify(o);
    const limit = body.limit ?? 100;
    const offset = body.offset ?? 0;
    const nodes = c.nodes.slice(offset, offset + limit).map((n) => ({
      ...n,
      parentClasses: n.parents.map((x) => c.nodes[x].classes[0]),
    }));
    // one class's node, as the server's `about` answers it
    let about;
    if (body.about) {
      const i = c.nodes.findIndex((n) => n.classes.includes(body.about));
      if (i < 0) return (fail(res, 404, `the taxonomy has no class <${body.about}>`), true);
      const n = c.nodes[i];
      about = {
        ...n,
        node: i,
        iri: body.about,
        unsatisfiable: false,
        parentClasses: n.parents.map((x) => c.nodes[x].classes[0]),
      };
    }
    send(res, 200, {
      ...opened(o),
      consistent: true,
      complete: true,
      reused: false,
      classes: c.classes,
      nodeCount: c.nodes.length,
      unsatisfiableCount: 0,
      unsatisfiable: [],
      excludedCount: 0,
      excluded: [],
      stats: { incremental: false, saturationMs: 0.4, indexMs: 0.1, taxonomyMs: 0.1 },
      ...(about ? { class: about } : {}),
      ...(body.nodes
        ? { nodes, offset, ...(offset + limit < c.nodes.length ? { next: offset + limit } : {}) }
        : {}),
    });
    return true;
  }
  const write =
    (req.method === 'POST' && sub === 'axioms') || (req.method === 'DELETE' && sub === 'entity');
  if (!write) return false;
  if (ds.protected)
    return (
      fail(res, 403, `branch ${ds.branchName} is protected`, { code: 'branch-protected' }), true
    );
  const dry = ['true', ''].includes(String(p.get('dryRun') ?? 'x'));
  const ifMatch = req.headers['if-match'];
  const precondition =
    ifMatch && ifMatch !== etag(ds, head())
      ? `If-Match: the dataset is at commit ${head()} now`
      : null;
  let body = {};
  if (req.method === 'POST') {
    try {
      body = JSON.parse((await readBody(req)) || '{}');
    } catch {
      return (fail(res, 400, 'invalid JSON'), true);
    }
  }
  const target = body.target ?? body.graph ?? graph;
  const g = graphTerm(target);
  const o = readOntology(ds, body.graph ?? graph);
  const prefixes = { ...ds.prefixes, '': ds.prefixes?.uni ?? '' };
  const read = (v, at) => {
    if (typeof v === 'string') {
      try {
        return parseOfn(v, prefixes);
      } catch (e) {
        throw Object.assign(new Error(`${at}: ${e.message}`), { code: 'invalid-axiom' });
      }
    }
    if (!v || typeof v !== 'object' || typeof v.type !== 'string')
      throw Object.assign(new Error(`${at}: an axiom in its JSON form`), { code: 'invalid-axiom' });
    if (['DisjointClasses', 'EquivalentClasses'].includes(v.type) && (v.classes?.length ?? 0) < 2)
      throw Object.assign(new Error(`${at}: a disjointness needs two operands`), {
        code: 'invalid-axiom',
      });
    return v;
  };
  let add;
  let remove;
  try {
    add = (body.add ?? []).map((v, i) => read(v, `add[${i}]`));
    remove = (body.remove ?? []).map((v, i) => read(v, `remove[${i}]`));
  } catch (e) {
    return (fail(res, 400, e.message, { code: e.code }), true);
  }
  const entity = req.method === 'DELETE' ? p.get('iri') : null;
  if (req.method === 'DELETE' && !entity)
    return (fail(res, 400, 'iri names the entity to remove'), true);
  if (req.method === 'POST' && !add.length && !remove.length)
    return (fail(res, 400, 'name axioms to add or remove'), true);
  // the axioms the edit removes, and those it adds that the ontology lacks
  const byKey = new Map();
  for (const a of o.axioms) {
    const k = axiomKey(a.json);
    if (!byKey.has(k)) byKey.set(k, []);
    byKey.get(k).push(a);
  }
  const removing = [];
  const absent = [];
  const removedKeys = new Set();
  const asked = entity
    ? o.axioms.filter((a) => mentions(a.json, entity)).map((a) => a.json)
    : remove;
  for (const a of asked) {
    const k = axiomKey(a);
    if (removedKeys.has(k)) continue;
    const hits = byKey.get(k);
    if (hits) {
      removedKeys.add(k);
      removing.push({ json: a, hits });
    } else absent.push(a);
  }
  const adding = [];
  let present = 0;
  const seen = new Set();
  for (const a of add) {
    const k = axiomKey(a);
    if (seen.has(k)) continue;
    seen.add(k);
    if (byKey.has(k) && !removedKeys.has(k)) present++;
    else adding.push(a);
  }
  const declared = [];
  if (body.declare !== false) {
    const known = new Set([...o.kinds].flatMap(([i, ks]) => [...ks].map((k) => `${k} ${i}`)));
    for (const a of [...adding]) {
      if (a.type === 'Declaration') continue;
      for (const [k, i] of signature(a)) {
        if (
          known.has(`${k} ${i}`) ||
          adding.some((x) => x.type === 'Declaration' && x.iri === i && x.kind === k)
        )
          continue;
        known.add(`${k} ${i}`);
        declared.push([k, i]);
        adding.push({ type: 'Declaration', kind: k, iri: i });
      }
    }
  }
  // the precondition is checked before anything is written
  if (!dry && precondition)
    return (fail(res, 412, precondition, { code: 'precondition-failed' }), true);
  const dump = dry ? ds.store.dump({ format: 'application/n-quads' }) : null;
  const before = quadSet(ds);
  const beforeQuads = dry ? ds.store.match() : null;
  try {
    for (const r of removing) for (const h of r.hits) for (const q of h.quads) ds.store.delete(q);
    for (const a of adding) for (const q of axiomTriples(a, g)) ds.store.add(q);
  } catch (e) {
    if (dump) {
      for (const q of ds.store.match()) ds.store.delete(q);
      ds.store.load(dump, { format: 'application/n-quads' });
    }
    return (fail(res, 400, e.message, { code: 'invalid-axiom' }), true);
  }
  const answer = {
    added: adding.length,
    present,
    removed: removing.length,
    absent: absent.map(r.axiom),
    declared: declared.map(([k, i]) => r.entity(k, i)),
    addedAxioms: adding.map((a) => ({ type: a.type, ofn: r.axiom(a), axiom: a })),
    removedAxioms: removing.map((x) => ({
      type: x.json.type,
      ofn: r.axiom(x.json),
      axiom: x.json,
    })),
    axiomsTruncated: false,
  };
  if (dry) {
    const after = ds.store.match();
    const had = new Set(beforeQuads.map(String));
    const has = new Set(after.map(String));
    const removed = beforeQuads.filter((q) => !has.has(String(q))).map(quadJson('-'));
    const added = after.filter((q) => !had.has(String(q))).map(quadJson('+'));
    for (const q of ds.store.match()) ds.store.delete(q);
    ds.store.load(dump, { format: 'application/n-quads' });
    const changes = [...removed, ...added];
    const limit = Number(p.get('changes') ?? 0);
    const wouldCommit = changes.length > 0 && !precondition;
    const preview = {
      dryRun: true,
      dataset: ds.name,
      committed: false,
      wouldCommit,
      outcome: precondition ? 'precondition-failed' : changes.length ? 'commit' : 'no-change',
      head: head(),
      commit: {
        seq: head() + (wouldCommit ? 1 : 0),
        inserted: added.length,
        deleted: removed.length,
      },
      ...(limit
        ? {
            changes: {
              total: changes.length,
              limit,
              truncated: changes.length > limit,
              quads: changes.slice(0, limit),
            },
          }
        : {}),
      ...(ifMatch
        ? {
            precondition: precondition
              ? { status: 'failed', error: precondition }
              : { status: 'passed' },
          }
        : {}),
      ...(precondition ? { error: precondition, code: 'precondition-failed' } : {}),
    };
    send(res, precondition ? 412 : 200, preview, 'application/json', {
      'Sparkles-Dry-Run': 'true',
    });
    return true;
  }
  const message = commitMessageOf(req) || p.get('message') || undefined;
  const receipt = commitWrite(ds, 'update', before, { message });
  const after = quadSet(ds);
  let inserted = 0;
  let deleted = 0;
  for (const q of after) if (!before.has(q)) inserted++;
  for (const q of before) if (!after.has(q)) deleted++;
  send(res, 200, { ...answer, inserted, deleted, kept: 0, ...receipt }, 'application/json', {
    'Sparkles-Commit': String(receipt.commit.seq),
  });
  return true;
}

/** `GET /{ds}/changes`: the commits after `after`, as JSON or as an event stream. */
function changes(req, res, url, seg, ctx) {
  const { datasets, send, fail, allCommits } = ctx;
  const ds = datasets.get(seg[0]);
  if (!ds) return (fail(res, 404, `no such dataset: /${seg[0]}`), true);
  const after = Number(String(url.searchParams.get('after') ?? '').replace(/^commit:/, '')) || 0;
  const since = (n) => allCommits(ds).filter((c) => c.seq > n);
  if (!String(req.headers.accept ?? '').includes('text/event-stream')) {
    const cs = since(after);
    send(res, 200, {
      dataset: ds.name,
      after,
      next: cs.at(-1)?.seq ?? after,
      commits: cs.map((c) => ({ commit: c, complete: true, changes: [] })),
    });
    return true;
  }
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-store',
    Connection: 'keep-alive',
  });
  let last = after;
  const flush = () => {
    for (const c of since(last)) {
      res.write(
        `event: commit\nid: ${c.seq}\ndata: ${JSON.stringify({ commit: c, complete: true })}\n\n`,
      );
      last = c.seq;
    }
  };
  flush();
  const tick = setInterval(flush, 250);
  const end = setTimeout(() => res.end(), 300_000);
  req.on('close', () => {
    clearInterval(tick);
    clearTimeout(end);
  });
  return true;
}

/**
 * The terms of an N-Quads line as written: IRIs, blank nodes, literals with their tags,
 * and triple terms, each one term however it nests.
 */
function nquadTerms(line) {
  const out = [];
  let i = 0;
  while (i < line.length) {
    const c = line[i];
    if (c === ' ' || c === '\t') {
      i++;
      continue;
    }
    const start = i;
    if (line.startsWith('<<', i)) {
      // a triple term: up to its matching `>>`, past the IRIs and literals inside it
      let depth = 0;
      while (i < line.length) {
        if (line.startsWith('<<', i)) {
          depth++;
          i += 2;
        } else if (line.startsWith('>>', i)) {
          depth--;
          i += 2;
          if (depth === 0) break;
        } else if (line[i] === '<') i = line.indexOf('>', i) + 1;
        else if (line[i] === '"') {
          i++;
          while (i < line.length && line[i] !== '"') i += line[i] === '\\' ? 2 : 1;
          i++;
        } else i++;
      }
    } else if (c === '<') i = line.indexOf('>', i) + 1;
    else if (c === '"') {
      i++;
      while (i < line.length && line[i] !== '"') i += line[i] === '\\' ? 2 : 1;
      i++;
      if (line[i] === '@') while (i < line.length && !/\s/.test(line[i])) i++;
      else if (line.startsWith('^^<', i)) i = line.indexOf('>', i) + 1;
    } else while (i < line.length && !/\s/.test(line[i])) i++;
    out.push(line.slice(start, i));
  }
  return out;
}

/**
 * The quad of an N-Quads line (without the final " .") with its blank nodes as labelled,
 * since loading a line into a store renames them.
 */
function quadOfLine(line) {
  const st = new ox.Store();
  st.load(`${line} .`, { format: 'application/n-quads' });
  const q = st.match()[0];
  const terms = nquadTerms(line);
  const fix = (t, i) =>
    t.termType === 'BlankNode' && terms[i]?.startsWith('_:') ? ox.blankNode(terms[i].slice(2)) : t;
  return ox.quad(fix(q.subject, 0), q.predicate, fix(q.object, 2), fix(q.graph, 3));
}

/** The cell of a quad, which a revert compares as the server's default `cell` scope does. */
const cellKey = (q) =>
  `${q.graph.termType === 'DefaultGraph' ? '' : q.graph}\u0000${q.subject}\u0000${q.predicate}`;

/**
 * `POST /$/revert/{ds}?branch=&commit=`: the revert of a commit of the branch, a three-way
 * merge of the commit's parent into the head with the commit as its base. The mock diffs
 * the states it kept before and after the commit, so blank nodes keep their identity, and
 * a cell the commit changed that a later commit changed again conflicts with `409`
 * `merge-conflict`. A revert whose changes the head no longer holds answers `upToDate`
 * with no commit, a dry run included. `expect.target` is the head the caller saw, and a
 * head that moved since answers `409` `head-moved`. With `dryRun` it answers the write
 * preview.
 */
async function revert(req, res, url, seg, ctx) {
  const { datasets, send, fail, readBody, headCommit, quadSet, commitWrite, allCommits } = ctx;
  const ds = datasets.get(seg[2]);
  if (!ds) return (fail(res, 404, `no such dataset: /${seg[2]}`), true);
  if (ds.protected)
    return (
      fail(res, 403, `branch ${ds.branchName} is protected`, { code: 'branch-protected' }), true
    );
  let body = {};
  try {
    body = JSON.parse((await readBody(req)) || '{}');
  } catch {
    return (fail(res, 400, 'invalid JSON'), true);
  }
  const seq = Number(url.searchParams.get('commit'));
  const list = allCommits(ds);
  const commit = list.find((c) => c.seq === seq);
  if (!commit || seq === 0)
    return (fail(res, 400, `cannot revert commit ${seq}`, { code: 'invalid-merge' }), true);
  // the state after the commit and the lines it changed, which the mock keeps for the
  // commits it makes after start-up
  const after = stateLines(commit);
  const parent = list.find((c) => c.seq === seq - 1);
  const parentState = parent && stateLines(parent);
  // a commit made without recorded lines, such as a seeded upload, diffs the two states
  const lines =
    linesOf.get(commit) ??
    (after && parentState
      ? {
          added: after.filter((l) => !new Set(parentState).has(l)),
          removed: parentState.filter((l) => !new Set(after).has(l)),
        }
      : null);
  if (!after || !lines)
    return (
      fail(res, 410, `commit ${seq} is outside the kept history`, { code: 'history-gone' }), true
    );
  const head = headCommit(ds).seq;
  if (body.expect?.target != null && body.expect.target !== head)
    return (
      fail(res, 409, `the branch is at commit ${head}, not ${body.expect.target}`, {
        code: 'head-moved',
      }),
      true
    );
  const branch = ds.branchName ?? 'main';
  const now = quadSet(ds);
  // the commit's changes, reversed: what it added goes, what it removed comes back
  const drop = lines.added.map(quadOfLine);
  const restore = lines.removed.map(quadOfLine);
  // the state before the commit, which the revert would leave in the cells it changed
  const gone = new Set(lines.added);
  const before = [...after.filter((l) => !gone.has(l)), ...lines.removed];
  // a cell the commit changed conflicts when the head holds other objects there than both
  // the commit left and the revert would leave
  const cells = new Set([...drop, ...restore].map(cellKey));
  // each cell's objects in a state, as a sorted list of lines
  const objects = (state) => {
    const m = new Map();
    for (const l of state) {
      const k = cellKey(quadOfLine(l));
      if (!cells.has(k)) continue;
      const v = m.get(k) ?? [];
      v.push(l);
      m.set(k, v);
    }
    return new Map([...m].map(([k, v]) => [k, v.sort().join('\n')]));
  };
  const left = objects(after);
  const reverted = objects(before);
  const current = objects([...now]);
  const conflicts = [...cells].filter((k) => {
    const c = current.get(k) ?? '';
    return c !== (left.get(k) ?? '') && c !== (reverted.get(k) ?? '');
  });
  if (conflicts.length)
    return (
      fail(res, 409, `the revert of commit ${seq} conflicts in ${conflicts.length} cells`, {
        code: 'merge-conflict',
        conflicts: { found: conflicts.length, resolved: 0 },
      }),
      true
    );
  const removed = drop.filter((q) => now.has(String(q)));
  const added = restore.filter((q) => !now.has(String(q)));
  if (!removed.length && !added.length) {
    send(res, 200, {
      merged: false,
      upToDate: true,
      fastForward: false,
      source: { branch, seq: seq - 1 },
      target: { branch, seq: head },
      base: { branch, seq },
      reverted: { branch, seq },
      commit: null,
      changes: { inserted: 0, deleted: 0 },
      conflicts: { found: 0, resolved: 0 },
    });
    return true;
  }
  const message = body.message || `revert commit ${seq}`;
  if (body.dryRun) {
    const changes = [...removed.map(quadJson('-')), ...added.map(quadJson('+'))];
    const limit = Number(body.changes ?? 0);
    send(res, 200, {
      dryRun: true,
      wouldCommit: true,
      outcome: 'commit',
      head,
      commit: { seq: head + 1, inserted: added.length, deleted: removed.length },
      ...(limit
        ? {
            changes: {
              total: changes.length,
              limit,
              truncated: changes.length > limit,
              quads: changes.slice(0, limit),
            },
          }
        : {}),
    });
    return true;
  }
  for (const q of removed) ds.store.delete(q);
  for (const q of added) ds.store.add(q);
  const receipt = commitWrite(ds, 'revert', now, { message });
  send(res, 200, {
    merged: true,
    upToDate: false,
    fastForward: false,
    source: { branch, seq: seq - 1 },
    target: { branch, seq: receipt.commit.seq },
    base: { branch, seq },
    reverted: { branch, seq },
    commit: receipt.commit,
    changes: { inserted: added.length, deleted: removed.length },
    conflicts: { found: 0, resolved: 0 },
  });
  return true;
}

// --- entity search ---------------------------------------------------------------------

const ENTITY_KINDS = {
  [OWL + 'Class']: 'class',
  [RDFS + 'Class']: 'class',
  [OWL + 'ObjectProperty']: 'objectProperty',
  [OWL + 'DatatypeProperty']: 'dataProperty',
  [OWL + 'AnnotationProperty']: 'annotationProperty',
  [RDF + 'Property']: 'objectProperty',
  [OWL + 'NamedIndividual']: 'individual',
  [RDFS + 'Datatype']: 'datatype',
  [SKOS + 'Concept']: 'concept',
  [SKOS + 'ConceptScheme']: 'conceptScheme',
  [SKOS + 'Collection']: 'collection',
};
const LABELS = { [RDFS + 'label']: 'label', [SKOS + 'prefLabel']: 'label' };
const SYNONYMS = { [SKOS + 'altLabel']: 'synonym', [SKOS + 'hiddenLabel']: 'synonym' };
const fold = (t) =>
  t
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase();

/**
 * `GET /$/ontology/{ds}/search` (docs/API.md, "Entity search"), in brief: the labels and
 * synonyms of the typed entities that equal, start with or contain the query, in that
 * order, without the fuzzy and definition tiers.
 */
function searchEntities(ds, graph, p) {
  const q = (p.get('q') ?? '').trim();
  const limit = Math.min(Number(p.get('limit') ?? 10), 100);
  const offset = Number(p.get('offset') ?? 0);
  const want = p.get('kind')?.split(',');
  const quads = ds.store.match(null, null, null, graphTerm(graph));
  const kinds = new Map();
  const texts = new Map();
  for (const x of quads) {
    if (x.subject.termType !== 'NamedNode') continue;
    const s = x.subject.value;
    const pr = x.predicate.value;
    if (pr === TYPE && ENTITY_KINDS[x.object.value]) {
      const k = kinds.get(s) ?? new Set();
      k.add(ENTITY_KINDS[x.object.value]);
      kinds.set(s, k);
    } else if ((LABELS[pr] || SYNONYMS[pr]) && x.object.termType === 'Literal') {
      const l = texts.get(s) ?? [];
      l.push({
        field: LABELS[pr] ?? SYNONYMS[pr],
        property: pr,
        text: x.object.value,
        lang: x.object.language || null,
      });
      texts.set(s, l);
    }
  }
  const fq = fold(q);
  const hits = [];
  for (const [iri, ks] of kinds) {
    const kindList = [...ks];
    if (
      want &&
      !kindList.some(
        (k) => want.includes(k) || (want.includes('property') && k.endsWith('Property')),
      )
    )
      continue;
    const all = texts.get(iri) ?? [];
    const labels = all.filter((t) => t.field === 'label');
    const label = labels.find((t) => !t.lang || t.lang.startsWith('en')) ?? labels[0] ?? null;
    let best = null;
    for (const t of all) {
      const f = fold(t.text);
      const at = f.indexOf(fq);
      if (!fq || at < 0) continue;
      const tier = (f === fq ? 0 : at === 0 ? 1 : 4) + (t.field === 'synonym' ? 2 : 0);
      if (!best || tier < best.tier) best = { ...t, tier, at };
    }
    if (!best) continue;
    hits.push({
      type: 'entity',
      iri,
      kinds: kindList,
      label: label?.text ?? null,
      labelLang: label?.lang ?? null,
      shortForm: iri.replace(/^.*[#/]/, ''),
      deprecated: false,
      match: {
        field: best.field,
        property: best.property,
        text: best.text,
        lang: best.lang,
        kind: best.tier % 2 === 0 && best.tier < 4 ? 'exact' : best.at === 0 ? 'prefix' : 'words',
        distance: 0,
        tier: best.tier,
        highlight: [[best.at, best.at + q.length]],
      },
    });
  }
  hits.sort(
    (a, b) => a.match.tier - b.match.tier || (a.label ?? a.iri).localeCompare(b.label ?? b.iri),
  );
  const page = hits.slice(offset, offset + limit);
  const more = offset + limit < hits.length;
  return {
    dataset: ds.name,
    commit: 0,
    graphs: [graph],
    imports: [],
    query: q,
    results: page,
    offset,
    limit,
    more,
    next: more ? offset + limit : null,
    index: 'current',
  };
}

// --- the browser ---------------------------------------------------------------------

const TREE_LINKS = {
  class: [[RDFS + 'subClassOf', false]],
  objectProperty: [[RDFS + 'subPropertyOf', false]],
  dataProperty: [[RDFS + 'subPropertyOf', false]],
  annotationProperty: [[RDFS + 'subPropertyOf', false]],
  concept: [
    [SKOS + 'broader', false],
    [SKOS + 'narrower', true],
  ],
};
const TOP = new Set([OWL + 'Thing', OWL + 'topObjectProperty', OWL + 'topDataProperty']);

/**
 * The names, kinds and hierarchies of a graph, in brief, for the mock's
 * `GET /$/ontology/{ds}/hierarchy` and `GET /$/ontology/{ds}/term`.
 */
function browseIndex(ds, graph) {
  const quads = ds.store.match(null, null, null, graphTerm(graph));
  const kinds = new Map();
  const labels = new Map();
  const deprecated = new Set();
  const mentioned = new Set();
  for (const x of quads) {
    for (const t of [x.subject, x.object]) if (t.termType === 'NamedNode') mentioned.add(t.value);
    if (x.subject.termType !== 'NamedNode') continue;
    const s = x.subject.value;
    const pr = x.predicate.value;
    if (pr === TYPE && ENTITY_KINDS[x.object.value]) {
      const k = kinds.get(s) ?? new Set();
      k.add(ENTITY_KINDS[x.object.value]);
      kinds.set(s, k);
    } else if (LABELS[pr] && x.object.termType === 'Literal') {
      const l = labels.get(s) ?? [];
      l.push({ text: x.object.value, lang: x.object.language || null });
      labels.set(s, l);
    } else if (pr === OWL + 'deprecated' && x.object.value === 'true') deprecated.add(s);
  }
  const name = (iri) => {
    const ls = labels.get(iri) ?? [];
    const l = ls.find((t) => t.lang?.startsWith('en')) ?? ls.find((t) => !t.lang) ?? ls[0] ?? null;
    return {
      iri,
      label: l?.text ?? null,
      labelLang: l?.lang ?? null,
      shortForm: iri.replace(/^.*[#/]/, '') || iri,
      kinds: [...(kinds.get(iri) ?? [])],
      deprecated: deprecated.has(iri),
    };
  };
  const trees = {};
  for (const [kind, links] of Object.entries(TREE_LINKS)) {
    const members = [...kinds].filter(([, ks]) => ks.has(kind)).map(([i]) => i);
    const set = new Set(members);
    const parents = new Map();
    const children = new Map();
    const link = (c, par) => {
      if (!set.has(c) || !set.has(par) || TOP.has(par)) return;
      const ps = parents.get(c) ?? [];
      if (ps.includes(par)) return;
      ps.push(par);
      parents.set(c, ps);
      const cs = children.get(par) ?? [];
      cs.push(c);
      children.set(par, cs);
    };
    for (const x of quads) {
      if (x.subject.termType !== 'NamedNode' || x.object.termType !== 'NamedNode') continue;
      for (const [pr, inverse] of links)
        if (x.predicate.value === pr)
          inverse ? link(x.object.value, x.subject.value) : link(x.subject.value, x.object.value);
    }
    trees[kind] = {
      parents,
      children,
      roots: members.filter((m) => !parents.has(m) && !TOP.has(m)),
    };
  }
  return { quads, kinds, name, trees, mentioned };
}
