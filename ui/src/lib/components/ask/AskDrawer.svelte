<script lang="ts">
  // The Ask drawer (C18 §6.2): the app's one thread docked beside any page. At 1200 px and
  // wider it is a column of the shell and the page narrows; at medium widths it lies over
  // the page's right side, and the page stays usable; on a phone it is a bottom sheet over
  // a scrim. It opens from the palette's Ask row, its commands and Shift+A outside text
  // fields, and Escape closes it. "Open as page" moves the thread to /ui/ask.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { askThread } from '$lib/ask-thread.svelte';
  import { dayGroup, threadLine } from '$lib/ask-thread';
  import { launcher, useCommands } from '$lib/launcher.svelte';
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import AskThread from './AskThread.svelte';

  const onAskPage = $derived(/\/ask\/?$/.test(page.url.pathname));
  const principal = $derived(auth.enabled ? (auth.who?.principal.name ?? null) : null);
  $effect(() => {
    void askThread.setDataset(app.current, principal);
  });

  let phone = $state(false);
  onMount(() => {
    const mq = window.matchMedia('(max-width: 760px)');
    phone = mq.matches;
    const on = () => (phone = mq.matches);
    mq.addEventListener('change', on);
    const key = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (
        e.key === 'A' &&
        e.shiftKey &&
        !e.ctrlKey &&
        !e.metaKey &&
        !e.altKey &&
        !el?.closest?.('input, textarea, select, [contenteditable], .cm-editor')
      ) {
        if (!askThread.canAsk) return;
        e.preventDefault();
        if (onAskPage) askThread.focus();
        else askThread.show();
      }
    };
    window.addEventListener('keydown', key);
    // the palette's Ask row, while the dataset can ask
    const unregister = launcher.register({
      id: 'ask',
      label: 'Ask',
      order: 90,
      recent: false,
      active: ({ ds }) => !!ds && askThread.canAsk,
      watch: () => askThread.canAsk,
      async search(raw) {
        const q = raw.trim();
        if (q.length < 2) return [];
        return [
          {
            id: '?ask',
            title: 'Ask a question…',
            subtitle: `“${q}”`,
            icon: 'sparkle',
            run: () => (onAskPage ? void askThread.ask(q) : askThread.show(q)),
          },
        ];
      },
    });
    return () => {
      mq.removeEventListener('change', on);
      window.removeEventListener('keydown', key);
      unregister();
    };
  });

  useCommands(() => [
    {
      id: 'ask.open',
      label: 'Ask a question',
      group: 'global',
      icon: 'sparkle',
      shortcut: 'Shift+A',
      keywords: ['question', 'assistant', 'drawer'],
      active: () => askThread.canAsk,
      run: () => (onAskPage ? askThread.focus() : askThread.show()),
    },
    {
      id: 'ask.page',
      label: 'Open Ask as a page',
      group: 'global',
      icon: 'expand',
      keywords: ['threads', 'history'],
      active: () => askThread.canAsk && !onAskPage,
      run: () => openAsPage(),
    },
    {
      id: 'ask.new',
      label: 'New Ask thread',
      group: 'global',
      icon: 'plus',
      active: () => askThread.canAsk && askThread.thread.turns.length > 0,
      run: () => {
        askThread.newThread();
        if (!onAskPage) askThread.show();
      },
    },
  ]);

  function openAsPage() {
    askThread.open = false;
    void goto(`${resolve('/ask')}${app.current ? `?ds=${encodeURIComponent(app.current)}` : ''}`);
  }

  let historyOpen = $state(false);
  function toggleHistory() {
    historyOpen = !historyOpen;
    if (historyOpen) void askThread.loadHistory();
  }

  const showing = $derived(askThread.open && !onAskPage && !!app.current);
  let sheet: HTMLDialogElement | undefined = $state();
  $effect(() => {
    if (!sheet) return;
    if (showing && phone && !sheet.open) sheet.showModal();
    else if ((!showing || !phone) && sheet.open) sheet.close();
  });
</script>

{#snippet body()}
  <div class="head">
    <span class="handle" aria-hidden="true"></span>
    <h2 id="ask-drawer-title">Ask</h2>
    {#if askThread.status?.historyDays}
      <div class="hist">
        <button
          type="button"
          class="btn ghost sm"
          aria-haspopup="menu"
          aria-expanded={historyOpen}
          onclick={toggleHistory}>History <Icon name="chevronDown" size={10} /></button
        >
        {#if historyOpen}
          <div class="dropdown-menu" role="menu" aria-label="Earlier threads">
            {#if !askThread.history}
              <span class="dropdown-item faint">Loading…</span>
            {:else if !askThread.history.length}
              <span class="dropdown-item faint">No earlier threads</span>
            {:else}
              {#each askThread.history.slice(0, 12) as t (t.id)}
                <button
                  type="button"
                  role="menuitem"
                  class="dropdown-item thread-item"
                  onclick={() => {
                    historyOpen = false;
                    void askThread.reopen(t.id);
                  }}
                  ><span class="tq">{t.title}</span><span class="faint small"
                    >{dayGroup(t.at)}, {threadLine(t)}</span
                  ></button
                >
              {/each}
            {/if}
          </div>
        {/if}
      </div>
    {/if}
    <button
      type="button"
      class="btn ghost sm"
      disabled={!askThread.thread.turns.length}
      onclick={() => askThread.newThread()}>New thread</button
    >
    <button type="button" class="btn ghost sm" onclick={openAsPage}>Open as page</button>
    <button
      type="button"
      class="btn ghost icon sm"
      aria-label="Close Ask"
      onclick={() => askThread.close()}><Icon name="x" size={14} /></button
    >
  </div>
  {#if askThread.canAsk}
    <AskThread compact />
  {:else}
    <p class="off">
      {askThread.status?.reason ?? `${app.current ?? 'This dataset'} has no assistant.`}
    </p>
  {/if}
{/snippet}

{#if phone}
  <dialog
    bind:this={sheet}
    class="sheet"
    aria-labelledby="ask-drawer-title"
    onclose={() => askThread.open && askThread.close()}
    onclick={(e) => e.target === sheet && askThread.close()}
  >
    {#if showing}<div class="drawer-in">{@render body()}</div>{/if}
  </dialog>
{:else if showing}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <aside
    class="drawer"
    aria-labelledby="ask-drawer-title"
    onkeydown={(e) => {
      if (e.key === 'Escape' && !e.defaultPrevented) {
        e.preventDefault();
        askThread.close();
      }
    }}
  >
    <div class="drawer-in">{@render body()}</div>
  </aside>
{/if}

<style>
  .drawer {
    grid-area: ask;
    width: 400px;
    min-height: 0;
    border-left: 1px solid var(--border);
    background: var(--surface);
  }
  .drawer-in {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    height: 100%;
    min-height: 0;
  }
  .head {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-height: var(--toolbar-h);
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .head h2 {
    margin: 0 auto 0 0;
    font-size: var(--fs-md);
  }
  .handle {
    display: none;
  }
  .hist {
    position: relative;
  }
  .hist .dropdown-menu {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 20;
    width: min(320px, calc(100vw - 32px));
    max-height: 60vh;
    overflow: auto;
  }
  .thread-item {
    display: grid !important;
    gap: 1px;
    text-align: left;
    height: auto !important;
    padding-block: 6px !important;
  }
  .tq {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .off {
    margin: 0;
    padding: var(--sp-4);
    color: var(--text-2);
  }
  /* medium: over the page's right side, below the top bar */
  @media (max-width: 1199px) {
    .drawer {
      position: fixed;
      z-index: 30;
      top: var(--topbar-h);
      right: 0;
      bottom: 0;
      width: min(400px, 100vw);
      box-shadow: var(--shadow-overlay);
    }
  }
  /* phone: a bottom sheet */
  .sheet {
    width: 100%;
    max-width: none;
    height: 86dvh;
    max-height: 86dvh;
    margin: auto 0 0;
    padding: 0;
    border: 1px solid var(--border);
    border-width: 1px 0 0;
    border-radius: var(--r-lg) var(--r-lg) 0 0;
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-overlay);
  }
  .sheet::backdrop {
    background: var(--scrim);
  }
  .sheet .head {
    padding-top: var(--sp-4);
  }
  .sheet .handle {
    display: block;
    position: absolute;
    top: 6px;
    left: 50%;
    width: 36px;
    height: 4px;
    margin-left: -18px;
    border-radius: 2px;
    background: var(--border-strong);
  }
  @media (prefers-reduced-motion: no-preference) {
    .sheet[open] {
      animation: rise 0.18s ease-out;
    }
  }
  @keyframes rise {
    from {
      transform: translateY(24px);
      opacity: 0.6;
    }
  }
</style>
