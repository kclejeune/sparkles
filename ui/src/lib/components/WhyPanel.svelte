<script lang="ts">
  // Why is this inferred? (docs/specs/U10-canvas-lenses.md §2.4): the derivation trees of
  // an inferred edge, with **Show on graph** to draw the premises, **Next** for another
  // derivation whose top rule differs, and the stale note of C08.
  import { onDestroy, untrack } from 'svelte';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import {
    explainTriple,
    footer,
    graft,
    how,
    maxId,
    sameRead,
    MAX_TREES,
    premiseTriples,
    staleNote,
    type Derivation,
    type WhyResult,
    type WhyTriple,
  } from '$lib/why';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    ds,
    triple,
    prefixes,
    past = false,
    onshow,
    onopen,
    onclose,
  }: {
    /** The dataset, as `name@branch` on a branch. */
    ds: string;
    triple: WhyTriple;
    prefixes: PrefixMap;
    /**
     * The canvas shows a past state. The route explains the current inferences only, so
     * the panel says so instead of a tree that may not hold at that state.
     */
    past?: boolean;
    /** Draw these triples on the canvas and highlight them. */
    onshow?: (triples: WhyTriple[]) => void;
    /** Open a resource the tree names. */
    onopen?: (iri: string) => void;
    onclose?: () => void;
  } = $props();

  let result = $state<WhyResult | null>(null);
  let index = $state(0);
  let trees = $state<Derivation[]>([]);
  /** Nodes folded by the person, by id. */
  let folded = $state(new Set<number>());
  /** `ref` nodes being expanded, by id. */
  let expanding = $state(new Set<number>());
  let refError = $state('');
  let announce = $state('');
  let ctl: AbortController | null = null;
  /** The requests of **Expand**, stopped when the explanation they belong to goes. */
  let refCtls = new Set<AbortController>();
  /** Counts the explanations loaded, so an expansion of an earlier one is dropped. */
  let generation = 0;
  let heading: HTMLHeadingElement | undefined = $state();

  function stopAll() {
    ctl?.abort();
    for (const c of refCtls) c.abort();
    refCtls = new Set();
    generation++;
    expanding = new Set();
  }

  async function load(t: WhyTriple) {
    stopAll();
    const c = (ctl = new AbortController());
    result = null;
    index = 0;
    trees = [];
    folded = new Set();
    refError = '';
    try {
      const r = await explainTriple(ds, t, { maxTrees: MAX_TREES, signal: c.signal });
      if (c.signal.aborted) return;
      result = r;
      if (r.ok) {
        trees = r.explanation.trees;
        const top = trees[0]?.rule?.name;
        announce = `Derived by ${top ?? 'a rule'}, ${trees.length} derivation${trees.length === 1 ? '' : 's'}.`;
      } else announce = r.message;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) throw e;
    }
  }

  $effect(() => {
    const t = triple;
    void ds;
    const atPast = past;
    untrack(() => {
      if (!atPast) return void load(t);
      stopAll();
      trees = [];
      index = 0;
      result = { ok: false, kind: 'unavailable', message: PAST };
      announce = PAST;
    });
  });
  $effect(() => {
    if (heading) heading.focus();
  });
  onDestroy(stopAll);

  const CHANGED =
    'The inferences have changed. Close Why? and open it again to see the current derivation.';
  const PAST =
    'Why? explains inferences at the latest commit only. Move the time slider to the latest commit to see how this edge was derived.';
  const explanation = $derived(result?.ok ? result.explanation : null);
  const tree = $derived(trees[index] ?? null);
  const stale = $derived(explanation ? staleNote(explanation) : null);
  const graphLabel = (g: string) => (g === 'default' ? 'default' : displayIri(g, prefixes));

  function next() {
    if (!trees.length) return;
    index = (index + 1) % trees.length;
    announce = `Derivation ${index + 1} of ${trees.length}, by ${trees[index]?.rule?.name ?? 'a rule'}.`;
  }

  function show() {
    if (tree) onshow?.(premiseTriples(tree));
  }

  function toggle(id: number) {
    const f = new Set(folded);
    if (f.has(id)) f.delete(id);
    else f.add(id);
    folded = f;
  }

  /** A `ref` node: ask for its own derivation and put it in place. */
  async function expandRef(d: Derivation) {
    if (expanding.has(d.id)) return;
    // the tree the ref belongs to, which Next may leave while the request runs
    const at = index;
    const gen = generation;
    const c = new AbortController();
    refCtls.add(c);
    expanding = new Set([...expanding, d.id]);
    refError = '';
    try {
      const r = await explainTriple(ds, d.triple, { maxTrees: 1, signal: c.signal });
      if (gen !== generation) return;
      if (r.ok && explanation && !sameRead(explanation, r.explanation)) {
        refError = CHANGED;
      } else if (r.ok && r.explanation.trees[0]) {
        const sub = r.explanation.trees[0];
        // the new ids pass every tree's, since folds and expansions are keyed by id
        const floor = maxId(trees);
        trees = trees.map((t, i) => (i === at ? graft(t, d.id, sub, floor) : t));
      } else if (!r.ok) refError = r.message;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) throw e;
    } finally {
      refCtls.delete(c);
      if (gen === generation) {
        const e = new Set(expanding);
        e.delete(d.id);
        expanding = e;
      }
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose?.();
    }
  }
</script>

{#snippet line(t: WhyTriple)}
  <span class="triple mono">
    <TermView term={t.s} {prefixes} {onopen} />
    <TermView term={t.p} {prefixes} />
    <TermView term={t.o} {prefixes} {onopen} />
  </span>
{/snippet}

{#snippet node(d: Derivation, root: boolean)}
  {@const kids = d.premises ?? []}
  {@const open = !folded.has(d.id)}
  <li class="node {d.kind}" data-kind={d.kind} data-rule={d.rule?.name ?? ''}>
    <div class="row">
      {#if kids.length}
        <button
          class="fold"
          aria-expanded={open}
          aria-label={open ? 'Fold premises' : 'Show premises'}
          onclick={() => toggle(d.id)}><span class="caret" class:open>▸</span></button
        >
      {:else}
        <span class="fold-pad" aria-hidden="true"></span>
      {/if}
      {#if !root}{@render line(d.triple)}{/if}
      <span class="how faint" class:root>{how(d, graphLabel)}</span>
      {#if d.kind === 'ref'}
        <button class="btn xs ghost" disabled={expanding.has(d.id)} onclick={() => expandRef(d)}>
          {#if expanding.has(d.id)}<span class="spinner"></span>{/if} Expand
        </button>
      {/if}
    </div>
    {#if open && d.bindings && Object.keys(d.bindings).length}
      <div class="pad small" aria-label="Rule bindings">
        {#each Object.entries(d.bindings) as [name, term] (name)}
          <span class="mono">?{name} = </span><TermView {term} {prefixes} {onopen} />{' '}
        {/each}
      </div>
    {/if}
    {#if open && d.builtins?.length}
      <ul aria-label="Passed conditions">
        {#each d.builtins as b}<li class="mono small">{b.call} ✓</li>{/each}
      </ul>
    {/if}
    {#if d.generalized}<p class="pad small faint">
        Intermediate rule fact. It is not stored as RDF.
      </p>{/if}
    {#if kids.length && open}
      <ul role="group">
        {#each kids as k, i (k.id + ':' + i)}
          {@render node(k, false)}
        {/each}
      </ul>
    {/if}
  </li>
{/snippet}

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="why" aria-label="Why is this inferred?" onkeydown={onKey} data-testid="why-panel">
  <div class="head">
    <h2 tabindex="-1" bind:this={heading}><Icon name="info" size={15} /> Why is this inferred?</h2>
    <button
      class="btn sm icon ghost"
      aria-label="Close Why?"
      title="Close (Escape)"
      onclick={onclose}><Icon name="x" size={14} /></button
    >
  </div>
  <div class="target">
    {@render line(triple)}
    <button class="btn sm" disabled={!tree} onclick={show}>
      <Icon name="graph" size={13} /> Show on graph
    </button>
  </div>
  <div class="sr-only" aria-live="polite">{announce}</div>
  {#if !result}
    <p class="faint row pad"><span class="spinner"></span> Searching for a derivation…</p>
  {:else if !result.ok}
    <p class="msg pad" class:error-box={result.kind === 'error'} role="status">{result.message}</p>
  {:else if tree}
    {#if stale}<p class="stale pad" role="note"><Icon name="alert" size={13} /> {stale}</p>{/if}
    {#if explanation?.partial}<p class="pad faint" role="note">
        Partial derivation. {explanation.reason}
      </p>{/if}
    <ul class="tree" role="tree" aria-label="Derivation">
      {@render node(tree, true)}
    </ul>
    {#if refError}<p class="msg pad" role="status">{refError}</p>{/if}
    <div class="foot">
      <span class="faint small">{footer(explanation!, index)}</span>
      <button
        class="btn sm"
        disabled={trees.length < 2}
        title={trees.length < 2
          ? 'No other derivation has a different top rule'
          : 'Show another derivation'}
        onclick={next}>Next</button
      >
    </div>
  {/if}
</section>

<style>
  .why {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: auto;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px 6px;
  }
  .head h2 {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-lg);
    margin: 0;
    outline: none;
  }
  .target {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 4px 16px 10px;
  }
  .triple {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0 6px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .pad {
    padding: 4px 16px;
    margin: 0;
  }
  .stale {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    color: var(--warning, var(--text-2));
    font-size: var(--fs-sm);
  }
  .tree,
  .tree ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .tree {
    padding: 4px 12px 8px;
  }
  .tree ul {
    padding-left: 12px;
    border-left: 1px solid var(--border);
    margin-left: 9px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 8px;
    padding: 3px 0;
    font-size: var(--fs-sm);
  }
  .how.root {
    font-weight: 600;
    color: var(--text);
  }
  .node.asserted > .row .how,
  .node.axiom > .row .how {
    font-style: italic;
  }
  .fold,
  .fold-pad {
    width: 18px;
    flex: 0 0 18px;
  }
  .fold {
    border: 0;
    background: transparent;
    padding: 0;
    cursor: pointer;
    color: var(--text-2);
  }
  .caret {
    display: inline-block;
    transition: transform 0.12s;
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 16px 12px;
    border-top: 1px solid var(--border);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  @media (prefers-reduced-motion: reduce) {
    .caret {
      transition: none;
    }
  }
  /* a phone: a full-height sheet, the tree indented 12 px per level */
  @media (max-width: 600px) {
    .why {
      position: fixed;
      inset: 0;
      z-index: 40;
      border: 0;
    }
  }
</style>
