<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { ApiError, errorMessage } from '$lib/api';
  import { auth } from '$lib/auth.svelte';
  import * as authApi from '$lib/auth-api';
  import { ALL_ACCESS, interactive, loginHref, normalizeUserCode, type Scope } from '$lib/auth';
  import AuthCard from '$components/AuthCard.svelte';
  import Icon from '$components/Icon.svelte';
  import TokenScopeForm from '$components/TokenScopeForm.svelte';

  let code = $state(normalizeUserCode(page.url.searchParams.get('code') ?? ''));
  let grant = $state<authApi.DeviceGrant | null>(null);
  let lookupError = $state<string | null>(null);
  let busy = $state(false);
  let outcome = $state<'approved' | 'denied' | null>(null);

  let name = $state('sparkles CLI');
  let expiresIn = $state('30d');
  let scope = $state<Scope>(ALL_ACCESS);

  async function lookup() {
    const c = normalizeUserCode(code);
    if (!c) return;
    busy = true;
    lookupError = null;
    grant = null;
    try {
      grant = await authApi.deviceGrant(c);
      if (grant.label) name = grant.hostname ? `${grant.label} on ${grant.hostname}` : grant.label;
      if (grant.status !== 'pending') lookupError = `This code was already ${grant.status}.`;
    } catch (e) {
      lookupError =
        e instanceof ApiError && e.status === 404
          ? 'No pending sign-in matches this code. Check the code in your terminal.'
          : errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function decide(approve: boolean) {
    if (!grant) return;
    busy = true;
    try {
      if (approve)
        await authApi.approveDevice(grant.userCode, {
          name,
          datasets: scope.datasets,
          server: scope.server,
          expiresIn,
        });
      else await authApi.denyDevice(grant.userCode);
      outcome = approve ? 'approved' : 'denied';
    } catch (e) {
      lookupError = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  onMount(async () => {
    await auth.ensure();
    if (!auth.enabled) return;
    if (!interactive(auth.who)) {
      await goto(loginHref(page.url.pathname + page.url.search), { replaceState: true });
      return;
    }
    if (code) await lookup();
  });
</script>

<svelte:head><title>Sign in a device | Sparkles</title></svelte:head>

<AuthCard title="Sign in a device">
  {#snippet subtitle()}
    {#if auth.who?.principal.name}
      {`Signed in as ${auth.who.principal.displayName ?? auth.who.principal.name}. `}
    {/if}
    Enter the code shown in your terminal.
  {/snippet}

  {#if !auth.enabled && auth.loaded}
    <p class="muted">This server runs without authentication; the CLI needs no login.</p>
  {:else if outcome === 'approved'}
    <div class="done">
      <Icon name="check" size={18} />
      <div>
        <strong>Device authorized</strong><br /><span class="muted">Return to your terminal.</span>
      </div>
    </div>
  {:else if outcome === 'denied'}
    <p class="muted">The sign-in was denied. You can close this tab.</p>
  {:else if !grant || grant.status !== 'pending'}
    <form
      class="form"
      onsubmit={(e) => {
        e.preventDefault();
        void lookup();
      }}
    >
      <label class="field"
        >Device code
        <input
          class="input code"
          placeholder="XXXX-XXXX"
          autocomplete="off"
          spellcheck="false"
          bind:value={code}
        />
      </label>
      <button class="btn primary" disabled={busy || !code.trim()}>Continue</button>
    </form>
    {#if lookupError}<p class="err" role="alert">{lookupError}</p>{/if}
  {:else}
    <div class="client">
      <Icon name="terminal" size={16} />
      <div>
        <strong>{grant.label || 'A command line client'}</strong>
        {#if grant.hostname}<span class="muted"> on {grant.hostname}</span>{/if}
        <div class="faint mono small">{grant.userCode}</div>
      </div>
    </div>
    <TokenScopeForm who={auth.who} bind:name bind:expiresIn bind:scope />
    {#if lookupError}<p class="err" role="alert">{lookupError}</p>{/if}
    <div class="buttons">
      <button class="btn" onclick={() => decide(false)} disabled={busy}>Deny</button>
      <button class="btn primary" onclick={() => decide(true)} disabled={busy || !name.trim()}>
        Authorize device
      </button>
    </div>
  {/if}
</AuthCard>

<style>
  .form {
    display: grid;
    gap: 10px;
  }
  .form .btn {
    justify-content: center;
    height: 32px;
  }
  .code {
    height: 40px;
    text-align: center;
    font-family: var(--font-mono);
    font-size: 18px;
    letter-spacing: 0.2em;
    text-transform: uppercase;
  }
  .client,
  .done {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .done :global(.icon) {
    color: var(--ok);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .err {
    margin: 0;
    color: var(--danger);
    font-size: var(--fs-sm);
  }
</style>
