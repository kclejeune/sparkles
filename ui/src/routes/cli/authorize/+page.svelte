<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { errorMessage } from '$lib/api';
  import { auth } from '$lib/auth.svelte';
  import * as authApi from '$lib/auth-api';
  import {
    ALL_ACCESS,
    deniedCallback,
    interactive,
    loginHref,
    parseCliAuthorize,
    type Scope,
  } from '$lib/auth';
  import AuthCard from '$components/AuthCard.svelte';
  import Icon from '$components/Icon.svelte';
  import TokenScopeForm from '$components/TokenScopeForm.svelte';

  const req = $derived(parseCliAuthorize(page.url.searchParams));
  let name = $state('sparkles CLI');
  let expiresIn = $state('30d');
  let scope = $state<Scope>(ALL_ACCESS);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let done = $state(false);

  async function approve() {
    if (!req) return;
    busy = true;
    error = null;
    try {
      const r = await authApi.authorizeCli({
        ...req,
        name,
        datasets: scope.datasets,
        server: scope.server,
        expiresIn,
      });
      done = true;
      // the CLI's own listener on 127.0.0.1 receives the one-time code
      window.location.assign(r.redirect);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function deny() {
    if (req) window.location.assign(deniedCallback(req));
  }

  onMount(async () => {
    await auth.ensure();
    if (req) name = req.hostname ? `${req.label} on ${req.hostname}` : req.label;
    if (auth.enabled && !interactive(auth.who))
      await goto(loginHref(page.url.pathname + page.url.search), { replaceState: true });
  });
</script>

<svelte:head><title>Authorize the CLI | Sparkles</title></svelte:head>

<AuthCard title="Authorize the Sparkles CLI">
  {#snippet subtitle()}
    {#if req}
      A token for <span class="mono">{req.hostname || 'this machine'}</span> will be sent to the CLI
      waiting on <span class="mono">127.0.0.1:{req.port}</span>.
    {/if}
  {/snippet}

  {#if !req}
    <div class="error-box">
      This authorization link is incomplete or invalid. Run
      <span class="mono">sparkles auth login</span> again.
    </div>
  {:else if !auth.enabled && auth.loaded}
    <p class="muted">This server runs without authentication; the CLI needs no login.</p>
  {:else if done}
    <div class="done">
      <Icon name="check" size={18} />
      <div>
        <strong>Authorized</strong><br /><span class="muted">Return to your terminal.</span>
      </div>
    </div>
  {:else}
    <TokenScopeForm who={auth.who} bind:name bind:expiresIn bind:scope />
    {#if error}<p class="err" role="alert">{error}</p>{/if}
    <div class="buttons">
      <button class="btn" onclick={deny} disabled={busy}>Deny</button>
      <button class="btn primary" onclick={approve} disabled={busy || !name.trim()}>
        Authorize CLI
      </button>
    </div>
    <p class="faint small">You can revoke it anytime from API tokens.</p>
  {/if}
</AuthCard>

<style>
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
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .err {
    margin: 0;
    color: var(--danger);
    font-size: var(--fs-sm);
  }
</style>
