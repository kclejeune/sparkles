<script lang="ts">
  import '@fontsource-variable/instrument-sans';
  import '@fontsource-variable/jetbrains-mono';
  import '../app.css';
  import { page } from '$app/state';
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { fmtCompact } from '$lib/format';
  import Icon from '$components/Icon.svelte';
  import Toasts from '$components/Toasts.svelte';
  import UserMenu from '$components/UserMenu.svelte';

  let { children } = $props();

  let switcherOpen = $state(false);
  let switcherEl: HTMLDivElement | undefined = $state();

  // Backups: server admins, and dataset admins in a reduced form
  const backupsVisible = $derived(
    auth.hasServer('server-admin') || Object.values(auth.who?.datasets ?? {}).includes('admin'),
  );
  const nav = $derived([
    { href: '/query', label: 'Query', icon: 'query' },
    { href: '/explore', label: 'Explore', icon: 'explore' },
    { href: '/similar', label: 'Similar', icon: 'similar' },
    { href: '/datasets', label: 'Datasets', icon: 'database' },
    ...(backupsVisible ? [{ href: '/backups', label: 'Backups', icon: 'archive' }] : []),
    { href: '/server', label: 'Server', icon: 'server' },
  ]);

  const currentInfo = $derived(app.datasets.find((d) => d.name === app.current));
  // sign-in and CLI approval pages render without the sidebar
  const bare = $derived(/^\/ui\/(login|cli)(\/|$)/.test(page.url.pathname));

  function isActive(href: string) {
    const p = page.url.pathname;
    const full = resolve(href as '/query');
    return p === full || p.startsWith(full + '/');
  }

  function linkFor(href: string) {
    const base = resolve(href as '/query');
    return (href === '/explore' || href === '/similar') && app.current
      ? `${base}?ds=${encodeURIComponent(app.current)}`
      : base;
  }

  function cycleTheme() {
    app.setTheme(app.theme === 'system' ? 'light' : app.theme === 'light' ? 'dark' : 'system');
  }

  onMount(() => {
    void auth.ensure().then(() => auth.guard());
    app.refreshDatasets();
    app.ping();
    const pingTimer = setInterval(() => app.ping(), 10_000);
    const dsTimer = setInterval(() => app.refreshDatasets(), 30_000);
    const onDoc = (e: MouseEvent) => {
      if (switcherOpen && switcherEl && !switcherEl.contains(e.target as Node))
        switcherOpen = false;
    };
    document.addEventListener('mousedown', onDoc);
    return () => {
      clearInterval(pingTimer);
      clearInterval(dsTimer);
      document.removeEventListener('mousedown', onDoc);
    };
  });
</script>

<div class="shell" class:bare>
  <aside class="sidebar">
    <a class="brand" href={resolve('/query')}>
      <svg viewBox="0 0 32 32" width="22" height="22" aria-hidden="true">
        <path d="M16 3l2.9 8.1L27 14l-8.1 2.9L16 25l-2.9-8.1L5 14l8.1-2.9z" fill="var(--spark)" />
        <circle cx="25.5" cy="6.5" r="2.2" fill="var(--spark)" />
        <circle cx="7" cy="25" r="1.6" fill="var(--spark)" opacity="0.6" />
      </svg>
      <span>Sparkles</span>
    </a>

    <div class="switcher" bind:this={switcherEl}>
      <button
        class="ds-button"
        aria-haspopup="listbox"
        aria-expanded={switcherOpen}
        onclick={() => (switcherOpen = !switcherOpen)}
        onkeydown={(e) => e.key === 'Escape' && (switcherOpen = false)}
      >
        <Icon name="database" size={14} />
        <span class="ds-name"
          >{app.current ?? (app.datasetsLoaded ? 'No dataset' : 'Loading…')}</span
        >
        {#if currentInfo}<span class="ds-count">{fmtCompact(currentInfo.quads)}</span>{/if}
        <Icon name="chevronDown" size={14} />
      </button>
      {#if switcherOpen}
        <div class="ds-menu" role="listbox" aria-label="Datasets">
          {#each app.datasets as d (d.name)}
            <button
              role="option"
              aria-selected={d.name === app.current}
              class="ds-item"
              onclick={() => {
                app.setDataset(d.name);
                switcherOpen = false;
              }}
            >
              <span class="ds-item-name">{d.name}</span>
              <span class="faint ds-item-meta">{d.type === 'mem' ? 'in-memory' : 'persistent'}</span
              >
              <span class="ds-count">{fmtCompact(d.quads)}</span>
            </button>
          {:else}
            <div class="ds-empty faint">{app.datasetsError ?? 'No datasets yet'}</div>
          {/each}
          <a class="ds-manage" href={resolve('/datasets')} onclick={() => (switcherOpen = false)}>
            <Icon name="plus" size={13} /> Manage datasets
          </a>
        </div>
      {/if}
    </div>

    <nav>
      {#each nav as item (item.href)}
        <a
          href={linkFor(item.href)}
          class:active={isActive(item.href)}
          aria-current={isActive(item.href) ? 'page' : undefined}
        >
          <Icon name={item.icon} size={16} />
          <span>{item.label}</span>
        </a>
      {/each}
    </nav>

    <div class="spacer"></div>

    <UserMenu />

    <div class="foot">
      <div
        class="status"
        title={app.online
          ? `Server reachable (${app.lastPingMs?.toFixed(0)} ms)`
          : 'Server unreachable'}
      >
        <span class="dot" class:on={app.online === true} class:off={app.online === false}></span>
        <span>
          {#if app.online === null}Connecting…{:else if app.online}Connected{:else}Offline{/if}
        </span>
        {#if app.online && app.lastPingMs != null}<span class="faint"
            >{app.lastPingMs.toFixed(0)} ms</span
          >{/if}
      </div>
      <button
        class="btn ghost icon sm"
        onclick={cycleTheme}
        title="Theme: {app.theme}"
        aria-label="Theme: {app.theme}"
      >
        <Icon
          name={app.theme === 'light' ? 'sun' : app.theme === 'dark' ? 'moon' : 'monitor'}
          size={15}
        />
      </button>
    </div>
  </aside>

  <main>
    {@render children()}
  </main>
</div>

<Toasts />

<style>
  .shell {
    display: grid;
    grid-template-columns: var(--sidebar-w) minmax(0, 1fr);
    height: 100vh;
    height: 100dvh;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 14px 10px 10px;
    border-right: 1px solid var(--border);
    background: var(--surface-2);
    min-height: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 6px;
    color: var(--text);
    font-weight: 650;
    font-size: 16px;
    letter-spacing: -0.02em;
    text-decoration: none;
  }
  .switcher {
    position: relative;
  }
  .ds-button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-weight: 550;
    cursor: pointer;
  }
  .ds-button:hover {
    border-color: var(--border-strong);
  }
  .ds-name {
    flex: 1;
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ds-count {
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .ds-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    width: max(100%, 240px);
    z-index: 20;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r);
    box-shadow: var(--shadow-pop);
    padding: 4px;
    display: grid;
  }
  .ds-item {
    display: grid;
    grid-template-columns: 1fr auto;
    grid-template-rows: auto auto;
    column-gap: 8px;
    text-align: left;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .ds-item:hover {
    background: var(--hover);
  }
  .ds-item[aria-selected='true'] {
    background: var(--active);
  }
  .ds-item-name {
    font-weight: 550;
  }
  .ds-item-meta {
    grid-row: 2;
    font-size: var(--fs-xs);
  }
  .ds-item .ds-count {
    grid-row: 1 / 3;
    grid-column: 2;
    align-self: center;
  }
  .ds-empty {
    padding: 8px;
    font-size: var(--fs-sm);
  }
  .ds-manage {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 7px 8px;
    margin-top: 4px;
    border-top: 1px solid var(--border);
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .ds-manage:hover {
    color: var(--text);
    text-decoration: none;
  }
  nav {
    display: grid;
    gap: 1px;
  }
  nav a {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 32px;
    padding: 0 10px;
    border-radius: var(--r);
    color: var(--text-2);
    font-weight: 500;
    text-decoration: none;
  }
  nav a:hover {
    background: var(--hover);
    color: var(--text);
  }
  nav a.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 0 0 1px var(--border);
  }
  nav a.active::before {
    content: '';
    position: absolute;
    left: -10px;
    top: 7px;
    bottom: 7px;
    width: 3px;
    border-radius: 0 2px 2px 0;
    background: var(--spark);
  }
  nav a.active :global(.icon) {
    color: var(--spark-ink);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 2px 0 6px;
    border-top: 1px solid var(--border);
    padding-top: 10px;
  }
  .status {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-3);
  }
  .dot.on {
    background: var(--ok);
    box-shadow: 0 0 0 3px var(--ok-soft);
  }
  .dot.off {
    background: var(--danger);
    box-shadow: 0 0 0 3px var(--danger-soft);
  }
  .shell.bare {
    grid-template-columns: minmax(0, 1fr);
  }
  .shell.bare .sidebar {
    display: none;
  }
  main {
    min-width: 0;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
  }

  @media (max-width: 760px) {
    /* minmax(0, …): a bare 1fr track grows to its widest item's min-content width (the
       nav links in a row), and the whole page then scrolls sideways */
    .shell {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: auto minmax(0, 1fr);
    }
    .sidebar {
      min-width: 0;
      flex-direction: row;
      flex-wrap: wrap;
      align-items: center;
      gap: 8px;
      padding: 8px 12px;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    .switcher {
      flex: 1;
      min-width: 0;
    }
    /* the switcher sits mid-row: open its menu leftwards, within the screen */
    .ds-menu {
      left: auto;
      right: 0;
      width: max(100%, min(240px, calc(100vw - 24px)));
    }
    nav {
      display: flex;
      order: 3;
      width: 100%;
      overflow-x: auto;
    }
    nav a.active::before {
      display: none;
    }
    nav a {
      flex: none;
    }
    .sidebar > .spacer {
      display: none;
    }
    .foot {
      border-top: 0;
      padding: 0;
    }
    .status span:not(.dot) {
      display: none;
    }
  }
  /* a phone: text-only nav links, so that all of them fit on one row */
  @media (max-width: 480px) {
    nav a {
      padding: 0 6px;
    }
    nav a :global(.icon) {
      display: none;
    }
  }
</style>
