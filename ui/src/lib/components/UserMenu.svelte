<script lang="ts">
  // The sidebar's signed-in user: name, how they signed in, API tokens, sign out.
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { displayName, loginHref, methodBadge } from '$lib/auth';
  import Icon from './Icon.svelte';

  const who = $derived(auth.who);
  const badge = $derived(methodBadge(who));
  const anonymous = $derived(who?.principal.kind === 'anonymous');

  async function signOut() {
    try {
      await auth.logout();
    } catch (e) {
      toasts.error('Sign out failed', e);
    }
  }
</script>

{#if who?.authEnabled}
  <div class="user">
    {#if anonymous}
      <a class="btn sm signin" href={loginHref(page.url.pathname + page.url.search)}>
        <Icon name="user" size={13} /> Sign in
      </a>
    {:else}
      <div class="who" title={who.principal.owner ?? `${who.principal.kind}:${who.principal.name}`}>
        <Icon name="user" size={14} />
        <span class="name">{displayName(who)}</span>
        {#if badge}<span class="badge">{badge}</span>{/if}
      </div>
      <div class="links">
        {#if who.canMintTokens}
          <a class="link" href={resolve('/tokens')}><Icon name="key" size={13} /> API tokens</a>
        {/if}
        {#if who.logout}
          <button class="link" onclick={signOut}><Icon name="logout" size={13} /> Sign out</button>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .user {
    display: grid;
    gap: 6px;
    padding: 8px 6px 0;
    border-top: 1px solid var(--border);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    font-size: var(--fs-sm);
    font-weight: 550;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    flex: none;
    font-size: var(--fs-xs);
  }
  .links {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-2);
    font: inherit;
    font-size: var(--fs-sm);
    cursor: pointer;
    text-decoration: none;
  }
  .link:hover {
    color: var(--text);
  }
  .signin {
    justify-content: center;
    text-decoration: none;
  }
  /* the sidebar is a top bar: the user gets a row of their own under the nav */
  @media (max-width: 760px) {
    .user {
      order: 4;
      width: 100%;
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 4px 12px;
      padding: 6px 0 0;
    }
    .signin {
      width: auto;
    }
  }
</style>
