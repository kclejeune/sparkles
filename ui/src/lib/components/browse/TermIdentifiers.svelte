<script lang="ts">
  // The identifiers of an entity in other databases, for the Metadata card: the database in a
  // short column and the ids after it as links, a database's ids on one line. The first rows
  // show and the rest open on request. Each says the best name there is, and keeps the exact
  // value as its title and its link target.
  import { hiddenRows, xrefName, xrefsByDatabase, type Sections } from '$lib/browse';
  import type { PrefixMap } from '$lib/rdf';

  let {
    xrefs,
    label,
    prefixes,
    limit = 8,
  }: {
    xrefs: Sections['xrefs'];
    /** The label this dataset gives an IRI, which the page has already read. */
    label: (iri: string) => string | null | undefined;
    prefixes: PrefixMap;
    limit?: number;
  } = $props();

  const rows = $derived(xrefsByDatabase(xrefs));
  let all = $state(false);
  const hidden = $derived(all ? 0 : hiddenRows(rows.length, limit));
</script>

{#if rows.length}
  <section class="ids" aria-labelledby="identifiers-h">
    <h3 id="identifiers-h">Identifiers</h3>
    <dl>
      {#each hidden ? rows.slice(0, limit) : rows as r (r.database)}
        <dt>{r.database || 'Other'}</dt>
        <dd>
          {#each r.ids as x, i (x.value)}{#if i > 0},{' '}{/if}{#if x.href}<a
                href={x.href}
                title={x.value}
                rel="noopener noreferrer"
                target="_blank">{xrefName(x, label, prefixes)}</a
              >{:else}<span title={x.value}>{xrefName(x, label, prefixes)}</span>{/if}{/each}
        </dd>
      {/each}
    </dl>
    {#if hidden}
      <button class="more-btn" type="button" onclick={() => (all = true)}>and {hidden} more</button>
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
  .more-btn {
    margin-top: var(--sp-2);
  }
</style>
