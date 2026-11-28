<script lang="ts">
  import { resolve } from '$app/paths';
  import PageActions from '$components/PageActions.svelte';
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';

  type Pin = { resource: string; version: string };
  type Version = { version: string; distributions?: { url: string; sha256?: string }[] };
  type Resource = {
    id: string;
    title: string;
    kind: 'source' | 'vocabulary' | 'dataset';
    license?: string;
    versions: Version[];
  };
  type Input = { source: Pin; graph: string; files?: string };
  type Manifest = { by: string; plan: { files: { graph: string; url: string }[] } };
  type Loaded = { receipt: { commit: { seq: number } }; manifest: Manifest };

  let resources = $state<Resource[]>([]);
  let source = $state('');
  let version = $state('');
  let graph = $state('');
  let files = $state('');
  let inputs = $state<Input[]>([]);
  let dataset = $state('');
  let create = $state(false);
  let name = $state('');
  let commit = $state<number | undefined>(undefined);
  let manifest = $state<Manifest | null>(null);
  let busy = $state(false);
  let error = $state('');
  let status = $state('');
  let adding = $state(false);
  let sourceId = $state('');
  let title = $state('');
  let sourceVersion = $state('');
  let url = $state('');
  let digest = $state('');
  let license = $state('');
  let bundleId = $state('');
  let bundleVersion = $state('');
  let active = $state<AbortController | null>(null);

  const admin = $derived(auth.hasServer('server-admin'));
  const sources = $derived(resources.filter((item) => item.kind === 'source'));
  const vocabularies = $derived(resources.filter((item) => item.kind === 'vocabulary'));
  const versions = $derived(sources.find((item) => item.id === source)?.versions ?? []);
  const bundleVersions = $derived(
    vocabularies.find((item) => item.id === bundleId)?.versions ?? [],
  );
  const enc = encodeURIComponent;

  async function run(work: () => Promise<void>) {
    busy = true;
    error = '';
    status = '';
    try {
      await work();
    } catch (err) {
      if (err instanceof DOMException && err.name === 'AbortError') status = 'Request cancelled.';
      else error = api.errorMessage(err);
    } finally {
      busy = false;
      active = null;
    }
  }

  function post<T>(path: string, value?: unknown): Promise<T> {
    const request: RequestInit = { method: 'POST', signal: active?.signal };
    if (value !== undefined) {
      request.headers = { 'Content-Type': 'application/json' };
      request.body = JSON.stringify(value);
    }
    return api.json<T>(path, request);
  }

  async function refresh() {
    resources = (await api.json<{ resources: Resource[] }>('/$/resources')).resources;
    await app.refreshDatasets();
  }

  $effect(() => {
    if (admin) void run(refresh);
  });

  function addInput() {
    inputs = [
      ...inputs,
      { source: { resource: source, version }, graph, ...(files ? { files } : {}) },
    ];
    graph = '';
    files = '';
    manifest = null;
  }

  async function registerSource(event: SubmitEvent) {
    event.preventDefault();
    await run(async () => {
      await post('/$/resources', {
        id: sourceId,
        kind: 'source',
        title,
        ...(license ? { license } : {}),
        versions: [{ version: sourceVersion, distributions: [{ url, sha256: digest }] }],
      });
      await refresh();
      adding = false;
      source = sourceId;
      version = sourceVersion;
      status = `Registered ${sourceId} ${sourceVersion}.`;
      sourceId = title = sourceVersion = url = digest = license = '';
    });
  }

  function showLoad(result: Loaded) {
    commit = result.receipt.commit.seq;
    manifest = result.manifest;
    status = `Loaded ${result.manifest.plan.files.length} files into /${dataset} at commit ${commit}.`;
  }

  async function load(event: SubmitEvent) {
    event.preventDefault();
    await run(async () => {
      active = new AbortController();
      const recipe = { kind: 'recipe', format: 1, inputs };
      if (create) {
        await post('/$/sources/plan', recipe);
        await api.createDataset(name, 'persistent');
        dataset = name;
        create = false;
        await app.refreshDatasets();
      }
      showLoad(await post<Loaded>(`/$/sources/${enc(dataset)}/load`, recipe));
      await app.refreshDatasets();
    });
  }

  const manifestPath = () => `/$/sources/${enc(dataset)}/manifests/${commit}`;

  async function readManifest() {
    await run(async () => {
      manifest = await api.json<Manifest>(manifestPath());
      status = `Read source manifest for /${dataset} at commit ${commit}.`;
    });
  }

  async function reload() {
    await run(async () => {
      active = new AbortController();
      showLoad(await post<Loaded>(`${manifestPath()}/reload`));
      await app.refreshDatasets();
    });
  }

  async function installBundle(event: SubmitEvent) {
    event.preventDefault();
    await run(async () => {
      active = new AbortController();
      const versionPath = `/$/resources/${enc(bundleId)}/versions/${enc(bundleVersion)}`;
      showLoad(await post<Loaded>(`${versionPath}/bundle/${enc(dataset)}`));
      await app.refreshDatasets();
    });
  }
</script>

<svelte:head><title>Sources | Sparkles</title></svelte:head>

<div class="page-container">
  <header class="page-head">
    <h1>Sources</h1>
    <PageActions>
      <a class="btn" href={resolve('/datasets')}>Datasets</a>
      {#if admin}
        <button class="btn" disabled={busy} onclick={() => run(refresh)}>Refresh</button>
        <button class="btn primary" disabled={busy} onclick={() => (adding = !adding)}>
          {adding ? 'Close form' : 'Add source'}
        </button>
      {/if}
    </PageActions>
  </header>

  {#if !admin}
    <p class="muted">Server administration access is required.</p>
  {:else}
    {#if error}<div class="error-box" role="alert">{error}</div>{/if}
    {#if status}<p role="status">{status}</p>{/if}
    {#if active}<button class="btn" onclick={() => active?.abort()}>Cancel request</button>{/if}

    {#if adding}
      <section class="panel">
        <div class="panel-head"><h2>Register a URL</h2></div>
        <form class="panel-body fields" onsubmit={registerSource}>
          <label
            >Source ID <input
              bind:value={sourceId}
              required
              pattern={String.raw`[A-Za-z0-9][A-Za-z0-9_.\-]{0,63}`}
            /></label
          >
          <label>Title <input bind:value={title} required maxlength={512} /></label>
          <label>Version <input bind:value={sourceVersion} required maxlength={64} /></label>
          <label>Distribution URL <input type="url" bind:value={url} required /></label>
          <label>SHA-256 <input bind:value={digest} required pattern={'[0-9a-fA-F]{64}'} /></label>
          <label>SPDX licence <input bind:value={license} /></label>
          <button class="btn primary" disabled={busy}>Register source</button>
        </form>
      </section>
    {/if}

    <section class="panel">
      <div class="panel-head"><h2>Registered sources</h2></div>
      {#if sources.length}
        <div class="table-scroll">
          <table class="data">
            <thead><tr><th>Source</th><th>Versions</th><th>Licence</th></tr></thead>
            <tbody>
              {#each sources as item (item.id)}
                <tr>
                  <td>{item.title} <span class="muted mono">{item.id}</span></td>
                  <td>{item.versions.map((v) => v.version).join(', ') || 'None'}</td>
                  <td>{item.license || 'Unknown'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else}
        <p class="panel-body muted">No sources registered.</p>
      {/if}
    </section>

    <section class="panel">
      <div class="panel-head"><h2>Load graphs</h2></div>
      <form
        class="panel-body fields"
        onsubmit={(event) => {
          event.preventDefault();
          addInput();
        }}
      >
        <label
          >Source
          <select bind:value={source} onchange={() => (version = '')} required>
            <option value="">Choose a source</option>
            {#each sources as item (item.id)}<option value={item.id}>{item.title}</option>{/each}
          </select>
        </label>
        <label
          >Version
          <select bind:value={version} required>
            <option value="">Choose a version</option>
            {#each versions as item (item.version)}<option value={item.version}
                >{item.version}</option
              >{/each}
          </select>
        </label>
        <label>Target graph IRI <input bind:value={graph} required /></label>
        <label>File pattern <input bind:value={files} placeholder="*.ttl" /></label>
        <button class="btn" disabled={busy}>Add graph input</button>
      </form>
      {#if inputs.length}
        <ul class="panel-body">
          {#each inputs as input, i (input)}
            <li>
              {input.source.resource}
              {input.source.version} → <span class="mono">{input.graph}</span>
              <button
                class="btn sm"
                disabled={busy}
                onclick={() => (inputs = inputs.filter((_, j) => j !== i))}
              >
                Remove
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      <form class="panel-body fields" onsubmit={load}>
        <label class="toggle"
          ><input type="checkbox" bind:checked={create} /> Create a dataset</label
        >
        {#if create}
          <label>New dataset name <input bind:value={name} required /></label>
        {:else}
          <label
            >Dataset
            <select bind:value={dataset} onchange={() => (manifest = null)} required>
              <option value="">Choose a dataset</option>
              {#each app.datasets as item (item.name)}<option value={item.name}>{item.name}</option
                >{/each}
            </select>
          </label>
        {/if}
        <button class="btn primary" disabled={busy || !inputs.length}>
          {create ? 'Create and load dataset' : 'Replace target graphs'}
        </button>
      </form>
    </section>

    <section class="panel">
      <div class="panel-head"><h2>Vocabulary bundle</h2></div>
      <form class="panel-body fields" onsubmit={installBundle}>
        <label
          >Vocabulary
          <select bind:value={bundleId} onchange={() => (bundleVersion = '')} required>
            <option value="">Choose a vocabulary</option>
            {#each vocabularies as item (item.id)}<option value={item.id}>{item.title}</option
              >{/each}
          </select>
        </label>
        <label
          >Version
          <select bind:value={bundleVersion} required>
            <option value="">Choose a version</option>
            {#each bundleVersions as item (item.version)}<option value={item.version}
                >{item.version}</option
              >{/each}
          </select>
        </label>
        <button class="btn" disabled={busy || !dataset}
          >Install in {dataset || 'selected dataset'}</button
        >
      </form>
    </section>

    <section class="panel">
      <div class="panel-head"><h2>Load provenance</h2></div>
      <div class="panel-body fields">
        <label
          >Commit on {dataset || 'selected dataset'}
          <input
            type="number"
            min="1"
            step="1"
            bind:value={commit}
            onchange={() => (manifest = null)}
          />
        </label>
        <button class="btn" disabled={busy || !dataset || !commit} onclick={readManifest}
          >Read manifest</button
        >
        <button class="btn" disabled={busy || !manifest || !dataset || !commit} onclick={reload}
          >Reload graphs</button
        >
      </div>
      {#if manifest}
        <div class="panel-body table-scroll">
          <table class="data">
            <thead><tr><th>Graph</th><th>Distribution</th></tr></thead>
            <tbody>
              {#each manifest.plan.files as file, i (i)}
                <tr><td class="mono">{file.graph}</td><td>{file.url}</td></tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .fields {
    display: flex;
    align-items: end;
    flex-wrap: wrap;
    gap: 0.75rem;
  }
  .fields label {
    display: grid;
    gap: 0.25rem;
  }
  .fields .toggle {
    display: flex;
    align-items: center;
  }
  li {
    margin-block: 0.25rem;
  }
</style>
