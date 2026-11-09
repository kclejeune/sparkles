// Bulk edits of the ontology editor (ROADMAP item 626, docs/specs/U12-ontology-workbench/
// SPEC.md, "Patterns, imports, rendering and bulk edits"): find and replace across labels
// and annotations, setting or removing an annotation on many entities, and moving many
// classes under a new parent. Each builds one `axioms` operation from the axioms the editor
// read at one commit, which the editor previews and commits like any other edit, so it is one
// commit on the undo stack. Every function here is pure; the component reads the axioms.
import type { EntityKind } from './ontology';
import {
  annotationAssertion,
  axiomKey,
  isLit,
  resolveInput,
  type AnnValue,
  type AxiomItem,
  type AxiomJson,
  type Lit,
  type Op,
} from './ontology';
import { sparqlIri, WELL_KNOWN, type PrefixMap } from './rdf';

const RDFS = WELL_KNOWN.rdfs;
const SKOS = WELL_KNOWN.skos;
const XSD = WELL_KNOWN.xsd;
const RDF = WELL_KNOWN.rdf;
const OWL_THING = WELL_KNOWN.owl + 'Thing';

/**
 * The most changes one bulk edit makes: annotation values replaced, added or removed, or
 * classes moved. Above it the editor refuses the edit and asks for a narrower scope, so the
 * preview stays readable and the edit's answer lists every axiom it changed.
 */
export const BULK_LIMIT = 1_000;
/**
 * The most axioms one bulk edit adds and removes together, which keeps the edit's answer
 * listing every axiom it changed. Only a move can reach it, through classes with many parents.
 */
export const BULK_AXIOM_LIMIT = 10_000;
const fmt = (n: number) => n.toLocaleString('en-US');
/** The most entities the entity list holds. */
export const BULK_ENTITY_LIMIT = BULK_LIMIT;
/**
 * The most axioms of one type a bulk edit reads from the graph. A graph with more is
 * refused rather than edited from a partial read, which could miss matches.
 */
export const BULK_SCAN_LIMIT = 100_000;

/** The annotation properties that the Labels scope covers. */
export const LABEL_SCOPE = [
  RDFS + 'label',
  SKOS + 'prefLabel',
  SKOS + 'altLabel',
  SKOS + 'hiddenLabel',
];

// --- matching ---------------------------------------------------------------------------

/** Which annotation properties a find covers: labels, every property, or one property. */
export type PropertyScope = { kind: 'labels' } | { kind: 'all' } | { kind: 'one'; iri: string };

/** Which languages a find or a removal covers: any, untagged values only, or one tag. */
export type LangFilter = { kind: 'any' } | { kind: 'none' } | { kind: 'tag'; tag: string };

export type FindSpec = {
  find: string;
  replace: string;
  regex: boolean;
  caseSensitive: boolean;
  properties: PropertyScope;
  lang: LangFilter;
  /** The entities whose annotations are searched, or null for every subject of the graph. */
  entities: ReadonlySet<string> | null;
};

/** The pattern of a find, or the reason it cannot be used. */
export function compileFind(
  spec: Pick<FindSpec, 'find' | 'regex' | 'caseSensitive'>,
): RegExp | string {
  if (!spec.find) return 'Type the text to find.';
  const flags = 'gu' + (spec.caseSensitive ? '' : 'i');
  const source = spec.regex ? spec.find : spec.find.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  let re: RegExp;
  try {
    re = new RegExp(source, flags);
  } catch (e) {
    return `The regular expression is not valid: ${e instanceof Error ? e.message : String(e)}`;
  }
  // a pattern that matches the empty string would insert the replacement between every character
  re.lastIndex = 0;
  if (re.test('')) return 'The pattern matches empty text. Make it match at least one character.';
  return re;
}

/** Whether a language tag passes the filter, by basic filtering of RFC 4647 for a tag. */
export function langMatches(tag: string | undefined, f: LangFilter): boolean {
  if (f.kind === 'any') return true;
  if (f.kind === 'none') return !tag;
  if (!tag) return false;
  const want = f.tag.trim().toLowerCase();
  const have = tag.toLowerCase();
  return have === want || have.startsWith(want + '-');
}

/** Whether a property is inside the find's property scope. */
export function propertyInScope(p: string, s: PropertyScope): boolean {
  if (s.kind === 'all') return true;
  if (s.kind === 'labels') return LABEL_SCOPE.includes(p);
  return p === s.iri;
}

/** A plain or language-tagged string, the literals whose text a replace may change. */
export function isTextLiteral(v: unknown): v is Lit {
  if (!isLit(v)) return false;
  const t = v['@type'];
  return !t || t === XSD + 'string' || t === RDF + 'langString';
}

/** One annotation value that a bulk edit changes: the axiom as stored and its replacement. */
export type Change = {
  subject: string;
  property: string;
  /** The axiom as the graph holds it, its own annotations included, or null for an addition. */
  before: AxiomJson | null;
  /** The axiom that replaces it, or null when the change only removes it. */
  after: AxiomJson | null;
};

export type FindResult = {
  changes: Change[];
  /** Values the replacement would leave empty, which are left out. */
  emptied: number;
  /**
   * A value in which the pattern matches empty text, such as a lookahead or a word boundary,
   * where the replacement would be inserted between characters. The find is then refused.
   */
  zeroWidth: string | null;
};

const assertionOf = (it: AxiomItem | AxiomJson): AxiomJson | null => {
  const a =
    'axiom' in it && typeof it.axiom === 'object' ? (it.axiom as AxiomJson) : (it as AxiomJson);
  return a.type === 'AnnotationAssertion' &&
    typeof a.subject === 'string' &&
    typeof a.property === 'string'
    ? a
    : null;
};

/**
 * The replacements of a find over the annotation assertions `axioms`. Only plain and
 * language-tagged strings are changed, keeping their language and their own annotations.
 */
export function findMatches(
  axioms: Iterable<AxiomItem | AxiomJson>,
  spec: FindSpec,
  re: RegExp,
): FindResult {
  const changes: Change[] = [];
  const seen = new Set<string>();
  let emptied = 0;
  let zeroWidth: string | null = null;
  for (const it of axioms) {
    const a = assertionOf(it);
    if (!a) continue;
    const subject = a.subject as string;
    const property = a.property as string;
    if (spec.entities && !spec.entities.has(subject)) continue;
    if (!propertyInScope(property, spec.properties)) continue;
    const v = a.value;
    if (!isTextLiteral(v) || !langMatches(v['@language'], spec.lang)) continue;
    re.lastIndex = 0;
    if (!re.test(v['@value'])) continue;
    re.lastIndex = 0;
    if ([...v['@value'].matchAll(re)].some((m) => m[0] === '')) {
      zeroWidth ??= v['@value'];
      continue;
    }
    re.lastIndex = 0;
    const text = spec.regex
      ? v['@value'].replace(re, spec.replace)
      : v['@value'].replace(re, () => spec.replace);
    if (text === v['@value']) continue;
    if (!text.trim()) {
      emptied++;
      continue;
    }
    const k = axiomKey(a);
    if (seen.has(k)) continue;
    seen.add(k);
    changes.push({
      subject,
      property,
      before: a,
      after: { ...a, value: { ...v, '@value': text } },
    });
  }
  return { changes, emptied, zeroWidth };
}

// --- set and remove ----------------------------------------------------------------------

export type SetMode = 'add' | 'replace';

/**
 * Set `property` to `value` on each of `entities`. `add` keeps the values they hold, and
 * `replace` also removes their other values of the property, in the value's language for a
 * literal with one. An entity that already holds exactly the value is left as it is.
 */
export function setAnnotation(
  entities: readonly string[],
  property: string,
  value: AnnValue,
  mode: SetMode,
  existing: Iterable<AxiomItem | AxiomJson>,
): { changes: Change[]; unchanged: number } {
  const want = new Set(entities);
  const held = new Map<string, AxiomJson[]>();
  for (const it of existing) {
    const a = assertionOf(it);
    if (!a || a.property !== property || !want.has(a.subject as string)) continue;
    const l = held.get(a.subject as string) ?? [];
    l.push(a);
    held.set(a.subject as string, l);
  }
  const newKey = valueKey(value);
  const sameSlot = (v: AnnValue) =>
    typeof value === 'string'
      ? true
      : isLit(v) &&
        (v['@language'] ?? '').toLowerCase() === (value['@language'] ?? '').toLowerCase();
  const changes: Change[] = [];
  let unchanged = 0;
  for (const s of new Set(entities)) {
    const mine = held.get(s) ?? [];
    const has = mine.some((a) => valueKey(a.value as AnnValue) === newKey);
    const drop =
      mode === 'replace'
        ? mine.filter(
            (a) => valueKey(a.value as AnnValue) !== newKey && sameSlot(a.value as AnnValue),
          )
        : [];
    if (has && !drop.length) {
      unchanged++;
      continue;
    }
    for (const a of drop) changes.push({ subject: s, property, before: a, after: null });
    if (!has)
      changes.push({
        subject: s,
        property,
        before: null,
        after: annotationAssertion(property, s, value),
      });
  }
  return { changes, unchanged };
}

/**
 * The identity of an annotation value as RDF compares it: an IRI, or a literal's text with its
 * language tag in lower case, since tags compare without case, and its datatype, with a plain
 * literal and an `xsd:string` the same.
 */
export function valueKey(v: AnnValue): string {
  if (typeof v === 'string') return JSON.stringify(['iri', v]);
  const lang = v['@language']?.toLowerCase();
  const type = lang ? RDF + 'langString' : (v['@type'] ?? XSD + 'string');
  return JSON.stringify(['lit', v['@value'], lang ?? '', type]);
}

/** Which values a removal takes: every value, those in a language, or one exact value. */
export type RemoveFilter =
  | { kind: 'any' }
  | { kind: 'lang'; lang: LangFilter }
  | { kind: 'value'; text: string; iri: boolean };

/** Remove the annotations of `property` that pass `filter` from each of `entities`. */
export function removeAnnotation(
  entities: readonly string[],
  property: string,
  filter: RemoveFilter,
  existing: Iterable<AxiomItem | AxiomJson>,
): Change[] {
  const want = new Set(entities);
  const out: Change[] = [];
  const seen = new Set<string>();
  for (const it of existing) {
    const a = assertionOf(it);
    if (!a || a.property !== property || !want.has(a.subject as string)) continue;
    const v = a.value as AnnValue;
    if (filter.kind === 'lang' && !(isLit(v) && langMatches(v['@language'], filter.lang))) continue;
    // an IRI value matches only an IRI, and a text value only a literal of that text
    if (
      filter.kind === 'value' &&
      (filter.iri ? v !== filter.text : !isLit(v) || v['@value'] !== filter.text)
    )
      continue;
    const k = axiomKey(a);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ subject: a.subject as string, property, before: a, after: null });
  }
  return out;
}

/**
 * The one operation of a set of annotation changes. The server declares an annotation
 * property the ontology does not declare yet, so its values read back as annotations; the
 * caller refuses a property that the ontology declares as another kind first.
 */
export function changesOp(changes: readonly Change[]): Op {
  const remove: AxiomJson[] = [];
  const add: AxiomJson[] = [];
  for (const c of changes) {
    if (c.before) remove.push(c.before);
    if (c.after) add.push(c.after);
  }
  return { kind: 'axioms', add, remove };
}

// --- moving classes ------------------------------------------------------------------------

/** The named superclasses of each class that the `SubClassOf` axioms state, with the axioms. */
export function namedParents(axioms: Iterable<AxiomItem | AxiomJson>): Map<string, AxiomJson[]> {
  const out = new Map<string, AxiomJson[]>();
  for (const it of axioms) {
    const a =
      'axiom' in it && typeof it.axiom === 'object' ? (it.axiom as AxiomJson) : (it as AxiomJson);
    if (
      a.type !== 'SubClassOf' ||
      typeof a.subClass !== 'string' ||
      typeof a.superClass !== 'string'
    )
      continue;
    const l = out.get(a.subClass) ?? [];
    l.push(a);
    out.set(a.subClass, l);
  }
  return out;
}

/** Every class below each of `roots` through named `SubClassOf` axioms, the roots left out. */
export function descendants(
  parents: Map<string, AxiomJson[]>,
  roots: Iterable<string>,
): Set<string> {
  const children = new Map<string, string[]>();
  for (const [c, as] of parents)
    for (const a of as) {
      const p = a.superClass as string;
      const l = children.get(p) ?? [];
      l.push(c);
      children.set(p, l);
    }
  const start = new Set(roots);
  const out = new Set<string>();
  const stack = [...start];
  while (stack.length) {
    const x = stack.pop()!;
    for (const c of children.get(x) ?? [])
      if (!out.has(c) && !start.has(c)) {
        out.add(c);
        stack.push(c);
      }
  }
  return out;
}

export type MovePlan =
  | { ok: true; op: Op; moved: string[]; unchanged: string[] }
  | { ok: false; reason: string };

/**
 * Move `classes` under `parent`, or to the top of the hierarchy when `parent` is null. The
 * edit declares nothing, so a listed IRI is never declared a class by the move. Each
 * class loses the named superclasses its `SubClassOf` axioms in the graph state, with those
 * axioms' annotations, unless `keep`, and gains `SubClassOf(class, parent)`. Superclass
 * expressions such as restrictions stay. A parent that is one of the classes or below one is
 * refused, since the move would make a cycle.
 */
export function planMove(
  classes: readonly string[],
  parent: string | null,
  subClassAxioms: Iterable<AxiomItem | AxiomJson>,
  keep: boolean,
  name: (iri: string) => string = (i) => i,
): MovePlan {
  const set = [...new Set(classes)];
  if (!set.length) return { ok: false, reason: 'Add the classes to move to the entity list.' };
  if (parent === OWL_THING) parent = null;
  if (parent === null && keep)
    return { ok: false, reason: 'Moving to the top keeps no parents, so it cannot keep them.' };
  const parents = namedParents(subClassAxioms);
  if (parent !== null) {
    if (set.includes(parent))
      return { ok: false, reason: `${name(parent)} is one of the classes to move.` };
    const below = descendants(parents, set);
    if (below.has(parent)) {
      const under = set.find((c) => descendants(parents, [c]).has(parent!))!;
      return {
        ok: false,
        reason: `${name(parent)} is below ${name(under)}, so the move would make a cycle.`,
      };
    }
  }
  const add: AxiomJson[] = [];
  const remove: AxiomJson[] = [];
  const moved: string[] = [];
  const unchanged: string[] = [];
  for (const c of set) {
    const stated = parents.get(c) ?? [];
    const has = parent !== null && stated.some((a) => a.superClass === parent);
    const drop = keep ? [] : stated.filter((a) => a.superClass !== parent);
    if ((parent === null || has) && !drop.length) {
      unchanged.push(c);
      continue;
    }
    remove.push(...drop);
    if (parent !== null && !has) add.push({ type: 'SubClassOf', subClass: c, superClass: parent });
    moved.push(c);
  }
  if (!moved.length)
    return { ok: false, reason: 'Every class is already where the move would put it.' };
  if (moved.length > BULK_LIMIT)
    return { ok: false, reason: overLimit(moved.length, 'class', 'classes') };
  if (add.length + remove.length > BULK_AXIOM_LIMIT)
    return {
      ok: false,
      reason: `The move would change ${fmt(add.length + remove.length)} SubClassOf axioms, more than the ${fmt(BULK_AXIOM_LIMIT)} that one bulk edit may change. Move fewer classes at once.`,
    };
  return { ok: true, op: { kind: 'axioms', add, remove, declare: false }, moved, unchanged };
}

// --- the entity list ----------------------------------------------------------------------

/**
 * The IRIs that pasted text names, one per line or separated by spaces or commas: IRIs, in
 * angle brackets or not, and prefixed names. Lines that name nothing are returned as bad.
 */
export function parseEntityList(
  text: string,
  prefixes: PrefixMap,
): { iris: string[]; bad: string[] } {
  const iris: string[] = [];
  const bad: string[] = [];
  for (const raw of text.split(/[\s,]+/)) {
    const s = raw.trim();
    if (!s) continue;
    const r = resolveInput(s, [], prefixes);
    if (typeof r === 'string') iris.push(r);
    else bad.push(s);
  }
  return { iris: [...new Set(iris)], bad };
}

/** The refusal of an edit above the bound. */
export function overLimit(n: number, one: string, many: string): string {
  return `The edit would change ${n.toLocaleString('en-US')} ${n === 1 ? one : many}, more than the ${BULK_LIMIT.toLocaleString('en-US')} that one bulk edit may change. Narrow the scope and run it in parts.`;
}

/** The literal or IRI a set or removal form describes. */
export function valueOf(
  text: string,
  lang: string,
  datatype: string,
  iri: boolean,
  prefixes: PrefixMap,
): AnnValue | null {
  if (iri) {
    const r = resolveInput(text, [], prefixes);
    return typeof r === 'string' ? r : null;
  }
  if (!text) return null;
  if (lang.trim()) return { '@value': text, '@language': lang.trim() };
  if (datatype && datatype !== XSD + 'string') return { '@value': text, '@type': datatype };
  return { '@value': text };
}

// --- kinds of listed entities ----------------------------------------------------------------

const OWL = WELL_KNOWN.owl;
/** The kind each OWL or RDFS type gives its instances. Any other type makes an individual. */
const TYPE_KIND: Record<string, EntityKind | null> = {
  [OWL + 'Class']: 'Class',
  [RDFS + 'Class']: 'Class',
  [OWL + 'ObjectProperty']: 'ObjectProperty',
  [OWL + 'DatatypeProperty']: 'DataProperty',
  [OWL + 'AnnotationProperty']: 'AnnotationProperty',
  [RDFS + 'Datatype']: 'Datatype',
  [OWL + 'NamedIndividual']: 'NamedIndividual',
  // characteristics and other vocabulary types say nothing beyond the declarations above
  [RDF + 'Property']: null,
  [OWL + 'FunctionalProperty']: null,
  [OWL + 'InverseFunctionalProperty']: null,
  [OWL + 'TransitiveProperty']: null,
  [OWL + 'SymmetricProperty']: null,
  [OWL + 'AsymmetricProperty']: null,
  [OWL + 'ReflexiveProperty']: null,
  [OWL + 'IrreflexiveProperty']: null,
  [OWL + 'Ontology']: null,
  [OWL + 'DeprecatedClass']: null,
  [OWL + 'DeprecatedProperty']: null,
  [OWL + 'Restriction']: null,
  [OWL + 'AllDisjointClasses']: null,
};

/** Jena's name for the stored default graph, which a query reads even under a union default. */
export const DEFAULT_GRAPH_IRI = 'urn:x-arq:DefaultGraph';

/**
 * The SPARQL that reads the `rdf:type` values of `iris` in `graphs`, the graph names the
 * ontology read with `default` for the stored default graph.
 */
export function typesQuery(iris: readonly string[], graphs: readonly string[]): string {
  const gs = graphs.map((g) => sparqlIri(g === 'default' ? DEFAULT_GRAPH_IRI : g)).join(' ');
  const ss = iris.map((i) => sparqlIri(i)).join(' ');
  return `SELECT DISTINCT ?s ?t WHERE { VALUES ?g { ${gs} } VALUES ?s { ${ss} } GRAPH ?g { ?s a ?t } FILTER(isIRI(?t)) }`;
}

/** The kinds that `rdf:type` values give each subject, as the ontology's lenient read does. */
export function kindsOfTypes(rows: { s: string; t: string }[]): Map<string, EntityKind[]> {
  const out = new Map<string, Set<EntityKind>>();
  for (const { s, t } of rows) {
    const k = t in TYPE_KIND ? TYPE_KIND[t] : 'NamedIndividual';
    if (!k) continue;
    if (!out.has(s)) out.set(s, new Set());
    out.get(s)!.add(k);
  }
  return new Map([...out].map(([s, ks]) => [s, [...ks]]));
}
