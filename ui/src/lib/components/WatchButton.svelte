<script lang="ts">
  // The watch button of the entity frame (ROADMAP item 619, U12 "Watches"). It shows whether
  // the caller watches this entity on this branch and graph, and opens a dialog to watch the
  // entity alone or with its asserted subtree, to an optional depth, or to stop watching.
  // The server decides who may watch: an anonymous caller or a static token gets 403
  // `watch-needs-owner`, and the button then hides.
  import { ApiError } from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { routes } from '$lib/routes';
  import {
    createWatch,
    deleteWatch,
    findWatch,
    listWatches,
    parseDepth,
    scopeLabel,
    type Watch,
    type WatchMail,
    type WatchScope,
  } from '$lib/watches';
  import { watchCount } from '$lib/watch-count.svelte';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    dataset,
    branch,
    graph,
    entity,
  }: { dataset: string; branch: string; graph: string | null; entity: string } = $props();

  let mine = $state<Watch[]>([]);
  let mail = $state<WatchMail | undefined>();
  /** false once the server says the caller cannot watch */
  let can = $state(true);
  let open = $state(false);
  let scope = $state<WatchScope>('entity');
  let depth = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function load(signal?: AbortSignal) {
    try {
      const l = await listWatches({ dataset, entity, graph }, signal);
      mine = findWatch(l.watches, { dataset, branch, graph, entity });
      mail = l.mail;
      can = true;
    } catch (e) {
      if (signal?.aborted) return;
      // no owner, or a server without watches: no button
      if (e instanceof ApiError && [403, 404, 405].includes(e.status)) can = false;
      mine = [];
    }
  }

  $effect(() => {
    const c = new AbortController();
    void [dataset, branch, graph, entity];
    void load(c.signal);
    return () => c.abort();
  });

  const parsed = $derived(scope === 'subtree' ? parseDepth(depth) : undefined);

  async function watch() {
    if (parsed === 'invalid') return;
    busy = true;
    error = null;
    try {
      await createWatch({ dataset, branch, graph, entity, scope, depth: parsed });
      await load();
      void watchCount.refresh();
      toasts.push(
        'success',
        scope === 'entity' ? 'Watching this entity' : 'Watching this entity and its subtree',
      );
      open = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function unwatch(w: Watch) {
    busy = true;
    error = null;
    try {
      await deleteWatch(w.id);
      await load();
      void watchCount.refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if can}
  <button
    class="btn sm ghost watch"
    aria-pressed={mine.length > 0}
    title={mine.length
      ? `Watching: ${mine.map(scopeLabel).join(', ')}`
      : 'Be told of commits that change this entity'}
    onclick={() => {
      error = null;
      open = true;
    }}
    ><Icon name="eye" size={13} />
    {mine.length ? 'Watching' : 'Watch'}{#if mine.some((w) => w.unread)}<span class="badge unread"
        >{mine.reduce((n, w) => n + w.unread, 0)}</span
      >{/if}</button
  >
{/if}

<Modal bind:open title="Watch this entity" width={460}>
  <div class="watch-dialog">
    <p class="faint mono iri">{entity}</p>
    {#if mine.length}
      <ul class="current" aria-label="Your watches on this entity">
        {#each mine as w (w.id)}
          <li>
            <span>{scopeLabel(w)}</span>
            {#if w.unread}<span class="badge">{w.unread} unread</span>{/if}
            <span class="spacer"></span>
            <button class="btn sm ghost" disabled={busy} onclick={() => unwatch(w)}
              >Stop watching</button
            >
          </li>
        {/each}
      </ul>
    {/if}
    <fieldset>
      <legend>Watch</legend>
      <label
        ><input type="radio" name="watch-scope" value="entity" bind:group={scope} /> This entity</label
      >
      <label
        ><input type="radio" name="watch-scope" value="subtree" bind:group={scope} /> Entity and subtree</label
      >
      {#if scope === 'subtree'}
        <label class="depth"
          >Levels below it
          <input
            type="text"
            inputmode="numeric"
            placeholder="all"
            bind:value={depth}
            aria-invalid={parsed === 'invalid'}
          /></label
        >
        {#if parsed === 'invalid'}<p class="error">
            A whole number from 1 to 1000, or empty for all.
          </p>{/if}
        <p class="faint">
          The subtree follows rdfs:subClassOf, rdfs:subPropertyOf, skos:broader and skos:narrower as
          stated in this graph.
        </p>
      {/if}
    </fieldset>
    <p class="faint">
      {#if mail?.address}
        Matches show under <a href={routes.watches()}>Watches</a> and are also mailed to {mail.address}.
      {:else}
        Matches show under <a href={routes.watches()}>Watches</a>.{#if mail?.channels && mail.why}
          They are not mailed: {mail.why}.{/if}
      {/if}
    </p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
  {#snippet actions()}
    <button class="btn" onclick={() => (open = false)}>Close</button>
    <button class="btn primary" disabled={busy || parsed === 'invalid'} onclick={watch}
      >Watch</button
    >
  {/snippet}
</Modal>

<style>
  .watch .unread {
    margin-left: var(--sp-1);
  }
  .watch-dialog {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .iri {
    overflow-wrap: anywhere;
    margin: 0;
  }
  .current {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .current li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--r);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
  }
  .depth {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .depth input {
    width: 6em;
  }
  .error {
    color: var(--danger);
    margin: 0;
  }
  p {
    margin: 0;
  }
</style>
