<script lang="ts">
  import { beforeNavigate, goto } from '$app/navigation';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { graphRestricted } from '$lib/auth';
  import { branchParam, MAIN, onBranch } from '$lib/branches';
  import { fmtCount, fmtInt } from '$lib/format';
  import { EditSession, startSessionBranch } from '$lib/edit-session.svelte';
  import { type Op, type Selection } from '$lib/ontology';
  import { displayTerm, iri, WELL_KNOWN } from '$lib/rdf';
  import { routes } from '$lib/routes';
  import { readHead } from '$lib/skos';
  import { save } from '$lib/storage';
  import { LatestRun, isAbort } from '$lib/supersede';
  import {
    definitionProblems,
    delta,
    graphQuery,
    graphResult,
    id,
    MAX_BYTES,
    parseIri,
    sampleNodes,
    SH,
    ShapeDraft,
    ShapeGraph,
    shapePatch,
    shapeTree,
  } from '$lib/shape-editor';
  import OntoTree from '$components/OntoTree.svelte';
  import OntoPreview from '$components/OntoPreview.svelte';
  import ShapeDefinition from '$components/ShapeDefinition.svelte';
  import FollowState from '$components/FollowState.svelte';
  import PageActions from '$components/PageActions.svelte';
  import Modal from '$components/Modal.svelte';

  const q = $derived(page.url.searchParams);
  const name = $derived(q.get('ds') ?? app.current ?? '');
  const branch = $derived(branchParam(q.get('branch')));
  const target = $derived(onBranch(name, branch));
  const graph = $derived(q.get('graph') || null);
  const sourceKey = $derived(JSON.stringify([target, graph]));
  const prefixes = $derived({ ...WELL_KNOWN, ...app.prefixes(name) });
  const sel: Selection = $derived({ graph, imports: false });
  type Source = {
    key: string;
    draft: ShapeDraft;
    head: number;
    etag: string | null;
    branchId: string;
  };
  let source = $state<Source | null>(null);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let stale = $state(false);
  let branchList = $state<api.Branch[]>([]);
  let serverReadOnly = $state(true);
  let selected = $state<string | null>(null);
  let selectedRow = $state<string | null>(null);
  let open = $state(new Set<string>());
  let filter = $state('');
  let graphInput = $state('');
  let pending = $state(new Set<string>());
  let resetForm = $state(0);
  const reads = new LatestRun();
  // A refresh closes the preview, which would drop the answer of a commit in flight. The
  // change feed's refreshes therefore wait until the preview has closed.
  let deferredReload = false;
  const session = new EditSession({
    reload: () => {
      if (session.previewOpen) {
        deferredReload = true;
        return Promise.resolve();
      }
      return load();
    },
    staleText:
      'Another commit landed. Your shape draft is retained. Discard it to load the new state.',
  });
  let pin = $state<{
    op: Op;
    target: string;
    sel: Selection;
    etag: string | null;
    head: number | null;
    sourceId: string;
  } | null>(null);
  const currentBranch = $derived(branchList.find((b) => b.name === (branch ?? MAIN)));
  // A triple-filtered graph is not a complete shape definition. Graph-only grants still
  // use server-side visibility, exact graph writes and patch endpoint authorization.
  const hiddenTriples = $derived(
    auth.who?.authEnabled === true &&
      !auth.hasServer('server-admin') &&
      !!auth.who.restricted?.[name]?.triples,
  );
  const readonly = $derived(
    !auth.loaded ||
      !!auth.error ||
      serverReadOnly ||
      hiddenTriples ||
      !auth.can(name, 'write') ||
      !currentBranch ||
      currentBranch.protected ||
      !!currentBranch.broken,
  );
  const ready = $derived(
    !!source && source.key === sourceKey && !loading && !loadError && !stale && !hiddenTriples,
  );
  const canSubmit = $derived(ready && !readonly && pending.size === 0);
  const model = $derived(source?.draft.graph ?? new ShapeGraph());
  const shapes = $derived(model.shapes());
  const terms = $derived(new Map(shapes.map((s) => [id(s), s])));
  const subject = $derived(selected ? terms.get(selected) : undefined);
  const projection = $derived(shapeTree(model));
  const roots = $derived(projection.roots);
  const nameOf = (key: string) => {
    const t = projection.nodes.get(key)?.term;
    if (!t) return key;
    const label = model.values(t, SH + 'name').find((t) => t.type === 'literal');
    return label?.type === 'literal' ? label.value : displayTerm(t, prefixes);
  };
  const children = (key: string) => projection.children.get(key) ?? [];
  const keep = $derived.by(() => {
    const f = filter.trim().toLowerCase();
    if (!f) return null;
    const keep = new Set(
      [...projection.nodes]
        .filter(([key, row]) => `${nameOf(key)} ${id(row.term)}`.toLowerCase().includes(f))
        .map(([key]) => key),
    );
    for (const key of [...keep]) {
      for (let p = projection.parent.get(key); p && !keep.has(p); p = projection.parent.get(p))
        keep.add(p);
    }
    return keep;
  });
  const changes = $derived(source ? delta(source.draft.basis, model) : { added: [], removed: [] });
  const problems = $derived(definitionProblems(model));
  const blockingProblems = $derived(definitionProblems(model, source?.draft.basis));

  $effect(() => {
    if (name && name !== app.current) app.setDataset(name);
    if (name) void app.loadPrefixes(name);
  });
  $effect(() => {
    const key = sourceKey;
    untrack(() => {
      source = null;
      stale = false;
      pending = new Set();
      selected = null;
      selectedRow = null;
      open = new Set();
      session.reset(key);
      session.notice = null;
      session.own.clear();
      pin = null;
      graphInput = graph ?? '';
      void load();
    });
    return () => reads.abortAll();
  });
  $effect(() => {
    if (session.previewOpen || !deferredReload) return;
    deferredReload = false;
    void load();
  });
  $effect(() => {
    const t = target;
    return session.follow(
      t,
      () => source?.head ?? 0,
      () => source?.key === sourceKey && !loading,
    );
  });
  onMount(() => {
    const key = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName) || el.isContentEditable))
        return;
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== 'z' || readonly || pending.size)
        return;
      e.preventDefault();
      if (e.shiftKey && source && source.draft.cursor < source.draft.states.length - 1)
        localUndo(true);
      else if (!e.shiftKey && source?.draft.dirty) localUndo();
      else revert(e.shiftKey);
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  });
  beforeNavigate((navigation) => {
    if (!source?.draft.dirty && !pending.size) return;
    const next = navigation.to?.url;
    const sameSource =
      next?.pathname === page.url.pathname &&
      (next.searchParams.get('ds') ?? app.current ?? '') === name &&
      branchParam(next.searchParams.get('branch')) === branch &&
      (next.searchParams.get('graph') || null) === graph;
    if (
      !sameSource &&
      !window.confirm('Discard your uncommitted shape and field edits and leave this source?')
    )
      navigation.cancel();
  });

  async function load(discard = false) {
    if (!name) return;
    const owns = reads.claim('graph');
    const signal = reads.signal('graph');
    const origin = { key: sourceKey, target, name, graph, branch };
    loading = true;
    loadError = null;
    session.previewOpen = false;
    try {
      await auth.ensure();
      if (origin.graph) parseIri(`<${origin.graph}>`, {});
      if (hiddenTriples)
        throw new Error(
          'Triple-filtered access cannot establish a complete shapes graph. Open this editor with read access to the whole shapes graph.',
        );
      // whoami describes main's grants. A cheap empty-schema SHACL read confirms
      // unrestricted visibility on a different branch before accepting a full graph.
      if (origin.branch && graphRestricted(auth.who, origin.name, origin.branch)) {
        try {
          await api.shacl(origin.target, '', {
            targets: [iri('urn:x-sparkles:shape-editor:visibility-probe')],
            reasoning: false,
            timeout: 1,
            signal,
            maxResponseBytes: 4096,
          });
        } catch (e) {
          if (e instanceof api.ApiError && e.status === 403)
            throw new Error(
              'The server cannot confirm complete shape visibility on this branch with your current grants.',
            );
          throw e;
        }
      }
      const [head, branches, server] = await Promise.all([
        readHead(origin.target, origin.graph, signal).catch((e) => {
          if (e instanceof api.ApiError && e.status === 404) return null;
          throw e;
        }),
        api.branches(origin.name, signal),
        api.cachedServerInfo(),
      ]);
      const b = branches.branches.find((b) => b.name === (origin.branch ?? MAIN));
      if (!b) throw new Error('The selected branch is not available.');
      const h = head ?? { etag: null, commit: b.head };
      if (!Number.isSafeInteger(h.commit) || h.commit < 0)
        throw new Error('The server did not return a complete commit identity.');
      const result = await api.query(origin.target, graphQuery(origin.graph), {
        at: String(h.commit),
        execution: 'eager',
        send: 5001,
        reasoning: false,
        nocache: true,
        timeout: 10,
        signal,
        maxResponseBytes: MAX_BYTES,
      });
      const read = graphResult(result);
      if (result.meta.datasetId !== b.id)
        throw new Error(
          'The dataset identity changed during the graph read. Refresh to read one complete source.',
        );
      if (result.meta.commit !== h.commit)
        throw new Error('The graph response did not confirm the requested commit.');
      if (!owns() || origin.key !== sourceKey) return;
      branchList = branches.branches;
      serverReadOnly = server.readOnly === true;
      if (source?.key === origin.key && source.branchId !== b.id) {
        session.reset(origin.key);
        session.own.clear();
        session.notice = 'The branch identity changed';
      }
      if (!discard && source?.key === origin.key && (source.draft.dirty || pending.size)) {
        stale = source.head !== h.commit || source.branchId !== b.id;
        return;
      }
      source = {
        key: origin.key,
        draft: new ShapeDraft(read),
        head: h.commit,
        etag: h.etag,
        branchId: b.id,
      };
      stale = false;
      if (discard) pending = new Set();
      // Field components hold buffers of the structures they edit, which a new revision can
      // change under the same blank node. Remount them for every accepted source.
      resetForm++;
      const all = read.shapes();
      selected = all.some((s) => id(s) === selected)
        ? selected
        : all.find((s) => id(s) === q.get('shape'))
          ? q.get('shape')
          : all[0]
            ? id(all[0])
            : null;
    } catch (e) {
      if (owns() && !isAbort(e)) {
        loadError = api.errorMessage(e);
        stale = !!source;
      }
    } finally {
      if (owns()) loading = false;
    }
  }
  function update(g: ShapeGraph) {
    if (readonly || !source || source.key !== sourceKey) return;
    source = { ...source, draft: source.draft.change(g) };
    error = null;
    if (selected && !g.shapes().some((s) => id(s) === selected))
      selected = g.shapes()[0] ? id(g.shapes()[0]) : null;
  }
  function pendingField(key: string, value: boolean) {
    const next = new Set(pending);
    if (value) next.add(key);
    else next.delete(key);
    pending = next;
  }
  function choose(term: api.Term, row = id(term)) {
    if (pending.size) {
      error = 'Apply or discard the current field edits before opening another shape.';
      return;
    }
    selected = id(term);
    selectedRow = row;
    error = null;
  }
  function localUndo(redo = false) {
    if (!source || pending.size || readonly) return;
    source = { ...source, draft: redo ? source.draft.redo() : source.draft.undo() };
    resetForm++;
  }
  function preview() {
    if (!source || !canSubmit || !source.draft.dirty || blockingProblems.length) return;
    try {
      const op: Op = {
        kind: 'patch',
        patch: shapePatch(source.draft.basis, model, graph, source.branchId, source.head),
      };
      pin = {
        op,
        target,
        sel: { ...sel },
        etag: source.etag,
        head: source.head,
        sourceId: source.branchId,
      };
      session.edit(op, 'Edit SHACL shapes');
    } catch (e) {
      error = api.errorMessage(e);
    }
  }
  function revert(redo = false) {
    if (!source || !canSubmit || source.draft.dirty || graphRestricted(auth.who, name, branch))
      return;
    if (redo) session.redoLast();
    else session.undoLast();
    if (session.previewOp)
      pin = {
        op: session.previewOp,
        target,
        sel: { ...sel },
        etag: source.etag,
        head: source.head,
        sourceId: source.branchId,
      };
  }
  async function navigate(over: Record<string, string | null>) {
    await goto(routes.shapes(name, { branch, graph, ...over }));
  }
  async function setGraph() {
    try {
      const g = graphInput.trim() ? parseIri(graphInput, prefixes) : null;
      await navigate({ graph: g });
    } catch (e) {
      error = api.errorMessage(e);
    }
  }
  async function startSession() {
    if (source?.draft.dirty || pending.size) {
      error = 'Commit or discard your draft before starting a session branch.';
      return;
    }
    try {
      const b = await startSessionBranch(name, {
        from: branch,
        branches: branchList,
        user: auth.who?.principal.name,
        prefix: 'shapes',
        note: 'SHACL authoring session',
      });
      await navigate({ branch: b });
    } catch (e) {
      error = api.errorMessage(e);
    }
  }
  let newOpen = $state(false);
  let newKind = $state<'node' | 'property'>('node');
  let newIri = $state('');
  let newParent = $state<api.Term | undefined>();
  function newShape(kind: 'node' | 'property', parent?: api.Term) {
    if (pending.size) {
      error = 'Apply or discard the current field edits first.';
      return;
    }
    newKind = kind;
    newParent = parent;
    newIri = '';
    newOpen = true;
  }
  function create() {
    if (readonly || !source) return;
    try {
      const result = model.create(
        newKind,
        newIri.trim() ? iri(parseIri(newIri, prefixes)) : model.fresh(),
        newParent,
      );
      update(result.graph);
      selected = id(result.term);
      if (newParent) open = new Set(open).add(id(newParent));
      newOpen = false;
    } catch (e) {
      error = api.errorMessage(e);
    }
  }
  let textOpen = $state(false);
  let transferOpen = $state(false);
  let transfer = $state(false);
  async function useText() {
    if (!transfer || !source) return;
    save(
      `sparkles.shacl.${name}`,
      `# Source ${JSON.stringify({ target, graph, commit: source.head, branchId: source.branchId })}\n${model.turtle()}`,
    );
    save(`sparkles.shacl.syntax.${name}`, 'turtle');
    transferOpen = false;
    // The graph draft stays here until the person explicitly leaves it.
    await goto(
      `/ui/datasets/${encodeURIComponent(name)}${branch ? `?branch=${encodeURIComponent(branch)}` : ''}#validate`,
    );
  }

  let samples = $state('');
  let sampleMode = $state('nodes');
  let dataGraph = $state('default');
  let reasoning = $state(false);
  let live = $state(true);
  let validationTick = $state(0);
  let validation = $state<api.ShaclReport | null>(null);
  let validationError = $state<string | null>(null);
  let validating = $state(false);
  let validationStatus = $state('Enter sample nodes to check this draft.');
  let validationCtl: AbortController | null = null;
  let lastManual = 0;
  $effect(() => {
    const snapshot = {
      text: model.turtle(),
      samples,
      sampleMode,
      dataGraph,
      reasoning,
      target,
      sourceId: source?.branchId,
      head: source?.head,
      ready,
      pending: pending.size,
      live,
      tick: validationTick,
    };
    validation = null;
    validationError = null;
    validating = false;
    validationStatus =
      snapshot.sampleMode === 'nodes' && !snapshot.samples.trim()
        ? 'Enter sample nodes to check this draft.'
        : 'The draft has not been checked in this state.';
    if (
      !snapshot.ready ||
      !snapshot.sourceId ||
      snapshot.head == null ||
      snapshot.pending ||
      (snapshot.sampleMode === 'nodes' && !snapshot.samples.trim()) ||
      (!snapshot.live && snapshot.tick === lastManual)
    )
      return;
    lastManual = snapshot.tick;
    const ctl = new AbortController();
    validationCtl = ctl;
    const timer = setTimeout(
      async () => {
        validating = true;
        validationStatus = 'Checking sample nodes…';
        const deadline = setTimeout(() => ctl.abort(), 10_000);
        try {
          const nodes =
            snapshot.sampleMode === 'nodes' ? sampleNodes(snapshot.samples, prefixes) : undefined;
          const g = ['default', 'union'].includes(snapshot.dataGraph)
            ? snapshot.dataGraph
            : parseIri(snapshot.dataGraph, prefixes);
          const report = await api.shacl(snapshot.target, snapshot.text, {
            targets: nodes,
            graph: g,
            reasoning: snapshot.reasoning,
            at: String(snapshot.head),
            source: { id: snapshot.sourceId!, commit: snapshot.head! },
            storedBlankNodes: true,
            timeout: 5,
            signal: ctl.signal,
            maxResponseBytes: MAX_BYTES,
          });
          if (!ctl.signal.aborted) {
            validation = report;
            validationStatus = report.conforms
              ? snapshot.sampleMode === 'nodes'
                ? 'These sample nodes conform to the draft.'
                : 'The targets in this sample graph conform to the draft.'
              : 'The sample check has validation results.';
          }
        } catch (e) {
          if (validationCtl !== ctl) return;
          if (!ctl.signal.aborted) {
            validationError = api.errorMessage(e);
            validationStatus = 'The sample check did not complete.';
          } else validationStatus = 'Sample check canceled or timed out.';
        } finally {
          clearTimeout(deadline);
          if (validationCtl === ctl) validating = false;
        }
      },
      snapshot.live ? 650 : 0,
    );
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  });
</script>

<svelte:head><title>SHACL shapes · Sparkles</title></svelte:head>

<div class="page page-container shape-page">
  <header class="page-head">
    <div class="title">
      <h1>SHACL shapes</h1>
      <p class="muted">Author a shapes graph, check sample data and preview one commit.</p>
    </div>
    <PageActions>
      <FollowState live={session.live} onretry={() => void load()} />
    </PageActions>
  </header>
  {#if !name}<p class="faint">Choose a dataset to author shapes.</p>{:else}
    <div class="source-controls">
      <label
        >Branch<select
          class="select"
          value={branch ?? MAIN}
          onchange={(e) => void navigate({ branch: branchParam(e.currentTarget.value) })}
          >{#each branchList as b (b.name)}<option value={b.name}
              >{b.name}{b.protected ? ' · protected' : ''}</option
            >{/each}</select
        ></label
      >
      <label class="graph"
        >Shapes graph<input
          class="input"
          bind:value={graphInput}
          placeholder="Default graph, or enter a graph IRI"
        /></label
      ><button class="btn" onclick={setGraph}>Open graph</button>
      <button class="btn" disabled={loading} onclick={() => void load()}>Refresh</button>
      <button
        class="btn"
        disabled={!auth.can(name, 'write') ||
          serverReadOnly ||
          !!source?.draft.dirty ||
          pending.size > 0}
        onclick={startSession}>Start session</button
      >
      {#if branch}<a
          class="btn ghost"
          href={`/ui/datasets/${encodeURIComponent(name)}/branches?branch=${encodeURIComponent(branch)}`}
          >Branches and merge</a
        >{/if}
    </div>
    <p class="identity">
      {target} · {graph ?? 'Stored default graph'}{#if source}
        · Commit {source.head}{/if}
    </p>
    {#if readonly && !loading}<p class="notice">
        {currentBranch?.protected
          ? 'This branch is protected. Start a session branch to edit.'
          : 'This source is read-only for your current access.'}
      </p>{/if}
    {#if session.notice}<p class="notice">
        {session.notice}. {source?.draft.dirty || pending.size
          ? 'Your draft remains at its original commit.'
          : 'The graph has refreshed.'}
      </p>{/if}
    {#if loadError}<p class="error-box" role="alert">{loadError}</p>{/if}
    {#if stale}<div class="notice">
        The source changed or could not be refreshed. Your draft is retained at commit {source?.head}.
        Commit is disabled.
        <button class="btn" onclick={() => void load(true)}>Discard draft and reload</button>
      </div>{/if}
    {#if error}<p class="error-box" role="alert">{error}</p>{/if}
    {#if loading}<p class="faint">
        <span class="spinner"></span> Reading the complete shapes graph…
      </p>{/if}
    {#if source}
      <div class="editor-actions">
        <button class="btn" disabled={readonly || loading} onclick={() => newShape('node')}
          >New node shape</button
        ><button class="btn" disabled={readonly || loading} onclick={() => newShape('property')}
          >New property shape</button
        >
        <span class="spacer"></span>
        <button
          class="btn ghost"
          disabled={readonly || pending.size > 0 || source.draft.cursor === 0}
          onclick={() => localUndo()}>Undo draft</button
        ><button
          class="btn ghost"
          disabled={readonly ||
            pending.size > 0 ||
            source.draft.cursor >= source.draft.states.length - 1}
          onclick={() => localUndo(true)}>Redo draft</button
        >
        <button
          class="btn ghost"
          disabled={!canSubmit ||
            source.draft.dirty ||
            !session.done.length ||
            graphRestricted(auth.who, name, branch)}
          onclick={() => revert()}>Undo commit</button
        ><button
          class="btn ghost"
          disabled={!canSubmit ||
            source.draft.dirty ||
            !session.undone.length ||
            graphRestricted(auth.who, name, branch)}
          onclick={() => revert(true)}>Redo commit</button
        >
        <button class="btn" onclick={() => (textOpen = !textOpen)}>View Turtle</button>
      </div>
      <div class="panes">
        <aside class="panel shapes-tree">
          {#if projection.omitted}<p class="help">
              Showing at most 5,000 repeated reference links. {projection.omitted} additional links remain
              available in each shape’s complete reference and list forms.
            </p>{/if}
          <div class="panel-head"><h2>Shapes <span class="faint">{shapes.length}</span></h2></div>
          <input
            class="input filter"
            aria-label="Filter shapes"
            placeholder="Filter shapes"
            bind:value={filter}
          />
          {#if shapes.length}<OntoTree
              {roots}
              {children}
              name={nameOf}
              sub={(key) => {
                const row = projection.nodes.get(key);
                return row
                  ? `${row.via ? 'sh:' + row.via + ' · ' : ''}${displayTerm(row.term, prefixes)}`
                  : null;
              }}
              badge={(key) => {
                const row = projection.nodes.get(key)!;
                return row.reference ? 'reference' : model.property(row.term) ? 'property' : 'node';
              }}
              {open}
              selected={selectedRow && projection.nodes.has(selectedRow) ? selectedRow : selected}
              {keep}
              label="SHACL shapes"
              onselect={(key) => {
                const t = projection.nodes.get(key)?.term;
                if (t) choose(t, key);
              }}
              ontoggle={(key) => {
                const next = new Set(open);
                if (next.has(key)) next.delete(key);
                else next.add(key);
                open = next;
              }}
            />{:else}<p class="empty">
              This graph has no shapes. Create a node shape to begin.
            </p>{/if}
        </aside>
        <main class="panel frame">
          <div class="panel-body">
            {#if subject}{#key `${selected}/${resetForm}`}<ShapeDefinition
                  graph={model}
                  {subject}
                  {prefixes}
                  readonly={readonly || loading}
                  onchange={update}
                  onpending={pendingField}
                  onselect={choose}
                  onnewproperty={() => newShape('property', subject)}
                />{/key}{:else}<p class="empty">Select a shape to edit its definition.</p>{/if}
          </div>
        </main>
      </div>
      {#if textOpen}<section class="panel source">
          <div class="panel-head">
            <h2>Complete draft as Turtle</h2>
            <button
              class="btn"
              disabled={pending.size > 0}
              onclick={() => {
                transfer = false;
                transferOpen = true;
              }}>Use in text validator</button
            >
          </div>
          <pre>{model.turtle()}</pre>
          <p class="help">
            This includes additional RDF that the forms preserve. The dataset's text validator also
            accepts SHACLC.
          </p>
        </section>{/if}
      <section class="panel samples" aria-label="Sample validation">
        <div class="panel-head">
          <h2>Sample validation</h2>
          <label class="check"><input type="checkbox" bind:checked={live} />Live check</label>
        </div>
        <div class="panel-body">
          <label
            >Sample selection<select
              class="select"
              aria-label="Sample selection"
              bind:value={sampleMode}
              ><option value="nodes">Explicit focus nodes</option><option value="targets"
                >All targets in the sample graph</option
              ></select
            ></label
          >
          {#if sampleMode === 'nodes'}<label
              >Sample nodes<textarea
                class="input"
                rows="3"
                bind:value={samples}
                placeholder="One IRI or blank-node label per line, up to 100"></textarea></label
            >{/if}
          <div class="sample-options">
            <label
              >Data graph<input
                class="input"
                bind:value={dataGraph}
                placeholder="default, union or graph IRI"
              /></label
            ><label class="check"
              ><input type="checkbox" bind:checked={reasoning} />Include inferences</label
            ><button
              class="btn"
              disabled={!ready || pending.size > 0 || (sampleMode === 'nodes' && !samples.trim())}
              onclick={() => validationTick++}>Validate samples</button
            >{#if validating}<button
                class="btn"
                onclick={() => {
                  validationCtl?.abort();
                  validation = null;
                }}>Cancel check</button
              >{/if}
          </div>
          <p class="help">
            Checks the draft against {sampleMode === 'nodes'
              ? 'these focus nodes'
              : 'the declared targets in the selected data graph'} in {target} at commit {source.head}.
            A sample result does not prove the whole dataset conforms.
          </p>
          <p aria-live="polite" class:conforms={validation?.conforms}>{validationStatus}</p>
          {#if validationError}<p class="error-box" role="alert">{validationError}</p>{/if}
          {#if validation?.results.length}<ul class="results" aria-label="Sample results">
              {#each validation.results.slice(0, 100) as result, i (i)}<li>
                  <strong>{displayTerm(result.focusNode, prefixes)}</strong> · {displayTerm(
                    result.severity,
                    prefixes,
                  )} · {displayTerm(result.sourceShape, prefixes)}
                  <p>
                    {result.messages.join(' · ') ||
                      displayTerm(result.sourceConstraintComponent, prefixes)}
                  </p>
                </li>{/each}
            </ul>
            {#if validation.results.length > 100}<p>
                Showing the first 100 of {validation.results.length} results.
              </p>{/if}{/if}
        </div>
      </section>
      <footer class="draft-actions">
        <div>
          <strong
            >{fmtCount(changes.added.length, 'triple')} added · {fmtInt(changes.removed.length)} removed</strong
          >{#if pending.size}<p>
              Apply or discard {fmtCount(pending.size, 'field edit')} before previewing.
            </p>{/if}
        </div>
        <button
          class="btn"
          disabled={!source.draft.dirty && !pending.size}
          onclick={() => void load(true)}>Discard draft</button
        ><button
          class="btn primary"
          disabled={!canSubmit || !source.draft.dirty || blockingProblems.length > 0}
          onclick={preview}>Preview commit</button
        >
      </footer>
      {#if problems.length}<ul class="problems" aria-label="Shape definition problems">
          {#each problems as problem (problem)}<li>
              {problem}{#if !blockingProblems.includes(problem)}
                Preserved from the stored graph; edit this field to replace it.{/if}
            </li>{/each}
        </ul>{/if}
    {/if}
  {/if}
</div>

<OntoPreview
  bind:open={session.previewOpen}
  title={session.previewTitle}
  {target}
  {sel}
  op={session.previewOp}
  etag={source?.etag ?? null}
  head={source?.head ?? null}
  {pin}
  disabled={!canSubmit}
  describe={(a) => (typeof a === 'string' ? a : JSON.stringify(a))}
  oncommitted={async (op, answer) => {
    if (source) source = { ...source, draft: new ShapeDraft(model) };
    pending = new Set();
    resetForm++;
    await session.committed(op, answer);
  }}
  onstale={() => void session.stale()}
/>
<Modal bind:open={newOpen} title={`New ${newKind} shape`}
  ><label class="new-iri"
    >Shape IRI<input
      class="input"
      bind:value={newIri}
      placeholder="ex:PersonShape, or empty for an anonymous shape"
    /></label
  >
  <p class="help">
    {newParent
      ? `Adds a property shape to ${displayTerm(newParent, prefixes)}.`
      : 'Creates a standalone shape in the selected graph.'}
  </p>
  {#snippet actions()}<button class="btn" onclick={() => (newOpen = false)}>Cancel</button><button
      class="btn primary"
      disabled={readonly}
      onclick={create}>Create shape</button
    >{/snippet}</Modal
>
<Modal bind:open={transferOpen} title="Use draft in text validator"
  ><p>
    Copy the complete Turtle draft into {name}'s text validator? This replaces its saved text draft.
    Your stored shapes change only through a previewed commit.
  </p>
  <label class="check"
    ><input type="checkbox" bind:checked={transfer} />Replace the saved text draft</label
  >{#snippet actions()}<button class="btn" onclick={() => (transferOpen = false)}>Cancel</button
    ><button class="btn primary" disabled={!transfer} onclick={useText}>Open text validator</button
    >{/snippet}</Modal
>

<style>
  .source-controls,
  .editor-actions,
  .sample-options,
  .draft-actions {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
  }
  .identity,
  .help {
    margin: 0;
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .source-controls {
    align-items: end;
  }
  label {
    display: grid;
    gap: 5px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .graph {
    flex: 1 1 260px;
    min-width: 0;
  }
  .identity {
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
  }
  .notice {
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    font-size: var(--fs-sm);
  }
  .spacer {
    flex: 1;
  }
  .panes {
    display: grid;
    grid-template-columns: minmax(260px, 2fr) minmax(0, 3fr);
    align-items: start;
    gap: 12px;
    min-width: 0;
  }
  .shapes-tree {
    min-width: 0;
    max-height: 70vh;
    overflow: auto;
  }
  h2 {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .filter {
    margin: 10px;
    width: calc(100% - 20px);
  }
  .frame {
    min-width: 0;
  }
  .empty {
    padding: 18px;
    margin: 0;
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .source {
    min-width: 0;
  }
  .source pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 320px;
    overflow: auto;
    padding: 12px;
    font-size: var(--fs-xs);
  }
  .source .help {
    padding: 0 12px 12px;
  }
  .samples .panel-body {
    display: grid;
    gap: 12px;
  }
  .sample-options {
    align-items: end;
  }
  .sample-options > label:first-child {
    flex: 1 1 220px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .results {
    margin: 0;
    padding-left: 20px;
    max-height: 320px;
    overflow: auto;
    font-size: var(--fs-sm);
  }
  .results li {
    margin-bottom: 10px;
    overflow-wrap: anywhere;
  }
  .results p {
    margin: 4px 0 0;
  }
  .conforms {
    color: var(--success);
  }
  .draft-actions {
    position: sticky;
    bottom: 0;
    z-index: 2;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 12px;
    font-size: var(--fs-sm);
  }
  .draft-actions > div {
    flex: 1 1 240px;
  }
  .draft-actions p {
    margin: 4px 0 0;
    color: var(--text-3);
  }
  .problems {
    font-size: var(--fs-sm);
    color: var(--danger);
    margin: 0;
  }
  .new-iri {
    min-width: 0;
  }
  @media (max-width: 760px) {
    .panes {
      grid-template-columns: minmax(0, 1fr);
    }
    .shapes-tree {
      max-height: 260px;
    }
    .draft-actions {
      position: static;
    }
  }
</style>
