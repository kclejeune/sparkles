<script lang="ts">
  import { onMount } from 'svelte';
  import { errorMessage } from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import * as authApi from '$lib/auth-api';
  import { ALL_ACCESS, fmtExpires, scopeSummary, type Scope } from '$lib/auth';
  import { fmtRelative } from '$lib/format';
  import Icon from '$components/Icon.svelte';
  import Modal from '$components/Modal.svelte';
  import TokenScopeForm from '$components/TokenScopeForm.svelte';

  let tokens = $state<authApi.TokenInfo[]>([]);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let showAll = $state(false);

  let createOpen = $state(false);
  let name = $state('token');
  let expiresIn = $state('30d');
  let scope = $state<Scope>(ALL_ACCESS);
  let creating = $state(false);
  let created = $state<(authApi.TokenInfo & { token: string }) | null>(null);
  let copied = $state(false);

  const admin = $derived(auth.hasServer('server-admin'));

  async function refresh() {
    loading = true;
    try {
      tokens = await authApi.listTokens(showAll && admin);
      loadError = null;
    } catch (e) {
      loadError = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  async function create() {
    creating = true;
    try {
      created = await authApi.mintToken({
        name,
        datasets: scope.datasets,
        server: scope.server,
        expiresIn,
      });
      createOpen = false;
      copied = false;
      await refresh();
    } catch (e) {
      toasts.error('Could not create the token', e);
    } finally {
      creating = false;
    }
  }

  async function revoke(t: authApi.TokenInfo) {
    if (!confirm(`Revoke the token “${t.name}” (${t.id})? Programs using it will stop working.`))
      return;
    try {
      await authApi.revokeToken(t.id);
      toasts.push('success', `Token ${t.name} revoked`);
      await refresh();
    } catch (e) {
      toasts.error('Could not revoke the token', e);
    }
  }

  async function copy() {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created.token);
      copied = true;
    } catch {
      copied = false;
    }
  }

  const via = (v?: string) =>
    ({ api: 'API', ui: 'web UI', 'cli-loopback': 'CLI (browser)', 'cli-device': 'CLI (device)' })[
      v ?? ''
    ] ?? v;

  onMount(async () => {
    await auth.ensure();
    await refresh();
  });
</script>

<svelte:head><title>API tokens | Sparkles</title></svelte:head>

<div class="page">
  <header class="head">
    <div>
      <h1>API tokens</h1>
      <p class="muted">
        Tokens let scripts and the <span class="mono">sparkles</span> CLI act for you. A token never exceeds
        your own access, now or later.
      </p>
    </div>
    <span class="spacer"></span>
    {#if admin}
      <label class="check small">
        <input type="checkbox" bind:checked={showAll} onchange={refresh} /> All tokens
      </label>
    {/if}
    <button class="btn" onclick={refresh} disabled={loading}>
      <Icon name="refresh" size={14} /> Refresh
    </button>
    {#if auth.who?.canMintTokens}
      <button class="btn primary" onclick={() => (createOpen = true)}>
        <Icon name="plus" size={14} /> New token
      </button>
    {/if}
  </header>

  {#if !auth.enabled && auth.loaded}
    <div class="error-box">This server runs without authentication: there are no tokens.</div>
  {:else if loadError}
    <div class="error-box"><strong>Could not load tokens.</strong> {loadError}</div>
  {/if}

  <section class="panel">
    <div class="scroll-x">
      <table class="data">
        <thead>
          <tr>
            <th>Name</th>
            <th>Id</th>
            <th>Scope</th>
            <th>Created</th>
            <th>Expires</th>
            <th>Last used</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each tokens as t (t.id)}
            <tr>
              <td>
                <strong>{t.name}</strong>
                {#if t.static}<span class="badge">static</span>{/if}
                {#if t.client?.hostname}<div class="faint small">
                    {via(t.via)} · {t.client.hostname}
                  </div>{:else if t.via}<div class="faint small">{via(t.via)}</div>{/if}
                {#if showAll && t.owner}<div class="faint small">{t.owner}</div>{/if}
              </td>
              <td class="mono small">{t.id}</td>
              <td class="small">{t.static ? t.grants : scopeSummary(t.scope ?? {})}</td>
              <td class="small">{t.created ? fmtRelative(t.created) : '—'}</td>
              <td class="small" title={t.expires ?? ''}>{fmtExpires(t.expires)}</td>
              <td class="small">{t.lastUsed ? fmtRelative(t.lastUsed) : 'never'}</td>
              <td class="actions">
                {#if !t.static}
                  <button class="btn sm danger" onclick={() => revoke(t)}>Revoke</button>
                {/if}
              </td>
            </tr>
          {:else}
            <tr>
              <td colspan="7" class="faint">
                {loading ? 'Loading…' : 'No tokens yet.'}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
</div>

<Modal bind:open={createOpen} title="New API token" width={480}>
  <TokenScopeForm who={auth.who} bind:name bind:expiresIn bind:scope />
  {#snippet actions()}
    <button class="btn" onclick={() => (createOpen = false)}>Cancel</button>
    <button class="btn primary" onclick={create} disabled={creating || !name.trim()}>
      Create token
    </button>
  {/snippet}
</Modal>

<Modal open={created !== null} title="Your new token" width={520} onclose={() => (created = null)}>
  {#if created}
    <p class="muted small">
      Copy it now: <strong>you won't see it again.</strong> Use it as
      <span class="mono">Authorization: Bearer …</span> or with
      <span class="mono">sparkles auth login --token</span>.
    </p>
    <div class="secret">
      <code class="mono">{created.token}</code>
      <button class="btn sm" onclick={copy}>
        <Icon name={copied ? 'check' : 'copy'} size={13} />
        {copied ? 'Copied' : 'Copy'}
      </button>
    </div>
    <p class="faint small">{created.id} · expires {created.expires}</p>
  {/if}
  {#snippet actions()}
    <button class="btn primary" onclick={() => (created = null)}>Done</button>
  {/snippet}
</Modal>

<style>
  .page {
    padding: 20px 24px 40px;
    display: grid;
    gap: 16px;
    align-content: start;
    max-width: 1100px;
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0 0 4px;
    font-size: 20px;
  }
  .head p {
    margin: 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 28px;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .scroll-x {
    overflow-x: auto;
  }
  .actions {
    text-align: right;
  }
  .secret {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .secret code {
    flex: 1;
    overflow-wrap: anywhere;
    user-select: all;
  }
</style>
