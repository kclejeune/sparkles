<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { fmtBytes, fmtCompact, fmtInt, fmtRelative } from '$lib/format';
  import { displayIri } from '$lib/rdf';
  import DatasetDialogs from '$components/DatasetDialogs.svelte';
  import Icon from '$components/Icon.svelte';
  import TaskList from '$components/TaskList.svelte';

  const name = $derived(page.params.name ?? '');
  const info = $derived(app.datasets.find((d) => d.name === name));
  const prefixes = $derived(app.prefixes(name));

  let stats = $state<api.DatasetStats | null>(null);
  let statsError = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let taskKick = $state(0);
  let deleteTarget = $state<string | null>(null);

  async function loadStats() {
    loading = true;
    try {
      stats = await api.datasetStats(name);
      statsError = null;
    } catch (e) {
      statsError = e as Error;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (!name) return;
    stats = null;
    void app.loadPrefixes(name);
    void loadStats();
  });

  function refreshAll() {
    void loadStats();
    void app.refreshDatasets();
    void app.loadPrefixes(name, true);
  }

  // --- actions -----------------------------------------------------------
  let acting = $state<string | null>(null);

  async function startTask(label: string, fn: () => Promise<api.Task>) {
    acting = label;
    try {
      const t = await fn();
      toasts.push('info', `${label} started`, t?.id ? `Task ${t.id}` : undefined);
      taskKick++;
    } catch (e) {
      toasts.error(`${label} failed`, e);
    } finally {
      acting = null;
    }
  }

  // reasoning
  let profile = $state<api.ReasonProfile>('rdfs');
  let rules = $state(`# Jena rule syntax. rdf:, rdfs:, owl: and xsd: are predefined.
# Add a built-in rule set with:  @include <rdfs> .   (or <owl-rl>)
@prefix ex: <http://example.org/> .

# Everyone who authored something is a Researcher.
[author: (?p ex:authorOf ?d) -> (?p rdf:type ex:Researcher)]`);
  let dropping = $state(false);

  async function dropInf() {
    dropping = true;
    try {
      await api.dropInferences(name);
      toasts.push('success', 'Dropped inferred triples');
      refreshAll();
    } catch (e) {
      toasts.error('Could not drop inferences', e);
    } finally {
      dropping = false;
    }
  }

  // upload
  let files = $state<File[]>([]);
  let graph = $state('');
  let dragging = $state(false);
  let uploading = $state(false);
  let progress = $state(0);
  let uploadError = $state<string | null>(null);
  let uploadCtl: AbortController | null = null;
  let fileInput: HTMLInputElement | undefined = $state();

  const ACCEPT = '.ttl,.nt,.nq,.trig,.rdf,.owl,.xml,.jsonld,.n3,.gz';

  function addFiles(list: FileList | null | undefined) {
    if (!list) return;
    const incoming = [...list].filter((f) => !files.some((x) => x.name === f.name && x.size === f.size));
    files = [...files, ...incoming];
  }

  async function doUpload() {
    if (!files.length) return;
    uploading = true;
    uploadError = null;
    progress = 0;
    uploadCtl = new AbortController();
    try {
      const res = (await api.upload(name, files, {
        graph: graph.trim() || undefined,
        onProgress: (p) => (progress = p.total ? p.loaded / p.total : 0),
        signal: uploadCtl.signal,
      })) as { count?: number; tripleCount?: number; quadCount?: number } | string;
      const n = typeof res === 'object' ? (res.quadCount ?? res.tripleCount ?? res.count) : undefined;
      toasts.push('success', `Uploaded ${files.length} file${files.length === 1 ? '' : 's'}`, n != null ? `${fmtInt(n)} quads added` : undefined);
      files = [];
      refreshAll();
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) uploadError = api.errorMessage(e);
    } finally {
      uploading = false;
      uploadCtl = null;
    }
  }

  // --- derived visuals ------------------------------------------------------
  const maxPred = $derived(Math.max(1, ...(stats?.predicates.map((p) => p.count) ?? [1])));
  const maxClass = $derived(Math.max(1, ...(stats?.classes.map((c) => c.instances) ?? [1])));
  const hitRate = $derived(
    stats && stats.cache.hits + stats.cache.misses > 0 ? stats.cache.hits / (stats.cache.hits + stats.cache.misses) : null,
  );
  const delta = $derived.by(() => {
    if (!stats) return null;
    const total = Math.max(stats.baseQuads + stats.deltaInserts, 1);
    return {
      base: ((stats.baseQuads - stats.deltaDeletes) / total) * 100,
      del: (stats.deltaDeletes / total) * 100,
      ins: (stats.deltaInserts / total) * 100,
      dirty: stats.deltaInserts + stats.deltaDeletes > 0,
    };
  });
  let predShown = $state(15);
  let classShown = $state(15);

  function queryPredicate(iri: string) {
    const q = `SELECT ?s ?o\nWHERE {\n  ?s <${iri}> ?o\n}\nLIMIT 100`;
    openInQuery(q);
  }
  function queryClass(iri: string) {
    openInQuery(`SELECT ?s\nWHERE {\n  ?s a <${iri}>\n}\nLIMIT 100`);
  }
  function openInQuery(q: string) {
    app.pendingQuery = { query: q };
    app.setDataset(name);
    goto(resolve('/query'));
  }
  const explore = (iri: string) => `${resolve('/explore')}?ds=${encodeURIComponent(name)}&iri=${encodeURIComponent(iri)}`;
</script>

<svelte:head><title>{name} | Sparkles</title></svelte:head>

<div class="page">
  <nav class="crumbs"><a href={resolve('/datasets')}>Datasets</a><Icon name="chevron" size={12} /><span>{name}</span></nav>

  <header class="head">
    <div class="title">
      <h1 class="mono">{name}</h1>
      <div class="row meta">
        {#if info}<span class="badge">{info.type === 'mem' ? 'in-memory' : 'persistent'}</span>{/if}
        {#if info?.reasoning}
          <span class="badge ok">{info.reasoning.profile} reasoning</span>
        {/if}
        <span class="mono faint">{info?.endpoints?.query ?? `/${name}/sparql`}</span>
      </div>
    </div>
    <span class="spacer"></span>
    <button class="btn" onclick={refreshAll} disabled={loading}>
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={14} />{/if} Refresh
    </button>
    <button
      class="btn"
      onclick={() => {
        app.setDataset(name);
        goto(resolve('/query'));
      }}><Icon name="query" size={14} /> Query</button
    >
    <a class="btn" href={explore('')}><Icon name="explore" size={14} /> Explore</a>
    <button class="btn danger" onclick={() => (deleteTarget = name)}><Icon name="trash" size={14} /> Delete</button>
  </header>

  {#if statsError}
    <div class="error-box">
      <strong>{statsError instanceof api.ApiError && statsError.status === 404 ? `No dataset named “${name}”.` : 'Could not load statistics.'}</strong>
      <span class="muted">{api.errorMessage(statsError)}</span>
    </div>
  {/if}

  {#if stats}
    <!-- headline numbers -->
    <dl class="figures panel">
      <div><dt>Quads</dt><dd>{fmtInt(stats.quads)}</dd></div>
      <div><dt>Terms</dt><dd>{fmtInt(stats.terms)}</dd></div>
      <div><dt>Graphs</dt><dd>{fmtInt(stats.graphs.length)}</dd></div>
      <div><dt>Predicates</dt><dd>{fmtInt(stats.predicates.length)}{stats.predicates.length >= 100 ? '+' : ''}</dd></div>
      <div><dt>Classes</dt><dd>{fmtInt(stats.classes.length)}{stats.classes.length >= 100 ? '+' : ''}</dd></div>
      <div><dt>On disk</dt><dd>{info?.type === 'mem' ? 'in memory' : fmtBytes(stats.diskBytes)}</dd></div>
    </dl>

    <div class="cols">
      <div class="col">
        <!-- storage: base vs delta -->
        <section class="panel">
          <div class="panel-head">
            <h2>Storage</h2>
            <span class="spacer"></span>
            <button class="btn sm" onclick={() => startTask('Compaction', () => api.compact(name))} disabled={acting != null || !delta?.dirty}
              title={delta?.dirty ? 'Merge pending updates into a freshly sorted base index' : 'Nothing to compact'}>
              <Icon name="layers" size={13} /> Compact
            </button>
            <button class="btn sm" onclick={() => startTask('Backup', () => api.backup(name))} disabled={acting != null}>
              <Icon name="archive" size={13} /> Backup
            </button>
          </div>
          <div class="panel-body storage">
            {#if delta}
              <div class="stack" role="img" aria-label="Base {stats.baseQuads} quads, {stats.deltaInserts} inserted, {stats.deltaDeletes} deleted">
                <span class="base" style:width="{delta.base}%"></span>
                <span class="del" style:width="{delta.del}%"></span>
                <span class="ins" style:width="{delta.ins}%"></span>
              </div>
              <div class="stack-legend">
                <span><i class="base"></i>Base index <strong>{fmtInt(stats.baseQuads)}</strong></span>
                <span><i class="ins"></i>Inserted since compaction <strong>+{fmtInt(stats.deltaInserts)}</strong></span>
                <span><i class="del"></i>Deleted <strong>−{fmtInt(stats.deltaDeletes)}</strong></span>
              </div>
              <p class="faint note">
                {delta.dirty
                  ? 'Updates live in a delta alongside the sorted base permutations. Compacting rebuilds the base and clears the delta.'
                  : 'Everything is in the sorted base index. Nothing to compact.'}
              </p>
            {/if}
            <div class="cache">
              <div><span class="faint">Result cache</span> <strong>{fmtInt(stats.cache.entries)}</strong> entries, {fmtBytes(stats.cache.bytes)}</div>
              <div>
                <span class="faint">Hit rate</span>
                <strong>{hitRate == null ? '—' : `${(hitRate * 100).toFixed(1)}%`}</strong>
                <span class="faint">({fmtInt(stats.cache.hits)} hits, {fmtInt(stats.cache.misses)} misses)</span>
              </div>
            </div>
          </div>
        </section>

        <!-- predicates -->
        <section class="panel">
          <div class="panel-head">
            <h2>Top predicates</h2>
            <span class="faint">by triple count</span>
          </div>
          {#if stats.predicates.length === 0}
            <p class="empty">No triples yet. Upload some data to get started.</p>
          {:else}
            <table class="data bars">
              <thead>
                <tr><th>Predicate</th><th class="num">Triples</th><th class="num" title="Distinct subjects">Subj.</th><th class="num" title="Distinct objects">Obj.</th></tr>
              </thead>
              <tbody>
                {#each stats.predicates.slice(0, predShown) as p (p.iri)}
                  <tr>
                    <td class="bar-cell">
                      <span class="bar" style:width="{(p.count / maxPred) * 100}%"></span>
                      <button class="linkish t-iri mono" title="{p.iri}  (click to query)" onclick={() => queryPredicate(p.iri)}>{displayIri(p.iri, prefixes)}</button>
                    </td>
                    <td class="num">{fmtInt(p.count)}</td>
                    <td class="num faint">{fmtCompact(p.distinctSubjects)}</td>
                    <td class="num faint">{fmtCompact(p.distinctObjects)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
            {#if stats.predicates.length > predShown}
              <button class="btn ghost sm more" onclick={() => (predShown += 25)}>Show more ({stats.predicates.length - predShown} hidden)</button>
            {/if}
          {/if}
        </section>

        <!-- classes -->
        <section class="panel">
          <div class="panel-head">
            <h2>Top classes</h2>
            <span class="faint">by rdf:type instances</span>
          </div>
          {#if stats.classes.length === 0}
            <p class="empty">No typed resources.</p>
          {:else}
            <table class="data bars">
              <thead><tr><th>Class</th><th class="num">Instances</th><th></th></tr></thead>
              <tbody>
                {#each stats.classes.slice(0, classShown) as c (c.iri)}
                  <tr>
                    <td class="bar-cell">
                      <span class="bar cls" style:width="{(c.instances / maxClass) * 100}%"></span>
                      <button class="linkish t-iri mono" title="{c.iri}  (click to list instances)" onclick={() => queryClass(c.iri)}>{displayIri(c.iri, prefixes)}</button>
                    </td>
                    <td class="num">{fmtInt(c.instances)}</td>
                    <td class="num"><a class="faint" href={explore(c.iri)} title="Open in Explore"><Icon name="explore" size={13} /></a></td>
                  </tr>
                {/each}
              </tbody>
            </table>
            {#if stats.classes.length > classShown}
              <button class="btn ghost sm more" onclick={() => (classShown += 25)}>Show more ({stats.classes.length - classShown} hidden)</button>
            {/if}
          {/if}
        </section>
      </div>

      <div class="col">
        <!-- upload -->
        <section class="panel">
          <div class="panel-head"><h2>Upload data</h2></div>
          <div class="panel-body upload">
            <div
              class="drop"
              class:over={dragging}
              role="button"
              tabindex="0"
              aria-label="Drop RDF files here or press to choose files"
              ondragover={(e) => {
                e.preventDefault();
                dragging = true;
              }}
              ondragleave={() => (dragging = false)}
              ondrop={(e) => {
                e.preventDefault();
                dragging = false;
                addFiles(e.dataTransfer?.files);
              }}
              onclick={() => fileInput?.click()}
              onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && fileInput?.click()}
            >
              <Icon name="upload" size={20} />
              <span><strong>Drop RDF files</strong> or click to choose</span>
              <span class="faint">Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD. Format comes from the file extension.</span>
            </div>
            <input bind:this={fileInput} type="file" multiple accept={ACCEPT} hidden onchange={(e) => addFiles(e.currentTarget.files)} />
            {#if files.length}
              <ul class="files">
                {#each files as f, i (f.name + f.size)}
                  <li>
                    <span class="mono">{f.name}</span>
                    <span class="faint">{fmtBytes(f.size)}</span>
                    <button class="btn ghost icon sm" aria-label="Remove {f.name}" disabled={uploading} onclick={() => (files = files.filter((_, j) => j !== i))}>
                      <Icon name="x" size={12} />
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
            <label class="field">
              Target graph <span class="faint">(optional; default graph when empty, ignored for quad formats)</span>
              <input class="input mono" bind:value={graph} placeholder="http://example.org/graph/…" />
            </label>
            {#if uploading}
              <div class="progress"><span style:width="{Math.round(progress * 100)}%"></span></div>
              <div class="row faint">
                <span class="spinner"></span>
                {progress < 1 ? `Sending ${Math.round(progress * 100)}%` : 'Parsing and indexing on the server…'}
                <span class="spacer"></span>
                <button class="btn sm" onclick={() => uploadCtl?.abort()}>Cancel</button>
              </div>
            {/if}
            {#if uploadError}<div class="error-box"><strong>Upload failed.</strong> {uploadError}</div>{/if}
            <div class="row">
              <span class="spacer"></span>
              <button class="btn primary" disabled={!files.length || uploading} onclick={doUpload}>
                <Icon name="upload" size={14} /> Upload {files.length ? `${files.length} file${files.length === 1 ? '' : 's'}` : ''}
              </button>
            </div>
          </div>
        </section>

        <!-- reasoning -->
        <section class="panel">
          <div class="panel-head">
            <h2>Reasoning</h2>
            <span class="spacer"></span>
            {#if info?.reasoning}
              <span class="faint">{info.reasoning.profile}, {fmtInt(info.reasoning.inferred)} inferred, {fmtRelative(info.reasoning.at)}</span>
            {/if}
          </div>
          <div class="panel-body reason">
            <div class="profiles" role="radiogroup" aria-label="Reasoning profile">
              {#each [{ v: 'rdfs', l: 'RDFS', d: 'subClassOf, subPropertyOf, domain, range' }, { v: 'owl-rl', l: 'OWL 2 RL', d: 'RDFS plus inverse, symmetric, transitive, sameAs…' }, { v: 'rules', l: 'Custom rules', d: 'Your own rules, Jena rule syntax' }] as const as p (p.v)}
                <label class="opt" class:sel={profile === p.v}>
                  <input type="radio" bind:group={profile} value={p.v} />
                  <span><strong>{p.l}</strong><span class="faint">{p.d}</span></span>
                </label>
              {/each}
            </div>
            {#if profile === 'rules'}
              <textarea class="textarea" rows="7" bind:value={rules} spellcheck="false" aria-label="Custom rules"></textarea>
            {/if}
            <div class="row">
              <button class="btn" onclick={dropInf} disabled={dropping || !info?.reasoning}>
                {#if dropping}<span class="spinner"></span>{:else}<Icon name="trash" size={13} />{/if} Drop inferences
              </button>
              <span class="spacer"></span>
              <button
                class="btn primary"
                disabled={acting != null || (profile === 'rules' && !rules.trim())}
                onclick={() => startTask('Reasoning', () => api.reason(name, profile, rules))}
              >
                <Icon name="wand" size={14} /> Materialize inferences
              </button>
            </div>
          </div>
        </section>

        <!-- graphs -->
        <section class="panel">
          <div class="panel-head"><h2>Graphs</h2><span class="faint">{stats.graphs.length}</span></div>
          <table class="data">
            <thead><tr><th>Graph</th><th class="num">Quads</th></tr></thead>
            <tbody>
              {#each stats.graphs as g (g.name ?? '')}
                <tr>
                  <td class="mono gname" title={g.name ?? 'Default graph'}>
                    {#if g.name == null}<span class="muted">default graph</span>{:else}<span class="t-iri">{displayIri(g.name, prefixes)}</span>{/if}
                  </td>
                  <td class="num">{fmtInt(g.quads)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </section>

        <!-- tasks -->
        <section class="panel">
          <div class="panel-head"><h2>Tasks</h2></div>
          <div class="panel-body">
            <TaskList dataset={name} refreshKey={taskKick} ondone={(t) => {
              if (t.state === 'failed') toasts.push('error', `${t.kind} failed`, t.message);
              else toasts.push('success', `${t.kind} finished`, t.message);
              refreshAll();
            }} />
          </div>
        </section>
      </div>
    </div>
  {:else if !statsError}
    <div class="empty"><span class="spinner"></span> Loading statistics…</div>
  {/if}
</div>

<DatasetDialogs bind:deleteTarget ondeleted={() => goto(resolve('/datasets'))} />

<style>
  .page {
    padding: 18px 28px 40px;
    display: grid;
    gap: 16px;
    max-width: 1320px;
    width: 100%;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .crumbs a {
    color: var(--text-2);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .title {
    display: grid;
    gap: 6px;
  }
  .title h1 {
    font-family: var(--font-mono);
    font-size: 22px;
    letter-spacing: -0.03em;
  }
  .meta {
    font-size: var(--fs-sm);
  }
  a.btn {
    text-decoration: none;
  }
  .figures {
    display: grid;
    grid-template-columns: repeat(6, 1fr);
    margin: 0;
  }
  .figures > div {
    padding: 12px 16px;
    border-right: 1px solid var(--border);
  }
  .figures > div:last-child {
    border-right: 0;
  }
  dt {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  dd {
    margin: 2px 0 0;
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1.25fr) minmax(0, 1fr);
    gap: 16px;
    align-items: start;
  }
  .col {
    display: grid;
    gap: 16px;
    min-width: 0;
  }
  .storage {
    display: grid;
    gap: 10px;
  }
  .stack {
    display: flex;
    height: 12px;
    border-radius: 6px;
    overflow: hidden;
    background: var(--surface-3);
  }
  .stack span {
    min-width: 0;
  }
  .stack .base,
  .stack-legend i.base {
    background: var(--iri);
  }
  .stack .ins,
  .stack-legend i.ins {
    background: var(--ok);
  }
  .stack .del,
  .stack-legend i.del {
    background: var(--danger);
  }
  .stack-legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .stack-legend i {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    margin-right: 6px;
  }
  .stack-legend strong {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    margin-left: 4px;
  }
  .note {
    font-size: var(--fs-sm);
  }
  .cache {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 24px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  table.bars td {
    height: 30px;
  }
  .bar-cell {
    position: relative;
    max-width: 0;
    width: 70%;
  }
  .bar {
    position: absolute;
    left: 6px;
    top: 5px;
    bottom: 5px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--iri) 13%, transparent);
    pointer-events: none;
  }
  .bar.cls {
    background: color-mix(in srgb, var(--literal) 14%, transparent);
  }
  .linkish {
    all: unset;
    position: relative;
    cursor: pointer;
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--iri);
  }
  .linkish:hover {
    text-decoration: underline;
  }
  .linkish:focus-visible {
    box-shadow: var(--focus);
  }
  .more {
    margin: 4px 8px 8px;
  }
  .upload,
  .reason {
    display: grid;
    gap: 12px;
  }
  .drop {
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 20px 12px;
    border: 1.5px dashed var(--border-strong);
    border-radius: var(--r);
    text-align: center;
    color: var(--text-2);
    cursor: pointer;
    font-size: var(--fs-sm);
  }
  .drop:hover,
  .drop.over {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
    color: var(--text);
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .files li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 3px 4px 3px 8px;
    border-radius: 4px;
    background: var(--surface-2);
    font-size: var(--fs-sm);
  }
  .files li .mono {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .progress {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--spark);
    transition: width 0.2s;
  }
  .profiles {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
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
  .gname {
    max-width: 0;
    width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  @media (max-width: 1100px) {
    .cols {
      grid-template-columns: 1fr;
    }
    .figures {
      grid-template-columns: repeat(3, 1fr);
    }
    .figures > div:nth-child(3) {
      border-right: 0;
    }
    .figures > div:nth-child(-n + 3) {
      border-bottom: 1px solid var(--border);
    }
  }
  @media (max-width: 760px) {
    .page {
      padding: 14px 16px 32px;
    }
    .profiles {
      grid-template-columns: 1fr;
    }
    .figures {
      grid-template-columns: repeat(2, 1fr);
    }
    .figures > div {
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    .figures > div:nth-child(odd) {
      border-right: 1px solid var(--border);
    }
    .figures > div:nth-last-child(-n + 2) {
      border-bottom: 0;
    }
  }
</style>
