<script lang="ts">
  // The Ask page (C18 §6.2): the app's thread at full width, with the history's threads
  // beside it. The drawer shows the same thread, so a thread moved here from the drawer
  // keeps its turns. From 761 to 1199 px and on a phone the list folds into a Threads menu.
  import { page } from '$app/state';
  import { app, toasts } from '$lib/app.svelte';
  import { askThread } from '$lib/ask-thread.svelte';
  import { dayGroup, threadLine } from '$lib/ask-thread';
  import AskThread from '$components/ask/AskThread.svelte';
  import Icon from '$components/Icon.svelte';

  {
    const urlDs = page.url.searchParams.get('ds');
    if (urlDs && urlDs !== app.current) app.setDataset(urlDs);
  }
  // the drawer would show the same thread twice
  askThread.open = false;

  $effect(() => {
    if (askThread.ds && askThread.status?.historyDays) void askThread.loadHistory();
  });

  const groups = $derived.by(() => {
    const out: { day: string; items: NonNullable<typeof askThread.history> }[] = [];
    for (const t of askThread.history ?? []) {
      const d = dayGroup(t.at);
      const last = out[out.length - 1];
      if (last?.day === d) last.items.push(t);
      else out.push({ day: d, items: [t] });
    }
    return out;
  });
  const title = $derived(askThread.thread.turns[0]?.question ?? 'New thread');
  let menuOpen = $state(false);

  async function reopen(id: string) {
    menuOpen = false;
    try {
      await askThread.reopen(id);
    } catch (e) {
      toasts.error('Could not open the thread', e);
    }
  }

  async function remove() {
    const id = askThread.thread.id;
    if (!id) return;
    try {
      await askThread.deleteThread(id);
    } catch (e) {
      toasts.error('Could not delete the thread', e);
    }
  }
</script>

<svelte:head><title>Ask | Sparkles</title></svelte:head>

{#snippet list()}
  {#if askThread.historyError}
    <p class="faint small pad">{askThread.historyError}</p>
  {:else if !askThread.status?.historyDays}
    <p class="faint small pad">This dataset keeps no history, so earlier threads are not listed.</p>
  {:else if askThread.history && !askThread.history.length}
    <p class="faint small pad">Threads you ask appear here.</p>
  {/if}
  <ol>
    {#each groups as g (g.day)}
      <li class="day" aria-hidden="true">{g.day}</li>
      {#each g.items as t (t.id)}
        <li>
          <button
            type="button"
            class="item"
            class:on={askThread.thread.id === t.id}
            aria-current={askThread.thread.id === t.id ? 'true' : undefined}
            onclick={() => void reopen(t.id)}
            ><span class="q">{t.title}</span><span class="m">{threadLine(t)}</span></button
          >
        </li>
      {/each}
    {/each}
  </ol>
{/snippet}

<h1 class="sr-only">Ask</h1>
{#if !app.current}
  <p class="pad">Choose a dataset to ask about.</p>
{:else if askThread.status && !askThread.canAsk}
  <p class="pad">{askThread.status.reason ?? `${app.current} has no assistant.`}</p>
{:else}
  <div class="ask-page">
    <aside class="threads" aria-label="Threads">
      <div class="threads-head">
        <h2>Threads</h2>
        <span class="spacer"></span>
        <button type="button" class="btn sm" onclick={() => askThread.newThread()}
          ><Icon name="plus" size={12} /> New thread</button
        >
      </div>
      {@render list()}
    </aside>
    <section class="main" aria-label="Thread">
      <div class="bar">
        <div class="menu">
          <button
            type="button"
            class="btn sm"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onclick={() => {
              menuOpen = !menuOpen;
              if (menuOpen) void askThread.loadHistory();
            }}
            onkeydown={(e) => e.key === 'Escape' && (menuOpen = false)}
            >Threads <Icon name="chevronDown" size={10} /></button
          >
          {#if menuOpen}
            <div class="dropdown-menu threads-menu" role="menu">{@render list()}</div>
          {/if}
        </div>
        <h2 class="title">{title}</h2>
        <span class="spacer"></span>
        {#if askThread.thread.id && askThread.status?.historyDays}
          <button type="button" class="btn ghost sm" onclick={remove}>Delete</button>
        {/if}
        <button
          type="button"
          class="btn sm"
          disabled={!askThread.thread.turns.length}
          onclick={() => askThread.newThread()}>New thread</button
        >
      </div>
      <AskThread />
    </section>
  </div>
{/if}

<style>
  .pad {
    padding: var(--sp-4) var(--gutter);
    margin: 0;
  }
  .ask-page {
    flex: 1;
    display: grid;
    grid-template-columns: 264px minmax(0, 1fr);
    min-height: 0;
    height: 100%;
  }
  .threads {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr);
    min-height: 0;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }
  .threads-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: var(--toolbar-h);
    padding: var(--sp-2) var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: var(--fs-md);
  }
  ol {
    list-style: none;
    margin: 0;
    padding: var(--sp-2);
    overflow: auto;
    display: grid;
    gap: 2px;
    align-content: start;
  }
  .day {
    padding: var(--sp-2) var(--sp-2) 2px;
    font-size: var(--fs-xs);
    font-weight: 600;
    color: var(--text-3);
  }
  .item {
    display: grid;
    gap: 1px;
    width: 100%;
    padding: 6px var(--sp-2);
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .item:hover {
    background: var(--hover);
  }
  .item.on {
    background: var(--surface-2);
    box-shadow: inset 2px 0 0 var(--spark);
  }
  .item:focus-visible {
    outline: none;
    box-shadow: var(--focus);
  }
  .q {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .m {
    font-size: var(--fs-xs);
    color: var(--text-3);
  }
  .main {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    min-height: 0;
    min-width: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: var(--toolbar-h);
    padding: var(--sp-2) var(--gutter);
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .menu {
    position: relative;
    display: none;
  }
  .threads-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 20;
    width: min(320px, calc(100vw - 32px));
    max-height: 60vh;
    overflow: auto;
  }
  @media (max-width: 1199px) {
    .ask-page {
      grid-template-columns: minmax(0, 1fr);
    }
    .threads {
      display: none;
    }
    .menu {
      display: block;
    }
  }
  @media (max-width: 760px) {
    .title {
      display: none;
    }
  }
</style>
