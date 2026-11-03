<script lang="ts">
  // The top bar's scope and search field: the dataset switcher as its first segment, then
  // the launcher's input, a combobox whose list groups the items of every registered
  // provider (lib/launcher.svelte.ts). Ctrl+K, Cmd+K and `/` focus it from anywhere and
  // Escape gives the focus back. On a phone the bar shows a search button instead, and the
  // keys or the button expand the field over the whole bar, with a back button.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { onMount, tick } from 'svelte';
  import { app } from '$lib/app.svelte';
  import { errorMessage } from '$lib/api';
  import { entityProvider } from '$lib/entity-search';
  import {
    highlightParts,
    isLauncherKey,
    launcher,
    type LauncherGroup,
    type LauncherItem,
  } from '$lib/launcher.svelte';
  import DatasetMenu from './DatasetMenu.svelte';
  import Icon from './Icon.svelte';

  const uid = $props.id();
  const listId = `launcher-list-${uid}`;

  let input: HTMLInputElement | undefined = $state();
  let root: HTMLDivElement | undefined = $state();
  let opener: HTMLButtonElement | undefined = $state();
  let groups = $state<LauncherGroup[]>([]);
  /** The dataset and query that `groups` answer. */
  let groupsFor = $state('');
  let active = $state(0);
  let busy = $state(false);
  /** Enter was pressed before the results of the current query arrived. */
  let pickPending = false;
  /** Where the focus was before the keys opened the launcher. */
  let returnTo: HTMLElement | null = null;
  const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);

  const base = resolve('/').replace(/\/$/, '');
  const ds = $derived(app.current);
  const items = $derived(groups.flatMap((g) => g.items));
  const searchKey = $derived(`${ds ?? ''}\u0000${launcher.query.trim()}`);
  /** Whether the rows shown answer what the field now says; Enter waits until they do. */
  const fresh = $derived(groupsFor === searchKey);
  const showResults = $derived(launcher.open && (!!launcher.query.trim() || items.length > 0));
  const placeholder = $derived(
    ds ? 'Search classes, properties and concepts' : 'Open a dataset to search it',
  );

  onMount(() => {
    const off = launcher.register(entityProvider(base));
    launcher.bindFocus(() => void focusField());
    const onKey = (e: KeyboardEvent) => {
      if (!isLauncherKey(e)) return;
      if (document.querySelector('dialog[open]')) return;
      e.preventDefault();
      e.stopPropagation();
      const a = document.activeElement;
      if (a instanceof HTMLElement && !root?.contains(a)) returnTo = a;
      launcher.show();
    };
    // the capture phase: before CodeMirror and the pages' own key handlers
    window.addEventListener('keydown', onKey, true);
    return () => {
      off();
      launcher.bindFocus(null);
      window.removeEventListener('keydown', onKey, true);
    };
  });

  async function focusField() {
    launcher.open = true;
    await tick();
    input?.focus();
    input?.select();
  }

  async function close(restore = true) {
    launcher.close();
    if (!restore) return;
    const back = returnTo?.isConnected ? returnTo : null;
    returnTo = null;
    // the search button shows again once the field has collapsed
    await tick();
    // on a phone the field collapses into the search button, which gets the focus back
    if (back) back.focus();
    else if (opener && opener.offsetParent) opener.focus();
    else input?.blur();
  }

  // the search: debounced, the previous request aborted. Every provider answers, an empty
  // query included, since a provider may suggest items before anything is typed.
  $effect(() => {
    const q = launcher.query.trim();
    const d = ds;
    const key = searchKey;
    const providers = launcher.providers.filter((p) => !p.active || p.active({ ds: d }));
    if (!launcher.open && !q) {
      groups = [];
      groupsFor = key;
      busy = false;
      return;
    }
    const ctrl = new AbortController();
    busy = true;
    const timer = setTimeout(
      async () => {
        const out = await Promise.all(
          providers.map(async (p): Promise<LauncherGroup> => {
            const label = typeof p.label === 'function' ? p.label({ ds: d }) : p.label;
            try {
              return {
                provider: p.id,
                label,
                items: await p.search(q, { ds: d, signal: ctrl.signal }),
              };
            } catch (e) {
              return { provider: p.id, label, items: [], error: errorMessage(e) };
            }
          }),
        );
        if (ctrl.signal.aborted) return;
        groups = out.filter((g) => g.items.length || g.error);
        groupsFor = key;
        active = 0;
        busy = false;
        if (pickPending) {
          pickPending = false;
          pick(groups.flatMap((g) => g.items)[0]);
        }
      },
      q ? 140 : 0,
    );
    return () => {
      clearTimeout(timer);
      ctrl.abort();
    };
  });

  function pick(it: LauncherItem | undefined) {
    if (!it) return;
    launcher.query = '';
    void close(false);
    input?.blur();
    if (it.href) void goto(it.href);
    else it.run?.();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      launcher.open = true;
      if (items.length) active = (active + 1) % items.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (items.length) active = (active - 1 + items.length) % items.length;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      // rows of an earlier query or dataset are not picked; the first row of this one is
      if (fresh) pick(items[active]);
      else if (launcher.query.trim()) pickPending = true;
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      pickPending = false;
      void close();
    }
  }

  // the active row stays in view as the arrow keys move it
  $effect(() => {
    if (!showResults || !items.length) return;
    document.getElementById(optionId(active))?.scrollIntoView({ block: 'nearest' });
  });

  $effect(() => {
    if (!launcher.open) return;
    const onDoc = (e: MouseEvent) => {
      if (root && !root.contains(e.target as Node)) launcher.close();
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  });

  /** The flat index of the item `j` of group `g`. */
  const flatIndex = (g: number, j: number) =>
    groups.slice(0, g).reduce((n, x) => n + x.items.length, 0) + j;
  const optionId = (i: number) => `${listId}-${i}`;
</script>

<div
  class="scope-search"
  class:expanded={launcher.open}
  bind:this={root}
  onfocusout={(e) => {
    const to = e.relatedTarget as Node | null;
    if (to && !root?.contains(to)) launcher.close();
  }}
>
  <button
    bind:this={opener}
    class="btn ghost icon opener"
    aria-label="Open search"
    title="Search ({mac ? '⌘' : 'Ctrl+'}K)"
    onclick={() => {
      returnTo = opener ?? null;
      void focusField();
    }}><Icon name="search" size={18} /></button
  >
  <div class="field">
    <button class="btn ghost icon back" aria-label="Close search" onclick={() => void close()}>
      <Icon name="stepBack" size={18} />
    </button>
    <div class="scope"><DatasetMenu /></div>
    <label class="query">
      <Icon name="search" size={15} />
      <span class="sr-only">Search {ds ?? ''}</span>
      <input
        bind:this={input}
        bind:value={launcher.query}
        type="search"
        role="combobox"
        aria-expanded={showResults}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={launcher.open && items.length ? optionId(active) : undefined}
        autocomplete="off"
        spellcheck="false"
        {placeholder}
        onfocus={() => (launcher.open = true)}
        {onkeydown}
      />
      {#if busy}<span class="spinner"></span>{/if}
      <kbd class="hint" aria-hidden="true">{mac ? '⌘' : 'Ctrl'} K</kbd>
    </label>
  </div>

  {#if showResults}
    <div class="results" class:stale={!fresh}>
      <div id={listId} role="listbox" aria-label="Search results">
        {#each groups as g, gi (g.provider)}
          <div role="group" aria-labelledby="{listId}-g{gi}">
            <div class="group" id="{listId}-g{gi}">{g.label}</div>
            {#if g.error}<div class="note error" role="presentation">{g.error}</div>{/if}
            {#each g.items as it, j (it.id)}
              {@const i = flatIndex(gi, j)}
              <div
                id={optionId(i)}
                role="option"
                tabindex="-1"
                aria-selected={i === active}
                class="hit"
                onmousedown={(e) => e.preventDefault()}
                onclick={() => pick(it)}
                onkeydown={(e) => e.key === 'Enter' && pick(it)}
                onmousemove={() => (active = i)}
              >
                <span class="title"
                  >{#each highlightParts(it.title, it.highlight) as p, k (k)}{#if p.hit}<mark
                        >{p.text}</mark
                      >{:else}{p.text}{/if}{/each}</span
                >
                {#if it.subtitle}<span class="sub">{it.subtitle}</span>{/if}
                {#if it.tag}<span class="count-badge tag">{it.tag}</span>{/if}
              </div>
            {/each}
          </div>
        {:else}
          <div class="note" role="presentation">
            {#if busy}Searching…{:else if !ds}Open a dataset to search its entities.{:else}No entity
              in {ds} matches “{launcher.query.trim()}”.{/if}
          </div>
        {/each}
      </div>
      <div class="foot" aria-hidden="true">
        <span><kbd>↑</kbd><kbd>↓</kbd> move</span><span><kbd>Enter</kbd> open</span><span
          ><kbd>Esc</kbd> close</span
        >
      </div>
    </div>
  {/if}
</div>

<style>
  .scope-search {
    position: relative;
    flex: 1;
    min-width: 0;
    max-width: 640px;
    display: flex;
  }
  .opener,
  .back {
    display: none;
  }
  .field {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: stretch;
    height: 34px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .field:focus-within {
    border-color: var(--iri);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--iri) 18%, transparent);
    background: var(--surface);
  }
  .scope {
    display: flex;
    min-width: 0;
    flex: none;
    max-width: 45%;
    border-right: 1px solid var(--border);
  }
  .query {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 0 var(--sp-2) 0 var(--sp-3);
    color: var(--text-3);
    cursor: text;
  }
  .query input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: 0;
    outline: none;
    background: none;
    color: var(--text);
    font: inherit;
  }
  .query input::placeholder {
    color: var(--text-3);
  }
  .query input::-webkit-search-cancel-button {
    display: none;
  }
  .query input:focus-visible {
    box-shadow: none;
  }
  .results.stale .hit {
    opacity: 0.6;
  }
  .results {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    right: 0;
    z-index: 50;
    max-height: min(520px, calc(100dvh - var(--topbar-h) - 24px));
    overflow: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
    padding: var(--sp-1);
  }
  .group {
    padding: var(--sp-2) var(--sp-3) var(--sp-1);
    font-size: var(--fs-xs);
    font-weight: 600;
    color: var(--text-3);
  }
  .hit {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: var(--sp-3);
    padding: 7px var(--sp-3);
    border-radius: var(--r);
    cursor: pointer;
  }
  .hit[aria-selected='true'] {
    background: var(--hover);
    box-shadow: inset 2px 0 0 var(--spark);
  }
  .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title mark {
    background: var(--spark-soft);
    color: inherit;
    border-radius: 2px;
  }
  .sub {
    grid-column: 1;
    font-size: var(--fs-sm);
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    grid-row: 1 / 3;
    grid-column: 2;
    align-self: center;
  }
  .note {
    padding: var(--sp-3);
    color: var(--text-2);
  }
  .note.error {
    color: var(--danger);
  }
  .foot {
    display: flex;
    gap: var(--sp-4);
    padding: var(--sp-2) var(--sp-3) var(--sp-1);
    margin-top: var(--sp-1);
    border-top: 1px solid var(--border);
    color: var(--text-3);
    font-size: var(--fs-xs);
  }
  .foot span {
    display: inline-flex;
    gap: 3px;
    align-items: center;
  }

  /* medium: the hint goes, the field keeps its room */
  @media (max-width: 1199px) {
    .hint {
      display: none;
    }
  }

  /* a phone: a search button; the field expands over the whole bar */
  @media (max-width: 760px) {
    .scope-search {
      flex: none;
      max-width: none;
      margin-left: auto;
    }
    .opener {
      display: inline-flex;
    }
    .field {
      display: none;
    }
    .scope {
      display: none;
    }
    .foot {
      display: none;
    }
    .expanded .opener {
      visibility: hidden;
    }
    .expanded .field {
      display: flex;
      position: fixed;
      z-index: 60;
      top: 0;
      left: 0;
      right: 0;
      height: var(--topbar-h);
      padding: 6px var(--sp-2);
      gap: var(--sp-1);
      border: 0;
      border-bottom: 1px solid var(--border);
      border-radius: 0;
      background: var(--surface);
      box-shadow: none;
    }
    .expanded .back {
      display: inline-flex;
      align-self: center;
    }
    .expanded .query {
      border: 1px solid var(--iri);
      border-radius: var(--r);
      background: var(--surface);
    }
    .results {
      position: fixed;
      z-index: 60;
      top: var(--topbar-h);
      left: 0;
      right: 0;
      bottom: 0;
      max-height: none;
      border: 0;
      border-radius: 0;
      box-shadow: none;
    }
  }
</style>
