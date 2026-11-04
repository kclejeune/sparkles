<script lang="ts">
  // The signed-in user: their name, how they signed in, API tokens and sign out. In the
  // top bar it is a button with a menu; in the phone drawer the same links sit inline.
  // With auth off it renders nothing.
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { displayName, loginHref, methodBadge } from '$lib/auth';
  import Icon from './Icon.svelte';

  let { variant = 'bar' }: { variant?: 'bar' | 'drawer' } = $props();

  const who = $derived(auth.who);
  const badge = $derived(methodBadge(who));
  const anonymous = $derived(who?.principal.kind === 'anonymous');
  const name = $derived(displayName(who));
  const initials = $derived(
    name
      .split(/[\s._@-]+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((w) => w[0]?.toUpperCase())
      .join('') || '?',
  );

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();
  let button: HTMLButtonElement | undefined = $state();

  async function signOut() {
    open = false;
    try {
      await auth.logout();
    } catch (e) {
      toasts.error('Could not sign out', e);
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

{#snippet links()}
  {#if who?.canMintTokens}
    <a
      class="link"
      role={variant === 'bar' ? 'menuitem' : undefined}
      href={resolve('/admin/tokens')}><Icon name="key" size={14} /> API tokens</a
    >
  {/if}
  {#if who?.logout}
    <button class="link" role={variant === 'bar' ? 'menuitem' : undefined} onclick={signOut}
      ><Icon name="logout" size={14} /> Sign out</button
    >
  {/if}
{/snippet}

{#if who?.authEnabled}
  {#if anonymous}
    <a class="btn sm signin" href={loginHref(page.url.pathname + page.url.search)}>
      <Icon name="user" size={13} /> Sign in
    </a>
  {:else if variant === 'drawer'}
    <div class="user drawer">
      <div class="who" title={who.principal.owner ?? `${who.principal.kind}:${who.principal.name}`}>
        <span class="avatar" aria-hidden="true">{initials}</span>
        <span class="name">{name}</span>
        {#if badge}<span class="badge">{badge}</span>{/if}
      </div>
      <div class="links">{@render links()}</div>
    </div>
  {:else}
    <div
      class="user bar"
      bind:this={root}
      role="presentation"
      onkeydown={(e) => {
        if (e.key === 'Escape' && open) {
          e.stopPropagation();
          open = false;
          button?.focus();
        }
      }}
    >
      <button
        bind:this={button}
        class="avatar-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Account: {name}"
        title={name}
        onclick={() => (open = !open)}><span class="avatar">{initials}</span></button
      >
      {#if open}
        <div class="menu" role="menu" aria-label="Account">
          <div class="who" role="presentation">
            <span class="name">{name}</span>
            {#if badge}<span class="badge">{badge}</span>{/if}
          </div>
          {@render links()}
        </div>
      {/if}
    </div>
  {/if}
{/if}

<style>
  .user.bar {
    position: relative;
  }
  .avatar-btn {
    display: grid;
    place-items: center;
    width: var(--ctl-h);
    height: var(--ctl-h);
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: none;
    cursor: pointer;
  }
  .avatar {
    display: grid;
    place-items: center;
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--surface-3);
    color: var(--text-2);
    font-size: var(--fs-xs);
    font-weight: 650;
  }
  .avatar-btn:hover .avatar {
    background: var(--border);
    color: var(--text);
  }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 40;
    min-width: 200px;
    display: grid;
    padding: var(--sp-1);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
  }
  .menu .who {
    padding: var(--sp-2);
    margin-bottom: var(--sp-1);
    border-bottom: 1px solid var(--border);
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    font-weight: 600;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    flex: none;
  }
  .link {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 7px var(--sp-2);
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text-2);
    font: inherit;
    text-align: left;
    cursor: pointer;
    text-decoration: none;
  }
  .link:hover {
    background: var(--hover);
    color: var(--text);
    text-decoration: none;
  }
  .drawer {
    display: grid;
    gap: var(--sp-1);
  }
  .drawer .links {
    display: grid;
  }
  .signin {
    text-decoration: none;
  }
</style>
