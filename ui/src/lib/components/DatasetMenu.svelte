<script lang="ts">
  // The dataset switcher: the open dataset (and a branch the page has open) as a button,
  // and the list of datasets under it. The top bar shows it as the scope of its search
  // field, the phone drawer as a full-width row. Choosing a dataset on a page whose URL
  // names one reopens that page on the new dataset.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { app } from '$lib/app.svelte';
  import { fmtCompact } from '$lib/format';
  import { datasetSwitchHref } from '$lib/shell';
  import Icon from './Icon.svelte';

  let { variant = 'scope', onchoose }: { variant?: 'scope' | 'drawer'; onchoose?: () => void } =
    $props();

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();
  let button: HTMLButtonElement | undefined = $state();

  const info = $derived(app.datasets.find((d) => d.name === app.current));
  /** The branch the page has open, when it is not main. */
  const branch = $derived.by(() => {
    const b =
      page.url.searchParams.get('branch') ??
      (page.url.pathname.endsWith('/query') ? app.queryBranch : '');
    return b && b !== 'main' ? b : '';
  });

  function choose(name: string) {
    open = false;
    const href = datasetSwitchHref(page.url, name);
    app.setDataset(name);
    onchoose?.();
    if (href) void goto(href);
    button?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) {
      // preventDefault too: in the phone drawer, Escape would otherwise also close the dialog
      e.preventDefault();
      e.stopPropagation();
      open = false;
      button?.focus();
    }
  }

  $effect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (root && !root.contains(e.target as Node)) open = false;
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  });
</script>

<div class="switcher {variant}" bind:this={root} {onkeydown} role="presentation">
  <button
    bind:this={button}
    class="ds-button"
    aria-haspopup="listbox"
    aria-expanded={open}
    title={info ? `${info.name}: ${fmtCompact(info.quads)} quads` : undefined}
    onclick={() => (open = !open)}
  >
    <Icon name="database" size={14} />
    <span class="ds-name">{app.current ?? (app.datasetsLoaded ? 'No dataset' : 'Loading…')}</span>
    {#if branch}<span class="ds-branch"><Icon name="branch" size={12} />{branch}</span>{/if}
    {#if info && variant === 'drawer'}<span class="ds-count">{fmtCompact(info.quads)}</span>{/if}
    <Icon name="chevronDown" size={13} />
  </button>
  {#if open}
    <div class="ds-menu" role="listbox" aria-label="Datasets">
      {#each app.datasets as d (d.name)}
        <button
          role="option"
          aria-selected={d.name === app.current}
          class="ds-item"
          onclick={() => choose(d.name)}
        >
          <span class="ds-item-name">{d.name}</span>
          <span class="ds-item-meta">{d.type === 'mem' ? 'in-memory' : 'persistent'}</span>
          <span class="ds-count">{fmtCompact(d.quads)}</span>
        </button>
      {:else}
        <div class="ds-empty">{app.datasetsError ?? 'No datasets yet'}</div>
      {/each}
      <a
        class="ds-manage"
        href={resolve('/datasets')}
        onclick={() => {
          open = false;
          onchoose?.();
        }}
      >
        <Icon name="plus" size={13} /> Manage datasets
      </a>
    </div>
  {/if}
</div>

<style>
  .switcher {
    position: relative;
    min-width: 0;
    display: flex;
  }
  .ds-button {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    height: 100%;
    padding: 0 var(--sp-2) 0 var(--sp-3);
    border: 0;
    background: none;
    color: var(--text);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    border-radius: var(--r) 0 0 var(--r);
  }
  .ds-button:hover {
    background: var(--hover);
  }
  .drawer {
    width: 100%;
  }
  .drawer .ds-button {
    width: 100%;
    height: var(--ctl-h);
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
  }
  .ds-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 16ch;
  }
  .drawer .ds-name {
    flex: 1;
    max-width: none;
    text-align: left;
  }
  .ds-branch {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--text-2);
    max-width: 14ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ds-count {
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }
  .ds-menu {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    width: max(100%, 260px);
    max-height: min(420px, calc(100dvh - 120px));
    overflow: auto;
    z-index: 40;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
    padding: var(--sp-1);
    display: grid;
  }
  .drawer .ds-menu {
    width: 100%;
  }
  .ds-item {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: var(--sp-2);
    text-align: left;
    padding: 6px var(--sp-2);
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .ds-item:hover {
    background: var(--hover);
  }
  .ds-item[aria-selected='true'] {
    background: var(--spark-soft);
  }
  .ds-item-name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ds-item-meta {
    grid-row: 2;
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .ds-item .ds-count {
    grid-row: 1 / 3;
    grid-column: 2;
    align-self: center;
  }
  .ds-empty {
    padding: var(--sp-2);
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .ds-manage {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: var(--sp-2);
    margin-top: var(--sp-1);
    border-top: 1px solid var(--border);
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .ds-manage:hover {
    color: var(--text);
    text-decoration: none;
  }
</style>
