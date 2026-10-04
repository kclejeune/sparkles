<script lang="ts">
  // Changes as signed N-Quads lines: the diff view of the History panel, the commit
  // graph and the merge page.
  import type * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import { diffLine } from '$lib/history';

  let {
    quads,
    total = quads.length,
    label = 'Changes',
    maxHeight = 280,
  }: {
    quads: api.DiffQuad[];
    /** Every change, of which `quads` lists the first. */
    total?: number;
    /** The accessible name of the list. */
    label?: string;
    maxHeight?: number;
  } = $props();
</script>

{#if quads.length}
  <pre
    class="diff-lines"
    aria-label={label}
    style:max-height="{maxHeight}px">{#each quads as q, i (i)}<span
        class:ins={q.op === '+'}
        class:del={q.op === '-'}>{diffLine(q)}{'\n'}</span
      >{/each}</pre>
{/if}
{#if total > quads.length}
  <p class="faint more">
    Showing the first {fmtInt(quads.length)} of {fmtInt(total)} changes.
  </p>
{/if}

<style>
  .diff-lines {
    margin: 0;
    overflow: auto;
    font-size: 11.5px;
    line-height: 1.45;
    white-space: pre;
  }
  .ins {
    color: var(--ok);
  }
  .del {
    color: var(--danger);
  }
  .more {
    margin: 0;
  }
</style>
