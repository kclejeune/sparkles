<script lang="ts">
  // The pages of the shell, in two groups split by a rule: the rail's links at large and
  // medium widths (icons only at medium, each with its name as a tooltip and accessible
  // name), and the drawer's on a phone.
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { reviewCounts } from '$lib/review-count.svelte';
  import { navActive, navItems, navLink } from '$lib/shell';
  import Icon from './Icon.svelte';
  import ReviewBadge from './ReviewBadge.svelte';

  let { variant = 'rail', onnavigate }: { variant?: 'rail' | 'drawer'; onnavigate?: () => void } =
    $props();

  const base = resolve('/').replace(/\/$/, '');
  // Backups: server admins, and dataset admins in a reduced form
  const backupsVisible = $derived(
    auth.hasServer('server-admin') || Object.values(auth.who?.datasets ?? {}).includes('admin'),
  );
  const items = $derived(navItems(backupsVisible));
  const review = $derived(reviewCounts.of(app.current));
</script>

<nav class="nav {variant}" aria-label="Pages">
  {#each items as item, i (item.href)}
    {@const active = navActive(base, item.href, page.url.pathname)}
    {#if i > 0 && item.group !== items[i - 1].group}<div
        class="rule"
        role="presentation"
      ></div>{/if}
    <a
      href={navLink(base, item.href, app.current)}
      class:active
      aria-current={active ? 'page' : undefined}
      title={variant === 'rail' ? item.label : undefined}
      onclick={() => onnavigate?.()}
    >
      <Icon name={item.icon} size={16} />
      <span class="label">{item.label}</span>
      {#if item.href === '/memory'}<ReviewBadge counts={review} />{/if}
    </a>
  {/each}
</nav>

<style>
  .nav {
    display: grid;
    gap: 2px;
    align-content: start;
  }
  .rule {
    height: 1px;
    margin: var(--sp-2) var(--sp-2);
    background: var(--border);
  }
  a {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 34px;
    padding: 0 10px;
    border-radius: var(--r);
    color: var(--text-2);
    font-weight: 500;
    text-decoration: none;
  }
  a:hover {
    background: var(--hover);
    color: var(--text);
  }
  a.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 0 0 1px var(--border);
  }
  /* the active link's outline gives way to the focus ring */
  a:focus-visible {
    box-shadow: var(--focus);
  }
  a.active::before {
    content: '';
    position: absolute;
    left: calc(-1 * var(--sp-2));
    top: 8px;
    bottom: 8px;
    width: 3px;
    border-radius: 0 2px 2px 0;
    background: var(--spark);
  }
  a.active :global(.icon) {
    color: var(--spark-ink);
  }
  a :global(.review-badge) {
    margin-left: auto;
  }
  @media (max-width: 1199px) {
    .rail a {
      justify-content: center;
      padding: 0;
      height: 38px;
    }
    .rail .label {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip: rect(0 0 0 0);
      white-space: nowrap;
    }
    /* the count shrinks to a dot on the icon; its accessible name keeps the count */
    .rail a :global(.review-badge) {
      position: absolute;
      top: 6px;
      right: 8px;
      width: 8px;
      min-width: 0;
      height: 8px;
      padding: 0;
      margin: 0;
      font-size: 0;
      background: var(--spark);
    }
    .rail .rule {
      margin: var(--sp-2) 6px;
    }
  }
</style>
