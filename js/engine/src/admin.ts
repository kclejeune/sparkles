import type { Dataset } from './index.js';
import type {
  RDF,
  OperationOptions,
  QueryOptions,
  UpdateOptions,
  CommitReceipt,
} from '@sparkles-rdf/common';
import { QueryTimeoutError, decodeTerm, unsigned } from '@sparkles-rdf/common';
export type Configuration = Record<string, unknown>;
export interface CommitInfo {
  seq: bigint;
  parent?: bigint | null;
  kind: string;
  quads: bigint;
  inserted: bigint;
  deleted: bigint;
  timestamp: string;
  message?: string;
}
export interface DatasetInfo {
  id: string;
  branch: string;
  quads: bigint;
  head: CommitInfo;
}
export interface SnapshotInfo {
  name: string;
  seq: bigint;
  commit: CommitInfo | null;
  createdMs: number;
  note: string | null;
  expiresMs: number | null;
  reconstructable: boolean;
  warm: boolean;
}
export interface BranchInfo {
  name: string;
  id: string;
  protected: boolean;
  note?: string;
  head: CommitInfo;
  upstream?: string;
}
/** What relinking a branch to main's index did. */
export interface RelinkResult {
  generation: string;
  quads: bigint;
  baseCommit: bigint;
  caughtUpCommits: bigint;
  abandoned?: string;
  mode: string;
  lockMs: number;
  buildMs: number;
  totalMs: number;
  [key: string]: unknown;
}
export interface MergeResult {
  merged: boolean;
  upToDate?: boolean;
  report?: Configuration;
  conflicts?: Configuration;
}
export interface GuardOutcome {
  installed: boolean;
  removed?: boolean;
  validation?: Configuration;
}
export interface ValidationReport {
  conforms: boolean;
  results: Configuration[];
  turtle?: string;
  warnings?: string[];
}
export interface Change {
  op: 'add' | 'remove';
  quad: RDF.Quad;
}
export interface CommitChanges {
  commit: CommitInfo;
  added: bigint;
  removed: bigint;
  complete: boolean;
  changes: Change[];
}
export interface ChangePage {
  after: bigint;
  next: bigint;
  head: CommitInfo;
  commits: CommitChanges[];
}
export interface HistoryStatus {
  head: bigint;
  reconstructable: [bigint, bigint][];
  bytes: bigint;
  firstCommit: bigint;
  snapshots: number;
  generations: Configuration[];
  [key: string]: unknown;
}
export type AdminCall = <T>(
  op: string,
  args?: Configuration,
  options?: UpdateOptions,
  lock?: boolean,
) => Promise<T>;
export interface Setting<T = Configuration> {
  get(options?: OperationOptions): Promise<T>;
  set(value: T, options?: UpdateOptions): Promise<unknown>;
  reset(options?: UpdateOptions): Promise<unknown>;
}
const at = (v: string | number | bigint = 'head') =>
  typeof v === 'string' ? v : 'commit:' + unsigned(v);
const setting = <T>(call: AdminCall, name: string): Setting<T> => ({
  get: (o) => call(`settings.${name}.get`, {}, o, false),
  set: (value, o) => call(`settings.${name}.set`, value as Configuration, o, true),
  reset: (o) => call(`settings.${name}.reset`, {}, o, true),
});
/** What a graph overview request groups and how (see "Graph overview" in API.md). */
export interface GroupsRequest {
  /**
   * A class IRI, or a group id from an earlier answer. It is left out only with
   * `by: { reports }`, which groups the whole dataset.
   */
  within?: { class: string } | { group: string };
  by?:
    | 'auto'
    | 'members'
    | { community: string }
    | { property: string; buckets?: number }
    /**
     * The GraphRAG community hierarchy (C22), starting at `level`, which is 0 by default.
     * `graph` is the reports graph and defaults to `urn:x-sparkles:graphrag:reports`.
     */
    | { reports: { level?: number; graph?: string } };
  /** Groups or members, 1 to 5,000 (the default). */
  limit?: number;
  /** A commit, a time or a named snapshot (`snapshot:<name>`). */
  at?: number | bigint | Date | string;
}
/** The options of `graph.groups`: those of a query that bear on the view and the budgets. */
export type GroupsOptions = Omit<
  QueryOptions,
  'at' | 'execution' | 'allowMaterialization' | 'bindings' | 'factory' | 'batchSize' | 'batchBytes'
>;
/** A GraphRAG report's review status (C22). */
export type GroupReportStatus = 'unreviewed' | 'reviewed' | 'rejected' | 'stale' | 'failed';
/** The answer of `graph.groups`. `commit` is the commit the answer was computed at. */
export interface GroupsAnswer {
  commit: bigint;
  within: {
    class?: string;
    group?: string;
    size: number;
    /** The reports graph of a GraphRAG grouping and the level it started at. */
    reports?: { graph: string; level: number };
    community?: string;
    level?: number;
    title?: string;
    folded?: boolean;
  };
  grouping: {
    kind: 'members' | 'values' | 'ranges' | 'communities' | 'reports';
    predicate: string | null;
    level?: number;
  };
  communities: boolean;
  groups: {
    id: string;
    label: string;
    size: number;
    top: { iri: string; label?: string }[];
    /** The community of a GraphRAG group, null for the members that no child holds. */
    community?: string | null;
    level?: number;
    /** The number of communities one level below. */
    children?: number;
    /** The community's report, null when it has none. */
    report?: { iri: string; title: string | null; status: GroupReportStatus | null } | null;
    /** True for the members that no community below holds. */
    folded?: boolean;
  }[];
  members?: { iri: string; label?: string; class?: string }[];
  edges: { source: string; target: string; predicate: string | null; count: number }[];
  truncated: boolean;
  note?: string;
  /** False for a GraphRAG grouping that the caller may not read or that has no reports. */
  available?: boolean;
  unavailable?: 'access' | 'none';
  reason?: string;
  levels?: { level: number; communities: number }[];
}
export interface MappingInputs {
  sources?: Record<string, string>;
  connections?: Record<string, string>;
  directory?: string;
  base?: string;
}
export type MappingRunOptions = MappingInputs &
  UpdateOptions & {
    version?: number | bigint;
    replace?: boolean;
    author?: string;
    message?: string;
  };
export interface MappingRun {
  receipt: CommitReceipt;
  stats: { rows: bigint; quads: bigint; warnings: string[]; joins: string[] };
}
/** A graph an ontology reads: `"default"` or an IRI. */
export type OntologyGraph = string;
/** What ontology to read (see "Ontology" in API.md). */
export interface OntologySelection {
  /** the graphs read, the default graph when absent; the first takes edits */
  graph?: OntologyGraph | OntologyGraph[];
  /** follow `owl:imports` to graphs of the dataset (true by default) */
  imports?: boolean;
  mode?: 'lenient' | 'strict';
}
/** A read of an ontology at a state, with or without its materialized inferences. */
export interface OntologyRead extends OntologySelection {
  /** the state read, as queries take it: `commit:N`, a snapshot or a time */
  at?: string | number | bigint;
  /** also read the materialized inferences, when the dataset has them */
  inferences?: boolean;
}
/** An OWL 2 axiom in its JSON form: `type` is its functional-style name. */
export interface OwlAxiom {
  type: string;
  annotations?: Configuration[];
  [member: string]: unknown;
}
/** An axiom given to an edit: its JSON form, or a string in functional-style syntax. */
export type AxiomInput = OwlAxiom | string;
/** One axiom of a listing or a diff. */
export interface OntologyAxiom {
  type: string;
  /** the axiom in functional-style syntax with the dataset's prefixes */
  ofn: string;
  axiom: OwlAxiom;
  /** the graph the axiom was read from */
  graph?: string;
}
/** The inferences an inferred read included, and their freshness. */
export interface OntologyInferences {
  included: boolean;
  graph?: string;
  profile?: string;
  stale?: boolean | null;
  commitsSince?: bigint | null;
  reason?: string | null;
}
/** What every ontology answer says about the read. */
export interface OntologyState {
  dataset: string;
  commit: bigint;
  mode: string;
  graphs: string[];
  imports: { iri: string; graph: string | null }[];
  warnings: string[];
  inferences?: OntologyInferences;
}
export interface OntologySummary extends OntologyState {
  header: {
    iri: string | null;
    versionIri: string | null;
    imports: string[];
    annotations: string[];
  };
  axioms: Record<string, number>;
  axiomCount: number;
  entities: Record<string, number>;
  triples: number;
  unmapped: number;
  diagnostics: Configuration;
  cached: boolean;
}
export interface OntologyAxiomQuery extends OntologyRead {
  /** axiom types such as `SubClassOf`, or a list such as `SubClassOf,ClassAssertion` */
  type?: string | string[];
  /** keep the axioms that mention this IRI */
  about?: string;
  /** also list the assertions the store holds */
  assertions?: boolean;
}
export interface OntologyAxiomPage extends OntologyState {
  axioms: OntologyAxiom[];
  total: number;
  offset: number;
  next: bigint | null;
}
/** One entity of a listing: its kind and IRI. */
export interface OntologyEntityRef {
  kind: string;
  iri: string;
}
export interface OntologyEntityPage extends OntologyState {
  entities: OntologyEntityRef[];
  total: number;
  offset: number;
  next: bigint | null;
}
/** One entity an ontology search found, as `GET /$/ontology/{ds}/search` lists it. */
export interface OntologySearchHit {
  type: 'entity';
  iri: string;
  /** `class`, `objectProperty`, `concept` and the other kinds the search filters by */
  kinds: string[];
  label: string | null;
  labelLang: string | null;
  shortForm: string | null;
  deprecated: boolean;
  match: {
    /** `label`, `synonym`, `shortForm` or `definition` */
    field: string;
    property: string | null;
    text: string;
    lang: string | null;
    /** `exact`, `prefix`, `words` or `fuzzy` */
    kind: string;
    distance: number;
    tier: number;
    /** `[start, end)` ranges of `text` in UTF-16 code units */
    highlight: [number, number][];
  };
}
/** One page of an ontology search. */
export interface OntologySearchPage {
  dataset: string;
  commit: bigint;
  graphs: string[];
  imports: { iri: string; graph: string | null }[];
  warnings: string[];
  query: string;
  results: OntologySearchHit[];
  offset: number;
  limit: number;
  more: boolean;
  next: bigint | null;
  /** `current`, `updated` or `built` */
  index: string;
}
/** An entity as the browser's answers name it: its label, short form and kinds. */
export interface OntologyName {
  iri: string;
  label: string | null;
  labelLang: string | null;
  /** its `oboInOwl:id`, `skos:notation`, prefixed name or local name */
  shortForm: string | null;
  kinds: string[];
  deprecated: boolean;
}
/** A node of a hierarchy, with the numbers of its direct children and parents. */
export interface OntologyHierarchyNode extends OntologyName {
  children: number;
  parents: number;
}
/** One level of a hierarchy, as `GET /$/ontology/{ds}/hierarchy` answers it. */
export interface OntologyHierarchy {
  dataset: string;
  commit: bigint;
  graphs: string[];
  imports: { iri: string; graph: string | null }[];
  warnings: string[];
  kind: string;
  node: OntologyHierarchyNode | null;
  children: OntologyHierarchyNode[];
  total: number;
  offset: number;
  limit: number;
  next: bigint | null;
  /** `label`, or `iri` for a node with more than 20,000 children */
  order: string;
  /** where the roots came from: `ontology`, `scheme` or `computed` */
  roots?: string;
  deprecatedRoots?: number;
  ancestors?: {
    nodes: OntologyHierarchyNode[];
    /** `[child, parent]` IRI pairs */
    edges: [string, string][];
    truncated: boolean;
  };
}
/** The statements of one predicate that point at an entity. */
export interface OntologyIncoming {
  property: string;
  count: number;
  more: boolean;
  subjects: string[];
}
/** What the graphs say about one entity, as `GET /$/ontology/{ds}/term` answers it. */
export interface OntologyTerm extends OntologyName {
  dataset: string;
  commit: bigint;
  graphs: string[];
  imports: { iri: string; graph: string | null }[];
  warnings: string[];
  types: string[];
  ontologies: { graph: string; ontology: string | null }[];
  statements: {
    property: string;
    /** an IRI, or a literal in its JSON form */
    value: string | Configuration;
    graph: string;
    annotations: { property: string; value: string | Configuration }[];
  }[];
  statementsTruncated: boolean;
  /** the literal forms of the entity's SKOS-XL labels */
  xlLabels: { property: string; value: Configuration; graph: string }[];
  axioms: OntologyAxiom[];
  mentions: { axioms: OntologyAxiom[]; total: number; truncated: boolean };
  incoming: OntologyIncoming[];
  usage: OntologyIncoming | null;
  names: Record<string, Omit<OntologyName, 'iri'>>;
}
/** An expression of an entity answer: its functional-style form and its JSON form. */
export interface OntologyExpression {
  ofn: string;
  expression: unknown;
}
/** What the ontology says about one entity, by kind (see "Ontology" in API.md). */
export interface OntologyEntity extends OntologyState {
  iri: string;
  kinds: string[];
  labels: Configuration[];
  annotations: { ofn: string; annotation: Configuration }[];
  class?: {
    superClasses: OntologyExpression[];
    subClasses: OntologyExpression[];
    equivalentClasses: OntologyExpression[];
    disjointClasses: OntologyExpression[];
    disjointUnions: OntologyExpression[][];
    keys: { objectProperties: OntologyExpression[]; dataProperties: string[] }[];
    instances: OntologyExpression[];
  };
  objectProperty?: {
    domains: OntologyExpression[];
    ranges: OntologyExpression[];
    superProperties: OntologyExpression[];
    subProperties: OntologyExpression[];
    equivalentProperties: OntologyExpression[];
    disjointProperties: OntologyExpression[];
    inverseProperties: OntologyExpression[];
    characteristics: string[];
  };
  dataProperty?: {
    domains: OntologyExpression[];
    ranges: OntologyExpression[];
    superProperties: string[];
    subProperties: string[];
    equivalentProperties: string[];
    disjointProperties: string[];
    functional: boolean;
  };
  annotationProperty?: {
    superProperties: string[];
    subProperties: string[];
    domains: string[];
    ranges: string[];
  };
  datatype?: { definitions: OntologyExpression[] };
  namedIndividual?: {
    types: OntologyExpression[];
    sameIndividuals: OntologyExpression[];
    differentIndividuals: OntologyExpression[];
    assertions: OntologyAxiom[];
  };
  assertions: boolean;
  truncated: boolean;
}
export interface OntologyCheck extends OntologyState {
  conforms: boolean;
  assertions: boolean;
  reports: Configuration[];
}
export interface OntologyEdit {
  added: bigint;
  present: number;
  removed: bigint;
  absent: string[];
  declared: string[];
  inserted: bigint;
  deleted: bigint;
  kept: number;
  committed: boolean;
}
export interface OntologyDiff {
  dataset: string;
  from: number;
  to: number;
  graphs: string[];
  mode: string;
  assertions: boolean;
  added: OntologyAxiom[];
  removed: OntologyAxiom[];
  addedCount: number;
  removedCount: number;
  truncated: boolean;
  changedQuads: number;
  method: string;
}
/** One node of a taxonomy: equivalent classes and the nodes of the direct superclasses. */
export interface OntologyTaxonomyNode {
  node: number;
  classes: string[];
  parents: number[];
  parentClasses: string[];
  children: number;
}
/** An axiom a classification leaves out (`rule`, `message`) or uses in part (`partial`). */
export interface OntologyExcluded extends OntologyAxiom {
  rule?: string;
  message?: string;
  partial?: string;
}
/** The OWL 2 EL classification of an ontology (spec F24). */
export interface OntologyClassification extends OntologyState {
  /** false only when the ontology is proved inconsistent; see `consistency` */
  consistent: boolean;
  /** whether the ontology has a model as far as the run knows: only a partial answer is `unknown` */
  consistency: 'consistent' | 'inconsistent' | 'unknown';
  /**
   * false when an axiom is left out or used in part, or the answer is partial, so that
   * entailments may be missing
   */
  complete: boolean;
  /** true for the partial answer of a run that its deadline, cancellation or memory budget stopped */
  partial: boolean;
  /** the rules the ontology's axioms need */
  tier: 'el' | 'horn' | 'disjunctive';
  /** the classes whose superclasses a partial answer may miss */
  incompleteCount: number;
  /** those classes, the first `limit`, in IRI order */
  incomplete: string[];
  /** whether the dataset's kept classification of the same state answered */
  reused: boolean;
  classes: number;
  nodeCount: number;
  unsatisfiableCount: number;
  unsatisfiable: string[];
  excludedCount: number;
  excluded: OntologyExcluded[];
  stats: Configuration;
  nodes?: OntologyTaxonomyNode[];
  next?: number;
  class?: OntologyTaxonomyNode & {
    iri: string;
    unsatisfiable: boolean;
    childClasses: string[];
    superClasses: string[];
  };
  written?: OntologyEdit;
}
/** What `ontology.parse` reads Manchester text as. */
export type ManchesterAs = 'classExpression' | 'dataRange' | 'individual' | 'literal' | 'axiom';
/** Manchester text read against the ontology (spec F21, item 608). */
export interface ManchesterParsed extends OntologyState {
  as: ManchesterAs;
  /** the value in its JSON form */
  value: unknown;
  ofn: string;
  manchester: string | null;
}
/** The answers a DL query can ask for. */
export type DlAnswer =
  | 'equivalents'
  | 'superclasses'
  | 'directSuperclasses'
  | 'subclasses'
  | 'directSubclasses'
  | 'instances';
/** An axiom outside the profile of the engine that answered a DL query. */
export interface DlGap extends Partial<OntologyAxiom> {
  message: string;
  rule: string | null;
  partial: boolean;
  manchester?: string | null;
}
/** A DL query's answers and the reasoning that found them (spec U12). */
export interface DlQueryAnswer extends OntologyState {
  expression: { manchester: string; ofn: string; json: unknown };
  reasoning: {
    engine: 'el' | 'rl';
    title: string;
    why: string;
    complete: boolean;
    instancesComplete: boolean | null;
    gapCount: number;
    gaps: DlGap[];
    notes: string[];
  };
  satisfiable: boolean | null;
  consistent: boolean | null;
  incremental: boolean;
  elapsedMs: number;
  counts: Partial<Record<DlAnswer, number>>;
  equivalents?: string[];
  superclasses?: string[];
  directSuperclasses?: string[];
  subclasses?: string[];
  directSubclasses?: string[];
  instances?: string[];
}
export class Administration {
  readonly snapshots;
  readonly history;
  readonly settings;
  readonly prefixes;
  readonly queries;
  readonly mappings;
  readonly ontology;
  readonly algorithms;
  readonly graph;
  readonly branches;
  readonly indexes;
  readonly reasoning;
  readonly validation;
  constructor(
    private call: AdminCall,
    getBranch: (name: string) => Promise<Dataset>,
    runQuery: (
      name: string,
      params: Configuration,
      options: any,
    ) => Promise<import('@sparkles-rdf/common').QueryResult>,
  ) {
    this.prefixes = {
      list: (o?: OperationOptions) => call<Record<string, string>>('prefixes.list', {}, o, false),
      set: (prefix: string, iri: string, o?: UpdateOptions) =>
        call<void>('prefixes.set', { prefix, iri }, o, true),
      delete: (prefix: string, o?: UpdateOptions) =>
        call<boolean>('prefixes.delete', { prefix }, o, true),
    };
    this.snapshots = {
      list: (o?: OperationOptions) => call<SnapshotInfo[]>('snapshots.list', {}, o, false),
      get: (name: string, o?: OperationOptions) =>
        call<SnapshotInfo | null>('snapshots.get', { name }, o, false),
      create: (
        name: string,
        options: {
          at?: string | number | bigint;
          note?: string;
          expiresMs?: number;
          warm?: boolean;
        } & UpdateOptions = {},
      ) =>
        call<{ snapshot: SnapshotInfo; created: boolean }>(
          'snapshots.create',
          { ...options, name, at: at(options.at) },
          options,
          true,
        ),
      delete: (name: string, o?: UpdateOptions) =>
        call<boolean>('snapshots.delete', { name }, o, true),
    };
    this.history = {
      tick: (o?: UpdateOptions) => call<Configuration>('history.tick', {}, o, true),
      query: async (
        options: OperationOptions & {
          from?: string | number | bigint;
          to?: string | number | bigint;
          limit?: number;
        } = {},
      ) => {
        const p = await call<any>(
          'history.query',
          {
            ...options,
            from: options.from === undefined ? undefined : at(options.from),
            to: options.to === undefined ? undefined : at(options.to),
          },
          options,
          false,
        );
        for (const change of p.changes) change.quad = decodeTerm(change.quad);
        return p as Configuration;
      },
      status: (o?: OperationOptions) => call<HistoryStatus>('history.status', {}, o, false),
      commit: (reference: string | number | bigint = 'head', o?: OperationOptions) =>
        call<CommitInfo | null>('history.commit', { reference: at(reference) }, o, false),
      commits: (
        options: {
          before?: number | bigint;
          after?: number | bigint;
          limit?: number;
        } & OperationOptions = {},
      ) =>
        call<Configuration>(
          'history.commits',
          {
            ...options,
            before: options.before === undefined ? undefined : unsigned(options.before),
            after: options.after === undefined ? undefined : unsigned(options.after),
          },
          options,
          false,
        ),
      changes: async (
        after: number | bigint,
        options: { maxCommits?: number; maxQuads?: number } & OperationOptions = {},
      ) => {
        const p = await call<any>(
          'history.changes',
          { ...options, after: unsigned(after) },
          options,
          false,
        );
        for (const c of p.commits)
          for (const change of c.changes) change.quad = decodeTerm(change.quad);
        return p as ChangePage;
      },
      diff: async (
        from: string | number | bigint,
        to: string | number | bigint = 'head',
        options: OperationOptions = {},
      ) => {
        const p = await call<{ changes: any[] }>(
          'history.diff',
          { from: at(from), to: at(to) },
          options,
          false,
        );
        return p.changes.map((c) => ({ ...c, quad: decodeTerm(c.quad) })) as Change[];
      },
      prune: (o?: UpdateOptions) => call<bigint>('history.prune', {}, o, true),
    };
    this.settings = {
      describe: setting(call, 'describe'),
      compaction: setting(call, 'compaction'),
      quota: {
        get: (o?: OperationOptions) => call<Configuration>('settings.quota.get', {}, o, false),
        set: (maxBytes: number | bigint, o?: UpdateOptions) =>
          call<Configuration>('settings.quota.set', { maxBytes: unsigned(maxBytes) }, o, true),
        reset: (o?: UpdateOptions) => call<Configuration>('settings.quota.reset', {}, o, true),
      },
      retention: setting(call, 'retention'),
      changeLog: setting(call, 'changeLog'),
    };
    // the members of a read the native call takes; options such as `signal` stay out
    const ontologyArgs = (
      o: OntologyAxiomQuery & {
        limit?: number;
        offset?: number;
        profiles?: string[];
        maxViolations?: number;
      },
    ) => ({
      graph: o.graph,
      imports: o.imports,
      mode: o.mode,
      at: o.at === undefined || typeof o.at === 'string' ? o.at : unsigned(o.at),
      inferences: o.inferences,
      type: Array.isArray(o.type) ? o.type.join(',') : o.type,
      about: o.about,
      assertions: o.assertions,
      limit: o.limit,
      offset: o.offset,
      profiles: o.profiles,
      maxViolations: o.maxViolations,
    });
    // the members that name a read
    const readSelection = (o: OntologyRead) => ({
      graph: o.graph,
      imports: o.imports,
      mode: o.mode,
      at: o.at === undefined || typeof o.at === 'string' ? o.at : unsigned(o.at),
      inferences: o.inferences,
    });
    // what an edit or a diff reads; `at` and `inferences` go on, for the native call to refuse
    const writeSelection = (o: OntologySelection) => {
      const { at, inferences } = o as OntologyRead;
      return {
        graph: o.graph,
        imports: o.imports,
        mode: o.mode,
        at: at === undefined || typeof at === 'string' ? at : unsigned(at),
        inferences,
      };
    };
    this.ontology = {
      /** The header, the counts of entities and axioms, the graphs read and the diagnostics. */
      summary: (o: OntologyRead & OperationOptions = {}) =>
        call<OntologySummary>('ontology.summary', ontologyArgs(o), o, false),
      /** One page of the axioms (`limit` and `offset`), as `GET /$/ontology/{ds}/axioms` lists them. */
      axiomPage: (
        o: OntologyAxiomQuery & OperationOptions & { limit?: number; offset?: number } = {},
      ) => call<OntologyAxiomPage>('ontology.axioms', ontologyArgs(o), o, false),
      /**
       * The axioms, read in batches of `batch` (1,000 by default) from the commit of the first
       * batch, so that a commit made meanwhile does not mix into the listing. `timeout`
       * bounds the whole listing, from the first batch on.
       */
      axioms: (
        o: OntologyAxiomQuery & OperationOptions & { batch?: number } = {},
      ): AsyncIterable<OntologyAxiom> => {
        const { batch = 1000, timeout, ...rest } = o;
        return {
          [Symbol.asyncIterator]: async function* () {
            let offset = 0;
            let at = rest.at;
            const end = timeout === undefined ? undefined : Date.now() + timeout;
            for (;;) {
              const left = end === undefined ? undefined : end - Date.now();
              if (left !== undefined && left <= 0) throw new QueryTimeoutError();
              const page = await call<OntologyAxiomPage>(
                'ontology.axioms',
                ontologyArgs({ ...rest, at, limit: batch, offset }),
                { ...rest, timeout: left },
                false,
              );
              // the later batches read the commit the first one read, whatever `at` named
              at = 'commit:' + page.commit.toString();
              yield* page.axioms;
              if (page.next === null || page.next === undefined) return;
              offset = Number(page.next);
            }
          },
        };
      },
      /** One page of the entities of the kinds in `kind` (all kinds by default). */
      entities: (
        o: OntologyRead &
          OperationOptions & { kind?: string | string[]; limit?: number; offset?: number } = {},
      ) =>
        call<OntologyEntityPage>(
          'ontology.entities',
          {
            ...readSelection(o),
            kind: Array.isArray(o.kind) ? o.kind.join(',') : o.kind,
            limit: o.limit,
            offset: o.offset,
          },
          o,
          false,
        ),
      /**
       * What the ontology says about the entity `iri`: its kinds, labels and annotations,
       * and for each kind its superclasses, domains, ranges, types and the other accessors.
       * With `assertions`, the instances, types and property assertions the store holds too,
       * at most `limit` of each. An IRI the ontology does not have as an entity is a
       * not-found error.
       */
      entity: (
        iri: string | { termType: 'NamedNode'; value: string },
        o: OntologyRead &
          OperationOptions & { kind?: string; assertions?: boolean; limit?: number } = {},
      ) =>
        call<OntologyEntity>(
          'ontology.entity',
          {
            ...readSelection(o),
            iri: typeof iri === 'string' ? iri : iri.value,
            kind: o.kind,
            assertions: o.assertions,
            limit: o.limit,
          },
          o,
          false,
        ),
      /**
       * Search the entities by label, synonym, short form and definition, with typos
       * tolerated, ranked as `GET /$/ontology/{ds}/search` ranks them. `lang` lists the
       * preferred languages, most preferred first.
       */
      search: (
        q: string,
        o: Pick<OntologyRead, 'graph' | 'imports' | 'mode'> &
          OperationOptions & {
            kind?: string | string[];
            lang?: string | string[];
            limit?: number;
            offset?: number;
            fuzzy?: boolean;
            definitions?: boolean;
          } = {},
      ) =>
        call<OntologySearchPage>(
          'ontology.search',
          {
            graph: o.graph,
            imports: o.imports,
            mode: o.mode,
            q,
            kind: Array.isArray(o.kind) ? o.kind.join(',') : o.kind,
            lang: Array.isArray(o.lang) ? o.lang.join(',') : o.lang,
            limit: o.limit,
            offset: o.offset,
            fuzzy: o.fuzzy,
            definitions: o.definitions,
          },
          o,
          false,
        ),
      /**
       * One level of the class, property or concept hierarchy, read from the store's
       * indexes without reading the whole ontology: the children of `iri`, or the roots
       * when it is absent, a page at a time, and its ancestors with `ancestors`.
       */
      hierarchy: (
        o: OntologyRead &
          OperationOptions & {
            iri?: string | { termType: 'NamedNode'; value: string };
            kind?: 'class' | 'objectProperty' | 'dataProperty' | 'annotationProperty' | 'concept';
            ancestors?: boolean;
            scheme?: string | { termType: 'NamedNode'; value: string };
            deprecated?: boolean;
            lang?: string | string[];
            limit?: number;
            offset?: number;
          } = {},
      ) =>
        call<OntologyHierarchy>(
          'ontology.hierarchy',
          {
            ...readSelection(o),
            iri: o.iri === undefined || typeof o.iri === 'string' ? o.iri : o.iri.value,
            kind: o.kind,
            ancestors: o.ancestors,
            scheme:
              o.scheme === undefined || typeof o.scheme === 'string' ? o.scheme : o.scheme.value,
            deprecated: o.deprecated,
            lang: Array.isArray(o.lang) ? o.lang.join(',') : o.lang,
            limit: o.limit,
            offset: o.offset,
          },
          o,
          false,
        ),
      /**
       * What the graphs say about the entity `iri`, read without reading the whole
       * ontology: its statements, the axioms they map to, the axioms of other entities
       * that mention it, the statements that point at it, and the names of every IRI used.
       */
      term: (
        iri: string | { termType: 'NamedNode'; value: string },
        o: OntologyRead &
          OperationOptions & {
            mentions?: number;
            incoming?: number;
            lang?: string | string[];
          } = {},
      ) =>
        call<OntologyTerm>(
          'ontology.term',
          {
            ...readSelection(o),
            iri: typeof iri === 'string' ? iri : iri.value,
            mentions: o.mentions,
            incoming: o.incoming,
            lang: Array.isArray(o.lang) ? o.lang.join(',') : o.lang,
          },
          o,
          false,
        ),
      /** One report per profile (`el`, `ql`, `rl`, `dl`; `dl` by default). */
      check: (
        o: OntologyRead &
          OperationOptions & {
            profiles?: string[];
            assertions?: boolean;
            maxViolations?: number;
          } = {},
      ) => call<OntologyCheck>('ontology.check', ontologyArgs(o), o, false),
      /** Add and remove axioms in one commit. */
      edit: (
        change: {
          add?: AxiomInput[];
          remove?: AxiomInput[];
          declare?: boolean;
          target?: OntologyGraph;
          message?: string;
        },
        o: OntologySelection & UpdateOptions = {},
      ) => {
        return call<OntologyEdit>('ontology.edit', { ...writeSelection(o), ...change }, o, true);
      },
      /** Remove an entity: its declarations, the axioms that mention it and the assertions about it. */
      removeEntity: (
        iri: string,
        o: OntologySelection & UpdateOptions & { message?: string } = {},
      ) => {
        return call<OntologyEdit>(
          'ontology.removeEntity',
          { ...writeSelection(o), iri, message: o.message },
          o,
          true,
        );
      },
      /** The axioms that differ between two states (`to` is the head by default). */
      diff: (
        from: string | number | bigint,
        o: OntologySelection &
          OperationOptions & {
            to?: string | number | bigint;
            assertions?: boolean;
            limit?: number;
          } = {},
      ) => {
        const { to, assertions, limit } = o;
        return call<OntologyDiff>(
          'ontology.diff',
          {
            ...writeSelection(o),
            from: typeof from === 'string' ? from : unsigned(from),
            to: to === undefined || typeof to === 'string' ? to : unsigned(to),
            assertions,
            limit,
          },
          o,
          false,
        );
      },
      /**
       * Classify by OWL 2 EL (spec F24): consistency, the unsatisfiable classes, the axioms
       * left out or used in part, one class (`about`) and the taxonomy's nodes (`nodes`,
       * `limit` from `offset`). `target` writes the taxonomy to that named graph in one commit.
       * `partial` answers what a run with the disjunctive rules proved when its deadline or
       * cancellation stops it, with `partial` true, the `incomplete` classes and the
       * `consistency`.
       */
      classify: (
        o: OntologySelection &
          UpdateOptions & {
            at?: string | number | bigint;
            assertions?: boolean;
            about?: string;
            nodes?: boolean;
            limit?: number;
            offset?: number;
            threads?: number;
            target?: OntologyGraph;
            message?: string;
            /** answer what a run with the disjunctive rules proved when `timeout` or `signal` stops it */
            partial?: boolean;
            /** the expansion strategy of the disjunctive rules */
            expansion?: 'cautious' | 'eager';
          } = {},
      ) =>
        call<OntologyClassification>(
          'ontology.classify',
          {
            graph: o.graph,
            imports: o.imports,
            mode: o.mode,
            at: o.at === undefined || typeof o.at === 'string' ? o.at : unsigned(o.at),
            assertions: o.assertions,
            about: o.about,
            nodes: o.nodes,
            limit: o.limit,
            offset: o.offset,
            threads: o.threads,
            target: o.target,
            message: o.message,
            partial: o.partial,
            expansion: o.expansion,
          },
          // a partial answer comes after the timeout that stopped the run
          o.partial ? ({ ...o, answersWhenStopped: true } as typeof o) : o,
          o.target !== undefined,
        ),
      /**
       * Read Manchester text against the ontology's names and labels as `as`
       * (`classExpression` by default). Text that does not read throws an
       * `InvalidInputError` whose details carry the reason, the span in UTF-16 code
       * units and the candidates of an ambiguous name.
       */
      parse: (text: string, o: OntologySelection & OperationOptions & { as?: ManchesterAs } = {}) =>
        call<ManchesterParsed>(
          'ontology.parse',
          { graph: o.graph, imports: o.imports, mode: o.mode, text, as: o.as },
          o,
          false,
        ),
      /**
       * A DL query (spec U12): the equivalents, superclasses, subclasses and instances
       * of a class expression in Manchester syntax, functional-style syntax
       * (`syntax: 'functional'`) or the JSON form. The EL classifier answers inside
       * OWL 2 EL and the RL rules outside it, and `reasoning` names the engine used.
       */
      dlQuery: (
        expression: string | Record<string, unknown>,
        o: OntologySelection &
          OperationOptions & {
            syntax?: 'manchester' | 'functional';
            reasoning?: 'auto' | 'el' | 'rl';
            answers?: DlAnswer[];
            limit?: number;
            threads?: number;
            maxDerived?: number;
          } = {},
      ) =>
        call<DlQueryAnswer>(
          'ontology.dlQuery',
          {
            graph: o.graph,
            imports: o.imports,
            mode: o.mode,
            expression,
            syntax: o.syntax,
            reasoning: o.reasoning,
            answers: o.answers,
            limit: o.limit,
            threads: o.threads,
            maxDerived: o.maxDerived,
          },
          o,
          false,
        ),
    };
    this.mappings = {
      list: (o?: OperationOptions) => call<Configuration[]>('mappings.list', {}, o, false),
      get: (name: string, version?: number | bigint, o?: OperationOptions) =>
        call<Configuration | null>(
          'mappings.get',
          { name, version: version === undefined ? undefined : unsigned(version) },
          o,
          false,
        ),
      versions: (name: string, o?: OperationOptions) =>
        call<Configuration[] | null>('mappings.versions', { name }, o, false),
      lastRun: (name: string, o?: OperationOptions) =>
        call<Configuration | null>('mappings.lastRun', { name }, o, false),
      put: (
        name: string,
        text: string,
        options: UpdateOptions & {
          kind?: string;
          description?: string;
          author?: string;
          message?: string;
          ifVersion?: number | bigint;
        } = {},
      ) =>
        call<Configuration>(
          'mappings.put',
          {
            ...options,
            name,
            text,
            ifVersion: options.ifVersion === undefined ? undefined : unsigned(options.ifVersion),
          },
          options,
          true,
        ),
      delete: (name: string, options: UpdateOptions & { ifVersion?: number | bigint } = {}) =>
        call<boolean>(
          'mappings.delete',
          {
            name,
            ifVersion: options.ifVersion === undefined ? undefined : unsigned(options.ifVersion),
          },
          options,
          true,
        ),
      run: (name: string, options: MappingRunOptions = {}) =>
        call<MappingRun>(
          'mappings.run',
          {
            ...options,
            name,
            version: options.version === undefined ? undefined : unsigned(options.version),
          },
          options,
          true,
        ),
    };
    this.queries = {
      run: (
        name: string,
        params: Configuration = {},
        options: import('@sparkles-rdf/common').QueryOptions & { version?: number | bigint } = {},
      ) => runQuery(name, params, options),
      list: (o?: OperationOptions) => call<[string, Configuration][]>('queries.list', {}, o, false),
      get: (name: string, version?: number | bigint, o?: OperationOptions) =>
        call<Configuration | null>(
          'queries.get',
          { name, version: version === undefined ? undefined : unsigned(version) },
          o,
          false,
        ),
      versions: (name: string, o?: OperationOptions) =>
        call<Configuration[] | null>('queries.versions', { name }, o, false),
      put: (
        name: string,
        definition: Configuration,
        options: UpdateOptions & { author?: string; ifVersion?: number | bigint } = {},
      ) =>
        call<Configuration>(
          'queries.put',
          {
            ...options,
            name,
            definition,
            ifVersion: options.ifVersion === undefined ? undefined : unsigned(options.ifVersion),
          },
          options,
          true,
        ),
      delete: (name: string, options: UpdateOptions & { ifVersion?: number | bigint } = {}) =>
        call<boolean>(
          'queries.delete',
          {
            name,
            ifVersion: options.ifVersion === undefined ? undefined : unsigned(options.ifVersion),
          },
          options,
          true,
        ),
    };
    const unsignedRevision = (v: number | bigint | undefined) =>
      v === undefined ? undefined : unsigned(v);
    this.algorithms = {
      projections: {
        list: (o?: OperationOptions) =>
          call<{ name: string; stored: Configuration }[]>('algo.projections.list', {}, o, false),
        get: (name: string, o?: OperationOptions) =>
          call<Configuration | null>('algo.projections.get', { name }, o, false),
        put: (
          name: string,
          definition: Configuration,
          options: UpdateOptions & { author?: string; ifRevision?: number | bigint } = {},
        ) =>
          call<{ stored: Configuration; created: boolean }>(
            'algo.projections.put',
            {
              name,
              definition,
              author: options.author,
              ifRevision: unsignedRevision(options.ifRevision),
            },
            options,
            true,
          ),
        delete: (name: string, options: UpdateOptions & { ifRevision?: number | bigint } = {}) =>
          call<boolean>(
            'algo.projections.delete',
            { name, ifRevision: unsignedRevision(options.ifRevision) },
            options,
            true,
          ),
      },
      queryText: (request: Configuration, summary = false, o?: OperationOptions) =>
        call<string>('algo.queryText', { request, summary }, o, false),
      stats: (request: Configuration, o?: OperationOptions) =>
        call<Configuration>('algo.stats', { request }, o, false),
      runWrite: (request: Configuration, options: UpdateOptions & { author?: string } = {}) =>
        call<Configuration>('algo.runWrite', { request, author: options.author }, options, true),
    };
    this.graph = {
      groups: (request: GroupsRequest, options: GroupsOptions = {}) =>
        call<GroupsAnswer>(
          'graph.groups',
          {
            request: {
              ...request,
              at:
                request.at instanceof Date
                  ? 'time:' + request.at.toISOString()
                  : typeof request.at === 'number' || typeof request.at === 'bigint'
                    ? 'commit:' + unsigned(request.at, 'at')
                    : request.at,
            },
          },
          options,
          false,
        ),
    };
    this.branches = {
      commitGraph: (
        options: OperationOptions & { branches?: string[]; before?: string; limit?: number } = {},
      ) => call<Configuration>('branches.commitGraph', { ...options }, options, false),
      list: (o?: OperationOptions) => call<BranchInfo[]>('branches.list', {}, o, false),
      get: (name: string, o?: OperationOptions) =>
        call<BranchInfo>('branches.get', { name }, o, false),
      open: getBranch,
      create: (
        name: string,
        options: UpdateOptions & {
          from?: string;
          at?: string | number | bigint;
          protected?: boolean;
          note?: string;
        } = {},
      ) =>
        call<BranchInfo>(
          'branches.create',
          { ...options, name, at: at(options.at) },
          options,
          true,
        ),
      delete: (
        name: string,
        options: UpdateOptions & { force?: boolean; reparent?: boolean } = {},
      ) => call<void>('branches.delete', { ...options, name }, options, true),
      rename: (name: string, to: string, o?: UpdateOptions) =>
        call<BranchInfo>('branches.rename', { name, to }, o, true),
      protect: (name: string, protect = true, o?: UpdateOptions) =>
        call<BranchInfo>('branches.protect', { name, protected: protect }, o, true),
      note: (name: string, note: string | null, o?: UpdateOptions) =>
        call<BranchInfo>('branches.note', { name, note }, o, true),
      relink: (name: string, o?: UpdateOptions) =>
        call<RelinkResult>('branches.relink', { name }, o, true),
      merge: (source: string, target = 'main', options: Configuration & UpdateOptions = {}) =>
        call<MergeResult>('branches.merge', { ...options, source, target }, options, true),
      previewMerge: (
        source: string,
        target = 'main',
        options: Configuration & UpdateOptions = {},
      ) =>
        call<Configuration>('branches.previewMerge', { ...options, source, target }, options, true),
      revert: (branch: string, commit: number | bigint, o: UpdateOptions = {}) =>
        call<MergeResult>('branches.revert', { branch, commit: unsigned(commit) }, o, true),
      previewRevert: (branch: string, commit: number | bigint, o: UpdateOptions = {}) =>
        call<Configuration>(
          'branches.previewRevert',
          { branch, commit: unsigned(commit) },
          o,
          true,
        ),
      cherryPick: (
        source: string,
        commit: number | bigint,
        target = 'main',
        o: UpdateOptions = {},
      ) =>
        call<MergeResult>(
          'branches.cherryPick',
          { source, target, commit: unsigned(commit) },
          o,
          true,
        ),
      previewCherryPick: (
        source: string,
        commit: number | bigint,
        target = 'main',
        o: UpdateOptions = {},
      ) =>
        call<Configuration>(
          'branches.previewCherryPick',
          { source, target, commit: unsigned(commit) },
          o,
          true,
        ),
      exempt: {
        get: (o?: OperationOptions) => call<string[]>('branches.exempt.get', {}, o, false),
        set: (predicates: string[], o?: UpdateOptions) =>
          call<string[]>('branches.exempt.set', { predicates }, o, true),
      },
    };
    const index = (name: string) => ({
      status: (o?: OperationOptions) =>
        call<Configuration | null>(`indexes.${name}.status`, {}, o, false),
      enable: (config: Configuration = {}, o?: UpdateOptions) =>
        call<Configuration>(`indexes.${name}.enable`, config, o, true),
      disable: (o?: UpdateOptions) => call<void>(`indexes.${name}.disable`, {}, o, true),
      rebuild: (o?: UpdateOptions) => call<Configuration>(`indexes.${name}.rebuild`, {}, o, true),
    });
    this.indexes = {
      text: {
        ...index('text'),
        search: (
          query: string,
          options: OperationOptions & { limit?: number; lang?: string; highlight?: boolean } = {},
        ) => call<Configuration>('indexes.text.search', { ...options, query }, options, false),
      },
      geo: {
        ...index('geo'),
        features: (
          bbox: [number, number, number, number],
          options: OperationOptions & {
            graph?: string;
            predicate?: string;
            limit?: number;
            tolerance?: number;
          } = {},
        ) => call<Configuration>('indexes.geo.features', { ...options, bbox }, options, false),
      },
      vector: {
        recall: (
          name: string,
          options: OperationOptions & { samples?: number; k?: number; ef?: number } = {},
        ) => call<Configuration>('indexes.vector.recall', { name, ...options }, options, false),
        reembed: (name: string, o?: UpdateOptions) =>
          call<void>('indexes.vector.reembed', { name }, o, true),
        /** Executes embedding work until idle; waitMs bounds the engine worker. */
        embedUntilIdle: (waitMs = 30000) => {
          if (!Number.isSafeInteger(waitMs) || waitMs <= 0 || waitMs > 600000)
            throw new RangeError('waitMs must be in 1..600000');
          return call<void>('indexes.vector.embedUntilIdle', { waitMs }, {}, true);
        },
        list: (o?: OperationOptions) => call<Configuration[]>('indexes.vector.list', {}, o, false),
        get: (name: string, o?: OperationOptions) =>
          call<Configuration | null>('indexes.vector.get', { name }, o, false),
        put: (name: string, config: Configuration, o?: UpdateOptions) =>
          call<boolean>('indexes.vector.put', { name, config }, o, true),
        drop: (name: string, o?: UpdateOptions) =>
          call<void>('indexes.vector.drop', { name }, o, true),
        rebuild: (name: string, o?: UpdateOptions) =>
          call<void>('indexes.vector.rebuild', { name }, o, true),
        wait: (name: string, o?: UpdateOptions) =>
          call<Configuration | null>('indexes.vector.wait', { name }, o, false),
      },
    };
    this.reasoning = {
      diagnostics: (
        options: OperationOptions & {
          checks?: string[];
          graphs?: string[];
          limit?: number;
          inferences?: boolean;
          justify?: boolean;
          maxChecks?: number;
          maxMemory?: number;
        } = {},
      ) => call<Configuration>('reasoning.diagnostics', { ...options }, options, false),
      /** Inferred additions and removals under one program, without writing. */
      diff: (from: string, to = 'head', options: OperationOptions & Configuration = {}) =>
        call<Configuration>('reasoning.diff', { ...options, from, to }, options, false),
      /** Minimal asserted supports for a finding, a class, or all relevant classes. */
      justify: (
        target: { finding?: Configuration; class?: string; classes?: 'all' },
        options: OperationOptions & Configuration = {},
      ) => call<Configuration>('reasoning.justify', { ...options, ...target }, options, false),
      /** Why the dataset entails a triple, given in Turtle with the dataset's prefixes: derivation trees, or `entailed: false`. */
      explain: (
        triple: string,
        options: OperationOptions & {
          mode?: 'why' | 'whyNot';
          source?: 'materialized' | 'demand';
          profile?: string;
          maxTrees?: number;
          maxDepth?: number;
          maxMissing?: number;
          maxHypotheses?: number;
        } = {},
      ) => call<Configuration>('reasoning.explain', { triple, ...options }, options, false),
      status: (o?: OperationOptions) =>
        call<Configuration | null>('reasoning.status', {}, o, false),
      /** `maxInferred` is the derived triples a run may produce before it fails: 50 million by default, 0 for no limit. */
      run: (
        options: UpdateOptions & {
          profile?: string;
          incremental?: boolean;
          maxInferred?: number;
        } = {},
      ) => call<Configuration>('reasoning.run', { profile: 'rdfs', ...options }, options, true),
      clear: (o?: UpdateOptions) => call<bigint>('reasoning.clear', {}, o, true),
      rdfs: {
        get: (o?: OperationOptions) =>
          call<{ enabled: boolean }>('reasoning.rdfs.get', {}, o, false),
        set: (graph: string, o?: UpdateOptions) =>
          call<void>('reasoning.rdfs.set', { graph }, o, true),
        reset: (o?: UpdateOptions) => call<void>('reasoning.rdfs.reset', {}, o, true),
      },
    };
    this.validation = {
      shacl: (
        shapes: string,
        options: OperationOptions & { format?: string; baseIri?: string } = {},
      ) =>
        call<ValidationReport>(
          'validation.shacl',
          { format: 'ttl', ...options, shapes },
          options,
          false,
        ),
      shex: (schema: string, shapeMap: string, o?: OperationOptions) =>
        call<ValidationReport>('validation.shex', { schema, shapeMap }, o, false),
      guard: {
        get: (o?: OperationOptions) =>
          call<Configuration | null>('validation.guard.get', {}, o, false),
        setShacl: (config: Configuration, o?: UpdateOptions) =>
          call<GuardOutcome>('validation.guard.setShacl', config, o, true),
        setShex: (config: Configuration, o?: UpdateOptions) =>
          call<GuardOutcome>('validation.guard.setShex', config, o, true),
        reset: (o?: UpdateOptions) => call<void>('validation.guard.reset', {}, o, true),
      },
    };
  }
}
// Native numeric strings avoid lossy JSON parsing. Convert only numeric schema fields,
// leaving user literals, prefixes, notes and stored query text untouched.
const counts = new Set([
  'seq',
  'parent',
  'quads',
  'rows',
  'inserted',
  'deleted',
  'head',
  'after',
  'next',
  'added',
  'removed',
  'commit',
  'firstCommit',
  'baseSeq',
  'endSeq',
  'bytes',
  'cacheBytes',
  'hits',
  'misses',
  'materializations',
  'maxBytes',
  'usedBytes',
  'inferred',
  'version',
  'ifVersion',
  'minDeltaQuads',
  'maxDeltaQuads',
  'maxDeltaMb',
  'maxWalMb',
  'idleSeconds',
  'maxAgeSeconds',
  'minIntervalSeconds',
  'keepCommits',
  'keepAgeMs',
  'segmentBytes',
  'defaultMaxBytes',
  'bulkMaxQuads',
]);
const numbers = new Set([
  'createdMs',
  'expiresMs',
  'snapshots',
  'cacheEntries',
  'format',
  'dimension',
  'iterations',
  'millis',
  'limit',
  'reportLimit',
  'maxDepth',
  'version',
  'ifVersion',
]);
export function adminResult(v: any, key = ''): any {
  if (['definition', 'config', 'variables', 'data', 'default'].includes(key)) return v;
  if (typeof v === 'string' && /^\d+$/.test(v)) {
    if (counts.has(key)) return BigInt(v);
    if (numbers.has(key)) return Number(v);
  }
  if (Array.isArray(v) && key === 'reconstructable') return v.map((pair) => pair.map(BigInt));
  if (Array.isArray(v)) return v.map((item) => adminResult(item, key));
  if (v && typeof v === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, value] of Object.entries(v)) out[k] = adminResult(value, k);
    return out;
  }
  return v;
}
