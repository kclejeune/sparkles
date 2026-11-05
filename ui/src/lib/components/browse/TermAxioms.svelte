<script lang="ts">
  import { displayName } from '$lib/browse';
  import { iriForm } from '$lib/iri-form.svelte';
  // The Axioms card of an entity's page (ROADMAP item 633): its asserted frame in Manchester
  // syntax, one clause per keyword (SubClassOf, EquivalentTo, Domain, Characteristics and the
  // rest) with every name a link, axioms of a form the syntax has no clause for in
  // functional-style syntax, and a copy of the whole frame as Manchester text. A class also
  // shows what the dataset's kept classification infers of it and no axiom asserts. A class
  // with neither asserted nor inferred axioms shows no card.
  import DatatypeChip from './DatatypeChip.svelte';
  import {
    classifiedHere,
    classifyAbout,
    frameText,
    inferredOf,
    manchesterFrame,
    namesQuery,
    parseDefinitions,
    type AxiomGroup,
    type Inferred,
    type Linker,
  } from '$lib/browse';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import type { EntityKind, ManchesterToken } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from '$components/Icon.svelte';
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
    historic = false,
    editor,
  }: {
    kind: EntityKind;
    iri: string;
    groups: AxiomGroup[];
    linker: Linker;
    prefixes: PrefixMap;
    target: string;
    graph: string | null;
    /** The commit the page shows, so that an edit asks the classification again. */
    commit?: number | null;
    /** The page shows a past state, which the kept classification does not describe. */
    historic?: boolean;
    /** Edit mode's frame, which takes the card's body in place of the read-only one. */
    editor?: Snippet;
  } = $props();

  const frame = $derived(manchesterFrame(kind, groups, linker.name, prefixes));
  const isClass = $derived(kind === 'Class');

  type Classified = NonNullable<Awaited<ReturnType<typeof classifyAbout>>>;
  /** What the classification answered, which the asserted groups are taken out of below. */
  let answer = $state<Classified | null>(null);
  /** The inferences that no axiom of the frame already asserts, so an edit updates them too. */
  const inferred = $derived<Inferred | null>(
    answer ? inferredOf(iri, groups, answer.class, answer.complete, answer.reused) : null,
  );
  let names = $state(new Map<string, string>());
  const langs = $derived(iriForm.languages);
  const nameOf = (i: string) =>
    names.has(i) ? displayName(i, { [i]: { label: names.get(i)! } }, prefixes) : linker.name(i);
  let missing = $state(false);
  let loading = $state(false);
  let failure = $state<string | null>(null);
  let seq = 0;

  async function load() {
    const my = ++seq;
    loading = true;
    failure = null;
    try {
      const r = await classifyAbout(target, graph, iri);
      if (my !== seq) return;
      missing = !r;
      // the inferred classes are not among the names the page read, so their labels are asked
      const ask = r ? [...r.class.parentClasses, ...r.class.classes].filter((c) => c !== iri) : [];
      if (ask.length)
        try {
          const rows = await api.select(target, namesQuery(ask, graph));
          if (my !== seq) return;
          names = parseDefinitions(rows as never, langs);
        } catch {
          // the names stay as the page writes unknown IRIs
        }
      answer = r;
    } catch (e) {
      if (my === seq) failure = api.errorMessage(e);
    } finally {
      if (my === seq) loading = false;
    }
  }

  // a classification that ran in this tab is asked again on each class page, which the
  // dataset answers from its kept taxonomy
  $effect(() => {
    void iri;
    void target;
    void graph;
    // an edit commits a new state, which the inferences are asked of again
    void commit;
    answer = null;
    missing = false;
    failure = null;
    if (isClass && !historic && classifiedHere(target)) void load();
  });

  const hasInferred = $derived(
    !!inferred && (inferred.superClasses.length > 0 || inferred.equivalents.length > 0),
  );
  const asserted = $derived(frame.sections.length > 0 || frame.other.length > 0);
  const shown = $derived(asserted || hasInferred || !!editor);

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

{#snippet name(i: string)}{#if linker.href(i)}<a href={linker.href(i)} title={i}>{nameOf(i)}</a
    >{:else}<span class="builtin" title={i}>{nameOf(i)}</span>{/if}{/snippet}

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
      {#if isClass && !editor}
        <div class="inferred" class:alone={!asserted} aria-live="polite">
          {#if hasInferred && inferred}
            <dl class="frame" aria-label="Inferred">
              {#if inferred.superClasses.length}
                <div class="clause" data-section="inferred-sub">
                  <dt>SubClassOf:</dt>
                  <dd>
                    <ul>
                      {#each inferred.superClasses as c (c)}
                        <li>{@render name(c)} <span class="badge">Inferred</span></li>
                      {/each}
                    </ul>
                  </dd>
                </div>
              {/if}
              {#if inferred.equivalents.length}
                <div class="clause" data-section="inferred-equivalent">
                  <dt>EquivalentTo:</dt>
                  <dd>
                    <ul>
                      {#each inferred.equivalents as c (c)}
                        <li>{@render name(c)} <span class="badge">Inferred</span></li>
                      {/each}
                    </ul>
                  </dd>
                </div>
              {/if}
            </dl>
            {#if !inferred.complete}
              <p class="note">
                The classification left out axioms outside OWL 2 EL, so it can miss superclasses.
              </p>
            {/if}
          {:else if historic}
            <!-- the kept classification describes the current state only -->
          {:else if loading}
            <p class="note"><span class="spinner"></span>Classifying the ontology…</p>
          {:else if failure}
            <p class="note" role="alert">Could not classify: {failure}</p>
          {:else if missing}
            <p class="note">The classification has no node for this class.</p>
          {:else if inferred}
            <p class="note">The classification infers nothing more than these axioms state.</p>
          {:else}
            <p class="note">
              Inferred superclasses show once the ontology is classified.
              <button type="button" class="btn sm ghost" onclick={() => void load()}
                >Classify</button
              >
            </p>
          {/if}
        </div>
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
  .inferred {
    margin-top: var(--sp-4);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .inferred.alone,
  .inferred:empty {
    margin-top: 0;
    padding-top: 0;
    border-top: 0;
  }
  .inferred .badge {
    margin-left: var(--sp-2);
    vertical-align: 1px;
  }
  .note {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .frame + .note {
    margin-top: var(--sp-3);
  }
</style>
