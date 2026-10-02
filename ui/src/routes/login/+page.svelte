<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { errorMessage } from '$lib/api';
  import { auth } from '$lib/auth.svelte';
  import * as authApi from '$lib/auth-api';
  import { loginErrorMessage, safeReturnTo, signedIn } from '$lib/auth';
  import AuthCard from '$components/AuthCard.svelte';
  import Icon from '$components/Icon.svelte';

  const returnTo = $derived(safeReturnTo(page.url.searchParams.get('return_to')));
  const urlError = $derived(loginErrorMessage(page.url.searchParams.get('error')));
  const methods = $derived(auth.config?.methods ?? []);

  let token = $state('');
  let user = $state('');
  let password = $state('');
  let busy = $state(false);
  let formError = $state<string | null>(null);

  const ssoHref = $derived(
    `${auth.config?.oidc?.loginUrl ?? '/$/auth/oidc/login'}?return_to=${encodeURIComponent(returnTo)}`,
  );

  async function done() {
    await auth.load();
    // a full navigation, so every page reloads its data as the new principal
    window.location.assign(returnTo);
  }

  async function submit(body: { token: string } | { user: string; password: string }) {
    busy = true;
    formError = null;
    try {
      await authApi.login(body);
      password = '';
      token = '';
      await done();
    } catch (e) {
      formError = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  onMount(async () => {
    await auth.ensure();
    if (!auth.enabled) await goto(returnTo, { replaceState: true });
    else if (signedIn(auth.who) && !page.url.searchParams.get('error'))
      await goto(returnTo, { replaceState: true });
  });
</script>

<svelte:head><title>Sign in | Sparkles</title></svelte:head>

<AuthCard title="Sign in">
  {#snippet subtitle()}
    to use the datasets of this server
  {/snippet}

  {#if urlError}<div class="error-box" role="alert">{urlError}</div>{/if}

  {#if !auth.loaded}
    <p class="faint">Loading…</p>
  {:else}
    {#if methods.includes('oidc')}
      <a class="btn primary big" href={ssoHref} data-sveltekit-reload>
        <Icon name="user" size={15} /> Sign in with {auth.config?.oidc?.displayName ??
          'single sign-on'}
      </a>
    {/if}

    {#if methods.includes('password')}
      {#if methods.includes('oidc')}<div class="or faint"><span>or</span></div>{/if}
      <form
        class="form"
        onsubmit={(e) => {
          e.preventDefault();
          void submit({ user, password });
        }}
      >
        <label class="field"
          >User
          <input class="input" autocomplete="username" bind:value={user} required />
        </label>
        <label class="field"
          >Password
          <input
            class="input"
            type="password"
            autocomplete="current-password"
            bind:value={password}
            required
          />
        </label>
        <button class="btn" disabled={busy || !user || !password}>Sign in</button>
      </form>
    {/if}

    {#if methods.includes('token')}
      {#if methods.includes('oidc') || methods.includes('password')}
        <div class="or faint"><span>or use an API token</span></div>
      {/if}
      <form
        class="form"
        onsubmit={(e) => {
          e.preventDefault();
          void submit({ token: token.trim() });
        }}
      >
        <label class="field"
          >API token
          <input
            class="input mono"
            type="password"
            placeholder="spk_…"
            autocomplete="off"
            spellcheck="false"
            bind:value={token}
            required
          />
        </label>
        <button class="btn" disabled={busy || !token.trim()}>
          <Icon name="key" size={14} /> Sign in with token
        </button>
      </form>
    {/if}

    {#if formError}<p class="err" role="alert">{formError}</p>{/if}
    {#if methods.length === 1 && methods[0] === 'proxy'}
      <p class="muted">This server signs you in through its proxy. Reload the page to retry.</p>
    {/if}
  {/if}
</AuthCard>

<style>
  .big {
    height: 36px;
    justify-content: center;
    text-decoration: none;
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
  }
  .form .btn {
    justify-content: center;
    height: 32px;
  }
  .or {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: var(--fs-xs);
  }
  .or::before,
  .or::after {
    content: '';
    flex: 1;
    border-top: 1px solid var(--border);
  }
  .err {
    margin: 0;
    color: var(--danger);
    font-size: var(--fs-sm);
  }
</style>
