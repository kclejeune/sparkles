// Bounded inferred trees. Each region holds one exact source pin and never computes.
import { errorMessage } from './api';
import {
  fetchHierarchy,
  fetchIndividuals,
  type Hierarchy,
  type Name,
  type TreeNode,
} from './browse';
import {
  readKeptClassification,
  type ClassificationName,
  type ClassificationResult,
  type KeptClassification,
} from './classification-read';
import {
  captureView,
  sameView,
  validateMaterialization,
  type InferredView,
  type InferredPins,
  type MaterializationSource,
} from './inferred-read';
import { IndividualTree } from './individual-tree.svelte';
import { LevelTree } from './level-tree.svelte';

export type InferredKind = 'Class' | 'ObjectProperty' | 'DataProperty' | 'NamedIndividual';
type RevealContext = { generation: number; iri: string; signal: AbortSignal };
const OWL_THING = 'http://www.w3.org/2002/07/owl#Thing';
const MAX_ANCESTORS = 2000;
const MAX_ANCESTOR_READS = 64;
const emptyNode = (row: ClassificationName): TreeNode => {
  if (
    typeof row.iri !== 'string' ||
    !Number.isSafeInteger(row.children) ||
    row.children! < 0 ||
    !Number.isSafeInteger(row.parents) ||
    row.parents! < 0 ||
    !Number.isSafeInteger(row.equivalenceCount) ||
    row.equivalenceCount! < 1
  )
    throw new Error('The kept hierarchy returned invalid node counts.');
  return {
    ...row,
    shortForm: null,
    kinds: ['class'],
    deprecated: false,
    children: row.children!,
    parents: row.parents!,
    equivalenceCount: row.equivalenceCount,
  };
};
function sameResult(a: ClassificationResult, b: ClassificationResult) {
  return (
    a.resultId === b.resultId &&
    a.classifiedCommit === b.classifiedCommit &&
    a.boundStoreId === b.boundStoreId &&
    a.familyId === b.familyId &&
    a.inputFingerprint === b.inputFingerprint &&
    a.sourceFingerprint === b.sourceFingerprint &&
    a.contentFingerprint === b.contentFingerprint
  );
}

export class InferredTree {
  names = $state<Record<string, Name & { equivalenceCount?: number }>>({});
  aliases = $state<Record<string, string>>({});
  source = $state<MaterializationSource | null>(null);
  reply = $state<KeptClassification | null>(null);
  error = $state<string | null>(null);
  ancestryTruncated = $state(false);
  readonly tree: LevelTree;
  readonly individuals: IndividualTree;
  #view: InferredView | null = null;
  #kind: InferredKind = 'Class';
  #result: ClassificationResult | null = null;
  #epoch = 0;
  #abort = new AbortController();
  #revealGeneration = 0;
  #revealAbort = new AbortController();
  #after = new Map<string, string>();
  #blocked = false;
  #pins: InferredPins | undefined;

  constructor(
    private acceptMaterialization: (source: MaterializationSource) => boolean = () => true,
    private acceptClassification: (result: ClassificationResult) => boolean = () => true,
  ) {
    const take = (rows: Name[]) => {
      this.names = { ...this.names, ...Object.fromEntries(rows.map((row) => [row.iri, row])) };
    };
    this.tree = new LevelTree((query) => this.#read(query), take);
    this.individuals = new IndividualTree(
      (query, signal) => this.#readIndividuals(query, signal),
      take,
    );
  }
  canonical(iri: string) {
    return this.aliases[iri] ?? iri;
  }
  cancelReveal() {
    this.#revealGeneration++;
    this.#revealAbort.abort();
    this.#revealAbort = new AbortController();
  }
  drop() {
    this.#epoch++;
    this.cancelReveal();
    this.#abort.abort();
    this.#abort = new AbortController();
    this.#view = null;
    this.#result = null;
    this.#blocked = false;
    this.#after.clear();
    this.tree.drop();
    this.individuals.drop();
    this.names = {};
    this.aliases = {};
    this.source = null;
    this.reply = null;
    this.error = null;
    this.ancestryTruncated = false;
  }
  reset(view: InferredView | null, kind: InferredKind, force = false, pins?: InferredPins) {
    if (
      !force &&
      view &&
      this.#view &&
      kind === this.#kind &&
      pins === this.#pins &&
      sameView(view, this.#view)
    )
      return;
    this.drop();
    this.#pins = pins;
    if (!view) return;
    this.#view = captureView(view);
    this.#kind = kind;
    if (kind === 'NamedIndividual') this.individuals.reset();
    else this.tree.reset();
  }
  refresh() {
    const view = this.#view;
    this.reset(view, this.#kind, true, this.#pins);
  }
  #obsolete(reveal?: RevealContext) {
    return (
      reveal != null && (reveal.generation !== this.#revealGeneration || reveal.signal.aborted)
    );
  }
  #active(epoch: number, reveal?: RevealContext) {
    if (epoch !== this.#epoch || this.#abort.signal.aborted || this.#obsolete(reveal))
      throw new DOMException('The inferred view changed.', 'AbortError');
    if (this.#blocked) throw new Error('The inferred source changed. Read it again.');
  }
  #fail(error: unknown, epoch: number, reveal?: RevealContext) {
    if (epoch !== this.#epoch || this.#abort.signal.aborted || this.#obsolete(reveal)) return;
    if (this.#blocked) return;
    this.error = errorMessage(error);
    this.#blocked = true;
    this.tree.drop();
    this.individuals.drop();
    this.names = {};
    this.aliases = {};
    this.source = null;
  }
  #materialization(source: MaterializationSource | undefined, view: InferredView) {
    const valid = validateMaterialization(source, view, this.source?.sourceId);
    if (!this.acceptMaterialization(valid))
      throw new Error('The materialized frame and hierarchy sources differ. Read them again.');
    this.source = valid;
  }
  async #classPage(
    part: 'roots' | 'children' | 'parents',
    iri: string | undefined,
    offset = 0,
    limit = 100,
    reveal?: RevealContext,
  ) {
    const view = this.#view;
    if (!view) throw new Error('No displayed revision is available.');
    const epoch = this.#epoch;
    this.#active(epoch, reveal);
    const reply = await readKeptClassification(
      { ...view, iri: iri ?? OWL_THING, mode: 'lenient' },
      {
        part,
        about: iri,
        offset,
        limit,
        resultId: this.#result?.resultId ?? this.#pins?.classificationId,
      },
      reveal?.signal ?? this.#abort.signal,
    );
    this.#active(epoch, reveal);
    this.reply = reply;
    if (
      reply.commit !== view.commit ||
      reply.status !== 'available' ||
      !reply.result ||
      !reply.outcome
    )
      throw new Error(
        reply.status === 'running'
          ? 'Classification is running. Read the kept tree again when it finishes.'
          : 'No matching kept classification is available for this tree.',
      );
    const result = reply.result;
    if (
      result.classifiedCommit !== view.commit ||
      result.mode !== 'lenient' ||
      result.imports !== (view.selection.imports ? 'dataset' : 'none') ||
      !result.capabilities.includes('namedClassRoots') ||
      !result.capabilities.includes('namedClassDirectChildren') ||
      (this.#result && !sameResult(result, this.#result))
    )
      throw new Error('The kept hierarchy changed its classification source.');
    if (!this.acceptClassification(result))
      throw new Error(
        'The class frame and hierarchy sources differ. Reload this view to select a new source.',
      );
    this.#result = result;
    const page = reply.page;
    if (
      !page ||
      page.part !== part ||
      page.about !== (part === 'roots' ? null : iri) ||
      page.offset !== offset ||
      page.limit !== limit ||
      page.items.length > limit ||
      !Number.isSafeInteger(page.total) ||
      page.total < offset + page.items.length ||
      (page.nextOffset != null &&
        (page.nextOffset !== offset + page.items.length || page.nextOffset <= offset))
    )
      throw new Error('The kept hierarchy returned an invalid page.');
    if (iri && reply.entity?.present && reply.entity.representative)
      this.aliases = { ...this.aliases, [iri]: reply.entity.representative };
    return { reply, page, nodes: page.items.map((row) => emptyNode(row as ClassificationName)) };
  }
  async #classHierarchy(
    query: {
      iri?: string;
      ancestors?: boolean;
      offset?: number;
    },
    reveal?: RevealContext,
  ): Promise<Hierarchy> {
    const epoch = this.#epoch;
    const { reply, page, nodes } = await this.#classPage(
      query.iri ? 'children' : 'roots',
      query.iri,
      query.offset,
      query.ancestors ? 1 : 100,
      reveal,
    );
    this.#active(epoch, reveal);
    const entity = reply.entity;
    const node =
      entity?.present && entity.representative
        ? emptyNode({
            iri: entity.representative,
            label: this.names[entity.representative]?.label ?? null,
            labelLang: null,
            children: entity.children ?? undefined,
            parents: entity.parents ?? undefined,
            equivalenceCount: entity.equivalenceCount ?? undefined,
          })
        : null;
    const ancestors: NonNullable<Hierarchy['ancestors']> = {
      nodes: [],
      edges: [],
      truncated: false,
    };
    if (query.ancestors && node) {
      const seen = new Map<string, TreeNode>([[node.iri, node]]);
      const queue = [node.iri];
      let reads = 0;
      while (queue.length && !ancestors.truncated) {
        const child = queue.shift()!;
        let offset = 0;
        while (true) {
          if (++reads > MAX_ANCESTOR_READS) {
            ancestors.truncated = true;
            break;
          }
          const parents = await this.#classPage('parents', child, offset, 100, reveal);
          this.#active(epoch, reveal);
          for (const parent of parents.nodes) {
            if (parent.iri === OWL_THING && parent.equivalenceCount === 1) continue;
            if (
              ancestors.edges.length >= MAX_ANCESTORS * 4 ||
              (!seen.has(parent.iri) && seen.size >= MAX_ANCESTORS)
            ) {
              ancestors.truncated = true;
              break;
            }
            ancestors.edges.push([child, parent.iri]);
            if (!seen.has(parent.iri)) {
              seen.set(parent.iri, parent);
              queue.push(parent.iri);
            }
          }
          if (ancestors.truncated || parents.page.nextOffset == null) break;
          offset = parents.page.nextOffset;
        }
      }
      ancestors.nodes = [...seen.values()].filter((row) => row.iri !== node.iri);
      this.ancestryTruncated = ancestors.truncated;
    }
    return {
      dataset: this.#view!.target,
      commit: reply.commit,
      kind: 'class',
      node,
      children: nodes,
      total: page.total,
      offset: page.offset,
      limit: page.limit,
      next: page.nextOffset,
      order: 'iri',
      ...(query.ancestors ? { ancestors } : {}),
    };
  }
  async #read(
    query: { iri?: string; ancestors?: boolean; offset?: number },
    reveal?: RevealContext,
  ): Promise<Hierarchy> {
    const view = this.#view;
    if (!view) throw new Error('No displayed revision is available.');
    const epoch = this.#epoch;
    try {
      this.#active(epoch, reveal);
      if (query.iri && !this.#result && !this.source) await this.tree.load(null);
      this.#active(epoch, reveal);
      if (this.#kind === 'Class') return await this.#classHierarchy(query, reveal);
      const key = `${query.iri ?? ''}\u0000${query.offset ?? 0}`;
      const after = query.offset ? this.#after.get(key) : undefined;
      if (query.offset && after == null)
        throw new Error('The materialized page cursor is unavailable. Read the hierarchy again.');
      const page = await fetchHierarchy(
        view.target,
        { ...view.selection, at: `commit:${view.commit}`, inferences: true },
        {
          kind: this.#kind === 'ObjectProperty' ? 'objectProperty' : 'dataProperty',
          iri: query.iri,
          ancestors: query.ancestors,
          limit: query.ancestors ? 1 : 100,
          after,
          sourceId: this.source?.sourceId ?? this.#pins?.materializationId,
          lang: view.languages,
        },
        reveal?.signal ?? this.#abort.signal,
      );
      this.#active(epoch, reveal);
      this.#materialization(page.inferenceSource, view);
      if (
        page.commit !== view.commit ||
        page.hierarchyBasis !== 'directEquivalenceGroups' ||
        page.order !== 'iri' ||
        page.offset !== (query.offset ?? 0) ||
        page.children.length > (query.ancestors ? 1 : 100) ||
        (page.nextAfter != null &&
          (!page.children.length || page.nextAfter !== page.children.at(-1)?.iri))
      )
        throw new Error('The materialized hierarchy changed its page membership.');
      if (query.iri && page.node) this.aliases = { ...this.aliases, [query.iri]: page.node.iri };
      if (page.nextAfter != null)
        this.#after.set(
          `${query.iri ?? ''}\u0000${page.offset + page.children.length}`,
          page.nextAfter,
        );
      if (page.ancestors?.truncated) this.ancestryTruncated = true;
      return page;
    } catch (error) {
      this.#fail(error, epoch, reveal);
      throw error;
    }
  }
  async #readIndividuals(
    query: import('./individual-tree.svelte').IndividualQuery,
    signal: AbortSignal,
    reveal?: RevealContext,
  ) {
    const view = this.#view;
    if (!view) throw new Error('No displayed revision is available.');
    const epoch = this.#epoch;
    try {
      this.#active(epoch, reveal);
      const page = await fetchIndividuals(
        view.target,
        { ...view.selection, at: `commit:${view.commit}`, inferences: true },
        {
          ...query,
          at: `commit:${view.commit}`,
          sourceId: this.source?.sourceId ?? this.#pins?.materializationId,
          lang: view.languages,
        },
        signal,
      );
      this.#active(epoch, reveal);
      this.#materialization(page.inferenceSource, view);
      if (page.commit !== view.commit || page.typeBasis !== 'materializedNamedTypes')
        throw new Error('The materialized individual types changed source.');
      return page;
    } catch (error) {
      this.#fail(error, epoch, reveal);
      throw error;
    }
  }
  async reveal(iri: string) {
    this.cancelReveal();
    const epoch = this.#epoch;
    const reveal: RevealContext = {
      generation: this.#revealGeneration,
      iri,
      signal: AbortSignal.any([this.#abort.signal, this.#revealAbort.signal]),
    };
    try {
      this.#active(epoch, reveal);
      if (this.#kind === 'NamedIndividual')
        return await this.individuals.reveal(iri, {
          signal: reveal.signal,
          read: (query, signal) => this.#readIndividuals(query, signal, reveal),
        });
      await this.tree.load(null);
      this.#active(epoch, reveal);
      const answer = await this.#read({ iri, ancestors: true }, reveal);
      this.#active(epoch, reveal);
      return await this.tree.reveal(this.canonical(iri), {
        answer,
        signal: reveal.signal,
        read: (query) => this.#read(query, reveal),
      });
    } catch (error) {
      this.#fail(error, epoch, reveal);
      return [];
    }
  }
}
