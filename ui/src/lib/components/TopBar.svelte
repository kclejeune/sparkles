<script lang="ts">
  // The shell's top bar (docs/visualization/README.md, "Web UI"): the brand, the scope and
  // search field, the connection status, the theme and the account. On a phone the status,
  // theme, account, dataset switcher and pages move into a drawer that the menu button opens.
  import { afterNavigate } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { app } from '$lib/app.svelte';
  import DatasetMenu from './DatasetMenu.svelte';
  import Icon from './Icon.svelte';
  import NavLinks from './NavLinks.svelte';
  import ScopeSearch from './ScopeSearch.svelte';
  import UserMenu from './UserMenu.svelte';

  let drawer: HTMLDialogElement | undefined = $state();
  /** The drawer's contents exist only while it is open, so the page has one switcher. */
  let drawerOpen = $state(false);
  let menuButton: HTMLButtonElement | undefined = $state();

  const status = $derived(
    app.online === null
      ? 'Connecting…'
      : app.online
        ? `Connected${app.lastPingMs != null ? `, ${app.lastPingMs.toFixed(0)} ms` : ''}`
        : 'Offline: the server does not answer',
  );
  const themeName = $derived(
    app.theme === 'system'
      ? 'Theme: system'
      : app.theme === 'light'
        ? 'Theme: light'
        : 'Theme: dark',
  );

  function cycleTheme() {
    app.setTheme(app.theme === 'system' ? 'light' : app.theme === 'light' ? 'dark' : 'system');
  }

  function closeDrawer() {
    if (drawer?.open) drawer.close();
  }

  afterNavigate(() => closeDrawer());
</script>

{#snippet statusDot()}
  <span
    class="dot"
    class:on={app.online === true}
    class:off={app.online === false}
    role="img"
    aria-label={status}
    title={status}
  ></span>
{/snippet}

{#snippet themeButton()}
  <button class="btn ghost icon" onclick={cycleTheme} title={themeName} aria-label={themeName}>
    <Icon
      name={app.theme === 'light' ? 'sun' : app.theme === 'dark' ? 'moon' : 'monitor'}
      size={16}
    />
  </button>
{/snippet}

<header class="topbar">
  <a class="brand" href={resolve('/query')} aria-label="Sparkles home">
    <svg viewBox="0 0 32 32" width="22" height="22" aria-hidden="true">
      <path d="M16 3l2.9 8.1L27 14l-8.1 2.9L16 25l-2.9-8.1L5 14l8.1-2.9z" fill="var(--spark)" />
      <circle cx="25.5" cy="6.5" r="2.2" fill="var(--spark)" />
      <circle cx="7" cy="25" r="1.6" fill="var(--spark)" opacity="0.6" />
    </svg>
    <span class="name">Sparkles</span>
  </a>

  <ScopeSearch />

  <div class="end">
    <span class="wide">{@render statusDot()}</span>
    <span class="wide">{@render themeButton()}</span>
    <span class="wide"><UserMenu /></span>
    <button
      bind:this={menuButton}
      class="btn ghost icon menu-button"
      aria-label="Menu"
      aria-haspopup="dialog"
      onclick={() => {
        drawerOpen = true;
        drawer?.showModal();
      }}><Icon name="menu" size={18} /></button
    >
  </div>
</header>

<dialog
  class="drawer"
  bind:this={drawer}
  aria-label="Menu"
  onclick={(e) => e.target === drawer && closeDrawer()}
  onclose={() => {
    drawerOpen = false;
    menuButton?.focus();
  }}
>
  {#if drawerOpen}
    <div class="drawer-body">
      <div class="drawer-head">
        <span class="brand"
          ><svg viewBox="0 0 32 32" width="20" height="20" aria-hidden="true"
            ><path
              d="M16 3l2.9 8.1L27 14l-8.1 2.9L16 25l-2.9-8.1L5 14l8.1-2.9z"
              fill="var(--spark)"
            /></svg
          ><span class="name">Sparkles</span></span
        >
        <button class="btn ghost icon" aria-label="Close menu" onclick={closeDrawer}
          ><Icon name="x" size={18} /></button
        >
      </div>
      <DatasetMenu variant="drawer" />
      <NavLinks variant="drawer" onnavigate={closeDrawer} />
      <div class="drawer-foot">
        <UserMenu variant="drawer" />
        <div class="row">
          {@render statusDot()}<span class="status-text">{status}</span>
          <span class="spacer"></span>
          {@render themeButton()}
        </div>
      </div>
    </div>
  {/if}
</dialog>

<style>
  /* Three columns: the brand, the search and the end. The two outer columns share the
     free space equally, so the search sits in the middle of the viewport; a side that needs
     more room than its share (a long user name at medium width) pushes the search over
     rather than crowding it. */
  .topbar {
    grid-area: top;
    display: grid;
    grid-template-columns:
      minmax(max-content, 1fr) minmax(0, var(--search-w))
      minmax(max-content, 1fr);
    align-items: center;
    column-gap: var(--gutter);
    height: var(--topbar-h);
    /* the same inset on both sides, so the middle column is the viewport's middle */
    padding: 0 var(--sp-3);
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
    width: calc(var(--rail-w) - var(--sp-4));
    margin-left: calc(var(--sp-4) - var(--sp-3));
    padding: var(--sp-1) 0;
    color: var(--text);
    font-weight: 650;
    font-size: 15px;
    letter-spacing: -0.02em;
    text-decoration: none;
  }
  .brand:hover {
    text-decoration: none;
  }
  .end {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    margin-left: auto;
    flex: none;
  }
  .wide {
    display: contents;
  }
  .dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin: 0 var(--sp-2);
    border-radius: 50%;
    background: var(--text-3);
    flex: none;
  }
  .dot.on {
    background: var(--ok);
    box-shadow: 0 0 0 3px var(--ok-soft);
  }
  .dot.off {
    background: var(--danger);
    box-shadow: 0 0 0 3px var(--danger-soft);
  }
  .menu-button {
    display: none;
  }

  @media (max-width: 1199px) {
    .brand {
      justify-content: center;
      width: var(--rail-w);
      margin-left: calc(-1 * var(--sp-3));
    }
    .brand .name {
      display: none;
    }
  }
  @media (max-width: 760px) {
    /* a phone: the brand, then the search and menu buttons together at the end */
    .topbar {
      display: flex;
      gap: var(--sp-1);
      padding: 0 var(--sp-2) 0 0;
    }
    .topbar > :global(.scope-search) {
      margin-left: auto;
    }
    .end {
      margin-left: 0;
    }
    .brand {
      width: auto;
      margin-left: var(--sp-4);
      justify-content: flex-start;
    }
    .topbar {
      padding-left: 0;
    }
    .brand .name {
      display: inline;
    }
    .wide {
      display: none;
    }
    .menu-button {
      display: inline-flex;
    }
  }

  .drawer {
    margin: 0 auto 0 0;
    width: min(320px, 86vw);
    max-width: none;
    height: 100dvh;
    max-height: none;
    padding: 0;
    border: 0;
    border-right: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    box-shadow: var(--shadow-overlay);
  }
  .drawer::backdrop {
    background: var(--scrim);
  }
  .drawer-body {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-height: 100%;
    padding: var(--sp-2) var(--sp-3) var(--sp-4);
  }
  .drawer-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 40px;
  }
  .drawer-head .brand {
    width: auto;
    margin-left: var(--sp-1);
  }
  .drawer-head .brand .name {
    display: inline;
  }
  .drawer-foot {
    margin-top: auto;
    display: grid;
    gap: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }
  .drawer-foot .dot {
    margin-left: var(--sp-1);
  }
  .status-text {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  @media (prefers-reduced-motion: no-preference) {
    .drawer[open] {
      animation: slide-in 160ms ease-out;
    }
  }
  @keyframes slide-in {
    from {
      transform: translateX(-24px);
      opacity: 0;
    }
  }
</style>
