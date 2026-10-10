<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { auth } from '$lib/auth.svelte';
  import * as b from '$lib/backups';
  import ActivityTab from '$components/ActivityTab.svelte';
  import BackupsTable from '$components/BackupsTable.svelte';
  import Icon from '$components/Icon.svelte';
  import PoliciesTab from '$components/PoliciesTab.svelte';
  import RepositoriesTab from '$components/RepositoriesTab.svelte';
  import { followTasks } from '$lib/tasks.svelte';

  type Tab = 'backups' | 'repositories' | 'policies' | 'activity';

  let repositories = $state<(b.Repository | b.RepositoryBrief)[]>([]);
  let loaded = $state(false);
  let error = $state<api.ApiError | Error | null>(null);
  let readOnly = $state(false);
  let policyNames = $state<string[]>([]);
  /** Bumped when a backup task finishes or data changes: tabs reload. */
  let kick = $state(0);

  const admin = $derived(auth.hasServer('server-admin'));
  const urlTab = $derived((page.url.searchParams.get('tab') ?? 'backups') as Tab);
  const tabs = $derived<{ id: Tab; label: string; icon: string }[]>([
    { id: 'backups', label: 'Backups', icon: 'archive' },
    { id: 'repositories', label: 'Repositories', icon: 'database' },
    ...(admin ? [{ id: 'policies' as const, label: 'Policies', icon: 'clock' }] : []),
    { id: 'activity', label: 'Activity', icon: 'cycle' },
  ]);
  const tab = $derived(tabs.some((t) => t.id === urlTab) ? urlTab : 'backups');
  let dataset = $state(page.url.searchParams.get('dataset') ?? '');

  // unsupported: the server predates backup repositories
  const unsupported = $derived(
    error instanceof api.ApiError && (error.status === 404 || error.status === 501),
  );

  async function loadRepositories() {
    try {
      repositories = await b.listRepositories();
      error = null;
      // repositories and policies share one namespace; the count shows on the tab
      if (admin)
        b.listPolicies().then(
          (p) => (policyNames = p.map((x) => x.name)),
          () => {},
        );
    } catch (e) {
      error = e as Error;
    } finally {
      loaded = true;
    }
  }

  function reload() {
    kick++;
    void loadRepositories();
  }

  function setTab(t: Tab) {
    const p = new URLSearchParams(page.url.searchParams);
    if (t === 'backups') p.delete('tab');
    else p.set('tab', t);
    const qs = p.toString();
    goto(`${resolve('/backups')}${qs ? `?${qs}` : ''}`, {
      replaceState: true,
      keepFocus: true,
      noScroll: true,
    });
  }

  // keep the dataset filter in the URL, so the dataset page can link here
  $effect(() => {
    const cur = page.url.searchParams.get('dataset') ?? '';
    if (dataset === cur) return;
    const p = new URLSearchParams(page.url.searchParams);
    if (dataset) p.set('dataset', dataset);
    else p.delete('dataset');
    goto(`${resolve('/backups')}?${p}`, { replaceState: true, keepFocus: true, noScroll: true });
  });

  function onTabKey(e: KeyboardEvent, i: number) {
    const n = tabs.length;
    const to =
      e.key === 'ArrowRight' ? (i + 1) % n : e.key === 'ArrowLeft' ? (i + n - 1) % n : null;
    if (to == null) return;
    e.preventDefault();
    setTab(tabs[to].id);
    document.getElementById(`tab-${tabs[to].id}`)?.focus();
  }

  onMount(() => {
    void auth.ensure().then(loadRepositories);
    api.cachedServerInfo().then(
      (s) => (readOnly = s?.readOnly === true),
      () => {},
    );
    // Repositories, policies and backups change through this page or through backup
    // tasks (scheduled policy runs included), so there is no timer of their own: the
    // page reloads when a backup task finishes, which the shared task poller reports.
    return followTasks(reload, b.isBackupTask);
  });

  const reachableCount = $derived(repositories.filter(b.reachable).length);
</script>

<svelte:head><title>Backups | Sparkles</title></svelte:head>

<div class="page page-container">
  <header class="head">
    <div>
      <h1>Backups</h1>
      <p class="muted">
        {#if !loaded}Loading…{:else if unsupported}Not available on this server{:else}{repositories.length}
          repositor{repositories.length === 1 ? 'y' : 'ies'}{repositories.length !== reachableCount
            ? `, ${repositories.length - reachableCount} unreachable`
            : ''}{admin ? '' : ' · the backups of the datasets you administer'}{/if}
      </p>
    </div>
    <span class="spacer"></span>
    <button class="btn" onclick={reload}><Icon name="refresh" size={14} /> Refresh</button>
  </header>

  {#if unsupported}
    <div class="empty panel">
      <Icon name="archive" size={24} />
      <p>This server has no backup repositories.</p>
      <p class="faint small">
        It predates them, or was built without the backup feature. <a href={resolve('/datasets')}
          >Datasets</a
        > still offer the N-Quads dump.
      </p>
    </div>
  {:else}
    {#if error}
      <div class="error-box">
        <strong>Could not load repositories.</strong>
        {api.errorMessage(error)}
      </div>
    {/if}

    <div class="tabs" role="tablist" aria-label="Backups">
      {#each tabs as t, i (t.id)}
        <button
          class="tab"
          role="tab"
          id="tab-{t.id}"
          aria-selected={tab === t.id}
          aria-controls="panel-{t.id}"
          tabindex={tab === t.id ? 0 : -1}
          onclick={() => setTab(t.id)}
          onkeydown={(e) => onTabKey(e, i)}
        >
          <Icon name={t.icon} size={14} />
          {t.label}
          {#if t.id === 'repositories' && loaded}<span class="count">{repositories.length}</span
            >{/if}
          {#if t.id === 'policies' && policyNames.length}<span class="count"
              >{policyNames.length}</span
            >{/if}
        </button>
      {/each}
    </div>

    <div class="tabpanel" role="tabpanel" id="panel-{tab}" aria-labelledby="tab-{tab}">
      {#if tab === 'backups'}
        {#if loaded}
          <BackupsTable {repositories} {admin} {readOnly} bind:dataset refreshKey={kick} />
        {/if}
      {:else if tab === 'repositories'}
        <RepositoriesTab {repositories} {admin} {readOnly} {policyNames} onchanged={reload} />
      {:else if tab === 'policies'}
        <PoliciesTab
          repositories={repositories.filter(b.isFull)}
          {readOnly}
          refreshKey={kick}
          onloaded={(p) => (policyNames = p.map((x) => x.name))}
          onstarted={() => kick++}
        />
      {:else}
        <ActivityTab {admin} {repositories} refreshKey={kick} />
      {/if}
    </div>
  {/if}
</div>

<style>
  .page {
    padding: 24px 28px 40px;
    gap: 16px;
  }
  .head {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    flex-wrap: wrap;
  }
  .head p {
    margin-top: 4px;
  }
  .tabs {
    border-bottom: 1px solid var(--border);
  }
  .tabpanel {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
    align-content: start;
  }
  .small {
    font-size: var(--fs-sm);
  }
  @media (max-width: 760px) {
    .page {
      padding: 16px;
    }
  }
</style>
