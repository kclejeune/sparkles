<script lang="ts">
  // The identifiers of an entity in other databases, for a card of the page's side column: the
  // database in a short column and the ids after it as links, a database's ids on one line. The
  // first rows show and the rest open on request.
  import { hiddenRows, xrefsByDatabase, type Sections } from '$lib/browse';

  let { xrefs, limit = 8 }: { xrefs: Sections['xrefs']; limit?: number } = $props();

  const rows = $derived(xrefsByDatabase(xrefs));
  let all = $state(false);
  const hidden = $derived(all ? 0 : hiddenRows(rows.length, limit));
</script>

{#if rows.length}
  <section class="panel ids" aria-labelledby="identifiers-h">
    <div class="panel-head"><h2 id="identifiers-h">Identifiers</h2></div>
    <div class="panel-body">
      <dl>
        {#each hidden ? rows.slice(0, limit) : rows as r (r.database)}
          <dt>{r.database || 'Other'}</dt>
          <dd>
            {#each r.ids as x, i (x.value)}{#if i > 0},{' '}{/if}{#if x.href}<a
                  href={x.href}
                  rel="noopener noreferrer"
                  target="_blank">{x.id}</a
                >{:else}{x.id}{/if}{/each}
          </dd>
        {/each}
      </dl>
      {#if hidden}
        <button class="more-btn" type="button" onclick={() => (all = true)}
          >and {hidden} more</button
        >
      {/if}
    </div>
  </section>
{/if}

<style>
  /* the columns of the Metadata card above it, so the two cards read as one list */
  dl {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    gap: 8px 12px;
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
  .panel-body {
    padding: var(--sp-3) 14px var(--sp-4);
  }
  .more-btn {
    margin-top: var(--sp-2);
  }
</style>
