// Edit mode on the ontology browser's pages (ROADMAP item 630, docs/specs/
// U12-ontology-workbench/SPEC.md, "Ontology browser"): the pure functions that turn a form
// of the page into one operation of the editors' write path. An OWL entity's edits are
// `axioms` operations of annotation assertions and hierarchy axioms, as the ontology
// editor sends them, and a SKOS resource's edits are `update` operations of ground
// triples, as the thesaurus editor sends them. Both are previewed and committed by
// OntoPreview, and nothing here writes.

import { isLiteral, type Statement, type Value } from './browse';
import {
  annotationAssertion,
  axiomKey,
  dataUpdate,
  diffAxioms,
  type Annotation,
  type AnnValue,
  type AxiomItem,
  type AxiomJson,
  type HeaderTriple,
  type HeaderValue,
  type Lit,
  type Op,
  termSparql,
} from './ontology';
import { sparqlIri, WELL_KNOWN } from './rdf';

const RDF = WELL_KNOWN.rdf;
const RDFS = WELL_KNOWN.rdfs;
const OWL = WELL_KNOWN.owl;
const SKOS = WELL_KNOWN.skos;
const OBO = 'http://purl.obolibrary.org/obo/';
const OIO = 'http://www.geneontology.org/formats/oboInOwl#';
const DC = 'http://purl.org/dc/elements/1.1/';
const DCT = 'http://purl.org/dc/terms/';

/** Whether a page's edits are OWL axioms or SKOS triples. */
export type EditMode = 'owl' | 'skos';

/** The property an edit mode writes a definition with. */
export const definitionProperty = (mode: EditMode) =>
  mode === 'skos' ? SKOS + 'definition' : OBO + 'IAO_0000115';

/** The synonym kinds an OWL page adds, as the oboInOwl properties. */
export const OWL_SYNONYMS: [string, string][] = [
  [OIO + 'hasExactSynonym', 'Exact'],
  [OIO + 'hasRelatedSynonym', 'Related'],
  [OIO + 'hasBroadSynonym', 'Broad'],
  [OIO + 'hasNarrowSynonym', 'Narrow'],
];
/** The property a new source of a definition or synonym is written with. */
export const SOURCE_PROPERTY = OIO + 'hasDbXref';
const SOURCE_PROPS = new Set([OIO + 'hasDbXref', DC + 'source', DCT + 'source', OIO + 'source']);

/** The graph name a statement carries for a selection's graph (null: the default graph). */
export const graphName = (graph: string | null) => graph ?? 'default';

/** Whether a statement or axiom is in the graph the page edits, so the page may change it. */
export const inGraph = (x: { graph?: string }, graph: string | null) =>
  (x.graph ?? 'default') === graphName(graph);

/** A value as an annotation's value: an IRI or a literal. */
const annValue = (v: Value): AnnValue => v;

/**
 * A statement of a page with the annotation assertions that state it, as the term route
 * maps them. The statement's own annotations merge those of every reification of its
 * triple and leave out nested ones, so an edit removes these axioms as the store keeps
 * them and not one rebuilt from the merged list.
 */
export type Stated = Statement & { asserted?: AxiomJson[] };

const bareKey = (a: AxiomJson) => {
  const { annotations: _, ...bare } = a;
  return axiomKey(bare);
};

/**
 * An assertion as the annotation assertion of the same triple. A property with a literal
 * that the ontology does not declare, such as `skos:definition` in a thesaurus without the
 * SKOS vocabulary, maps to a data property assertion, which states the same triple.
 */
const asAnnotation = (a: AxiomJson): AxiomJson | null =>
  a.type === 'AnnotationAssertion'
    ? a
    : a.type === 'DataPropertyAssertion'
      ? annotationAssertion(a.property as string, a.source as string, a.value as AnnValue)
      : null;

/** The assertions of `axioms` in `graph` that state `s` about `iri`, of either kind. */
export function assertionsOf(
  iri: string,
  s: Pick<Statement, 'property' | 'value'>,
  axioms: readonly AxiomItem[],
  graph: string | null,
): AxiomJson[] {
  const k = bareKey(annotationAssertion(s.property, iri, annValue(s.value)));
  return axioms
    .filter((a) => {
      const as = inGraph(a, graph) ? asAnnotation(a.axiom) : null;
      return !!as && bareKey(as) === k;
    })
    .map((a) => a.axiom);
}

/**
 * The assertion that states `property` and `value` about `iri`: of the kind the store keeps
 * for the statement it replaces when the property stays and the value is a literal, so
 * an undeclared property's triple is written as the same kind it was read as.
 */
function assertionLike(
  like: AxiomJson | undefined,
  iri: string,
  property: string,
  value: Value,
): AxiomJson {
  if (like?.type === 'DataPropertyAssertion' && like.property === property && isLiteral(value))
    return { type: 'DataPropertyAssertion', property, source: iri, value };
  return annotationAssertion(property, iri, annValue(value));
}

/** The statements with the assertions that state them in `graph`. */
export const withAssertions = (
  iri: string,
  statements: readonly Statement[],
  axioms: readonly AxiomItem[],
  graph: string | null,
): Stated[] =>
  statements.map((s) =>
    inGraph(s, graph) ? { ...s, asserted: assertionsOf(iri, s, axioms, graph) } : s,
  );

/** The axioms that state `s`: those the store keeps, or one rebuilt from the statement. */
const removalsOf = (iri: string, s: Stated): AxiomJson[] =>
  s.asserted?.length ? s.asserted : [statementAxiom(iri, s)];

/** The annotations of every axiom that states `s`, nested ones kept, each once. */
function annotationsOf(s: Stated): Annotation[] {
  if (!s.asserted?.length)
    return s.annotations.map((a) => ({ property: a.property, value: a.value }));
  const seen = new Set<string>();
  const out: Annotation[] = [];
  for (const a of s.asserted)
    for (const x of a.annotations ?? []) {
      const k = axiomKey({ type: 'a', annotations: [x] });
      if (!seen.has(k)) {
        seen.add(k);
        out.push(x);
      }
    }
  return out;
}

/** The annotation assertion that states a statement, with the statement's annotations. */
export function statementAxiom(
  iri: string,
  s: Pick<Statement, 'property' | 'value' | 'annotations'>,
) {
  const a = annotationAssertion(s.property, iri, annValue(s.value));
  return s.annotations.length
    ? { ...a, annotations: s.annotations.map((x) => ({ property: x.property, value: x.value })) }
    : a;
}

const headerValue = (v: Value): HeaderValue => (isLiteral(v) ? (v as Lit) : { iri: v as string });

/** A plain literal with a language tag, or none. */
export function textLit(text: string, lang: string): Lit {
  const l = lang.trim().toLowerCase();
  return l ? { '@value': text, '@language': l } : { '@value': text };
}

const sourceText = (v: AnnValue) => (isLiteral(v as Value) ? (v as Lit)['@value'] : (v as string));

/** The sources a statement's annotations name, each text once. */
export const sourcesOf = (s: Pick<Statement, 'annotations'> | null) => [
  ...new Set(
    (s?.annotations ?? [])
      .filter((a) => SOURCE_PROPS.has(a.property))
      .map((a) => sourceText(a.value)),
  ),
];

/** The sources of a form with the text typed in its add field and not added yet. */
export const withPending = (sources: string[], pending: string) => {
  const p = pending.trim();
  return p && !sources.includes(p) ? [...sources, p] : sources;
};

/**
 * A definition form's draft: its text, language and sources, and the statement it began
 * from, whose property and other annotations an addition of the draft keeps.
 */
export type DefinitionDraft = {
  text: string;
  lang: string;
  sources: string[];
  basis?: Stated | null;
};

/** A statement as a form edits it: a property, a value, and the sources of an OWL page. */
export type Draft = { property: string; value: Value; sources: string[] };

/**
 * The annotations of a changed statement: the old ones that name no source, kept as they
 * were, then the sources in the order the form lists them. A source the statement had keeps
 * its property and value, and a new one is an `oboInOwl:hasDbXref` string.
 */
function annotationsFor(before: Stated | null, sources: string[]): Annotation[] {
  const old = before ? annotationsOf(before) : [];
  const keep = old.filter((a) => !SOURCE_PROPS.has(a.property));
  const src = sources.flatMap((text) => {
    // every source annotation with this text, so `hasDbXref "X"` and `dc:source "X"` both stay
    const was = old.filter((a) => SOURCE_PROPS.has(a.property) && sourceText(a.value) === text);
    return was.length ? was : [{ property: SOURCE_PROPERTY, value: { '@value': text } as Lit }];
  });
  return [...keep, ...src];
}

/**
 * The operation that replaces the statement `before` with `after` about `iri` in `graph`:
 * an addition when `before` is null and a removal when `after` is null. On an OWL page it
 * removes and adds annotation assertions, which carry the sources as axiom annotations, and
 * on a SKOS page it deletes and inserts the triples, which have no sources. Null when it
 * would change nothing.
 */
export function statementOp(
  mode: EditMode,
  iri: string,
  before: Stated | null,
  after: Draft | null,
  graph: string | null,
  /** The statement a draft began from when it is gone, whose annotations an addition keeps. */
  basis?: Stated | null,
): Op | null {
  // a SKOS statement with axiom annotations, such as a definition's sources, is changed as
  // an annotation assertion, so its reification follows the new value
  // an annotated SKOS triple whose assertion the term route did not answer, such as one
  // past its limits, is changed in place, its reifications retargeted to the new value
  if (mode === 'skos' && before?.annotations.length && !before.asserted?.length)
    return { kind: 'update', sparql: retarget(iri, before, after, graph) };
  if (mode === 'skos' && !before?.annotations.length && !(!before && basis?.annotations.length)) {
    const remove: HeaderTriple[] = before
      ? [{ s: iri, p: before.property, o: headerValue(before.value) }]
      : [];
    const add: HeaderTriple[] = after
      ? [{ s: iri, p: after.property, o: headerValue(after.value) }]
      : [];
    if (sameTriples(remove, add)) return null;
    return { kind: 'update', sparql: dataUpdate(remove, add, graph).sparql };
  }
  const remove = before ? removalsOf(iri, before) : [];
  // a draft whose statement another writer changed keeps that statement's annotations
  const base = before ?? basis ?? null;
  const anns = after ? annotationsFor(base, after.sources) : [];
  const add: AxiomJson[] = after
    ? [
        {
          ...assertionLike(base?.asserted?.[0], iri, after.property, after.value),
          ...(anns.length ? { annotations: anns } : {}),
        },
      ]
    : [];
  const d = diffAxioms(remove, add);
  if (!d.add.length && !d.remove.length) return null;
  return { kind: 'axioms', add: d.add, remove: d.remove };
}

/**
 * The update that replaces the triple of `before` with that of `after`, or removes it, and
 * moves the reifications of the old triple to the new one or removes them with it.
 */
function retarget(iri: string, before: Statement, after: Draft | null, graph: string | null) {
  const wrap = (b: string) => (graph ? `GRAPH ${sparqlIri(graph)} { ${b} }` : b);
  const s = sparqlIri(iri);
  const oldP = sparqlIri(before.property);
  const oldO = termSparql(headerValue(before.value));
  const reified = `?a <${OWL}annotatedSource> ${s} ; <${OWL}annotatedProperty> ${oldP} ; <${OWL}annotatedTarget> ${oldO} .`;
  const del = `${s} ${oldP} ${oldO} .`;
  if (!after)
    return (
      `DELETE DATA { ${wrap(del)} } ;\n` +
      `DELETE { ${wrap('?a ?x ?y .')} } WHERE { ${wrap(`${reified} ?a ?x ?y .`)} }`
    );
  const newP = sparqlIri(after.property);
  const newO = termSparql(headerValue(after.value));
  return (
    `DELETE DATA { ${wrap(del)} } ;\nINSERT DATA { ${wrap(`${s} ${newP} ${newO} .`)} } ;\n` +
    `DELETE { ${wrap(`?a <${OWL}annotatedProperty> ${oldP} ; <${OWL}annotatedTarget> ${oldO} .`)} } ` +
    `INSERT { ${wrap(`?a <${OWL}annotatedProperty> ${newP} ; <${OWL}annotatedTarget> ${newO} .`)} } ` +
    `WHERE { ${wrap(reified)} }`
  );
}

const tripleKey = (t: HeaderTriple) => JSON.stringify([t.s, t.p, t.o]);
const sameTriples = (a: HeaderTriple[], b: HeaderTriple[]) =>
  a.length === b.length && a.every((t, i) => tripleKey(t) === tripleKey(b[i]));

// --- annotations ------------------------------------------------------------------------

/** Properties of the RDF, RDFS and OWL vocabularies that are annotations. */
const BUILT_IN_ANNOTATIONS = new Set([
  RDFS + 'label',
  RDFS + 'comment',
  RDFS + 'seeAlso',
  RDFS + 'isDefinedBy',
  OWL + 'deprecated',
  OWL + 'versionInfo',
  OWL + 'priorVersion',
  OWL + 'backwardCompatibleWith',
  OWL + 'incompatibleWith',
]);

/** SKOS properties that link resources or that other sections of a SKOS page edit. */
const SKOS_STRUCTURE = new Set(
  [
    'prefLabel',
    'altLabel',
    'hiddenLabel',
    'broader',
    'narrower',
    'related',
    'broaderTransitive',
    'narrowerTransitive',
    'inScheme',
    'topConceptOf',
    'hasTopConcept',
    'member',
    'memberList',
    'exactMatch',
    'closeMatch',
    'broadMatch',
    'narrowMatch',
    'relatedMatch',
    'mappingRelation',
    'semanticRelation',
  ].map((l) => SKOS + l),
);
const SKOS_XL = 'http://www.w3.org/2008/05/skos-xl#';

/**
 * Whether a statement is an annotation the Annotations section edits: not an axiom of the
 * RDF, RDFS or OWL vocabularies, not a fact of a declared object or data property, not a
 * SKOS link or label, and not one of the properties in `owned`, which other sections edit.
 */
export function isAnnotation(
  s: Pick<Statement, 'property' | 'value'>,
  owned: ReadonlySet<string>,
  kindsOf: (iri: string) => readonly string[] | undefined,
): boolean {
  const p = s.property;
  if (owned.has(p)) return false;
  if (p === RDF + 'type') return false;
  if (!BUILT_IN_ANNOTATIONS.has(p) && [RDF, RDFS, OWL].some((ns) => p.startsWith(ns))) return false;
  if (SKOS_STRUCTURE.has(p) || p.startsWith(SKOS_XL)) return false;
  const k = kindsOf(p);
  if (k?.some((x) => x === 'objectProperty' || x === 'dataProperty')) return false;
  return true;
}

/** A statement as the editor's annotation rows show it, with every annotation it carries. */
export const toAnnotation = (s: Stated): Annotation => {
  const anns = annotationsOf(s);
  return {
    property: s.property,
    value: annValue(s.value),
    ...(anns.length ? { annotations: anns } : {}),
  };
};

/**
 * The operation that turns the annotations `before` (statements of the edited graph) into
 * the rows `next`, as the editor's annotation rows hand them back. Null when nothing changed.
 */
export function annotationsOp(
  mode: EditMode,
  iri: string,
  before: Stated[],
  next: Annotation[],
  graph: string | null,
): Op | null {
  const nextKeys = new Set(next.map((a) => axiomKey(rowAxiom(iri, a))));
  const removed = before.filter((s) => !nextKeys.has(axiomKey(rowAxiom(iri, toAnnotation(s)))));
  if (mode === 'skos' && !removed.some((s) => s.annotations.length)) {
    const t = (a: { property: string; value: AnnValue }): HeaderTriple => ({
      s: iri,
      p: a.property,
      o: headerValue(a.value as Value),
    });
    const b = new Map(before.map((s) => [tripleKey(t(s)), t(s)]));
    const n = new Map(next.map((a) => [tripleKey(t(a)), t(a)]));
    const remove = [...b].filter(([k]) => !n.has(k)).map(([, x]) => x);
    const add = [...n].filter(([k]) => !b.has(k)).map(([, x]) => x);
    if (!remove.length && !add.length) return null;
    return { kind: 'update', sparql: dataUpdate(remove, add, graph).sparql };
  }
  // a row the caller left as it was keeps the axioms that state it, however many there are
  const beforeKeys = new Set(before.map((s) => axiomKey(rowAxiom(iri, toAnnotation(s)))));
  const remove = removed.flatMap((s) => removalsOf(iri, s));
  const add = next
    .filter((a) => !beforeKeys.has(axiomKey(rowAxiom(iri, a))))
    .map((a) => rowAxiom(iri, a));
  if (!add.length && !remove.length) return null;
  return { kind: 'axioms', add, remove };
}

/** The annotation assertion a row of the annotation rows states. */
const rowAxiom = (iri: string, a: Annotation): AxiomJson => ({
  ...annotationAssertion(a.property, iri, a.value),
  ...(a.annotations?.length ? { annotations: a.annotations } : {}),
});

// --- the hierarchy ----------------------------------------------------------------------

/** The axiom type that makes one entity of a hierarchy's kind the child of another. */
const PARENT_AXIOM: Record<string, [string, string, string]> = {
  class: ['SubClassOf', 'subClass', 'superClass'],
  objectProperty: ['SubObjectPropertyOf', 'subProperty', 'superProperty'],
  dataProperty: ['SubDataPropertyOf', 'subProperty', 'superProperty'],
  annotationProperty: ['SubAnnotationPropertyOf', 'subProperty', 'superProperty'],
};

/** Whether the browser's hierarchy of `kind` has parents an OWL page can edit. */
export const parentsEditable = (kind: string | null) => !!kind && kind in PARENT_AXIOM;

/** The axiom that states `parent` as a parent of `iri` in the hierarchy of `kind`. */
export function parentAxiom(kind: string, iri: string, parent: string): AxiomJson | null {
  const t = PARENT_AXIOM[kind];
  return t ? { type: t[0], [t[1]]: iri, [t[2]]: parent } : null;
}

/** The asserted axiom of `axioms` that makes `parent` a parent of `iri`, if there is one. */
export function findParentAxiom(
  kind: string,
  iri: string,
  parent: string,
  axioms: AxiomItem[],
): AxiomItem | null {
  const t = PARENT_AXIOM[kind];
  if (!t) return null;
  return (
    axioms.find((a) => a.type === t[0] && a.axiom[t[1]] === iri && a.axiom[t[2]] === parent) ?? null
  );
}

/**
 * The parents of `iri` among the hierarchy's ancestors, each with the axiom of the edited
 * graph that states it, or null when another graph states it, so the page cannot remove it.
 */
export function parentRows(
  kind: string,
  iri: string,
  edges: readonly [string, string][],
  axioms: AxiomItem[],
  graph: string | null,
): { iri: string; axiom: AxiomItem | null }[] {
  const seen = new Set<string>();
  const out: { iri: string; axiom: AxiomItem | null }[] = [];
  for (const [c, p] of edges) {
    if (c !== iri || seen.has(p)) continue;
    seen.add(p);
    const a = findParentAxiom(kind, iri, p, axioms);
    out.push({ iri: p, axiom: a && inGraph(a, graph) ? a : null });
  }
  return out;
}

/** The operation that adds or removes one axiom, as written or as found. */
export const axiomOp = (a: AxiomJson | string, remove: boolean): Op => ({
  kind: 'axioms',
  add: remove ? [] : [a],
  remove: remove ? [a] : [],
});

/** How a form hands its change to the page, which previews it; `after` runs on commit. */
export type OnEdit = (op: Op, label: string, after?: () => void) => void;

/** A statement's identity across reloads: its graph, property, value and annotations. */
export const statementKey = (s: Statement) =>
  JSON.stringify([s.graph, s.property, s.value, s.annotations]);

/** The text and language of a literal value, or the IRI of another. */
export const valueParts = (v: Value): { text: string; lang: string } =>
  isLiteral(v)
    ? { text: v['@value'], lang: v['@language'] ?? '' }
    : { text: v as string, lang: '' };
