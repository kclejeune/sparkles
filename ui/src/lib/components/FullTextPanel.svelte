<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { onBranch } from '$lib/branches';
  import { toasts } from '$lib/app.svelte';
  import { fmtBytes, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import { parsePredicateList, TEXT_PREDICATES } from '$lib/textsearch';
  import { poll } from '$lib/poll';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    name,
    branch = null,
    prefixes,
    predicates = [],
    readOnly = false,
    busy = false,
    refreshKey = 0,
    onstart,
    onstarted,
    onchanged,
  }: {
    name: string;
    /** The branch to work on (null: `main`). */
    branch?: string | null;
    prefixes: PrefixMap;
    /** The dataset's predicates (most used first), offered when configuring. */
    predicates?: string[];
    readOnly?: boolean;
    /** Another action is starting; disables the task buttons. */
    busy?: boolean;
    /** Bump to reload the status (a task finished, data changed). */
    refreshKey?: number;
    /** Start a task with a toast (the page's task runner). */
    onstart: (label: string, fn: () => Promise<api.Task>) => Promise<void>;
    /** A task was started from the configuration dialog. */
    onstarted: (t: api.Task) => void;
    /** Something changed on the server: refresh the dataset. */
    onchanged: () => void;
  } = $props();

  const target = $derived(onBranch(name, branch));

  type Loaded =
    | { kind: 'enabled'; status: api.TextStatus }
    | { kind: 'disabled' }
    | { kind: 'unsupported' };

  let loaded = $state<Loaded | null>(null);
  let error = $state<string | null>(null);
  let now = $state(Date.now());
  const runs = new LatestRun();

  async function load() {
    const owns = runs.claim('text');
    try {
      const s = await api.textStatus(target);
      if (!owns()) return;
      loaded = s ? { kind: 'enabled', status: s } : { kind: 'disabled' };
      error = null;
    } catch (e) {
      if (!owns()) return;
      if (e instanceof api.ApiError && e.status === 501) loaded = { kind: 'unsupported' };
      else error = api.errorMessage(e);
    }
  }

  $effect(() => {
    void target;
    void refreshKey;
    void load();
  });

  const status = $derived(loaded?.kind === 'enabled' ? loaded.status : null);
  const behind = $derived(!!status && status.seq !== status.storeSeq);

  let panel = $state<HTMLElement>();

  onMount(() => {
    // relative times only: no request
    const t = setInterval(() => (now = Date.now()), 2000);
    // follow an index that is being rebuilt, while the panel is on screen
    const p = poll(load, {
      interval: 2000,
      when: () => !!status && (status.state !== 'ready' || behind),
      target: () => panel,
      immediate: false,
    });
    return () => {
      clearInterval(t);
      p.stop();
    };
  });

  const stateClass = (s: api.TextState) =>
    s === 'ready' ? 'ok' : s === 'failed' ? 'danger' : 'warn';

  async function rebuild() {
    await onstart('Full-text rebuild', () => api.rebuildText(target));
    void load();
  }

  // --- configure (enable / reconfigure) -------------------------------------------
  let configOpen = $state(false);
  let scope = $state<'all' | 'some'>('all');
  let predText = $state('');
  let saving = $state(false);
  let configError = $state<string | null>(null);

  function openConfig() {
    const cur = status?.config.predicates;
    scope = Array.isArray(cur) ? 'some' : 'all';
    predText = Array.isArray(cur) ? cur.map((p) => displayIri(p, prefixes)).join('\n') : '';
    configError = null;
    configOpen = true;
  }

  const parsed = $derived(parsePredicateList(predText, prefixes));
  const suggestions = $derived.by(() => {
    const known = new Set(predicates);
    const first = TEXT_PREDICATES.filter((p) => known.has(p));
    const rest = predicates.filter((p) => !first.includes(p));
    return [...first, ...rest].filter((p) => !parsed.iris.includes(p)).slice(0, 14);
  });
  const configValid = $derived(
    scope === 'all' || (parsed.iris.length > 0 && parsed.bad.length === 0),
  );

  function addPredicate(iri: string) {
    const line = displayIri(iri, prefixes);
    predText = predText.trim() ? `${predText.trimEnd()}\n${line}` : line;
  }

  async function saveConfig(e: Event) {
    e.preventDefault();
    if (!configValid) return;
    saving = true;
    configError = null;
    // keep the settings this dialog does not edit
    const config: api.TextConfig = {
      ...(status?.config ?? {}),
      predicates: scope === 'all' ? 'all' : parsed.iris,
    };
    try {
      const t = await api.enableText(target, config);
      toasts.push(
        'info',
        status ? 'Reconfiguring full-text search' : 'Enabling full-text search',
        t?.id ? `Building the index (task ${t.id})` : undefined,
      );
      onstarted(t);
      configOpen = false;
      void load();
    } catch (err) {
      configError = api.errorMessage(err);
    } finally {
      saving = false;
    }
  }

  // --- disable -----------------------------------------------------------------------
  let disableOpen = $state(false);
  let disabling = $state(false);

  async function disable() {
    disabling = true;
    try {
      await api.disableText(target);
      toasts.push('success', 'Full-text search disabled', 'The index was deleted.');
      disableOpen = false;
      onchanged();
      void load();
    } catch (e) {
      toasts.error('Could not disable full-text search', e);
    } finally {
      disabling = false;
    }
  }

  const searchHref = $derived(`${resolve('/explore')}?ds=${encodeURIComponent(name)}&tab=search`);
</script>

<section class="panel" bind:this={panel}>
  <div class="panel-head">
    <h2>Full-text search</h2>
    <span class="spacer"></span>
    {#if status}
      <span
        class="badge {stateClass(status.state)}"
        title={status.message ?? (behind ? 'The index is behind the data' : 'Index is up to date')}
      >
        <Icon name={status.state === 'ready' && !behind ? 'check' : 'alert'} size={12} />
        {status.state === 'ready' && behind ? 'catching up' : status.state}
      </span>
    {:else if loaded?.kind === 'disabled'}
      <span class="badge">off</span>
    {/if}
  </div>
  <div class="panel-body text">
    {#if error && !loaded}
      <div class="error-box">
        <strong>Could not load the full-text status.</strong>
        <span class="muted">{error}</span>
      </div>
    {:else if !loaded}
      <div class="row faint"><span class="spinner"></span> Loading…</div>
    {:else if loaded.kind === 'unsupported'}
      <p class="faint">
        This server was built without full-text search (the <span class="mono">text</span> feature).
      </p>
    {:else if loaded.kind === 'disabled'}
      <p class="faint">
        Full-text search is off for this dataset. Enabling it indexes string and language-tagged
        literals for ranked search with <span class="mono">text:query</span>.
      </p>
      <div class="row">
        <span class="spacer"></span>
        <button
          class="btn primary"
          onclick={openConfig}
          disabled={readOnly}
          title={readOnly ? 'The server is read-only' : undefined}
          ><Icon name="search" size={14} /> Enable…</button
        >
      </div>
    {:else if status}
      <dl class="facts">
        <div>
          <dt>Documents</dt>
          <dd>{fmtInt(status.docs)}</dd>
        </div>
        <div>
          <dt>On disk</dt>
          <dd>{fmtBytes(status.diskBytes)}</dd>
        </div>
        <div>
          <dt>Segments</dt>
          <dd>{fmtInt(status.segments)}</dd>
        </div>
        <div>
          <dt title="The commit the index reflects">At commit</dt>
          <dd>{status.seq}</dd>
        </div>
      </dl>
      {#if behind}
        <p class="warnline">
          <Icon name="alert" size={13} /> The index reflects commit {status.seq}, the data is at
          commit {status.storeSeq}. Text queries answer 503 until it has caught up.
        </p>
      {/if}
      {#if status.message}<p class="faint small">{status.message}</p>{/if}
      <div class="kv">
        <span class="faint">Predicates</span>
        <span>
          {#if !Array.isArray(status.config.predicates)}
            all string literals
          {:else}
            {#each status.config.predicates as p (p)}<span class="chip t-iri mono" title={p}
                >{displayIri(p, prefixes)}</span
              >{/each}
          {/if}
        </span>
        {#if Array.isArray(status.config.graphs?.include) || status.config.graphs?.exclude?.length}
          <span class="faint">Graphs</span>
          <span class="small">
            {Array.isArray(status.config.graphs?.include)
              ? status.config.graphs.include.map((g) => displayIri(g, prefixes)).join(', ')
              : 'all'}{#if status.config.graphs?.exclude?.length}, except {status.config.graphs.exclude
                .map((g) => displayIri(g, prefixes))
                .join(', ')}{/if}
          </span>
        {/if}
        <span class="faint">Last rebuild</span>
        <span>
          {#if status.lastRebuild}
            <span title={fmtTime(status.lastRebuild.at)}
              >{fmtRelative(status.lastRebuild.at, now)}</span
            >
            <span class="faint"
              >· {fmtInt(status.lastRebuild.docs)} documents in {fmtMs(status.lastRebuild.ms)}</span
            >
          {:else}
            <span class="faint">—</span>
          {/if}
        </span>
      </div>
      <div class="row actions">
        <a class="btn sm" href={searchHref}><Icon name="search" size={13} /> Search</a>
        <span class="spacer"></span>
        <button
          class="btn sm"
          onclick={() => (disableOpen = true)}
          disabled={readOnly}
          title="Delete the index"><Icon name="trash" size={13} /> Disable</button
        >
        <button class="btn sm" onclick={openConfig} disabled={readOnly || busy}
          ><Icon name="filter" size={13} /> Configure…</button
        >
        <button
          class="btn sm"
          onclick={rebuild}
          disabled={readOnly || busy}
          title="Rebuild the index from the current data"
          ><Icon name="refresh" size={13} /> Rebuild</button
        >
      </div>
    {/if}
  </div>
</section>

<Modal
  bind:open={configOpen}
  title={status ? 'Configure full-text search' : 'Enable full-text search'}
  width={520}
>
  <form id="text-config" class="form" onsubmit={saveConfig}>
    <div class="scopes" role="radiogroup" aria-label="Indexed predicates">
      <label class="opt" class:sel={scope === 'all'}>
        <input type="radio" bind:group={scope} value="all" />
        <span
          ><strong>All predicates</strong><span class="faint"
            >Every string and language-tagged literal</span
          ></span
        >
      </label>
      <label class="opt" class:sel={scope === 'some'}>
        <input type="radio" bind:group={scope} value="some" />
        <span
          ><strong>Only these predicates</strong><span class="faint"
            >Smaller index; others cannot be searched</span
          ></span
        >
      </label>
    </div>
    {#if scope === 'some'}
      <label class="field">
        Predicates <span class="faint">(one per line: IRIs or prefixed names)</span>
        <textarea
          class="textarea mono"
          rows="5"
          bind:value={predText}
          spellcheck="false"
          placeholder="rdfs:label&#10;dcterms:description"
          aria-invalid={parsed.bad.length > 0}></textarea>
      </label>
      {#if parsed.bad.length}
        <p class="bad">Not an IRI or known prefixed name: {parsed.bad.join(', ')}</p>
      {/if}
      {#if suggestions.length}
        <div class="suggest">
          <span class="faint small">Add:</span>
          {#each suggestions as p (p)}
            <button type="button" class="chip add mono" title={p} onclick={() => addPredicate(p)}
              >+ {displayIri(p, prefixes)}</button
            >
          {/each}
        </div>
      {/if}
    {/if}
    <p class="faint small">
      The index is built by a background task. Text queries answer 503 until it is ready.
    </p>
    {#if configError}<div class="error-box">{configError}</div>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (configOpen = false)}>Cancel</button>
    <button class="btn primary" type="submit" form="text-config" disabled={saving || !configValid}>
      {#if saving}<span class="spinner"></span>{/if}
      {status ? 'Save and rebuild' : 'Enable'}
    </button>
  {/snippet}
</Modal>

<Modal bind:open={disableOpen} title="Disable full-text search?">
  <p>
    This deletes the full-text index of <span class="mono">{name}</span>. Queries using
    <span class="mono">text:query</span> fail until it is enabled and rebuilt again. The data is not changed.
  </p>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (disableOpen = false)}>Cancel</button>
    <button class="btn danger" type="button" onclick={disable} disabled={disabling}>
      {#if disabling}<span class="spinner"></span>{:else}<Icon name="trash" size={13} />{/if}
      Disable and delete index
    </button>
  {/snippet}
</Modal>

<style>
  .text {
    display: grid;
    gap: 10px;
    font-size: var(--fs-sm);
  }
  .text > p {
    margin: 0;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
    margin: 0;
  }
  .facts dt {
    color: var(--text-2);
    font-size: var(--fs-xs);
  }
  .facts dd {
    margin: 2px 0 0;
    font-size: var(--fs-md);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .warnline {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    color: var(--warn);
  }
  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 14px;
    align-items: baseline;
  }
  .chip {
    display: inline-block;
    margin: 0 4px 3px 0;
    padding: 1px 7px;
    border-radius: 10px;
    font-size: 11px;
    background: color-mix(in srgb, var(--iri) 10%, transparent);
  }
  .chip.add {
    border: 0;
    cursor: pointer;
    color: var(--iri);
    font-family: var(--font-mono);
  }
  .chip.add:hover {
    background: color-mix(in srgb, var(--iri) 18%, transparent);
  }
  .small {
    font-size: 11px;
  }
  .actions {
    flex-wrap: wrap;
  }
  a.btn {
    text-decoration: none;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .form p {
    margin: 0;
  }
  .scopes {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .opt {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    cursor: pointer;
    font-size: var(--fs-sm);
  }
  .opt > span {
    display: grid;
    gap: 2px;
  }
  .opt.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
  }
  .opt input {
    margin-top: 2px;
    accent-color: var(--iri);
  }
  .textarea {
    font-size: 12px;
    resize: vertical;
  }
  .bad {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .suggest {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px;
  }
  @media (max-width: 760px) {
    .facts,
    .scopes {
      grid-template-columns: 1fr 1fr;
    }
  }
</style>
