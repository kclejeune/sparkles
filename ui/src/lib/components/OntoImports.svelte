<script lang="ts">
  // The Imports tab of the ontology editor (ROADMAP item 624, docs/specs/U12-ontology-workbench
  // /SPEC.md "Import resolution"): the imports closure from the summary, with its cycles and
  // unresolved imports, and the dataset's import map. An administrator adds and removes map
  // entries, loads an XML catalog with its files and fetches or refreshes an import. A catalog
  // load and a fetch are confirmed actions whose commits show in the history like any other.
  import * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import {
    closureCounts,
    closureTree,
    fetchImport,
    importMap,
    patchImportMap,
    uploadCatalog,
    type ClosureNode,
    type ImportEdge,
    type ImportMap,
  } from '$lib/ontology-imports';
  import { isAbort, LatestRun } from '$lib/supersede';
  import { onDestroy } from 'svelte';

  let {
    target,
    root,
    closure,
    etag = null,
    readonly = true,
    imports = true,
    name = (i: string) => i,
    oncommitted,
    onchanged,
  }: {
    /** The dataset on its branch (`ds@branch`). */
    target: string;
    /** The selected graph the closure starts from. */
    root: string;
    /** The summary's closure. */
    closure: readonly ImportEdge[];
    /** The summary's entity tag, which a catalog load or a fetch names. */
    etag?: string | null;
    /** The branch takes no writes here: a reader or a protected branch. */
    readonly?: boolean;
    /** Whether the page reads imported axioms. Without them the summary lists no closure. */
    imports?: boolean;
    /** The display name of a graph or IRI. */
    name?: (iri: string) => string;
    /** A catalog load or a fetch committed `seq`. */
    oncommitted?: (seq: number | null) => void;
    /** The map changed without a commit. */
    onchanged?: () => void;
  } = $props();

  let map = $state<ImportMap | null>(null);
  let error = $state<unknown>(null);
  let notice = $state<string | null>(null);
  let busy = $state<string | null>(null);
  const run = new LatestRun();

  async function load() {
    const current = run.claim('map');
    try {
      const m = await importMap(target, run.signal('map'));
      if (current()) {
        map = m;
        error = null;
      }
    } catch (e) {
      if (current() && !isAbort(e)) error = e;
    }
  }
  onDestroy(() => run.abortAll());
  $effect(() => {
    void target;
    void load();
  });

  const tree = $derived(closureTree(root, closure));
  const counts = $derived(closureCounts(closure));
  const admin = $derived(!!map?.admin);
  const canAct = $derived(admin && !readonly);
  const unresolved = $derived([
    ...new Set(closure.filter((e) => e.graphs.length === 0).map((e) => e.iri)),
  ]);
  const VIA: Record<string, string> = {
    map: 'import map',
    graph: 'graph of the IRI',
    resource: 'vocabulary resource',
  };

  async function act(key: string, f: () => Promise<void>) {
    busy = key;
    notice = null;
    error = null;
    try {
      await f();
    } catch (e) {
      error = e;
    } finally {
      busy = null;
    }
  }

  // --- the map -----------------------------------------------------------------------
  let newIri = $state('');
  let newGraph = $state('');
  const addEntry = () =>
    act('add', async () => {
      map = await patchImportMap(
        target,
        { set: { [newIri.trim()]: newGraph.trim() } },
        map?.revision ?? null,
      );
      newIri = '';
      newGraph = '';
      notice = 'The entry was added. The views read the imports again.';
      onchanged?.();
    });
  const removeEntry = (iri: string) =>
    act(`rm ${iri}`, async () => {
      map = await patchImportMap(target, { remove: [iri] }, map?.revision ?? null);
      notice = `The entry for ${name(iri)} was removed.`;
      onchanged?.();
    });

  // --- catalogs and fetches ------------------------------------------------------------
  let catalogFile = $state<File | null>(null);
  let files = $state<File[]>([]);
  const upload = () =>
    act('catalog', async () => {
      if (!catalogFile) return;
      const a = await uploadCatalog(target, catalogFile, files, {
        etag,
        message: `Load the XML catalog ${catalogFile.name}`,
      });
      const skipped = a.skipped.length
        ? ` ${fmtInt(a.skipped.length)} skipped: ${a.skipped.map((s) => `${s.entry} (${s.reason})`).join('; ')}.`
        : '';
      notice = `${a.changed ? 'Loaded' : 'Nothing changed in'} ${fmtInt(a.loaded.length)} file${a.loaded.length === 1 ? '' : 's'}, mapped ${fmtInt(a.entries.length)} import${a.entries.length === 1 ? '' : 's'}.${skipped}`;
      catalogFile = null;
      files = [];
      await load();
      oncommitted?.(a.changed ? committed(a) : null);
    });
  const fetchOne = (iri: string) =>
    act(`fetch ${iri}`, async () => {
      const a = await fetchImport(target, { iri }, { etag, message: `Fetch the import ${iri}` });
      notice = a.changed
        ? `Fetched ${name(iri)} into ${name(a.entry.graph)}, ${fmtInt(a.triples)} triples.`
        : `${name(iri)} is unchanged.`;
      await load();
      oncommitted?.(a.changed ? committed(a) : null);
    });
  /** The commit a receipt-carrying answer names. */
  function committed(a: object): number | null {
    const c = (a as { commit?: { seq?: number } }).commit;
    return typeof c?.seq === 'number' ? c.seq : null;
  }
  const fetchable = (iri: string) => /^https?:\/\//.test(iri);
</script>

{#snippet nodes(list: ClosureNode[])}
  <ul class="closure">
    {#each list as n, i (`${n.edge.from}|${n.edge.iri}|${i}`)}
      <li>
        <span class="mono iri" title={n.edge.iri}>{name(n.edge.iri)}</span>
        {#if n.edge.graphs.length === 0}
          <span class="badge warn">unresolved</span>
        {:else}
          <span class="faint small"
            >→ {n.edge.graphs.map(name).join(', ')}, by the {VIA[n.edge.via ?? ''] ?? 'graph'}</span
          >
          {#if n.edge.cycle}<span class="badge warn">cycle</span>{/if}
          {#if n.repeated && !n.edge.cycle}<span class="faint small">(shown above)</span>{/if}
        {/if}
        {#if n.children.length}{@render nodes(n.children)}{/if}
      </li>
    {/each}
  </ul>
{/snippet}

<div class="imports">
  {#if error}<p class="error-box" role="alert">{api.errorMessage(error)}</p>{/if}
  {#if notice}<p class="notice" role="status">{notice}</p>{/if}

  <h3>Closure</h3>
  {#if !imports}
    <p class="faint">The views exclude imported axioms. Include them to see the closure.</p>
  {:else if closure.length === 0}
    <p class="faint">{name(root)} imports nothing.</p>
  {:else}
    <p class="small" role="status">
      {fmtInt(counts.imports)} import{counts.imports === 1 ? '' : 's'}, {fmtInt(counts.unresolved)}
      unresolved, {fmtInt(counts.cycles)}
      {counts.cycles === 1 ? 'edge closes a cycle' : 'edges close cycles'}.
    </p>
    <div class="tree" aria-label="Imports closure">
      <span class="mono">{name(root)}</span>
      {@render nodes(tree)}
    </div>
    {#if unresolved.length}
      <ul class="unresolved" aria-label="Unresolved imports">
        {#each unresolved as iri (iri)}
          <li>
            <span class="mono iri">{iri}</span> resolves to no graph that you can read.
            {#if canAct && fetchable(iri)}
              <button class="btn sm" disabled={busy != null} onclick={() => fetchOne(iri)}
                >{#if busy === `fetch ${iri}`}<span class="spinner"></span>{/if} Fetch into graph</button
              >
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {/if}

  <h3>Import map</h3>
  <p class="faint small">
    The map sends an import to a graph of this dataset before the graph its IRI names. Every branch
    shares it.{#if map && !admin}
      You see the entries whose graph you can read, and only administrators change the map.{/if}
  </p>
  {#if map}
    {#if map.entries.length}
      <div class="table-wrap">
        <table class="table" aria-label="Import map">
          <thead>
            <tr><th>Import</th><th>Graph</th><th>Source</th><th>Details</th><th></th></tr>
          </thead>
          <tbody>
            {#each map.entries as e (e.iri)}
              <tr>
                <td class="mono iri" title={e.iri}>{name(e.iri)}</td>
                <td class="mono iri" title={e.graph}>{name(e.graph)}</td>
                <td>{e.source}</td>
                <td class="small details">
                  {#if e.file}<div>File {e.file}</div>{/if}
                  {#if e.location}<div class="mono">{e.location}</div>{/if}
                  {#if e.etag}<div>ETag <span class="mono">{e.etag}</span></div>{/if}
                  {#if e.digest}<div class="mono" title={e.digest}>
                      {e.digest.slice(0, 19)}…
                    </div>{/if}
                  {#if e.fetched}<div>
                      Fetched {e.fetched}{e.branch ? ` on ${e.branch}` : ''}
                    </div>{/if}
                </td>
                <td class="actions">
                  {#if canAct && e.source === 'fetch'}
                    <button class="btn sm" disabled={busy != null} onclick={() => fetchOne(e.iri)}
                      >{#if busy === `fetch ${e.iri}`}<span class="spinner"></span>{/if} Refresh</button
                    >
                  {/if}
                  {#if canAct}
                    <button
                      class="btn sm ghost"
                      disabled={busy != null}
                      onclick={() => removeEntry(e.iri)}
                      aria-label="Remove the entry for {e.iri}">Remove</button
                    >
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="faint">The map has no entries{admin ? '' : ' that you can see'}.</p>
    {/if}
    {#if canAct}
      <form
        class="row"
        aria-label="Add an entry"
        onsubmit={(e) => {
          e.preventDefault();
          void addEntry();
        }}
      >
        <input
          class="input sm"
          placeholder="Import IRI"
          aria-label="Import IRI"
          bind:value={newIri}
        />
        <input class="input sm" placeholder="Graph" aria-label="Graph" bind:value={newGraph} />
        <button class="btn sm" disabled={!newIri.trim() || !newGraph.trim() || busy != null}
          >Add entry</button
        >
      </form>
      <h3>XML catalog</h3>
      <form
        class="row"
        aria-label="Load an XML catalog"
        onsubmit={(e) => {
          e.preventDefault();
          void upload();
        }}
      >
        <label class="form-row"
          >Catalog <input
            type="file"
            accept=".xml,application/xml,text/xml"
            onchange={(e) => (catalogFile = e.currentTarget.files?.[0] ?? null)}
          /></label
        >
        <label class="form-row"
          >Files <input
            type="file"
            multiple
            onchange={(e) => (files = [...(e.currentTarget.files ?? [])])}
          /></label
        >
        <!-- a folder keeps each file's path below it, which a catalog's sub/lib.ttl names -->
        <label class="form-row"
          >Folder <input
            type="file"
            webkitdirectory
            onchange={(e) => (files = [...(e.currentTarget.files ?? [])])}
          /></label
        >
        <button class="btn sm" disabled={!catalogFile || busy != null}
          >{#if busy === 'catalog'}<span class="spinner"></span>{/if} Load catalog</button
        >
      </form>
      <p class="faint small">
        Each file replaces the graph of the import its catalog entry names, in one commit. Choose
        the catalog's folder when its entries name files in subfolders.
      </p>
    {/if}
  {:else if !error}
    <p class="faint small"><span class="spinner"></span> Reading the import map…</p>
  {/if}
</div>

<style>
  .imports {
    display: flex;
    flex-direction: column;
    gap: var(--space-2, 8px);
    min-width: 0;
  }
  h3 {
    margin: var(--space-3, 12px) 0 0;
    font-size: 1rem;
  }
  .closure {
    list-style: none;
    margin: 0;
    padding-left: 1.25rem;
    border-left: 1px solid var(--border);
  }
  .closure li {
    margin: 2px 0;
  }
  .iri {
    overflow-wrap: anywhere;
  }
  .badge.warn {
    color: var(--warn-fg, var(--fg));
    background: var(--warn-bg, transparent);
  }
  .unresolved {
    margin: 0;
    padding-left: 1.25rem;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2, 8px);
    align-items: center;
  }
  .row .input {
    flex: 1 1 12rem;
    min-width: 0;
  }
  .table-wrap {
    overflow-x: auto;
  }
  .details div {
    overflow-wrap: anywhere;
  }
  .actions {
    white-space: nowrap;
  }
</style>
