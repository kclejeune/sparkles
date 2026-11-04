<script lang="ts">
  // A diff rendered as a change set (ROADMAP item 633): one block a subject, named by its
  // label, with each added or removed property and value on a line of its own. The raw
  // N-Quads stay one link away, in the full diff the caller links to.
  import type * as api from '$lib/api';
  import { changeGroups, type CsTerm } from '$lib/changeset';
  import { fmtInt } from '$lib/format';
  import { shorten, type PrefixMap } from '$lib/rdf';

  let {
    quads,
    total = quads.length,
    name,
    prefixes = {},
    onopen,
    label = 'Changes',
  }: {
    quads: api.DiffQuad[];
    /** Every change, of which `quads` lists the first. */
    total?: number;
    /** An IRI's label, or null to show it by its short form. */
    name: (iri: string) => string | null;
    prefixes?: PrefixMap;
    /** Opens a subject or a value that is an IRI; without it they are plain text. */
    onopen?: (iri: string) => void;
    /** The accessible name of the list. */
    label?: string;
  } = $props();

  const groups = $derived(changeGroups(quads));
  const short = (iri: string) => shorten(iri, prefixes) ?? iri;
  const named = (iri: string) => name(iri) ?? short(iri);
</script>

{#snippet term(t: CsTerm)}
  {#if t.type === 'iri'}
    {#if onopen}<button class="link" onclick={() => onopen(t.value)} title={t.value}
        >{named(t.value)}</button
      >{:else}<span title={t.value}>{named(t.value)}</span>{/if}
  {:else if t.type === 'bnode'}
    <span class="mono faint">_:{t.value}</span>
  {:else}
    <span class="lit">“{t.value}”</span>{#if t.lang}<span class="faint small">@{t.lang}</span
      >{:else if t.datatype}<span class="faint small mono">^^{short(t.datatype)}</span>{/if}
  {/if}
{/snippet}

{#if groups.length}
  <div class="changeset" role="list" aria-label={label}>
    {#each groups as g (g.subject.value)}
      <section class="cs-group" role="listitem">
        <header>
          <span class="subject">{@render term(g.subject)}</span>
          {#if g.subject.type === 'iri' && name(g.subject.value)}
            <span class="mono faint small">{short(g.subject.value)}</span>
          {/if}
          <span class="spacer"></span>
          {#if g.added}<span class="ins small">+{fmtInt(g.added)}</span>{/if}
          {#if g.removed}<span class="del small">−{fmtInt(g.removed)}</span>{/if}
        </header>
        <ul>
          {#each g.changes as c, i (i)}
            <li class:ins={c.op === '+'} class:del={c.op === '-'}>
              <span class="sign" aria-label={c.op === '+' ? 'added' : 'removed'}
                >{c.op === '+' ? '+' : '−'}</span
              >
              <span class="prop" title={c.predicate}>{named(c.predicate)}</span>
              <span class="val">{@render term(c.object)}</span>
            </li>
          {/each}
        </ul>
      </section>
    {/each}
  </div>
{/if}
{#if total > quads.length}
  <p class="faint small more">
    Showing the first {fmtInt(quads.length)} of {fmtInt(total)} changes.
  </p>
{/if}

<style>
  .changeset {
    display: grid;
    gap: var(--sp-2);
  }
  .cs-group {
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-2);
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
  }
  .subject {
    font-weight: 600;
  }
  ul {
    margin: 0;
    padding: 4px 0;
    list-style: none;
  }
  li {
    display: grid;
    grid-template-columns: 1.25rem minmax(7rem, 12rem) minmax(0, 1fr);
    gap: var(--sp-2);
    align-items: baseline;
    padding: 2px 10px;
  }
  li.ins {
    background: color-mix(in srgb, var(--ok) 7%, transparent);
  }
  li.del {
    background: color-mix(in srgb, var(--danger) 7%, transparent);
  }
  .sign {
    font-family: var(--font-mono);
    font-weight: 600;
    text-align: center;
  }
  li.ins .sign,
  .ins.small {
    color: var(--ok);
  }
  li.del .sign,
  .del.small {
    color: var(--danger);
  }
  li.del .val {
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--danger) 50%, transparent);
  }
  .prop {
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .val {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .more {
    margin: var(--sp-2) 0 0;
  }
  @media (max-width: 760px) {
    li {
      grid-template-columns: 1.25rem minmax(0, 1fr);
    }
    .val {
      grid-column: 2;
    }
  }
</style>
