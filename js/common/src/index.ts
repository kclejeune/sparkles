import type * as RDF from '@rdfjs/types';

export type { RDF };
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const RDF_NS = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';
const quote = (s: string) => JSON.stringify(s);
const iri = (s: string) => {
  if (!/^[A-Za-z][A-Za-z0-9+.-]*:[^\s\u0000-\u0020\u007F-\u009F\uD800-\uDFFF<>"{}|\\^`]*$/u.test(s))
    throw new InvalidInputError(`Invalid absolute IRI: ${s}`);
  return s;
};

export class SparklesError extends Error {
  constructor(
    message: string,
    public code = 'ERR_SPARKLES',
    public details: Record<string, unknown> = {},
  ) {
    super(message);
    this.name = new.target.name;
  }
}
export class SparqlSyntaxError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_SPARQL_SYNTAX', d);
  }
}
export class RdfSyntaxError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_RDF_SYNTAX', d);
  }
}
export class InvalidInputError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_INVALID_INPUT', d);
  }
}
export class UnsupportedError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_UNSUPPORTED', d);
  }
}
export class QueryTimeoutError extends SparklesError {
  constructor(m = 'Operation timed out', d = {}) {
    super(m, 'ERR_SPARKLES_TIMEOUT', d);
  }
}
export class CancelledError extends SparklesError {
  constructor(m = 'Operation cancelled', d = {}) {
    super(m, 'ERR_SPARKLES_CANCELLED', d);
  }
}
export class BudgetExceededError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_BUDGET', d);
  }
}
export class StorageError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_STORAGE', d);
  }
}
export class DatasetLockedError extends StorageError {
  constructor(m: string, d = {}) {
    super(m, d);
    this.code = 'ERR_SPARKLES_DATASET_LOCKED';
  }
}
export class ConflictError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_CONFLICT', d);
  }
}
export class NotFoundError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_NOT_FOUND', d);
  }
}
export class PermissionDeniedError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_PERMISSION', d);
  }
}
export class ServiceError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_SERVICE', d);
  }
}
export class WriteRejectedError extends SparklesError {
  constructor(m: string, d = {}) {
    super(m, 'ERR_SPARKLES_WRITE_REJECTED', d);
  }
}
const errorClasses: Record<string, new (m: string, d?: Record<string, unknown>) => SparklesError> =
  {
    SparqlSyntaxError,
    RdfSyntaxError,
    InvalidInputError,
    UnsupportedError,
    QueryTimeoutError,
    CancelledError,
    BudgetExceededError,
    StorageError,
    DatasetLockedError,
    ConflictError,
    NotFoundError,
    PermissionDeniedError,
    ServiceError,
    WriteRejectedError,
  };
export function nativeError(error: unknown): Error {
  if (!(error instanceof Error)) return new SparklesError(String(error));
  try {
    const data = JSON.parse(error.message);
    const Constructor = errorClasses[data.kind];
    if (Constructor) return new Constructor(data.message, data.details);
  } catch {
    /* Foreign runtime error. */
  }
  return error;
}

abstract class BaseTerm {
  abstract readonly termType: RDF.Term['termType'];
  constructor(public readonly value: string) {}
  equals(other: RDF.Term | null | undefined): boolean {
    return !!other && this.termType === other.termType && this.value === other.value;
  }
  abstract toString(): string;
}
export class NamedNode<Iri extends string = string> extends BaseTerm implements RDF.NamedNode<Iri> {
  readonly termType = 'NamedNode';
  declare readonly value: Iri;
  constructor(value: Iri) {
    super(iri(value));
  }
  toString() {
    return `<${this.value}>`;
  }
}
export class BlankNode extends BaseTerm implements RDF.BlankNode {
  readonly termType = 'BlankNode';
  toString() {
    return `_:${this.value}`;
  }
}
export class Variable extends BaseTerm implements RDF.Variable {
  readonly termType = 'Variable';
  toString() {
    return `?${this.value}`;
  }
}
export class DefaultGraph extends BaseTerm implements RDF.DefaultGraph {
  readonly termType = 'DefaultGraph';
  declare readonly value: '';
  constructor() {
    super('');
  }
  toString() {
    return '';
  }
}
export class Literal extends BaseTerm implements RDF.Literal {
  readonly termType = 'Literal';
  constructor(
    value: string,
    public readonly language = '',
    public readonly datatype: NamedNode = new NamedNode(XSD + 'string'),
    public readonly direction: '' | 'ltr' | 'rtl' = '',
  ) {
    super(value);
  }
  override equals(other: RDF.Term | null | undefined) {
    return (
      !!other &&
      other.termType === 'Literal' &&
      this.value === other.value &&
      this.language === other.language &&
      this.direction === (other.direction ?? '') &&
      this.datatype.equals(other.datatype)
    );
  }
  toString() {
    return (
      quote(this.value) +
      (this.language
        ? `@${this.language}${this.direction ? '--' + this.direction : ''}`
        : this.datatype.value === XSD + 'string'
          ? ''
          : `^^${this.datatype}`)
    );
  }
  toJs(): unknown {
    const t = this.datatype.value.slice(XSD.length);
    if (!this.datatype.value.startsWith(XSD)) return this.value;
    if (t === 'boolean' && ['true', 'false', '0', '1'].includes(this.value))
      return ['true', '1'].includes(this.value);
    if (t === 'integer' && /^[+-]?\d+$/.test(this.value)) {
      const v = BigInt(this.value);
      return v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER)
        ? Number(v)
        : v;
    }
    if (['double', 'float'].includes(t)) {
      if (this.value === 'INF') return Infinity;
      if (this.value === '-INF') return -Infinity;
      if (this.value === 'NaN') return NaN;
      if (/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(this.value))
        return Number(this.value);
    }
    if (t === 'dateTime') {
      const d = new Date(this.value);
      if (!Number.isNaN(d.valueOf())) return d;
    }
    if (t === 'base64Binary') {
      try {
        return Uint8Array.from(atob(this.value), (c) => c.charCodeAt(0));
      } catch {
        return this.value;
      }
    }
    if (t === 'hexBinary' && /^(?:[\da-f]{2})*$/i.test(this.value))
      return Uint8Array.from(this.value.match(/../g) ?? [], (c) => parseInt(c, 16));
    return this.value;
  }
}
export class Quad extends BaseTerm implements RDF.Quad {
  readonly termType = 'Quad';
  declare readonly value: '';
  constructor(
    public readonly subject: RDF.Quad_Subject,
    public readonly predicate: RDF.Quad_Predicate,
    public readonly object: RDF.Quad_Object,
    public readonly graph: RDF.Quad_Graph = defaultGraph,
  ) {
    super('');
  }
  override equals(other: RDF.Term | null | undefined) {
    return (
      !!other &&
      other.termType === 'Quad' &&
      this.subject.equals(other.subject) &&
      this.predicate.equals(other.predicate) &&
      this.object.equals(other.object) &&
      this.graph.equals(other.graph)
    );
  }
  toString() {
    return `${this.subject} ${this.predicate} ${this.object}${this.graph.termType === 'DefaultGraph' ? '' : ` ${this.graph}`}`;
  }
}
const defaultGraph = new DefaultGraph();
// Turtle PN_CHARS_BASE, PN_CHARS_U and PN_CHARS (RDF blank-node labels).
const pnBase =
  'A-Za-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD\\u{10000}-\\u{EFFFF}';
const blankLabel = new RegExp(
  `^[${pnBase}_0-9](?:[${pnBase}_0-9\\-\\u00B7\\u0300-\\u036F\\u203F-\\u2040.]*[${pnBase}_0-9\\-\\u00B7\\u0300-\\u036F\\u203F-\\u2040])?$`,
  'u',
);
let blankCounter = 0;
const blankSeed =
  globalThis.crypto?.randomUUID?.().replaceAll('-', '') ?? Math.random().toString(36).slice(2);
export const factory: RDF.DataFactory & { fromJs(value: unknown): Literal } = {
  namedNode: <Iri extends string>(v: Iri) => new NamedNode(v),
  blankNode: (v?: string) => {
    const label = v ?? `js${blankSeed}_${blankCounter++}`;
    if (!blankLabel.test(label)) throw new InvalidInputError('Invalid blank node label');
    return new BlankNode(label);
  },
  variable: (v: string) => {
    if (!/^[\p{L}_\d][\p{L}\p{N}_]*$/u.test(v))
      throw new InvalidInputError('Invalid variable name');
    return new Variable(v);
  },
  defaultGraph: () => defaultGraph,
  literal: (
    value: string,
    languageOrDatatype?: string | RDF.NamedNode | RDF.DirectionalLanguage,
  ) => {
    if (typeof value !== 'string') throw new TypeError('Literal value must be a string');
    if (languageOrDatatype === '') return new Literal(value);
    if (
      typeof languageOrDatatype === 'string' ||
      (languageOrDatatype && 'language' in languageOrDatatype)
    ) {
      const language = (
        typeof languageOrDatatype === 'string' ? languageOrDatatype : languageOrDatatype.language
      ).toLowerCase();
      const direction =
        typeof languageOrDatatype === 'string' ? '' : (languageOrDatatype.direction ?? '');
      if (!/^[a-z]+(?:-[a-z\d]+)*$/i.test(language))
        throw new InvalidInputError('Invalid language tag');
      if (!['', 'ltr', 'rtl'].includes(direction)) throw new InvalidInputError('Invalid direction');
      return new Literal(
        value,
        language,
        new NamedNode(RDF_NS + (direction ? 'dirLangString' : 'langString')),
        direction,
      );
    }
    return new Literal(value, '', new NamedNode(languageOrDatatype?.value ?? XSD + 'string'));
  },
  quad: (s, p, o, g = defaultGraph) => new Quad(s, p, o, g),
  fromTerm: (term: RDF.Term): any => decodeTerm(term),
  fromQuad: (q: RDF.Quad) =>
    new Quad(
      decodeTerm(q.subject) as RDF.Quad_Subject,
      decodeTerm(q.predicate) as RDF.Quad_Predicate,
      decodeTerm(q.object) as RDF.Quad_Object,
      decodeTerm(q.graph) as RDF.Quad_Graph,
    ),
  fromJs(value: unknown) {
    if (typeof value === 'string') return new Literal(value);
    if (typeof value === 'boolean')
      return new Literal(String(value), '', new NamedNode(XSD + 'boolean'));
    if (typeof value === 'bigint')
      return new Literal(String(value), '', new NamedNode(XSD + 'integer'));
    if (typeof value === 'number') {
      if (Number.isInteger(value) && !Number.isSafeInteger(value))
        throw new InvalidInputError('Unsafe integer: use bigint');
      return new Literal(
        value === Infinity ? 'INF' : value === -Infinity ? '-INF' : String(value),
        '',
        new NamedNode(XSD + (Number.isInteger(value) ? 'integer' : 'double')),
      );
    }
    if (value instanceof Date)
      return new Literal(value.toISOString(), '', new NamedNode(XSD + 'dateTime'));
    if (value instanceof Uint8Array) {
      let text = '';
      for (const b of value) text += String.fromCharCode(b);
      return new Literal(btoa(text), '', new NamedNode(XSD + 'base64Binary'));
    }
    throw new TypeError('Unsupported JavaScript literal value');
  },
};

export type WireTerm = {
  termType: string;
  value: string;
  language?: string;
  direction?: '' | 'ltr' | 'rtl';
  datatype?: { value: string };
  subject?: WireTerm;
  predicate?: WireTerm;
  object?: WireTerm;
  graph?: WireTerm;
};
export function encodeTerm(term: RDF.Term): WireTerm {
  if (!term || typeof term.value !== 'string') throw new TypeError('Expected an RDF/JS term');
  switch (term.termType) {
    case 'NamedNode':
      return { termType: term.termType, value: iri(term.value) };
    case 'BlankNode':
    case 'Variable':
    case 'DefaultGraph':
      return { termType: term.termType, value: term.value };
    case 'Literal':
      return {
        termType: term.termType,
        value: term.value,
        language: term.language,
        direction: term.direction ?? '',
        datatype: { value: term.datatype.value },
      };
    case 'Quad':
      return {
        termType: 'Quad',
        value: '',
        subject: encodeTerm(term.subject),
        predicate: encodeTerm(term.predicate),
        object: encodeTerm(term.object),
        graph: encodeTerm(term.graph),
      };
    default:
      throw new TypeError('Unknown RDF/JS term type');
  }
}
export function decodeTerm(term: WireTerm | RDF.Term, f: RDF.DataFactory = factory): RDF.Term {
  switch (term.termType) {
    case 'NamedNode':
      return f.namedNode(term.value);
    case 'BlankNode':
      return f.blankNode(term.value);
    case 'Variable':
      return f.variable!(term.value);
    case 'DefaultGraph':
      return f.defaultGraph();
    case 'Literal':
      return f.literal(
        term.value,
        term.language
          ? term.direction
            ? { language: term.language, direction: term.direction }
            : term.language
          : f.namedNode((term as WireTerm).datatype?.value ?? XSD + 'string'),
      );
    case 'Quad': {
      const q = term as WireTerm;
      return f.quad(
        decodeTerm(q.subject!, f) as RDF.Quad_Subject,
        decodeTerm(q.predicate!, f) as RDF.Quad_Predicate,
        decodeTerm(q.object!, f) as RDF.Quad_Object,
        decodeTerm(q.graph!, f) as RDF.Quad_Graph,
      );
    }
    default:
      throw new InvalidInputError(`Unknown term type ${term.termType}`);
  }
}

export class Bindings implements Iterable<[Variable, RDF.Term]> {
  readonly type = 'bindings';
  private terms: Map<string, RDF.Term>;
  constructor(entries: Iterable<[string | RDF.Variable, RDF.Term]> = []) {
    this.terms = new Map(Array.from(entries, ([k, v]) => [typeof k === 'string' ? k : k.value, v]));
  }
  get size() {
    return this.terms.size;
  }
  get(key: string | RDF.Variable) {
    return this.terms.get(typeof key === 'string' ? key.replace(/^[?$]/, '') : key.value);
  }
  has(key: string | RDF.Variable) {
    return this.get(key) !== undefined;
  }
  *keys() {
    for (const k of this.terms.keys()) yield new Variable(k);
  }
  values(): IterableIterator<RDF.Term> {
    return this.terms.values();
  }
  *[Symbol.iterator](): Iterator<[Variable, RDF.Term]> {
    for (const [k, v] of this.terms) yield [new Variable(k), v];
  }
  forEach(f: (term: RDF.Term, variable: Variable) => void) {
    for (const [k, v] of this) f(v, k);
  }
  equals(other: Bindings) {
    return this.size === other.size && Array.from(this).every(([k, v]) => v.equals(other.get(k)));
  }
  filter(f: (term: RDF.Term, variable: Variable) => boolean) {
    return new Bindings(Array.from(this).filter(([k, v]) => f(v, k)));
  }
  map(f: (term: RDF.Term, variable: Variable) => RDF.Term) {
    return new Bindings(Array.from(this, ([k, v]) => [k, f(v, k)]));
  }
  merge(other: Bindings) {
    return new Bindings([...this, ...other]);
  }
  toObject() {
    return Object.fromEntries(this.terms);
  }
}
export class LazyBindings extends Bindings {
  constructor(
    private names: string[],
    private cells: number[],
    private decode: (index: number) => RDF.Term,
  ) {
    super();
  }
  override get size() {
    return this.cells.filter(Boolean).length;
  }
  override get(key: string | RDF.Variable) {
    const i = this.names.indexOf(typeof key === 'string' ? key.replace(/^[?$]/, '') : key.value);
    return i >= 0 && this.cells[i] ? this.decode(this.cells[i]) : undefined;
  }
  override *[Symbol.iterator](): Iterator<[Variable, RDF.Term]> {
    for (let i = 0; i < this.names.length; i++)
      if (this.cells[i]) yield [new Variable(this.names[i]), this.decode(this.cells[i])];
  }
  override *keys() {
    for (let i = 0; i < this.names.length; i++)
      if (this.cells[i]) yield new Variable(this.names[i]);
  }
  override *values() {
    for (const [, v] of this) yield v;
  }
  override toObject() {
    return Object.fromEntries(Array.from(this, ([k, v]) => [k.value, v]));
  }
}

export interface OperationOptions {
  signal?: AbortSignal;
  timeout?: number;
}
export interface QueryOptions extends OperationOptions {
  /** Opt-in engine cursor; existing calls use eager execution. */
  execution?: 'eager' | 'streaming' | 'auto';
  allowMaterialization?: boolean;
  unionDefaultGraph?: boolean;
  noCache?: boolean;
  baseIri?: string;
  prefixes?: Record<string, string>;
  bindings?: Record<string, RDF.Term> | Bindings;
  defaultGraph?: string[];
  namedGraphs?: string[];
  includeInferred?: boolean;
  at?: number | bigint | Date | string;
  maxRows?: number;
  maxMemoryBytes?: number;
  maxRowsProduced?: number;
  factory?: RDF.DataFactory;
  batchSize?: number;
  batchBytes?: number;
  describe?: string | Record<string, unknown>;
}
export interface UpdateOptions extends OperationOptions {
  dryRun?: boolean | { changes?: number; maxChanges?: number };
  baseIri?: string;
  prefixes?: Record<string, string>;
  message?: string;
  ifHead?: bigint | number;
  noWait?: boolean;
}
export interface CommitReceipt {
  datasetId: string;
  committed: boolean;
  commit: {
    seq: bigint;
    parent?: bigint | null;
    kind: string;
    inserted?: bigint;
    deleted?: bigint;
    quads?: bigint;
    timestamp?: string;
    message?: string;
    [key: string]: unknown;
  };
  validation?: unknown;
}
export interface UpdateResult {
  inserted: bigint;
  deleted: bigint;
  receipt?: CommitReceipt;
  [key: string]: unknown;
}
export interface IterableResult<T> extends AsyncIterable<T>, AsyncIterator<T> {
  readonly type: string;
  readonly size?: number;
  readonly plan?: unknown;
  readonly timing?: unknown;
  /** Live execution metadata when supported by the engine. */
  stats?(): Promise<unknown>;
  toArray(): Promise<T[]>;
  close(): Promise<void>;
}
export interface BindingsResult extends IterableResult<Bindings> {
  readonly type: 'bindings';
  readonly variables: RDF.Variable[];
}
export interface QuadsResult extends IterableResult<RDF.Quad> {
  readonly type: 'quads';
}
export type QueryResult =
  | BindingsResult
  | QuadsResult
  | { type: 'boolean'; value: boolean; plan?: unknown; timing?: unknown };
export interface SparqlDataset {
  query(q: string, o?: QueryOptions): Promise<QueryResult>;
  select(q: string, o?: QueryOptions): Promise<BindingsResult>;
  ask(q: string, o?: QueryOptions): Promise<boolean>;
  construct(q: string, o?: QueryOptions): Promise<QuadsResult>;
  update(u: string, o?: UpdateOptions): Promise<UpdateResult>;
}
export function unsigned(value: number | bigint, name = 'integer'): string {
  if (typeof value === 'number' && !Number.isSafeInteger(value))
    throw new InvalidInputError(`${name} must be a safe integer or bigint`);
  const v = BigInt(value);
  if (v < 0n || v > 18446744073709551615n) throw new InvalidInputError(`${name} is outside u64`);
  return String(v);
}
export function receiptOf(value: any): CommitReceipt | undefined {
  const r = value?.receipt ?? value;
  if (!r?.commit || !r?.datasetId) return undefined;
  const c = { ...r.commit };
  for (const key of ['seq', 'parent', 'inserted', 'deleted', 'quads'])
    if (c[key] != null) {
      if (typeof c[key] === 'number' && !Number.isSafeInteger(c[key]))
        throw new InvalidInputError(`Remote ${key} exceeds JavaScript's safe integer range`);
      c[key] = BigInt(unsigned(c[key], `Remote ${key}`));
    }
  return { ...r, commit: c };
}
