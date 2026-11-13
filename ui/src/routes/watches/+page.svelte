<script lang="ts">
  // The caller's watches on entities (ROADMAP item 619, U12 "Watches"): each watch with its
  // unread count, and for the open one its matched commits, newest first, each member linking
  // to the entity's History view in the Ontology editor. Mark read raises the watch's `seen`
  // to its newest match, and Remove ends the watch.
  import PageActions from '$components/PageActions.svelte';
  import Icon from '$components/Icon.svelte';
  import { errorMessage } from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtRelative } from '$lib/format';
  import { routes } from '$lib/routes';
  import { MAIN } from '$lib/branches';
  import { watchCount } from '$lib/watch-count.svelte';
  import {
    deleteWatch,
    listWatches,
    markSeen,
    matchText,
    scopeLabel,
    watchMatches,
    type Watch,
    type WatchList,
    type WatchMatch,
  } from '$lib/watches';

  let list = $state<WatchList | null>(null);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let openId = $state<string | null>(null);
  let matches = $state<WatchMatch[]>([]);
  let more = $state(false);
  let matchesError = $state<string | null>(null);

  async function refresh() {
    loading = true;
    try {
      list = await listWatches();
      loadError = null;
      void watchCount.refresh();
    } catch (e) {
      loadError = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  /** Counts the loads of matches, so only the latest one shows. */
  let generation = 0;
  let paging = $state(false);

  async function show(w: Watch, before?: number) {
    const mine = ++generation;
    if (!before) {
      openId = w.id;
      matches = [];
      more = false;
    }
    matchesError = null;
    paging = true;
    try {
      const page = await watchMatches(w.id, { before, limit: 50 });
      if (mine !== generation) return;
      matches = before ? [...matches, ...page.matches] : page.matches;
      more = page.more;
    } catch (e) {
      if (mine === generation) matchesError = errorMessage(e);
    } finally {
      if (mine === generation) paging = false;
    }
  }

  async function read(w: Watch) {
    if (w.latest == null) return;
    try {
      await markSeen(w.id, w.latest);
      await refresh();
      if (openId === w.id) await show(w);
    } catch (e) {
      toasts.error('Could not mark the watch read', e);
    }
  }

  async function remove(w: Watch) {
    try {
      await deleteWatch(w.id);
      if (openId === w.id) openId = null;
      await refresh();
    } catch (e) {
      toasts.error('Could not remove the watch', e);
    }
  }

  const history = (w: Watch, iri: string) =>
    routes.ontology(w.dataset, {
      iri,
      graph: w.graph ?? null,
      branch: w.branch === MAIN ? null : w.branch,
      view: 'history',
    });

  $effect(() => {
    void refresh();
  });
</script>

<svelte:head><title>Watches | Sparkles</title></svelte:head>

<div class="page-container">
  <header class="page-head">
    <div>
      <h1>Watches</h1>
      <p class="muted">
        The entities you watch, and the commits that changed them. Watch an entity from its frame in
        the Ontology editor.
      </p>
    </div>
    <PageActions>
      <button class="btn" onclick={refresh} disabled={loading}>
        <Icon name="refresh" size={14} /> Refresh
      </button>
    </PageActions>
  </header>

  {#if loadError}
    <div class="error-box"><strong>Could not load your watches.</strong> {loadError}</div>
  {/if}
  {#if list?.mail}
    <p class="faint small mail">
      {#if list.mail.address}
        Changes are also mailed to {list.mail.address}.
      {:else if list.mail.channels && list.mail.why}
        Changes are shown here only: {list.mail.why}.
      {/if}
    </p>
  {/if}

  <section class="panel">
    <div class="scroll-x">
      <table class="data" aria-label="Your watches">
        <thead>
          <tr>
            <th>Entity</th>
            <th>Dataset</th>
            <th>Scope</th>
            <th>Unread</th>
            <th>Since</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each list?.watches ?? [] as w (w.id)}
            <tr class:open={openId === w.id}>
              <td>
                <button class="link mono" onclick={() => show(w)} aria-expanded={openId === w.id}
                  >{w.entity}</button
                >
                {#if w.graph}<div class="faint small mono">{w.graph}</div>{/if}
                {#if w.truncated}<span
                    class="badge"
                    title="The graph's hierarchy is too large to follow, so only the entity itself is matched"
                    >entity only</span
                  >{/if}
                {#if w.overflow}<span class="badge" title="Only the newest 200 matches are kept"
                    >200+</span
                  >{/if}
              </td>
              <td>{w.dataset}{w.branch !== MAIN ? `@${w.branch}` : ''}</td>
              <td>{scopeLabel(w)}</td>
              <td
                >{#if w.unread}<span class="badge unread">{w.unread}</span>{:else}<span
                    class="faint">0</span
                  >{/if}</td
              >
              <td class="small">{fmtRelative(w.created)}</td>
              <td class="actions">
                <button class="btn sm" disabled={!w.unread} onclick={() => read(w)}
                  >Mark read</button
                >
                <button class="btn sm danger" onclick={() => remove(w)}>Remove</button>
              </td>
            </tr>
            {#if openId === w.id}
              <tr class="matches">
                <td colspan="6">
                  {#if matchesError}
                    <div class="error-box">{matchesError}</div>
                  {:else if !matches.length && paging}
                    <p class="faint">Loading…</p>
                  {:else if !matches.length}
                    <p class="faint">No commit has changed it since you started watching.</p>
                  {:else}
                    <ol class="commits" aria-label="Matched commits">
                      {#each matches as m (m.commit)}
                        <li class:unread={m.unread}>
                          <div class="line">
                            <span class="mono">#{m.commit}</span>
                            <span class="small faint">{fmtRelative(m.time)}</span>
                            {#if m.author}<span class="small">{m.author}</span>{/if}
                            {#if m.own}<span class="badge">yours</span>{/if}
                            {#if m.unread}<span class="badge unread">new</span>{/if}
                            {#if !m.exact}<span class="badge">unverified</span>{/if}
                            {#if m.partial}<span
                                class="badge"
                                title="The ancestor walk stopped at its bound, so members may be missing"
                                >partial</span
                              >{/if}
                          </div>
                          {#if (m.members ?? []).length}
                            <div class="members">
                              {#each m.members ?? [] as iri (iri)}
                                <a class="mono small" href={history(w, iri)}>{iri}</a>
                              {/each}
                            </div>
                          {/if}
                          {#if matchText(m)}<div class="small faint">{matchText(m)}</div>{/if}
                        </li>
                      {/each}
                    </ol>
                    {#if more}
                      <button
                        class="btn sm"
                        disabled={paging}
                        onclick={() => show(w, matches.at(-1)?.commit)}>Older commits</button
                      >
                    {/if}
                  {/if}
                </td>
              </tr>
            {/if}
          {:else}
            <tr>
              <td colspan="6" class="faint">
                {loading ? 'Loading…' : 'You watch no entities.'}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
</div>

<style>
  .small {
    font-size: var(--fs-sm);
  }
  .mail {
    margin: 0 0 var(--sp-2);
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    color: var(--link, var(--accent));
    cursor: pointer;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .actions {
    white-space: nowrap;
    text-align: right;
  }
  tr.open td {
    border-bottom: 0;
  }
  .commits {
    list-style: none;
    margin: 0 0 var(--sp-2);
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .commits li {
    padding-left: var(--sp-2);
    border-left: 2px solid var(--border);
  }
  .commits li.unread {
    border-left-color: var(--accent);
  }
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .members {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-3);
  }
  .members a {
    overflow-wrap: anywhere;
  }
</style>
