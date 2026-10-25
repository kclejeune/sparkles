<script lang="ts">
  import type { EntailmentDiff } from '$lib/api';
  import { fmtInt } from '$lib/format';
  import type { PrefixMap } from '$lib/rdf';
  import TermView from './TermView.svelte';
  let {
    diff,
    prefixes,
    onopen,
  }: { diff: EntailmentDiff; prefixes: PrefixMap; onopen?: (iri: string) => void } = $props();
</script>

<div class="entailments">
  <p>
    <strong>{fmtInt(diff.added)}</strong> inferred triple{diff.added === 1 ? '' : 's'} added,
    <strong>{fmtInt(diff.removed)}</strong> removed.
  </p>
  {#each [{ label: 'Added entailments', rows: diff.sample.added, sign: '+' }, { label: 'Removed entailments', rows: diff.sample.removed, sign: '−' }] as group (group.sign)}
    <section aria-label={group.label}>
      <h4>{group.label}</h4>
      {#if group.rows.length}
        <ul>
          {#each group.rows as triple, i (i)}<li class="mono">
              <span aria-hidden="true">{group.sign}</span>
              {#each triple as term, j (j)}<TermView {term} {prefixes} {onopen} />{' '}{/each}
            </li>{/each}
        </ul>
      {:else}<p class="faint">None in this sample.</p>{/if}
    </section>
  {/each}
  {#if diff.sampleTruncated}<p class="faint">
      The triple samples are truncated. Counts cover every change.
    </p>{/if}
  {#if Object.keys(diff.byPredicate).length}
    <details>
      <summary>Changes by predicate</summary>
      <table class="data">
        <thead><tr><th>Predicate</th><th>Added</th><th>Removed</th></tr></thead><tbody
          >{#each Object.entries(diff.byPredicate) as [predicate, counts] (predicate)}<tr
              ><td><TermView term={{ type: 'uri', value: predicate }} {prefixes} {onopen} /></td><td
                >{fmtInt(counts.added)}</td
              ><td>{fmtInt(counts.removed)}</td></tr
            >{/each}</tbody
        >
      </table>
    </details>
  {/if}
  {#if Object.keys(diff.byClass).length}
    <details>
      <summary>Changes by class</summary>
      <table class="data">
        <thead><tr><th>Class</th><th>Added</th><th>Removed</th></tr></thead><tbody
          >{#each Object.entries(diff.byClass) as [classIri, counts] (classIri)}<tr
              ><td><TermView term={{ type: 'uri', value: classIri }} {prefixes} {onopen} /></td><td
                >{fmtInt(counts.added)}</td
              ><td>{fmtInt(counts.removed)}</td></tr
            >{/each}</tbody
        >
      </table>
    </details>
  {/if}
  {#if diff.newFindings.length}<section>
      <h4>New diagnostic findings</h4>
      <ul>
        {#each diff.newFindings as finding, i (i)}<li>
            <span class="badge danger">{finding.check}</span>
            <TermView term={finding.focus} {prefixes} {onopen} />
            {finding.message}
          </li>{/each}
      </ul>
    </section>{/if}
  {#if !diff.findingsComplete}<p class="faint">The new-finding check is incomplete.</p>{/if}
  {#each diff.warnings as warning (warning)}<p class="faint">{warning}</p>{/each}
</div>

<style>
  .entailments {
    display: grid;
    gap: 12px;
  }
  h4 {
    margin-bottom: 6px;
  }
  ul {
    list-style: none;
    padding: 0;
    display: grid;
    gap: 5px;
  }
  li {
    overflow-wrap: anywhere;
  }
  details > summary {
    cursor: pointer;
  }
</style>
