<script lang="ts">
  // The infobox of an entity: its kind, status, identifiers, ontology, hierarchy counts and
  // the other facts a reader looks up first, with the downloads under them. The page builds
  // the rows, so another page can show the same box with rows of its own.
  import type { Snippet } from 'svelte';
  import { isLiteral, valueText, type InfoRow, type Linker } from '$lib/browse';
  import { literalDatatype } from '$lib/ontology';
  import { WELL_KNOWN, type PrefixMap } from '$lib/rdf';
  import DatatypeChip from './DatatypeChip.svelte';

  let {
    rows,
    linker,
    footer,
    heading,
    limit = 6,
    prefixes = WELL_KNOWN,
  }: {
    rows: InfoRow[];
    linker: Linker;
    footer?: Snippet;
    limit?: number;
    /** A panel heading, such as "Metadata" on a term page's card. */
    heading?: string;
    prefixes?: PrefixMap;
  } = $props();
  let open = $state<Record<string, boolean>>({});
</script>

<section class="panel info" aria-label={heading ?? 'Facts'}>
  {#if heading}<div class="panel-head"><h2>{heading}</h2></div>{/if}
  <div class="panel-body">
    <dl>
      {#each rows as r (r.label)}
        {@const vs = r.values ?? []}
        {@const all = open[r.label] || vs.length <= limit + 1}
        <dt>{r.label}</dt>
        <dd class:mono={r.mono}>
          {#if r.badge}<span class="badge {r.badge}">{r.text}</span>{:else if r.text}{r.text}{/if}
          {#each all ? vs : vs.slice(0, limit) as v, i (i)}{#if i > 0 || r.text},{' '}{/if}{#if !isLiteral(v) && linker.href(v)}<a
                href={linker.href(v)}>{linker.name(v)}</a
              >{:else if !isLiteral(v)}{linker.name(v)}{:else}{valueText(v)}<DatatypeChip
                iri={literalDatatype(v)}
                {prefixes}
              />{/if}{/each}
          {#if !all}
            <button class="more-btn" type="button" onclick={() => (open[r.label] = true)}
              >and {vs.length - limit} more</button
            >
          {/if}
          {#if r.note}<div class="note">{r.note}</div>{/if}
        </dd>
      {/each}
    </dl>
  </div>
  {#if footer}
    <div class="panel-body foot">{@render footer()}</div>
  {/if}
</section>

<style>
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
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  dd a {
    color: var(--iri);
  }
  .note {
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .panel-body {
    padding: var(--sp-3) 14px var(--sp-4);
  }
  .foot {
    border-top: 1px solid var(--border);
  }
</style>
