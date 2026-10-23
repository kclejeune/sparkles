<script lang="ts">
  // The panel of one element of the Pattern pane (spec U08 §2.5): what the element is,
  // where it is in the text, and the edits Phase 1 makes on the canvas (deleting a triple,
  // optional on or off). A locked block shows its text, which only the editor edits.
  import type { Drawing } from '$lib/pattern';
  import { describe } from '$lib/pattern';
  import type { Focusable } from './PatternCanvas.svelte';
  import Icon from './Icon.svelte';

  let {
    el,
    drawing,
    text,
    readonly = false,
    sheet = false,
    ondelete,
    onoptional,
    onclose,
  }: {
    el: Focusable;
    drawing: Drawing;
    /** the query text, for line numbers */
    text: string;
    /** no edits: a result's pattern, or text that does not parse */
    readonly?: boolean;
    /** a bottom sheet on a phone */
    sheet?: boolean;
    /** the edit, by the id of the triple's edge */
    ondelete?: (edge: string) => void;
    onoptional?: (edge: string) => void;
    onclose: () => void;
  } = $props();

  const lineOf = (offset: number) => text.slice(0, offset).split('\n').length;
  const triple = $derived('edge' in el ? el.edge.triple : undefined);
  const edgeId = $derived('edge' in el ? el.edge.id : '');
  const locked = $derived('node' in el ? el.node.locked : undefined);
</script>

<div class="panel" class:sheet role="dialog" aria-label="Pattern element" aria-modal="false">
  <div class="head">
    <strong>{describe(el, drawing)}</strong>
    <button class="btn ghost icon sm" aria-label="Close" title="Close (Escape)" onclick={onclose}
      ><Icon name="x" size={12} /></button
    >
  </div>
  {#if triple}
    <p class="faint small">
      Line {lineOf(triple.triple.range.from)}:
      <span class="mono">{triple.triple.s.text} {triple.triple.p.text} {triple.triple.o.text}</span>
    </p>
    <div class="actions">
      <button
        class="btn sm"
        disabled={readonly}
        onclick={() => onoptional?.(edgeId)}
        aria-pressed={triple.path.length > 1}
      >
        {triple.path.length > 1 ? 'Required' : 'Optional'}
      </button>
      <button class="btn sm danger" disabled={readonly} onclick={() => ondelete?.(edgeId)}>
        <Icon name="trash" size={13} /> Delete
      </button>
    </div>
  {:else if locked}
    <p class="faint small">
      Line {lineOf(locked.range.from)}. Locked parts are edited in the editor, and canvas edits
      leave them as they are.
    </p>
    <pre class="mono locked-text">{locked.text}</pre>
  {:else if 'node' in el}
    <p class="faint small">{el.node.title}</p>
  {/if}
</div>

<style>
  .panel {
    position: absolute;
    right: 10px;
    bottom: 10px;
    width: min(340px, calc(100% - 20px));
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-pop);
    padding: 10px 12px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .panel.sheet {
    left: 0;
    right: 0;
    bottom: 0;
    width: auto;
    border-radius: 12px 12px 0 0;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    justify-content: space-between;
    font-size: var(--fs-sm);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .sheet .actions :global(.btn) {
    min-height: 44px;
    flex: 1;
  }
  .small {
    font-size: var(--fs-xs, 12px);
    margin: 0;
  }
  .locked-text {
    margin: 0;
    max-height: 160px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-size: 11px;
    background: var(--surface-2);
    padding: 6px 8px;
    border-radius: 4px;
  }
</style>
