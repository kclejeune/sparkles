<script lang="ts">
  // The path of the selected entity from a root of its tree (ROADMAP item 633), above the
  // details panel. The path follows the tree's loaded levels, which the editor fills from
  // the hierarchy route's ancestors when an entity is selected; a long path keeps its first
  // and last steps with the middle behind a button.
  let {
    roots,
    children,
    target,
    name,
    onselect,
  }: {
    roots: string[];
    children: (id: string) => string[];
    target: string | null;
    name: (id: string) => string;
    onselect: (id: string) => void;
  } = $props();

  /** The first path from a root to `target`, breadth first over at most 50,000 nodes. */
  function pathTo(t: string): string[] | null {
    const from = new Map<string, string | null>();
    const queue: string[] = [];
    for (const r of roots) if (!from.has(r)) (from.set(r, null), queue.push(r));
    for (let i = 0; i < queue.length && i < 50_000; i++) {
      const n = queue[i];
      if (n === t) {
        const out: string[] = [];
        for (let c: string | null = n; c != null; c = from.get(c) ?? null) out.unshift(c);
        return out;
      }
      for (const k of children(n)) if (!from.has(k)) (from.set(k, n), queue.push(k));
    }
    return null;
  }

  const path = $derived(target ? pathTo(target) : null);
  let whole = $state(false);
  $effect(() => {
    void target;
    whole = false;
  });
  const shown = $derived.by(() => {
    if (!path) return [];
    if (whole || path.length <= 5) return path.map((id) => ({ id }));
    return [{ id: path[0] }, { id: '' }, ...path.slice(-3).map((id) => ({ id }))];
  });
</script>

{#if path && path.length > 1}
  <nav class="crumbs" aria-label="Path">
    {#each shown as c, i (c.id || `gap${i}`)}
      {#if i > 0}<span class="sep" aria-hidden="true">›</span>{/if}
      {#if !c.id}
        <button class="step gap" title="Show the whole path" onclick={() => (whole = true)}
          >… {(path.length - 4).toLocaleString('en-US')} more</button
        >
      {:else if c.id === target}
        <span class="step here" aria-current="location">{name(c.id)}</span>
      {:else}
        <button class="step" title={name(c.id)} onclick={() => onselect(c.id)}>{name(c.id)}</button>
      {/if}
    {/each}
  </nav>
{/if}

<style>
  .crumbs {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 4px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .step {
    max-width: 16rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  button.step {
    padding: 0;
    border: 0;
    background: none;
    color: var(--iri);
    font: inherit;
    cursor: pointer;
  }
  button.step:hover {
    text-decoration: underline;
  }
  button.step:focus-visible {
    box-shadow: var(--focus);
  }
  .gap {
    color: var(--text-2) !important;
  }
  .here {
    color: var(--text);
  }
</style>
