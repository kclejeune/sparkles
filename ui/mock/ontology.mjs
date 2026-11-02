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
  skos:definition "A human being who takes part in university life."@en .
uni:Student a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Student"@en , "Étudiant"@fr .
uni:GraduateStudent a owl:Class ; rdfs:subClassOf uni:Student ; rdfs:label "Graduate student"@en .
uni:Employee a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Employee"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ;
  rdfs:label "Professor"@en , "Professeur"@fr .
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

// --- the routes ------------------------------------------------------------------------

const etag = (ds, seq) => `W/"${ds.branchId ?? ds.id}:${seq}:owl"`;

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
  const message = req.headers['sparkles-commit-message'] || p.get('message') || undefined;
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
