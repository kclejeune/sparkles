<script lang="ts">
  // The Axioms card of an entity's page (ROADMAP item 633): its asserted frame in Manchester
  // syntax, one clause per keyword (SubClassOf, EquivalentTo, Domain, Characteristics and the
  // rest) with every name a link, axioms of a form the syntax has no clause for in
  // functional-style syntax, and a copy of the whole frame as Manchester text. A class also
  // shows what the dataset's kept classification infers of it and no axiom asserts. A class
  // always keeps its classification notices visible, including an empty frame.
  import DatatypeChip from './DatatypeChip.svelte';
  import { frameText, manchesterFrame, type AxiomGroup, type Linker } from '$lib/browse';
  import { toasts } from '$lib/app.svelte';
  import type { EntityKind, ManchesterToken } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from '$components/Icon.svelte';
  import ClassificationFrame from '$components/ClassificationFrame.svelte';
  import MaterializedFrame from '$components/MaterializedFrame.svelte';
  import type { InferredPins } from '$lib/inferred-read';
  import type { Snippet } from 'svelte';

  let {
    kind,
    iri,
    groups,
    linker,
    prefixes,
    target,
    graph,
    commit = null,
    imports = true,
    pins,
    editor,
  }: {
    kind: EntityKind;
    iri: string;
    groups: AxiomGroup[];
    linker: Linker;
    prefixes: PrefixMap;
    target: string;
    graph: string | null;
    /** The exact displayed commit for read-only classification pages. */
    commit?: number | null;
    imports?: boolean;
    pins?: InferredPins;
    /** Edit mode's frame, which takes the card's body in place of the read-only one. */
    editor?: Snippet;
  } = $props();

  const frame = $derived(manchesterFrame(kind, groups, linker.name, prefixes));
  const isClass = $derived(kind === 'Class');
  const hasMaterializedFrame = $derived(
    ['ObjectProperty', 'DataProperty', 'NamedIndividual'].includes(kind),
  );

  const asserted = $derived(frame.sections.length > 0 || frame.other.length > 0);
  const shown = $derived(asserted || isClass || hasMaterializedFrame || !!editor);

  async function copy() {
    try {
      await navigator.clipboard.writeText(frameText(frame, iri, linker.name));
      toasts.push('success', 'Copied the frame', undefined, 1400);
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }
</script>

{#snippet tokens(
  ts: ManchesterToken[],
)}{#each ts as t, i (i)}{#if t.role === 'name' && t.iri}{@const href = linker.href(
        t.iri,
      )}{#if href}<a {href} title={t.iri}>{linker.name(t.iri)}</a>{:else}<span
          class="builtin"
          title={t.iri}>{linker.name(t.iri)}</span
        >{/if}{:else if t.role === 'kw'}<span class="kw">{t.text}</span
      >{:else if t.role === 'lit'}<span class="lit" class:lexical={!!t.datatype}
        >{t.value ?? t.text}</span
      ><DatatypeChip iri={t.datatype ?? null} {prefixes} />{:else}{t.text}{/if}{/each}{/snippet}

{#if shown}
  <section class="b-ax" aria-labelledby="axioms-h">
    <div class="term-head-row">
      <h2 id="axioms-h">Axioms</h2>
      <span class="spacer"></span>
      {#if !editor && frame.sections.length}
        <button
          type="button"
          class="btn sm ghost"
          title="Copy the frame in Manchester syntax"
          onclick={copy}><Icon name="copy" size={14} />Copy</button
        >
      {/if}
    </div>
    <div class="ax-body">
      {#if editor}
        {@render editor()}
      {:else}
        {#if frame.sections.length}
          <dl class="frame" aria-label="Asserted">
            {#each frame.sections as s (s.word)}
              <div class="clause" data-section={s.id}>
                <dt>{s.word}:</dt>
                <dd>
                  <ul>
                    {#each s.items as it, i (i)}
                      <li>{@render tokens(it)}</li>
                    {/each}
                  </ul>
                </dd>
              </div>
            {/each}
          </dl>
        {/if}
        {#if frame.other.length}
          <div class="other" data-section="other">
            <h3>Other axioms</h3>
            <ul>
              {#each frame.other as a, i (i)}<li><code>{a.ofn}</code></li>{/each}
            </ul>
          </div>
        {/if}
      {/if}
      {#if isClass}
        <ClassificationFrame
          {target}
          {graph}
          {iri}
          {commit}
          {imports}
          {pins}
          items={groups.flatMap((g) => g.items.map((it) => it.axiom))}
          href={linker.href}
        />
      {:else if hasMaterializedFrame}
        <MaterializedFrame
          {target}
          {graph}
          {imports}
          {iri}
          {commit}
          {pins}
          items={groups.flatMap((g) => g.items.map((it) => it.axiom))}
        />
      {/if}
    </div>
  </section>
{/if}

<style>
  .b-ax {
    min-width: 0;
  }
  .ax-body {
    container-type: inline-size;
  }
  .frame {
    display: grid;
    gap: var(--sp-3);
    margin: 0;
  }
  /* the keyword beside its items, and above them in a narrow column */
  .clause {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 2px;
    min-width: 0;
  }
  @container (min-width: 520px) {
    .clause {
      grid-template-columns: 9.5rem minmax(0, 1fr);
      gap: var(--sp-3);
    }
  }
  dt {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    line-height: 1.75;
    color: var(--kw);
    font-weight: 600;
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  li {
    font-size: var(--fs-md);
    line-height: 1.5;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .frame a {
    color: var(--iri);
  }
  .kw {
    color: var(--kw);
    font-weight: 600;
    font-style: italic;
  }
  .lit {
    color: var(--lit, var(--text-2));
  }
  .builtin {
    color: var(--text-2);
  }
  .other {
    margin-top: var(--sp-4);
  }
  .other h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    margin: 0 0 var(--sp-2);
  }
  .other code {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
</style>
