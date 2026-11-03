<script lang="ts">
  // The top bar's scope and search field, which is also the command palette (ROADMAP items
  // 632 and 631): the dataset switcher as its first segment, then the launcher's input, a
  // combobox whose list groups the items of every registered provider
  // (lib/launcher.svelte.ts). Ctrl+K and Cmd+K open it from anywhere, `/` does too outside a
  // text field unless the page has a search of its own, and Escape gives the focus back. On
  // a phone the bar shows a search button instead, and the keys or the button expand the
  // field over the whole bar, with a back button.
  //
  // With nothing typed it lists the recent picks, the page's commands and the pages. A
  // prefix shows one kind only: `>` commands, `@` entities, `#` datasets and branches, `?`
  // saved queries, which the chips above the list also choose.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { onMount, tick } from 'svelte';
  import { app } from '$lib/app.svelte';
  import { errorMessage } from '$lib/api';
  import { auth } from '$lib/auth.svelte';
  import { entityProvider } from '$lib/entity-search';
  import {
    highlightParts,
    isLauncherKey,
    isPaletteKey,
    launcher,
    MODES,
    parseMode,
    shortcutKeys,
    type LauncherGroup,
    type LauncherItem,
    type LauncherMode,
    type LauncherProvider,
    type RecentEntry,
  } from '$lib/launcher.svelte';
  import {
    commandProvider,
    datasetProvider,
    pageProvider,
    savedQueryProvider,
    shellCommands,
  } from '$lib/palette.svelte';
  import { loadRecent, recentKey, recordRecent } from '$lib/recents';
  import DatasetMenu from './DatasetMenu.svelte';
  import Icon from './Icon.svelte';

  const uid = $props.id();
  const listId = `launcher-list-${uid}`;
  /** How many recent picks show on an empty query. */
  const RECENT_SHOWN = 5;

  let input: HTMLInputElement | undefined = $state();
  let root: HTMLDivElement | undefined = $state();
  let opener: HTMLButtonElement | undefined = $state();
  let groups = $state<LauncherGroup[]>([]);
  /** The dataset and query that `groups` answer. */
  let groupsFor = $state('');
  let active = $state(0);
  let busy = $state(false);
  let recents = $state<RecentEntry[]>(loadRecent());
  /**
   * The dataset and query of an Enter pressed before their results arrived. Their first row
   * opens when they do, unless the query changes or the launcher closes first.
   */
  let pendingKey: string | null = null;
  /** Where the focus was before the keys opened the launcher. */
  let returnTo: HTMLElement | null = null;
  const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);

  const base = resolve('/').replace(/\/$/, '');
  const ds = $derived(app.current);
  const parsed = $derived(parseMode(launcher.query));
  const items = $derived(groups.flatMap((g) => g.items));
  const searchKey = $derived(`${ds ?? ''}\u0000${launcher.query.trim()}`);
  /** Whether the rows shown answer what the field now says; Enter waits until they do. */
  const fresh = $derived(groupsFor === searchKey);
  const showResults = $derived(
    launcher.open && (!!launcher.query.trim() || items.length > 0 || busy),
  );
  const placeholder = $derived(
    launcher.hint ??
      (ds ? 'Search or type > for commands' : 'Search pages, or type > for commands'),
  );

  const backupsVisible = () =>
    auth.hasServer('server-admin') || Object.values(auth.who?.datasets ?? {}).includes('admin');

  onMount(() => {
    const entities = entityProvider(base);
    const offs = [
      launcher.register({
        ...entities,
        modes: ['@'],
        search: async (q, ctx) =>
          (await entities.search(q, ctx)).map((it) => ({ icon: 'target', ...it })),
      }),
      launcher.register(commandProvider('page')),
      launcher.register(commandProvider('global')),
      launcher.register(pageProvider(base, backupsVisible)),
      launcher.register(datasetProvider()),
      launcher.register(savedQueryProvider(base)),
      launcher.command(
        shellCommands(base, {
          createDataset: () => auth.hasServer('server-admin'),
          backups: backupsVisible,
        }),
      ),
    ];
    launcher.bindFocus(() => void focusField());
    const onKey = (e: KeyboardEvent) => {
      if (!isLauncherKey(e)) return;
      if (document.querySelector('dialog[open]')) return;
      const a = document.activeElement;
      const inside = a instanceof HTMLElement && !!root?.contains(a);
      // `/` on a page with a search of its own focuses that one
      if (!isPaletteKey(e) && launcher.focusPageSearch()) {
        e.preventDefault();
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      // the key again closes an open palette
      if (isPaletteKey(e) && launcher.open && inside) {
        void close();
        return;
      }
      if (a instanceof HTMLElement && !inside) returnTo = a;
      launcher.show();
    };
    // the capture phase: before CodeMirror and the pages' own key handlers
    window.addEventListener('keydown', onKey, true);
    return () => {
      for (const off of offs) off();
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

  /** The recent picks that still apply, as items, each with the provider it came from. */
  function recentGroup(
    providers: readonly LauncherProvider[],
    d: string | null,
  ): { items: LauncherItem[]; from: string[] } {
    const out: { items: LauncherItem[]; from: string[] } = { items: [], from: [] };
    for (const e of recents) {
      if (out.items.length >= RECENT_SHOWN) break;
      const p = providers.find((x) => x.id === e.provider);
      if (!p || p.recent === false) continue;
      if (p.perDataset !== false && e.ds !== d) continue;
      // one row per item, also for picks stored with a dataset before they had none
      if (out.items.some((it, i) => out.from[i] === p.id && it.id === e.id)) continue;
      const it = p.revive
        ? p.revive(e, { ds: d })
        : e.href
          ? {
              id: e.id,
              title: e.title,
              subtitle: e.subtitle,
              tag: e.tag,
              icon: e.icon,
              href: e.href,
            }
          : null;
      if (!it) continue;
      out.items.push(it);
      out.from.push(p.id);
    }
    return out;
  }

  // the search: debounced, the previous request aborted. Every provider answers, an empty
  // query included, since a provider may suggest items before anything is typed.
  $effect(() => {
    const { mode, query: q } = parsed;
    const d = ds;
    const key = searchKey;
    const rec = recents;
    if (pendingKey !== key) pendingKey = null;
    const providers = launcher.providers.filter(
      (p) => (!p.active || p.active({ ds: d })) && (!mode || !!p.modes?.includes(mode)),
    );
    if (!launcher.open && !launcher.query.trim()) {
      groups = [];
      groupsFor = key;
      busy = false;
      return;
    }
    // what the providers' items come from: the search runs again when it changes
    for (const p of providers) p.watch?.();
    const ctrl = new AbortController();
    busy = true;
    const timer = setTimeout(
      async () => {
        const out = await Promise.all(
          providers.map(async (p): Promise<LauncherGroup> => {
            const label = typeof p.label === 'function' ? p.label({ ds: d }) : p.label;
            const recent = new Set(rec.filter((e) => e.provider === p.id).map((e) => e.id));
            try {
              const found = await p.search(q, { ds: d, signal: ctrl.signal, mode, recent });
              return { provider: p.id, label, items: found };
            } catch (e) {
              return { provider: p.id, label, items: [], error: errorMessage(e) };
            }
          }),
        );
        if (ctrl.signal.aborted) return;
        let next = out.filter((g) => g.items.length || g.error);
        // nothing typed: the recent picks first, and not again in their own groups
        if (!q && !mode) {
          const r = recentGroup(providers, d);
          if (r.items.length) {
            const seen = new Set(r.items.map((it, i) => recentKey(r.from[i], it.id)));
            next = next
              .map((g) => ({
                ...g,
                items: g.items.filter((it) => !seen.has(recentKey(g.provider, it.id))),
              }))
              .filter((g) => g.items.length || g.error);
            next.unshift({
              provider: 'launcher.recent',
              label: 'Recent',
              items: r.items,
              from: r.from,
            });
          }
        }
        groups = next;
        groupsFor = key;
        active = 0;
        busy = false;
        if (pendingKey === key && launcher.open) {
          pendingKey = null;
          const g = groups.find((x) => x.items.length);
          if (g) pick(g, 0);
        }
      },
      q ? 140 : 0,
    );
    return () => {
      clearTimeout(timer);
      ctrl.abort();
    };
  });

  /** Remembers a pick of group `g`'s item `j`, unless its provider asks not to. */
  function remember(g: LauncherGroup, j: number) {
    const it = g.items[j];
    const pid = g.from?.[j] ?? g.provider;
    const p = launcher.providers.find((x) => x.id === pid);
    if (!p || p.recent === false || it.query !== undefined) return;
    recents = recordRecent(recents, {
      provider: pid,
      id: it.id,
      title: it.title,
      subtitle: it.subtitle,
      tag: it.tag,
      icon: it.icon,
      href: it.href,
      // a pick of a provider that is not per dataset belongs to none
      ds: p.perDataset === false ? null : ds,
      at: Date.now(),
    });
  }

  async function pick(g: LauncherGroup | undefined, j: number) {
    const it = g?.items[j];
    if (!g || !it) return;
    remember(g, j);
    // an item that sets the field, such as "Switch dataset or branch…"
    if (it.query !== undefined) {
      launcher.query = it.query;
      input?.focus();
      return;
    }
    launcher.query = '';
    if (it.href) {
      void close(false);
      input?.blur();
      void goto(it.href);
      return;
    }
    // the focus goes back first, so that a dialog the command opens takes it from there
    await close(true);
    try {
      await it.run?.();
    } catch (e) {
      console.error(e);
    }
  }

  /** The group and index within it of flat row `i`. */
  function locate(i: number): { g: number; j: number } {
    let n = i;
    for (let g = 0; g < groups.length; g++) {
      if (n < groups[g].items.length) return { g, j: n };
      n -= groups[g].items.length;
    }
    return { g: -1, j: -1 };
  }

  function pickFlat(i: number) {
    const { g, j } = locate(i);
    if (g >= 0) void pick(groups[g], j);
  }

  /** The flat index of the first row of the next (or previous) group with rows. */
  function groupStep(dir: 1 | -1): number | null {
    const { g } = locate(active);
    for (let k = g + dir; k >= 0 && k < groups.length; k += dir)
      if (groups[k].items.length) return flatIndex(k, 0);
    return null;
  }

  function setMode(m: LauncherMode | null) {
    launcher.query = (m ?? '') + parsed.query;
    launcher.open = true;
    input?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      launcher.open = true;
      if (items.length) active = (active + 1) % items.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (items.length) active = (active - 1 + items.length) % items.length;
    } else if (e.key === 'Tab' && showResults && !e.altKey && !e.ctrlKey && !e.metaKey) {
      // Tab moves between groups; past the last (or first) one the focus leaves as usual
      const to = groupStep(e.shiftKey ? -1 : 1);
      if (to != null) {
        e.preventDefault();
        active = to;
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      // rows of an earlier query or dataset are not picked; the first row of this one is
      if (fresh) pickFlat(active);
      else if (launcher.query.trim()) pendingKey = searchKey;
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      // a mode with nothing after it goes back to everything first
      if (parsed.mode && !parsed.query) launcher.query = '';
      else void close();
    }
  }

  // a dismissed launcher forgets a pending Enter, whichever way it closed
  $effect(() => {
    if (!launcher.open) pendingKey = null;
  });

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

  const emptyNote = $derived.by(() => {
    if (busy) return 'Searching…';
    const q = parsed.query;
    if (parsed.mode === '@' && !ds) return 'Open a dataset to search its entities.';
    if (parsed.mode === '@' && !q) return `Type to search the entities of ${ds}.`;
    if (parsed.mode === '?' && !ds) return 'Open a dataset to list its saved queries.';
    if (parsed.mode === '?' && !q) return `${ds} has no saved queries.`;
    if (!q) return 'Nothing to show here.';
    return ds ? `Nothing in ${ds} matches “${q}”.` : `Nothing matches “${q}”.`;
  });
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
      <div class="modes" role="toolbar" aria-label="Show">
        {#each MODES as m (m.label)}
          <button
            type="button"
            class="mode"
            aria-pressed={parsed.mode === m.mode}
            title={m.title}
            onmousedown={(e) => e.preventDefault()}
            onclick={() => setMode(m.mode)}
            >{m.label}{#if m.mode}<span class="prefix" aria-hidden="true">{m.mode}</span
              >{/if}</button
          >
        {/each}
      </div>
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
                class:single={!it.subtitle}
                onmousedown={(e) => e.preventDefault()}
                onclick={() => void pick(g, j)}
                onkeydown={(e) => e.key === 'Enter' && void pick(g, j)}
                onmousemove={() => (active = i)}
              >
                <span class="ic" aria-hidden="true"
                  >{#if it.icon}<Icon name={it.icon} size={15} />{/if}</span
                >
                <span class="title"
                  >{#each highlightParts(it.title, it.highlight) as p, k (k)}{#if p.hit}<mark
                        >{p.text}</mark
                      >{:else}{p.text}{/if}{/each}</span
                >
                {#if it.subtitle}<span class="sub">{it.subtitle}</span>{/if}
                {#if it.shortcut}{@const keys = shortcutKeys(it.shortcut, mac)}<span class="sr-only"
                    >, shortcut {keys.join(' ')}</span
                  ><span class="keys" aria-hidden="true"
                    >{#each keys as k, ki (ki)}<kbd>{k}</kbd>{/each}</span
                  >{:else if it.tag}<span class="count-badge tag">{it.tag}</span>{/if}
              </div>
            {/each}
          </div>
        {:else}
          <div class="note" role="presentation">{emptyNote}</div>
        {/each}
      </div>
      <div class="sr-only" aria-live="polite">
        {fresh && !busy ? `${items.length} result${items.length === 1 ? '' : 's'}` : ''}
      </div>
      <div class="foot" aria-hidden="true">
        <span><kbd>↑</kbd><kbd>↓</kbd> move</span><span><kbd>Tab</kbd> next group</span><span
          ><kbd>Enter</kbd> open</span
        ><span><kbd>&gt;</kbd> commands</span><span><kbd>Esc</kbd> close</span>
      </div>
    </div>
  {/if}
</div>

<style>
  .scope-search {
    position: relative;
    min-width: 0;
    width: 100%;
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
  .modes {
    display: flex;
    gap: var(--sp-1);
    padding: var(--sp-1) var(--sp-1) var(--sp-2);
    margin-bottom: var(--sp-1);
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
    scrollbar-width: none;
  }
  .modes::-webkit-scrollbar {
    display: none;
  }
  .mode {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--ctl-h-sm);
    padding: 0 var(--sp-2);
    border: 1px solid transparent;
    border-radius: var(--r-pill);
    background: none;
    color: var(--text-2);
    font: inherit;
    font-weight: 500;
    cursor: pointer;
  }
  .mode:hover {
    background: var(--hover);
    color: var(--text);
  }
  .mode[aria-pressed='true'] {
    background: var(--active);
    border-color: var(--border-strong);
    color: var(--text);
  }
  .mode .prefix {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    font-weight: 600;
    color: var(--text-3);
  }
  .mode[aria-pressed='true'] .prefix {
    color: var(--spark-ink);
  }
  .hit {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    align-items: center;
    column-gap: var(--sp-3);
    min-height: 34px;
    padding: 6px var(--sp-3);
    border-radius: var(--r);
    cursor: pointer;
  }
  /* a row without a subtitle keeps its title on the middle line */
  .single .title {
    grid-row: 1 / 3;
  }
  .ic {
    grid-row: 1 / 3;
    display: grid;
    place-items: center;
    color: var(--text-3);
  }
  .hit[aria-selected='true'] .ic {
    color: var(--spark-ink);
  }
  .keys {
    grid-row: 1 / 3;
    grid-column: 3;
    display: inline-flex;
    gap: 3px;
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
    grid-column: 2;
    font-size: var(--fs-sm);
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    grid-row: 1 / 3;
    grid-column: 3;
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
      width: auto;
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
    /* the chips scroll sideways, and the edge fades where more of them wait */
    .modes {
      mask-image: linear-gradient(to right, #000 calc(100% - 28px), transparent);
      padding-right: 28px;
    }
    .hit {
      min-height: 44px;
    }
    .keys {
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
