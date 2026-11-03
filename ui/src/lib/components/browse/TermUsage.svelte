<script lang="ts">
  // What links here: the axioms of other entities that mention this one, grouped by the
  // restriction that holds it ("part of some mitochondrion"), then the other statements that
  // point at it, by property with their counts, and for a property the subjects that use
  // it. Each list shows its first subjects, and More reads further pages.
  import { INCOMING_WORDS, type Incoming, type Linker, type MentionGroup } from '$lib/browse';
  import type { AxiomJson } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import AxiomProse from './AxiomProse.svelte';

  let {
    iri,
    groups,
    total,
    truncated = false,
    incoming,
    usage = null,
    linker,
    prefixes,
    more,
    skip = [],
    title = 'What links here',
  }: {
    iri: string;
    groups: MentionGroup[];
    /** The number of axioms that mention the entity. */
    total: number;
    truncated?: boolean;
    incoming: Incoming[];
    usage?: Incoming | null;
    linker: Linker;
    prefixes: PrefixMap;
    /** Read a further page of the subjects of a property, from an offset. */
    more?: (property: string, offset: number) => Promise<string[]>;
    /** Properties another section already shows, such as the hierarchy's. */
    skip?: string[];
    title?: string;
  } = $props();

  const lists = $derived(incoming.filter((i) => !skip.includes(i.property)));
  /** Subjects read past the first page, by property. */
  let extra = $state<Record<string, { items: string[]; done: boolean; busy: boolean }>>({});
  $effect(() => {
    void iri;
    extra = {};
  });
  const count = $derived(total + lists.reduce((n, i) => n + i.count, 0) + (usage?.count ?? 0));

  async function readMore(i: Incoming) {
    if (!more) return;
    const was = extra[i.property] ?? { items: [], done: false, busy: false };
    extra[i.property] = { ...was, busy: true };
    try {
      const got = await more(i.property, i.subjects.length + was.items.length);
      const seen = new Set([...i.subjects, ...was.items]);
      const items = [...was.items, ...got.filter((s) => !seen.has(s))];
      extra[i.property] = {
        items,
        done: i.subjects.length + items.length >= i.count || !got.length,
        busy: false,
      };
    } catch {
      extra[i.property] = { ...was, busy: false };
    }
  }
  const heading = (i: Incoming) =>
    INCOMING_WORDS[i.property] ?? `${linker.name(i.property)} ${linker.name(iri)}`;
</script>

{#snippet subjectList(subjects: string[])}
  <ul class="term-links">
    {#each subjects as s (s)}
      {@const h = linker.href(s)}
      <li>
        {#if h}<a href={h}>{linker.name(s)}</a>{:else}<span>{linker.name(s)}</span>{/if}
        {#if linker.short(s) && linker.short(s) !== linker.name(s)}<span class="sfx"
            >{linker.short(s)}</span
          >{/if}
      </li>
    {/each}
  </ul>
{/snippet}

{#snippet incomingList(i: Incoming)}
  {@const ex = extra[i.property]}
  <div class="grp" data-property={i.property}>
    <h3>{heading(i)} <span class="count-badge">{i.count.toLocaleString('en-US')}</span></h3>
    {@render subjectList([...i.subjects, ...(ex?.items ?? [])])}
    {#if more && i.count > i.subjects.length + (ex?.items.length ?? 0) && !ex?.done}
      <button class="more-btn" type="button" disabled={ex?.busy} onclick={() => readMore(i)}
        >{ex?.busy
          ? 'Reading…'
          : `Show more (${(i.count - i.subjects.length - (ex?.items.length ?? 0)).toLocaleString('en-US')} left)`}</button
      >
    {/if}
  </div>
{/snippet}

{#if count}
  <section class="term-sec" aria-labelledby="usage-h">
    <h2 id="usage-h">{title} <span class="count-badge">{count.toLocaleString('en-US')}</span></h2>
    <div class="groups">
      {#each groups as g, gi (gi)}
        {#if g.property}
          <div class="grp">
            <h3>
              {linker.name(g.property)} <span class="kw">{g.word}</span>
              {linker.name(iri)}
              <span class="count-badge">{g.subjects.length}</span>
            </h3>
            {@render subjectList(g.subjects)}
          </div>
        {:else}
          <div class="grp">
            <h3>Mentioned in axioms <span class="count-badge">{g.axioms.length}</span></h3>
            <ul class="term-links">
              {#each g.axioms as a, i (i)}
                {@const x = a.axiom as AxiomJson}
                {@const cs = Array.isArray(x.classes) ? (x.classes as unknown[]) : []}
                <li>
                  {#if x.type === 'SubClassOf' && typeof x.subClass === 'string'}
                    <span
                      ><AxiomProse value={x.subClass} {linker} {prefixes} />
                      <span class="kw">subclass of</span>
                      <AxiomProse value={x.superClass} {linker} {prefixes} /></span
                    >
                  {:else if x.type === 'EquivalentClasses' && cs.length === 2 && typeof cs[0] === 'string'}
                    <span
                      ><AxiomProse value={cs[0]} {linker} {prefixes} />
                      <span class="kw">equivalent to</span>
                      <AxiomProse value={cs[1]} {linker} {prefixes} /></span
                    >
                  {:else}
                    <code class="ofn">{a.ofn}</code>
                  {/if}
                </li>
              {/each}
            </ul>
          </div>
        {/if}
      {/each}
      {#if truncated}
        <p class="faint note">
          The page shows the first {groups.reduce(
            (n, g) => n + g.subjects.length + g.axioms.length,
            0,
          )} of {total.toLocaleString('en-US')} axioms.
        </p>
      {/if}
      {#each lists as i (i.property)}{@render incomingList(i)}{/each}
      {#if usage}
        <div class="grp">
          <h3>Used by <span class="count-badge">{usage.count.toLocaleString('en-US')}</span></h3>
          {@render subjectList(usage.subjects)}
          {#if usage.more}<p class="faint note">
              and {(usage.count - usage.subjects.length).toLocaleString('en-US')} more
            </p>{/if}
        </div>
      {/if}
    </div>
  </section>
{/if}

<style>
  .groups {
    display: grid;
    gap: var(--sp-4);
  }
  .grp :global(h3) {
    margin-top: 0;
  }
  .kw {
    color: var(--kw);
    font-weight: 600;
    font-style: italic;
  }
  .ofn {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  .note {
    font-size: var(--fs-sm);
  }
  .more-btn {
    margin-top: 4px;
  }
</style>
