<script lang="ts">
  // A row of filter controls that folds into one Filter button, with the number of active
  // filters, once the controls no longer fit on one line beside their neighbours (ROADMAP
  // item 633). Folded, the button opens the same controls in a panel below it, so their
  // bindings stay with the caller. A search field belongs outside the group and stays in view.
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    active = 0,
    label = 'Filters',
    children,
  }: { active?: number; label?: string; children: Snippet } = $props();

  let root = $state<HTMLElement>();
  let row = $state<HTMLElement>();
  let folded = $state(false);
  let open = $state(false);
  /** The width the controls needed on one line when the group folded. */
  let needed = 0;

  // fold when the controls wrap, unfold once the group is as wide as they needed
  $effect(() => {
    const el = root;
    if (!el) return;
    const check = () => {
      const r = row;
      if (!r) return;
      if (!folded) {
        const kids = [...r.children] as HTMLElement[];
        const wraps = kids.some((k) => k.offsetTop > kids[0].offsetTop + 2);
        if (wraps || r.scrollWidth > el.clientWidth + 1) {
          needed = kids.reduce((w, k) => w + k.offsetWidth + 8, 0);
          folded = true;
        }
      } else if (el.parentElement) {
        // the room the row would have: the parent's width less its other children and gaps
        const p = el.parentElement;
        const others = [...p.children].filter((c) => c !== el) as HTMLElement[];
        const room = others.reduce((w, c) => w - c.offsetWidth - 8, p.clientWidth);
        if (room >= needed) {
          folded = false;
          open = false;
        }
      }
    };
    const ro = new ResizeObserver(check);
    ro.observe(el);
    if (el.parentElement) ro.observe(el.parentElement);
    check();
    return () => ro.disconnect();
  });

  // a folded panel closes on Escape and on a click outside it
  $effect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (root && !root.contains(e.target as Node)) open = false;
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') open = false;
    };
    document.addEventListener('pointerdown', away);
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('pointerdown', away);
      document.removeEventListener('keydown', key);
    };
  });
</script>

<div class="filter-group" class:folded bind:this={root}>
  {#if folded}
    <button
      class="btn"
      aria-expanded={open}
      aria-label={active ? `${label}, ${active} active` : label}
      onclick={() => (open = !open)}
    >
      <Icon name="filter" size={13} />
      {label}
      {#if active}<span class="count-badge">{active}</span>{/if}
    </button>
  {/if}
  <div class="row" class:panel={folded} hidden={folded && !open} bind:this={row}>
    {@render children()}
  </div>
</div>

<style>
  .filter-group {
    position: relative;
    display: flex;
    flex: 1 1 0;
    min-width: 0;
  }
  .filter-group.folded {
    flex: 0 0 auto;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .row.panel {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 30;
    display: grid;
    gap: var(--sp-2);
    min-width: 220px;
    padding: var(--sp-3);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-overlay);
  }
  .row.panel[hidden] {
    display: none;
  }
  .row.panel > :global(*) {
    width: 100%;
  }
</style>
