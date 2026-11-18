// The rows of a materialized frame (ROADMAP item 622) as the clauses of the entity's frame:
// each inferred axiom in the section the editor's frame would give it, rendered as
// Manchester tokens with named links. Rows are a recorded profile's facts, never
// classifier proofs, and nothing here derives or hides a fact beyond the trivial ones it
// counts. No Svelte here.

import {
  OBJECT_CHARACTERISTICS,
  OWL_THING,
  SECTIONS,
  manchesterTokens,
  sectionOf,
  type AxiomItem,
  type AxiomJson,
  type EntityKind,
  type ManchesterToken,
} from './ontology';
import { WELL_KNOWN, type PrefixMap } from './rdf';

const OWL = WELL_KNOWN.owl;

/** The Manchester keyword of each frame section, and the editor's title for the rest. */
const WORDS: Record<string, string> = {
  sub: 'SubClassOf',
  equivalent: 'EquivalentTo',
  disjoint: 'DisjointWith',
  domains: 'Domain',
  ranges: 'Range',
  superProperties: 'SubPropertyOf',
  inverses: 'InverseOf',
  chains: 'SubPropertyChain',
  types: 'Types',
  same: 'SameAs',
  different: 'DifferentFrom',
  objectAssertions: 'Facts',
  dataAssertions: 'Facts',
  negative: 'Facts',
};

/** The classes every individual and class of a profile falls under. */
const TOP_CLASSES = new Set([OWL_THING, WELL_KNOWN.rdfs + 'Resource']);
/** The top property of each kind of property subsumption, which no other role shares. */
const TOP_PROPERTY: Record<string, string> = {
  SubObjectPropertyOf: OWL + 'topObjectProperty',
  SubDataPropertyOf: OWL + 'topDataProperty',
};
/** The Manchester word of each characteristic axiom. */
const CHARACTERISTICS = new Map<string, string>([
  ...OBJECT_CHARACTERISTICS,
  ['FunctionalDataProperty', 'Functional'],
]);

export type MaterializedRow = {
  tokens: ManchesterToken[];
  ofn: string;
  /** an asserted axiom of the frame states the same axiom */
  alsoAsserted: boolean;
};
export type MaterializedClause = { id: string; word: string; rows: MaterializedRow[] };
export type MaterializedClauses = {
  clauses: MaterializedClause[];
  /** axioms about the entity of a form no clause takes, shown in functional-style syntax */
  other: MaterializedRow[];
  /**
   * Entailments every entity of the profile has, such as a property's subproperty relation
   * to itself, a type of `owl:Thing` or a subproperty relation to the top property of the
   * same kind, which the frame counts instead of listing.
   */
  trivial: number;
};

/** Whether an inferred axiom says only what holds of every entity of its kind. */
export function trivialAxiom(a: AxiomJson): boolean {
  switch (a.type) {
    case 'SubClassOf':
      return (
        a.subClass === a.superClass ||
        (typeof a.superClass === 'string' && TOP_CLASSES.has(a.superClass))
      );
    case 'SubObjectPropertyOf':
    case 'SubDataPropertyOf':
      return (
        !a.chain && (a.subProperty === a.superProperty || a.superProperty === TOP_PROPERTY[a.type])
      );
    case 'EquivalentClasses':
    case 'EquivalentObjectProperties':
    case 'EquivalentDataProperties':
    case 'SameIndividual': {
      const members = (a.classes ?? a.properties ?? a.individuals) as unknown[] | undefined;
      return Array.isArray(members) && new Set(members.map((m) => JSON.stringify(m))).size < 2;
    }
    case 'ClassAssertion':
      return typeof a.class === 'string' && TOP_CLASSES.has(a.class);
    default:
      return false;
  }
}

/**
 * The inferred axioms of `items` about `iri` as frame clauses in the order of the editor's
 * sections. `asserted` holds the functional-style text of the asserted axioms, which
 * marks a row that a materialization also stores.
 */
export function materializedClauses(
  kind: EntityKind,
  iri: string,
  items: readonly AxiomItem[],
  asserted: ReadonlySet<string>,
  name: (iri: string) => string,
  prefixes: PrefixMap,
): MaterializedClauses {
  const order = SECTIONS[kind].map(([id]) => id);
  const titles = new Map(SECTIONS[kind]);
  const byId = new Map<string, MaterializedClause>();
  const other: MaterializedRow[] = [];
  let trivial = 0;
  const toks = (v: unknown) => manchesterTokens(v, name, prefixes);
  const space: ManchesterToken = { text: ' ', role: 'space' };
  for (const item of items) {
    const a = item.axiom;
    if (trivialAxiom(a)) {
      trivial++;
      continue;
    }
    const alsoAsserted = asserted.has(item.ofn);
    const characteristic = CHARACTERISTICS.get(a.type);
    if (characteristic && a.property === iri) {
      let clause = byId.get('characteristics');
      if (!clause)
        byId.set(
          'characteristics',
          (clause = { id: 'characteristics', word: 'Characteristics', rows: [] }),
        );
      clause.rows.push({
        tokens: [
          { text: characteristic.replace(/ (\w)/g, (_, c: string) => c.toUpperCase()), role: 'kw' },
        ],
        ofn: item.ofn,
        alsoAsserted,
      });
      continue;
    }
    // Declarations and annotations have no clause here, and keep their functional form.
    const hit = sectionOf(kind, iri, a);
    if (!hit) {
      other.push({ tokens: [], ofn: item.ofn, alsoAsserted });
      continue;
    }
    const [id, values] = hit;
    const word = WORDS[id] ?? titles.get(id) ?? id;
    let clause = byId.get(id);
    if (!clause) byId.set(id, (clause = { id, word, rows: [] }));
    if (id === 'objectAssertions' || id === 'dataAssertions' || id === 'negative') {
      const not: ManchesterToken[] = id === 'negative' ? [{ text: 'not', role: 'kw' }, space] : [];
      clause.rows.push({
        tokens: [...not, ...toks(values[0]), space, ...toks(values[1])],
        ofn: item.ofn,
        alsoAsserted,
      });
    } else {
      for (const v of values) clause.rows.push({ tokens: toks(v), ofn: item.ofn, alsoAsserted });
    }
  }
  // Characteristics follow the sections, as in the asserted frame.
  const rank = (id: string) => (order.includes(id) ? order.indexOf(id) : order.length);
  const clauses = [...byId.values()].sort((x, y) => rank(x.id) - rank(y.id));
  return { clauses, other, trivial };
}
