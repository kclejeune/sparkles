<script lang="ts">
  // The identifiers of an entity in other databases, for a panel of the facts: the database
  // in a short column and the ids after it as links, a database's ids on one line. The first
  // rows show and the rest open on request.
  import type { Sections } from '$lib/browse';

  let { xrefs, limit = 8 }: { xrefs: Sections['xrefs']; limit?: number } = $props();

  const rows = $derived.by(() => {
    const by = new Map<string, Sections['xrefs']>();
    for (const x of xrefs) by.set(x.database, [...(by.get(x.database) ?? []), x]);
    return [...by.entries()].map(([database, ids]) => ({ database, ids }));
  });
  let all = $state(false);
</script>

{#if rows.length}
  <section class="ids" aria-labelledby="identifiers-h">
    <h3 id="identifiers-h">Identifiers</h3>
    <dl>
      {#each all || rows.length <= limit + 1 ? rows : rows.slice(0, limit) as r (r.database)}
        <dt>{r.database}</dt>
        <dd>
          {#each r.ids as x, i (x.value)}{#if i > 0},{' '}{/if}{#if x.href}<a
                href={x.href}
                rel="noopener noreferrer"
                target="_blank">{x.id}</a
              >{:else}{x.id}{/if}{/each}
        </dd>
      {/each}
    </dl>
    {#if !all && rows.length > limit + 1}
      <button class="more-btn" type="button" onclick={() => (all = true)}
        >and {rows.length - limit} more</button
      >
    {/if}
  </section>
{/if}

<style>
  .ids {
    padding-bottom: var(--sp-3);
    margin-bottom: var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0 0 var(--sp-2);
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  dl {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    gap: 6px 12px;
    margin: 0;
  }
  dt {
    color: var(--text-3);
    font-size: var(--fs-sm);
    padding-top: 1px;
    overflow-wrap: anywhere;
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  dd a {
    color: var(--iri);
  }
</style>
