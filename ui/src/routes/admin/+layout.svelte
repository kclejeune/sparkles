<script lang="ts">
  // The Admin page (ROADMAP item 633): Server, Backups and Tokens as tabs at /admin/server,
  // /admin/backups and /admin/tokens. Each tab is its own route, so a link, a reload and
  // Back land on it; the tab's own head below names what it shows and holds its actions.
  import type { Snippet } from 'svelte';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { auth } from '$lib/auth.svelte';
  import { adminTabs } from '$lib/shell';
  import Icon from '$components/Icon.svelte';

  let { children }: { children: Snippet } = $props();

  const base = resolve('/').replace(/\/$/, '');
  // Backups: server admins, and dataset admins in a reduced form
  const tabs = $derived(
    adminTabs(
      auth.hasServer('server-admin') || Object.values(auth.who?.datasets ?? {}).includes('admin'),
    ),
  );
</script>

<div class="admin">
  <header class="admin-head">
    <h1>Admin</h1>
    <nav class="tabs" aria-label="Admin">
      {#each tabs as t (t.href)}
        {@const on = page.url.pathname.startsWith(base + t.href)}
        <a
          class="tab"
          href={base + t.href}
          aria-current={on ? 'page' : undefined}
          data-sveltekit-noscroll><Icon name={t.icon} size={13} />{t.label}</a
        >
      {/each}
    </nav>
  </header>
  {@render children()}
</div>

<style>
  .admin {
    display: flex;
    flex-direction: column;
    flex: 1 0 auto;
    min-width: 0;
  }
  .admin-head {
    display: grid;
    gap: var(--sp-3);
    padding: var(--gutter) var(--gutter) 0;
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .admin-head h1 {
    margin: 0;
  }
  .tab {
    text-decoration: none;
  }
  /* the tab's own page sits under the strip */
  .admin :global(.page-container) {
    padding-top: var(--sp-4);
  }
</style>
