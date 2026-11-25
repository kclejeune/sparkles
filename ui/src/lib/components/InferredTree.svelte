<script lang="ts">
  import { untrack } from 'svelte';
  import { InferredTree, type InferredKind } from '$lib/inferred-tree.svelte';
  import type { InferredPins } from '$lib/inferred-read';
  import { isGroup, groupIri } from '$lib/individual-tree.svelte';
  import { iriForm } from '$lib/iri-form.svelte';
  import { fmtCount } from '$lib/format';
  import { consistencyInfo } from '$lib/ontology';
  import OntoTree from './OntoTree.svelte';
  import MaterializationNotice from './MaterializationNotice.svelte';
  let {
    target,
    graph,
    imports = true,
    commit,
    kind,
    selected = null,
    onopen,
    pins,
    refresh = 0,
    card = false,
  }: {
    target: string;
    graph: string | null;
    imports?: boolean;
    commit: number | null;
    kind: InferredKind;
    selected?: string | null;
    onopen: (iri: string) => void;
    pins?: InferredPins;
    refresh?: number;
    /** Lay the frame out as a sidebar card: the heading in the card's ruled head and the rest in
     *  its padded body. The containing element supplies the `panel` surface. */
    card?: boolean;
  } = $props();
  const model = new InferredTree(
    (source) => pins?.acceptMaterialization(source) ?? true,
    (result) => pins?.acceptClassification(result.resultId) ?? true,
  );
  let open = $state(new Set<string>());
  let lastRefresh = -1;
  $effect(() => {
    const view =
      commit == null
        ? null
        : { target, selection: { graph, imports }, commit, languages: [...iriForm.languages] };
    const k = kind;
    const changed = refresh !== lastRefresh;
    lastRefresh = refresh;
    // The containing page gives both regions the same revision-scoped pin object.
    const currentPins = pins;
    untrack(() => {
      model.reset(view, k, changed, currentPins);
      open = new Set();
    });
  });
  $effect(() => {
    const iri = selected;
    const context = [
      target,
      graph,
      imports,
      commit,
      kind,
      refresh,
      pins,
      iriForm.languages.join('\u0000'),
    ];
    if (iri && commit != null) {
      untrack(
        () =>
          void model.reveal(iri).then((ancestors) => {
            if (
              context[0] === target &&
              context[1] === graph &&
              context[2] === imports &&
              context[3] === commit &&
              context[4] === kind &&
              context[5] === refresh &&
              context[6] === pins &&
              context[7] === iriForm.languages.join('\u0000') &&
              iri === selected
            )
              open = new Set([...open, ...ancestors]);
          }),
      );
    } else untrack(() => model.cancelReveal());
  });
  $effect(() => () => model.drop());
  const individuals = $derived(kind === 'NamedIndividual');
  const roots = $derived(individuals ? model.individuals.roots() : model.tree.roots());
  const label = $derived(
    kind === 'Class'
      ? 'Classes (kept inferred)'
      : kind === 'NamedIndividual'
        ? 'Individuals (materialized inferred types)'
        : `${kind === 'ObjectProperty' ? 'Object' : 'Data'} properties (materialized inferred)`,
  );
  const name = (id: string): string => {
    if (id === '@untyped') return 'No named type';
    const iri = groupIri(id) ?? id;
    return iriForm.mode === 'label' ? (model.names[iri]?.label ?? iri) : iri;
  };
  function toggle(id: string) {
    const next = new Set(open);
    if (next.has(id)) next.delete(id);
    else {
      next.add(id);
      void (individuals ? model.individuals.load(id) : model.tree.load(id));
    }
    open = next;
  }
  function select(id: string) {
    if (individuals && isGroup(id)) toggle(id);
    else onopen(id);
  }
</script>

<section class="inferred-tree" class:panel={card} aria-label={label}>
  <div class="heading" class:panel-head={card}>
    <svelte:element this={card ? 'h2' : 'h3'}>
      {kind === 'Class'
        ? 'Kept class hierarchy'
        : individuals
          ? 'Materialized individual types'
          : 'Materialized property hierarchy'}
    </svelte:element>
    <button class="btn sm ghost" disabled={commit == null} onclick={() => model.refresh()}
      >Read again</button
    >
  </div>
  <div class="frame-body" class:panel-body={card}>
    {#if commit == null}<p>Waiting for the displayed source commit.</p>{/if}
    {#if model.error}<p class="error-box" role="alert">{model.error}</p>{/if}
    {#if kind === 'Class' && model.reply?.result}
      {@const outcome = model.reply.outcome}
      {@const consistency = outcome
        ? consistencyInfo(outcome.consistency, undefined, outcome.partial)
        : null}
      {#if outcome}
        <div class="status-row" role="status">
          {#if outcome.tier}<span class="status-badge">{outcome.tier.toUpperCase()}</span>{/if}
          {#if outcome.partial}
            <span class="status-badge warn"
              >Partial · {fmtCount(outcome.incompleteCount, 'open class', 'open classes')}</span
            >
          {/if}
          {#if consistency}
            <span
              class="status-badge"
              class:ok={consistency.tone === 'ok'}
              class:warn={consistency.tone === 'warn'}
              class:danger={consistency.tone === 'danger'}>{consistency.label}</span
            >
          {/if}
        </div>
      {/if}
      <p class="faint">
        Kept classification at commit {model.reply.commit}.
        {#if outcome?.partial}
          Partial classification. {outcome.incompleteCount}
          {outcome.incompleteCount === 1 ? 'class remains' : 'classes remain'} incomplete.
        {:else if outcome?.complete}
          Complete for its supported source.
        {:else}
          Classification is incomplete.
        {/if}
        {#if outcome && !outcome.complete && outcome.excludedCount > 0}
          {outcome.excludedCount} omitted or partly used axioms.
        {/if}
        {#if outcome && !outcome.complete && outcome.unmappedCount > 0}
          {outcome.unmappedCount}
          {outcome.unmappedCount === 1 ? 'triple' : 'triples'} of the input map to no axiom.
        {/if}
        {#if outcome?.consistency === 'unknown'}Consistency is unknown.{/if}
        {outcome?.consistency === 'inconsistent'
          ? 'The ontology is inconsistent; every class is unsatisfiable.'
          : ''}
      </p>
      <details>
        <summary>Classification source</summary>
        <p class="mono small">{model.reply.result.resultId}</p>
      </details>
      <p class="faint">Equivalent names share one group. Explanations are unavailable.</p>
    {:else if model.source}
      <MaterializationNotice source={model.source} />
      <p class="faint">
        {individuals
          ? 'Groups contain named materialized types, including indirect types.'
          : 'Edges connect direct groups in the recorded closure; mutually related names share one group.'}
      </p>
    {/if}
    {#if model.ancestryTruncated}<p class="note">
        The ancestor read limit was reached. Some paths may be absent.
      </p>{/if}
    {#if selected && model.canonical(selected) !== selected}<p class="faint">
        The selected name belongs to the group represented by <code
          >{model.canonical(selected)}</code
        >.
      </p>{/if}
    {#if roots.length}
      <OntoTree
        {label}
        {roots}
        {name}
        {open}
        selected={selected ? model.canonical(selected) : null}
        children={(id) => (individuals ? model.individuals.children(id) : model.tree.children(id))}
        more={(id) => (individuals ? model.individuals.more(id) : model.tree.more(id))}
        clipped={(id) => (individuals ? model.individuals.clippedOf(id) : model.tree.clippedOf(id))}
        onmore={(id) => (individuals ? model.individuals.loadMore(id) : model.tree.loadMore(id))}
        count={(id) =>
          individuals ? (isGroup(id) ? model.individuals.counts[id] : 0) : model.tree.counts[id]}
        badge={(id) => {
          if (individuals && !isGroup(id))
            return (model.individuals.types[id] ?? []).map(name).join(', ') || null;
          const n = model.names[id]?.equivalenceCount ?? 1;
          const isOpen = !individuals && !!model.incomplete?.[id];
          const parts: string[] = [];
          if (n > 1) parts.push(`${n} equivalent names`);
          if (isOpen) parts.push('open');
          return parts.length ? parts.join(' · ') : null;
        }}
        badgeTone={(id) => (!individuals && model.incomplete?.[id] ? 'warn' : null)}
        onselect={select}
        ontoggle={toggle}
      />
    {:else if model.tree.loading || model.individuals.loading}<p>Reading inferred hierarchy…</p>
    {:else if !model.error && commit != null}<p>
        No groups were returned by this source. This does not establish completeness beyond the
        supported reasoning profile.
      </p>{/if}
  </div>
</section>

<style>
  .inferred-tree {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  h2,
  h3 {
    margin: 0;
  }
  /* in a card a long title wraps beside its button, which keeps to the right as in Hierarchy */
  .panel-head {
    flex-wrap: nowrap;
  }
  .panel-head > :global(h2) {
    flex: 1 1 0;
    min-width: 0;
  }
  .panel-head > button {
    flex: none;
    margin-left: auto;
  }
  /* in a card the first and last lines sit on the body's padding, as in the other cards */
  .panel-body > :global(:first-child),
  .panel-body > :global(.materialization-notice:first-child > p:first-child) {
    margin-top: 0;
  }
  .panel-body > :global(:last-child) {
    margin-bottom: 0;
  }
  .status-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
  }
  .status-badge {
    font-size: var(--fs-xs);
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--surface-2, var(--hover));
    color: var(--text-2);
    font-weight: 500;
  }
  .status-badge.ok {
    color: var(--ok);
    background: var(--ok-soft);
  }
  .status-badge.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .status-badge.danger {
    color: var(--danger);
    background: var(--danger-soft);
  }
</style>
