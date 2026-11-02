/**
 * Typed constructors for OWL 2 axioms and expressions (spec F21 §2.2), exported as the
 * `owl` namespace. Each constructor is named after its functional-style form and takes
 * its operands in the order the OWL 2 structural specification gives them. Axiom
 * constructors answer plain objects in the axioms' JSON form, which an edit takes, and
 * expressions are objects of the same form.
 *
 * An IRI can be given as a string, an RDF/JS named node or an entity made by
 * {@link Class}, {@link ObjectProperty} and the other entity constructors. A literal can be
 * given as {@link Literal}, an RDF/JS literal, a string (a plain string), a boolean, a
 * number or a bigint. The value of an annotation is a literal unless it is an {@link IRI},
 * a named node or an anonymous individual.
 *
 * ```ts
 * import { owl } from '@sparkles-rdf/engine';
 * const ax = owl.SubClassOf(
 *   owl.Class('http://ex.org/Professor'),
 *   owl.ObjectSomeValuesFrom(owl.ObjectProperty('http://ex.org/teaches'), owl.Class('http://ex.org/Course')),
 * );
 * await dataset.ontology.edit({ add: [ax] });
 * ```
 */
import type { OwlAxiom } from './admin.js';

const XSD_NS = 'http://www.w3.org/2001/XMLSchema#';
/** The XML Schema namespace, for datatypes and facets. */
export const XSD = XSD_NS;

/** The kinds of OWL 2 entities. */
export type EntityKind =
  | 'Class'
  | 'Datatype'
  | 'ObjectProperty'
  | 'DataProperty'
  | 'AnnotationProperty'
  | 'NamedIndividual';
/** An entity: its kind and IRI. */
export interface OwlEntity {
  kind: EntityKind;
  iri: string;
}
/** An IRI where a string would be a literal, as an annotation value. */
export interface OwlIri {
  kind: 'IRI';
  iri: string;
}
/** An IRI argument. */
export type IriArg = string | OwlEntity | OwlIri | { termType: 'NamedNode'; value: string };
/** A literal in the JSON form. */
export interface OwlLiteral {
  '@value': string;
  '@type'?: string;
  '@language'?: string;
}
/** A literal argument. */
export type LiteralArg =
  | OwlLiteral
  | string
  | boolean
  | number
  | bigint
  | { termType: 'Literal'; value: string; language: string; datatype: { value: string } };
/** A class expression, a data range or another expression in the JSON form. */
export type OwlExpression = string | { [member: string]: unknown };
/** An annotation in the JSON form. */
export interface OwlAnnotation {
  property: string;
  value: unknown;
  annotations?: OwlAnnotation[];
}
/** The options every axiom constructor takes. */
export interface AxiomOptions {
  annotations?: OwlAnnotation[];
}

function iri(x: IriArg): string {
  if (typeof x === 'string') {
    if (x.startsWith('_:')) throw new TypeError(`expected an IRI, not the blank node ${x}`);
    return x;
  }
  if (x && typeof x === 'object') {
    if ('termType' in x && x.termType === 'NamedNode') return x.value;
    if ('iri' in x && typeof x.iri === 'string') return x.iri;
  }
  throw new TypeError(`expected an IRI, not ${String(x)}`);
}

function entity(kind: EntityKind) {
  return (x: IriArg): OwlEntity => ({ kind, iri: iri(x) });
}

/** A class, by its IRI. */
export const Class = entity('Class');
/** A datatype, by its IRI. */
export const Datatype = entity('Datatype');
/** An object property, by its IRI. */
export const ObjectProperty = entity('ObjectProperty');
/** A data property, by its IRI. */
export const DataProperty = entity('DataProperty');
/** An annotation property, by its IRI. */
export const AnnotationProperty = entity('AnnotationProperty');
/** A named individual, by its IRI. */
export const NamedIndividual = entity('NamedIndividual');
/** An IRI as an annotation value or subject. */
export const IRI = (x: IriArg): OwlIri => ({ kind: 'IRI', iri: iri(x) });
/** An anonymous individual, by its blank node label (`_:label`). */
export function AnonymousIndividual(label: string): string {
  const l = label.startsWith('_:') ? label.slice(2) : label;
  if (!l) throw new TypeError('an anonymous individual needs a label');
  return `_:${l}`;
}

/**
 * A literal: `value` as its lexical form, with a datatype or a language tag. Without
 * either, a boolean is `xsd:boolean`, an integer number or a bigint is `xsd:integer`,
 * another number is `xsd:double` and a string is a plain string.
 */
export function Literal(
  value: string | boolean | number | bigint,
  o: { datatype?: IriArg; lang?: string } = {},
): OwlLiteral {
  if (o.datatype !== undefined && o.lang !== undefined) {
    throw new TypeError('a literal has a datatype or a language tag, not both');
  }
  if (o.lang !== undefined) return { '@value': String(value), '@language': o.lang };
  if (o.datatype !== undefined) return { '@value': String(value), '@type': iri(o.datatype) };
  if (typeof value === 'boolean') return { '@value': String(value), '@type': XSD_NS + 'boolean' };
  if (typeof value === 'bigint') return { '@value': value.toString(), '@type': XSD_NS + 'integer' };
  if (typeof value === 'number') {
    if (Number.isInteger(value)) return { '@value': String(value), '@type': XSD_NS + 'integer' };
    return { '@value': String(value), '@type': XSD_NS + 'double' };
  }
  return { '@value': value };
}

function literal(x: LiteralArg): OwlLiteral {
  if (x && typeof x === 'object') {
    if ('@value' in x) return x;
    if ('termType' in x && x.termType === 'Literal') {
      if (x.language) return { '@value': x.value, '@language': x.language };
      return { '@value': x.value, '@type': x.datatype.value };
    }
    throw new TypeError(`expected a literal, not ${JSON.stringify(x)}`);
  }
  return Literal(x);
}

function individual(x: IriArg): string {
  return typeof x === 'string' && x.startsWith('_:') ? AnonymousIndividual(x) : iri(x);
}

function ope(x: IriArg | OwlExpression): unknown {
  if (x && typeof x === 'object' && 'inverseOf' in x) return x;
  return iri(x as IriArg);
}

function expression(x: IriArg | OwlExpression): unknown {
  if (x && typeof x === 'object' && 'type' in x) return x;
  return iri(x as IriArg);
}

function annotationValue(x: unknown): unknown {
  if (typeof x === 'string') return x.startsWith('_:') ? x : { '@value': x };
  if (x && typeof x === 'object') {
    if ('kind' in x && 'iri' in x) return (x as OwlIri).iri;
    if ('termType' in x && (x as { termType: string }).termType === 'NamedNode') {
      return (x as unknown as { value: string }).value;
    }
  }
  return literal(x as LiteralArg);
}

/** An annotation: a property and a value, itself annotated by `annotations`. */
export function Annotation(
  property: IriArg,
  value: unknown,
  annotations?: OwlAnnotation[],
): OwlAnnotation {
  const a: OwlAnnotation = { property: iri(property), value: annotationValue(value) };
  if (annotations?.length) a.annotations = annotations;
  return a;
}

function axiom(type: string, members: Record<string, unknown>, o: AxiomOptions = {}): OwlAxiom {
  const ax: OwlAxiom = { type, ...members };
  if (o.annotations?.length) ax.annotations = o.annotations as unknown as OwlAxiom['annotations'];
  return ax;
}

// ------------------------------------------------------------------ expressions --

type Ce = IriArg | OwlExpression;

export const ObjectInverseOf = (p: IriArg) => ({ inverseOf: iri(p) });
/** The chain of a `SubObjectPropertyOf` axiom. */
export function ObjectPropertyChain(...properties: (IriArg | OwlExpression)[]) {
  if (properties.length < 2) throw new TypeError('a property chain has at least two properties');
  return { chain: properties.map(ope) };
}
export const DataIntersectionOf = (...ranges: Ce[]) => ({
  type: 'DataIntersectionOf',
  ranges: ranges.map(expression),
});
export const DataUnionOf = (...ranges: Ce[]) => ({
  type: 'DataUnionOf',
  ranges: ranges.map(expression),
});
export const DataComplementOf = (range: Ce) => ({
  type: 'DataComplementOf',
  range: expression(range),
});
export const DataOneOf = (...values: LiteralArg[]) => ({
  type: 'DataOneOf',
  values: values.map(literal),
});
/** A datatype restricted by `[facet, value]` pairs. */
export function DatatypeRestriction(datatype: IriArg, ...restrictions: [IriArg, LiteralArg][]) {
  if (restrictions.length === 0)
    throw new TypeError('a datatype restriction has at least one facet');
  return {
    type: 'DatatypeRestriction',
    datatype: iri(datatype),
    restrictions: restrictions.map(([f, v]) => ({ facet: iri(f), value: literal(v) })),
  };
}
export const ObjectIntersectionOf = (...classes: Ce[]) => ({
  type: 'ObjectIntersectionOf',
  classes: classes.map(expression),
});
export const ObjectUnionOf = (...classes: Ce[]) => ({
  type: 'ObjectUnionOf',
  classes: classes.map(expression),
});
export const ObjectComplementOf = (c: Ce) => ({ type: 'ObjectComplementOf', class: expression(c) });
export const ObjectOneOf = (...individuals: IriArg[]) => ({
  type: 'ObjectOneOf',
  individuals: individuals.map(individual),
});
export const ObjectSomeValuesFrom = (p: Ce, c: Ce) => ({
  type: 'ObjectSomeValuesFrom',
  property: ope(p),
  class: expression(c),
});
export const ObjectAllValuesFrom = (p: Ce, c: Ce) => ({
  type: 'ObjectAllValuesFrom',
  property: ope(p),
  class: expression(c),
});
export const ObjectHasValue = (p: Ce, i: IriArg) => ({
  type: 'ObjectHasValue',
  property: ope(p),
  individual: individual(i),
});
export const ObjectHasSelf = (p: Ce) => ({ type: 'ObjectHasSelf', property: ope(p) });

function cardinality(type: string, n: number, property: unknown, key: string, filler: unknown) {
  if (!Number.isInteger(n) || n < 0) throw new TypeError('a cardinality is a nonnegative integer');
  const j: Record<string, unknown> = { type, cardinality: n, property };
  if (filler !== undefined) j[key] = filler;
  return j;
}
export const ObjectMinCardinality = (n: number, p: Ce, c?: Ce) =>
  cardinality(
    'ObjectMinCardinality',
    n,
    ope(p),
    'class',
    c === undefined ? undefined : expression(c),
  );
export const ObjectMaxCardinality = (n: number, p: Ce, c?: Ce) =>
  cardinality(
    'ObjectMaxCardinality',
    n,
    ope(p),
    'class',
    c === undefined ? undefined : expression(c),
  );
export const ObjectExactCardinality = (n: number, p: Ce, c?: Ce) =>
  cardinality(
    'ObjectExactCardinality',
    n,
    ope(p),
    'class',
    c === undefined ? undefined : expression(c),
  );

function dataValues(type: string, args: Ce[]) {
  if (args.length < 2) throw new TypeError(`${type} takes data properties and a data range`);
  const props = args.slice(0, -1).map((p) => iri(p as IriArg));
  const j: Record<string, unknown> = { type, range: expression(args[args.length - 1]) };
  if (props.length === 1) j.property = props[0];
  else j.properties = props;
  return j;
}
/** `DataSomeValuesFrom(p1 … pn DR)`: data properties, then the data range. */
export const DataSomeValuesFrom = (...propertiesAndRange: Ce[]) =>
  dataValues('DataSomeValuesFrom', propertiesAndRange);
/** `DataAllValuesFrom(p1 … pn DR)`: data properties, then the data range. */
export const DataAllValuesFrom = (...propertiesAndRange: Ce[]) =>
  dataValues('DataAllValuesFrom', propertiesAndRange);
export const DataHasValue = (p: IriArg, v: LiteralArg) => ({
  type: 'DataHasValue',
  property: iri(p),
  value: literal(v),
});
export const DataMinCardinality = (n: number, p: IriArg, r?: Ce) =>
  cardinality(
    'DataMinCardinality',
    n,
    iri(p),
    'range',
    r === undefined ? undefined : expression(r),
  );
export const DataMaxCardinality = (n: number, p: IriArg, r?: Ce) =>
  cardinality(
    'DataMaxCardinality',
    n,
    iri(p),
    'range',
    r === undefined ? undefined : expression(r),
  );
export const DataExactCardinality = (n: number, p: IriArg, r?: Ce) =>
  cardinality(
    'DataExactCardinality',
    n,
    iri(p),
    'range',
    r === undefined ? undefined : expression(r),
  );

// ----------------------------------------------------------------------- axioms --

/** The declaration of an entity made by {@link Class} or another entity constructor. */
export function Declaration(e: OwlEntity, o?: AxiomOptions): OwlAxiom {
  if (!e || typeof e !== 'object' || !('kind' in e) || e.kind === ('IRI' as string)) {
    throw new TypeError('a declaration takes an entity such as owl.Class(iri)');
  }
  return axiom('Declaration', { kind: e.kind, iri: e.iri }, o);
}
export const SubClassOf = (sub: Ce, sup: Ce, o?: AxiomOptions) =>
  axiom('SubClassOf', { subClass: expression(sub), superClass: expression(sup) }, o);
export const EquivalentClasses = (classes: Ce[], o?: AxiomOptions) =>
  axiom('EquivalentClasses', { classes: classes.map(expression) }, o);
export const DisjointClasses = (classes: Ce[], o?: AxiomOptions) =>
  axiom('DisjointClasses', { classes: classes.map(expression) }, o);
export const DisjointUnion = (c: IriArg, classes: Ce[], o?: AxiomOptions) =>
  axiom('DisjointUnion', { class: iri(c), classes: classes.map(expression) }, o);
/** `sub` is an object property expression or an {@link ObjectPropertyChain}. */
export function SubObjectPropertyOf(
  sub: Ce | { chain: unknown[] },
  sup: Ce,
  o?: AxiomOptions,
): OwlAxiom {
  const members: Record<string, unknown> = { superProperty: ope(sup) };
  if (sub && typeof sub === 'object' && 'chain' in sub) members.chain = sub.chain;
  else members.subProperty = ope(sub as Ce);
  return axiom('SubObjectPropertyOf', members, o);
}
export const EquivalentObjectProperties = (properties: Ce[], o?: AxiomOptions) =>
  axiom('EquivalentObjectProperties', { properties: properties.map(ope) }, o);
export const DisjointObjectProperties = (properties: Ce[], o?: AxiomOptions) =>
  axiom('DisjointObjectProperties', { properties: properties.map(ope) }, o);
export const InverseObjectProperties = (first: Ce, second: Ce, o?: AxiomOptions) =>
  axiom('InverseObjectProperties', { first: ope(first), second: ope(second) }, o);
export const ObjectPropertyDomain = (p: Ce, domain: Ce, o?: AxiomOptions) =>
  axiom('ObjectPropertyDomain', { property: ope(p), domain: expression(domain) }, o);
export const ObjectPropertyRange = (p: Ce, range: Ce, o?: AxiomOptions) =>
  axiom('ObjectPropertyRange', { property: ope(p), range: expression(range) }, o);
const characteristic = (type: string) => (p: Ce, o?: AxiomOptions) =>
  axiom(type, { property: ope(p) }, o);
export const FunctionalObjectProperty = characteristic('FunctionalObjectProperty');
export const InverseFunctionalObjectProperty = characteristic('InverseFunctionalObjectProperty');
export const ReflexiveObjectProperty = characteristic('ReflexiveObjectProperty');
export const IrreflexiveObjectProperty = characteristic('IrreflexiveObjectProperty');
export const SymmetricObjectProperty = characteristic('SymmetricObjectProperty');
export const AsymmetricObjectProperty = characteristic('AsymmetricObjectProperty');
export const TransitiveObjectProperty = characteristic('TransitiveObjectProperty');
export const SubDataPropertyOf = (sub: IriArg, sup: IriArg, o?: AxiomOptions) =>
  axiom('SubDataPropertyOf', { subProperty: iri(sub), superProperty: iri(sup) }, o);
export const EquivalentDataProperties = (properties: IriArg[], o?: AxiomOptions) =>
  axiom('EquivalentDataProperties', { properties: properties.map(iri) }, o);
export const DisjointDataProperties = (properties: IriArg[], o?: AxiomOptions) =>
  axiom('DisjointDataProperties', { properties: properties.map(iri) }, o);
export const DataPropertyDomain = (p: IriArg, domain: Ce, o?: AxiomOptions) =>
  axiom('DataPropertyDomain', { property: iri(p), domain: expression(domain) }, o);
export const DataPropertyRange = (p: IriArg, range: Ce, o?: AxiomOptions) =>
  axiom('DataPropertyRange', { property: iri(p), range: expression(range) }, o);
export const FunctionalDataProperty = (p: IriArg, o?: AxiomOptions) =>
  axiom('FunctionalDataProperty', { property: iri(p) }, o);
export const DatatypeDefinition = (d: IriArg, range: Ce, o?: AxiomOptions) =>
  axiom('DatatypeDefinition', { datatype: iri(d), range: expression(range) }, o);
/** `HasKey(C (OPE…) (DPE…))`: the object properties and the data properties of the key. */
export const HasKey = (
  c: Ce,
  objectProperties: Ce[] = [],
  dataProperties: IriArg[] = [],
  o?: AxiomOptions,
) =>
  axiom(
    'HasKey',
    {
      class: expression(c),
      objectProperties: objectProperties.map(ope),
      dataProperties: dataProperties.map(iri),
    },
    o,
  );
export const SameIndividual = (individuals: IriArg[], o?: AxiomOptions) =>
  axiom('SameIndividual', { individuals: individuals.map(individual) }, o);
export const DifferentIndividuals = (individuals: IriArg[], o?: AxiomOptions) =>
  axiom('DifferentIndividuals', { individuals: individuals.map(individual) }, o);
export const ClassAssertion = (c: Ce, i: IriArg, o?: AxiomOptions) =>
  axiom('ClassAssertion', { class: expression(c), individual: individual(i) }, o);
export const ObjectPropertyAssertion = (p: Ce, source: IriArg, target: IriArg, o?: AxiomOptions) =>
  axiom(
    'ObjectPropertyAssertion',
    { property: ope(p), source: individual(source), target: individual(target) },
    o,
  );
export const NegativeObjectPropertyAssertion = (
  p: Ce,
  source: IriArg,
  target: IriArg,
  o?: AxiomOptions,
) =>
  axiom(
    'NegativeObjectPropertyAssertion',
    { property: ope(p), source: individual(source), target: individual(target) },
    o,
  );
export const DataPropertyAssertion = (
  p: IriArg,
  source: IriArg,
  value: LiteralArg,
  o?: AxiomOptions,
) =>
  axiom(
    'DataPropertyAssertion',
    { property: iri(p), source: individual(source), value: literal(value) },
    o,
  );
export const NegativeDataPropertyAssertion = (
  p: IriArg,
  source: IriArg,
  value: LiteralArg,
  o?: AxiomOptions,
) =>
  axiom(
    'NegativeDataPropertyAssertion',
    { property: iri(p), source: individual(source), value: literal(value) },
    o,
  );
/**
 * The subject is an IRI or an anonymous individual; the value is a literal unless it is
 * an {@link IRI}, a named node or an anonymous individual.
 */
export const AnnotationAssertion = (p: IriArg, subject: IriArg, value: unknown, o?: AxiomOptions) =>
  axiom(
    'AnnotationAssertion',
    { property: iri(p), subject: individual(subject), value: annotationValue(value) },
    o,
  );
export const SubAnnotationPropertyOf = (sub: IriArg, sup: IriArg, o?: AxiomOptions) =>
  axiom('SubAnnotationPropertyOf', { subProperty: iri(sub), superProperty: iri(sup) }, o);
export const AnnotationPropertyDomain = (p: IriArg, domain: IriArg, o?: AxiomOptions) =>
  axiom('AnnotationPropertyDomain', { property: iri(p), domain: iri(domain) }, o);
export const AnnotationPropertyRange = (p: IriArg, range: IriArg, o?: AxiomOptions) =>
  axiom('AnnotationPropertyRange', { property: iri(p), range: iri(range) }, o);
