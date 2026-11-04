<script lang="ts" generics="T extends string">
  // A page's one tab row (ROADMAP item 633): tabs with arrow-key movement, and an end group
  // for the row's own summary and actions, so they take no row of their own.
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    label,
    items,
    value,
    onchange,
    end,
  }: {
    /** The row's accessible name. */
    label: string;
    items: { id: T; label: string; icon?: string; count?: number | null }[];
    value: T;
    onchange: (id: T) => void;
    end?: Snippet;
  } = $props();

  function onkey(e: KeyboardEvent, i: number) {
    const n = items.length;
    const to =
      e.key === 'ArrowRight'
        ? (i + 1) % n
        : e.key === 'ArrowLeft'
          ? (i + n - 1) % n
          : e.key === 'Home'
            ? 0
            : e.key === 'End'
              ? n - 1
              : null;
    if (to == null) return;
    e.preventDefault();
    onchange(items[to].id);
    queueMicrotask(() => document.getElementById(`tab-${items[to].id}`)?.focus());
  }
</script>

<div class="subtabs">
  <div class="tabs" role="tablist" aria-label={label}>
    {#each items as t, i (t.id)}
      <button
        class="tab"
        role="tab"
        id="tab-{t.id}"
        aria-selected={value === t.id}
        aria-controls="panel-{t.id}"
        tabindex={value === t.id ? 0 : -1}
        onclick={() => onchange(t.id)}
        onkeydown={(e) => onkey(e, i)}
      >
        {#if t.icon}<Icon name={t.icon} size={14} />{/if}
        {t.label}
        {#if t.count != null}<span class="count">{t.count}</span>{/if}
      </button>
    {/each}
  </div>
  {#if end}<div class="end">{@render end()}</div>{/if}
</div>

<style>
  .subtabs {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
    border-bottom: 1px solid var(--border);
  }
  .subtabs > .tabs {
    flex: 1 1 auto;
    border-bottom: 0;
  }
  .end {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
    margin-left: auto;
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
</style>
