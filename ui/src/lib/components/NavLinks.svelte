<script lang="ts">
  // The pages of the shell, in two groups split by a rule: the rail's links at large and
  // medium widths (icons only at medium, each with its name as a tooltip and accessible
  // name), and the drawer's on a phone. A merged area is a group whose pages show under it
  // when it opens (ROADMAP item 633): open while one of its pages is current, and in a
  // flyout beside the icon rail at medium width.
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { reviewCounts } from '$lib/review-count.svelte';
  import { navActive, navItems, navLink, type NavItem, type NavLink } from '$lib/shell';
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

  const isActive = (l: NavLink) =>
    navActive(base, l.match ?? l.href, page.url.pathname, l.exact ?? false);
  const groupActive = (it: NavItem) => isActive(it) || (it.children ?? []).some((c) => isActive(c));

  /** The groups the viewer opened or closed by hand; otherwise a group is open while current. */
  let toggled = $state<Record<string, boolean>>({});
  const isOpen = (it: NavItem) => toggled[it.href] ?? groupActive(it);

  // the flyout of a group on the icon rail, placed beside its icon
  let fly = $state<{ href: string; top: number; left: number } | null>(null);
  let flyTimer: ReturnType<typeof setTimeout> | undefined;
  const iconRail = () => variant === 'rail' && window.matchMedia('(max-width: 1199px)').matches;
  function showFly(it: NavItem, el: HTMLElement) {
    if (!it.children || !iconRail()) return;
    clearTimeout(flyTimer);
    const r = el.getBoundingClientRect();
    fly = { href: it.href, top: r.top, left: r.right + 6 };
  }
  function hideFly() {
    clearTimeout(flyTimer);
    flyTimer = setTimeout(() => (fly = null), 150);
  }
</script>

<nav class="nav {variant}" aria-label="Pages">
  {#each items as item, i (item.href)}
    {@const active = groupActive(item)}
    {#if i > 0 && item.group !== items[i - 1].group}<div
        class="rule"
        role="presentation"
      ></div>{/if}
    {#if item.children}
      {@const open = isOpen(item)}
      <div
        class="group"
        role="presentation"
        onmouseenter={(e) => showFly(item, e.currentTarget)}
        onmouseleave={hideFly}
        onfocusin={(e) => showFly(item, e.currentTarget)}
        onfocusout={hideFly}
      >
        <div class="row">
          <a
            href={navLink(base, item.href, app.current)}
            class="parent"
            class:active
            title={variant === 'rail' ? item.label : undefined}
            onclick={() => onnavigate?.()}
          >
            <Icon name={item.icon} size={16} />
            <span class="label">{item.label}</span>
          </a>
          <button
            class="chev"
            aria-expanded={open}
            aria-label="{open ? 'Close' : 'Open'} {item.label}"
            onclick={() => (toggled = { ...toggled, [item.href]: !open })}
            ><Icon name="chevron" size={12} /></button
          >
        </div>
        {#if open}
          <ul class="children" aria-label={item.label}>
            {#each item.children as c (c.href)}
              {@const on = isActive(c)}
              <li>
                <a
                  href={navLink(base, c.href, app.current)}
                  class:active={on}
                  aria-current={on ? 'page' : undefined}
                  onclick={() => onnavigate?.()}><span class="label">{c.label}</span></a
                >
              </li>
            {/each}
          </ul>
        {/if}
        {#if fly?.href === item.href}
          <ul
            class="flyout"
            aria-label={item.label}
            style:top="{fly.top}px"
            style:left="{fly.left}px"
          >
            <li class="fly-title">{item.label}</li>
            {#each item.children as c (c.href)}
              {@const on = isActive(c)}
              <li>
                <a
                  href={navLink(base, c.href, app.current)}
                  class:active={on}
                  aria-current={on ? 'page' : undefined}
                  onclick={() => ((fly = null), onnavigate?.())}
                  ><Icon name={c.icon} size={14} /><span>{c.label}</span></a
                >
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    {:else}
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
    {/if}
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
  /* a group: its link and the button that opens it, then its pages */
  .row {
    position: relative;
    display: flex;
    align-items: center;
  }
  .row a {
    flex: 1 1 auto;
    min-width: 0;
    padding-right: 32px;
  }
  /* on the full rail the current page is marked, not its group */
  .rail .row a.parent.active,
  .drawer .row a.parent.active {
    background: none;
    box-shadow: none;
  }
  .rail .row a.parent.active::before,
  .drawer .row a.parent.active::before {
    display: none;
  }
  .chev {
    position: absolute;
    right: 4px;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-3);
    cursor: pointer;
  }
  .chev :global(.icon) {
    transform: rotate(-90deg);
    transition: transform 0.12s;
  }
  .chev[aria-expanded='true'] :global(.icon) {
    transform: none;
  }
  .chev:hover {
    background: var(--hover);
    color: var(--text);
  }
  .chev:focus-visible {
    box-shadow: var(--focus);
  }
  .children {
    display: grid;
    gap: 2px;
    margin: 2px 0 4px;
    padding: 0 0 0 26px;
    list-style: none;
  }
  .children a {
    height: 30px;
    font-size: var(--fs);
  }
  .flyout {
    display: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .chev :global(.icon) {
      transition: none;
    }
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
    /* on the icon rail a group's pages are in its flyout, and the group's icon is marked */
    .rail .chev,
    .rail .children {
      display: none;
    }
    .rail .row a.parent.active {
      background: var(--surface);
      box-shadow: 0 0 0 1px var(--border);
    }
    .rail .row a.parent.active::before {
      display: block;
    }
    .rail .flyout {
      position: fixed;
      z-index: 40;
      display: grid;
      gap: 2px;
      min-width: 160px;
      margin: 0;
      padding: var(--sp-1);
      list-style: none;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: var(--r-lg);
      box-shadow: var(--shadow-overlay);
    }
    .rail .flyout a {
      justify-content: flex-start;
      height: 32px;
      padding: 0 10px;
      gap: 8px;
    }
    .rail .flyout a.active::before {
      display: none;
    }
    .fly-title {
      padding: 4px 10px 2px;
      font-size: var(--fs-xs);
      font-weight: 600;
      color: var(--text-3);
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
