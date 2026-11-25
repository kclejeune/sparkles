<script lang="ts">
  // Pattern-based OWL 2 axiom generation tab (ROADMAP item 623,
  // docs/specs/U12-ontology-workbench/SPEC.md): upload or paste CSV/TSV rows, author
  // ROBOT-template column directives or DOSDP/Cellfie parameterized axiom rules, inspect
  // generated axioms and per-row diagnostics, check a server dry-run diff, and commit
  // atomically with `If-Match`.
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import type { EntityKind, Selection } from '$lib/ontology';
  import {
    applyPatterns,
    buildPatternSpec,
    dryRunPatterns,
    formatPrefixLines,
    parsePrefixLines,
    parsePreviewHeaders,
    PATTERN_PRESETS,
    patternInputSignature,
    previewPatterns,
    type PatternDeclarationSpec,
    type PatternMode,
    type PatternOnError,
    type PatternPreset,
    type PatternPreviewRequest,
    type PatternPreviewResponse,
    type PatternRuleSpec,
    type PatternSpec,
    type RobotColumnDirective,
    type TableFormat,
  } from '$lib/ontology-patterns';
  import { isAbort, LatestRun } from '$lib/supersede';
  import type { WritePreview } from '$lib/validation-overlay';
  import DiffLines from './DiffLines.svelte';

  let {
    target,
    sel,
    etag = null,
    readonly = true,
    oncommitted,
  }: {
    /** The dataset on its branch (`ds@branch`). */
    target: string;
    /** The selected graph and imports toggle. */
    sel: Selection;
    /** The ontology's entity tag, passed as `If-Match` when applying. */
    etag?: string | null;
    /** Readers and protected branches may preview patterns without committing. */
    readonly?: boolean;
    /** Called after a commit succeeds, with its commit sequence number. */
    oncommitted?: (seq: number | null) => void;
  } = $props();

  const ENTITY_KINDS: EntityKind[] = [
    'Class',
    'ObjectProperty',
    'DataProperty',
    'AnnotationProperty',
    'NamedIndividual',
    'Datatype',
  ];

  // --- authoring state --------------------------------------------------------------------
  let activePreset = $state(PATTERN_PRESETS[0].id);
  let mode = $state<PatternMode>('robot');
  let format = $state<TableFormat>(PATTERN_PRESETS[0].format);
  let tableData = $state(PATTERN_PRESETS[0].data);
  let baseIri = $state('');
  let prefixText = $state(formatPrefixLines(PATTERN_PRESETS[0].spec.prefixes));
  let headerTemplateRow = $state(true);
  let robotColumns = $state<RobotColumnDirective[]>([]);
  let rules = $state<PatternRuleSpec[]>([
    {
      name: 'subclass',
      template: '${class|iri} SubClassOf ${parent|iri}',
      syntax: 'manchester',
      skipIfEmpty: true,
    },
  ]);
  let declarations = $state<PatternDeclarationSpec[]>([]);
  let declareEntities = $state(true);
  let onError = $state<PatternOnError>('abort');
  let commitMessage = $state(PATTERN_PRESETS[0].commitMessage);
  let fileName = $state<string | null>(null);

  const MAX_UPLOAD_BYTES = 2 * 1024 * 1024;

  // --- preview & apply state --------------------------------------------------------------
  let preview = $state<PatternPreviewResponse | null>(null);
  let dryPreview = $state<WritePreview | null>(null);
  let previewSignature = $state<string | null>(null);
  let previewPinned = $state<{
    target: string;
    sel: Selection;
    req: PatternPreviewRequest;
  } | null>(null);
  let statusFilter = $state<'all' | 'new' | 'existing'>('all');
  let searchFilter = $state('');
  let readBusy = $state<'preview' | 'dry' | null>(null);
  let applying = $state(false);
  let error = $state<unknown>(null);
  let notice = $state<string | null>(null);

  const anyBusy = $derived(applying || readBusy != null);
  const run = new LatestRun();
  let destroyed = false;
  onDestroy(() => {
    destroyed = true;
    run.abortAll();
  });

  const headers = $derived(parsePreviewHeaders(tableData, format));
  const parsedPrefixes = $derived(parsePrefixLines(prefixText));
  const inputSignature = $derived(
    patternInputSignature({
      target,
      sel,
      mode,
      format,
      tableData,
      baseIri,
      prefixText,
      headerTemplateRow,
      robotColumns,
      rules,
      declarations,
      declareEntities,
    }),
  );

  let lastInputSignature: string | null = null;
  $effect(() => {
    const sig = inputSignature;
    if (lastInputSignature !== null && lastInputSignature !== sig) {
      // Abort read-only preview/dry-run requests on authoring edits, but never abort
      // an in-flight commit so its result and workbench refresh are always reconciled.
      run.claim('preview');
      run.claim('dry');
      readBusy = null;
      preview = null;
      dryPreview = null;
      previewSignature = null;
      previewPinned = null;
    }
    lastInputSignature = sig;
  });

  let lastOnError: PatternOnError | null = null;
  $effect(() => {
    const policy = onError;
    if (lastOnError !== null && lastOnError !== policy) {
      run.claim('dry');
      if (readBusy === 'dry') readBusy = null;
      dryPreview = null;
    }
    lastOnError = policy;
  });

  const filteredAxioms = $derived.by(() => {
    if (!preview) return [];
    const needle = searchFilter.trim().toLowerCase();
    return preview.axioms.filter((a) => {
      if (statusFilter !== 'all' && a.status !== statusFilter) return false;
      if (!needle) return true;
      return (
        a.ofn.toLowerCase().includes(needle) ||
        (a.manchester?.toLowerCase().includes(needle) ?? false) ||
        a.type.toLowerCase().includes(needle)
      );
    });
  });

  function loadPreset(preset: PatternPreset) {
    activePreset = preset.id;
    mode = preset.spec.mode;
    format = preset.format;
    tableData = preset.data;
    baseIri = preset.spec.baseIri ?? '';
    prefixText = formatPrefixLines(preset.spec.prefixes);
    commitMessage = preset.commitMessage;
    fileName = null;
    if (preset.spec.mode === 'robot') {
      headerTemplateRow = preset.spec.headerTemplateRow ?? true;
      robotColumns = (preset.spec.columns ?? []).map((c) => ({ ...c }));
    } else {
      rules = (preset.spec.rules ?? []).map((r) => ({
        ...r,
        requiredColumns: r.requiredColumns ? [...r.requiredColumns] : undefined,
        split: r.split ? { ...r.split } : undefined,
        annotations: r.annotations ? r.annotations.map((a) => ({ ...a })) : undefined,
      }));
      declarations = (preset.spec.declarations ?? []).map((d) => ({ ...d }));
    }
    preview = null;
    dryPreview = null;
    previewSignature = null;
    previewPinned = null;
    error = null;
    notice = null;
  }

  function buildSpec(): PatternSpec | null {
    if (parsedPrefixes.error) {
      error = new Error(parsedPrefixes.error);
      return null;
    }
    return buildPatternSpec({
      mode,
      baseIri,
      prefixes: parsedPrefixes.prefixes,
      headerTemplateRow,
      robotColumns,
      rules,
      declarations,
    });
  }

  async function onFileUpload(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    try {
      if (file.size > MAX_UPLOAD_BYTES) {
        error = new Error(
          `File "${file.name}" is ${fmtInt(file.size)} bytes, exceeding the 2 MiB spreadsheet limit.`,
        );
        return;
      }
      error = null;
      fileName = file.name;
      if (file.name.toLowerCase().endsWith('.tsv')) format = 'tsv';
      else if (file.name.toLowerCase().endsWith('.csv')) format = 'csv';
      tableData = await file.text();
      preview = null;
      dryPreview = null;
      previewSignature = null;
      previewPinned = null;
    } finally {
      input.value = '';
    }
  }

  function addRobotColumn(col = '') {
    robotColumns = [...robotColumns, { column: col, directive: 'A rdfs:comment', split: '' }];
  }

  function removeRobotColumn(idx: number) {
    robotColumns = robotColumns.filter((_, i) => i !== idx);
  }

  function addRule() {
    rules = [
      ...rules,
      {
        name: `rule-${rules.length + 1}`,
        template: '',
        syntax: 'manchester',
        skipIfEmpty: true,
      },
    ];
  }

  function removeRule(idx: number) {
    rules = rules.filter((_, i) => i !== idx);
  }

  function insertPlaceholder(col: string) {
    if (mode === 'robot') {
      if (!robotColumns.some((c) => c.column === col)) {
        addRobotColumn(col);
      }
      return;
    }
    if (rules.length === 0) addRule();
    const last = rules.length - 1;
    rules[last] = {
      ...rules[last],
      template: `${rules[last].template}${rules[last].template ? ' ' : ''}\${${col}}`,
    };
  }

  function addDeclaration() {
    declarations = [...declarations, { column: headers[0] ?? '', kind: 'Class', split: '' }];
  }

  function removeDeclaration(idx: number) {
    declarations = declarations.filter((_, i) => i !== idx);
  }

  function handlePreconditionError(e: unknown): boolean {
    if (e instanceof api.ApiError && e.status === 412) {
      preview = null;
      dryPreview = null;
      previewSignature = null;
      previewPinned = null;
      error = new Error(
        'The ontology, its import map, or dataset prefixes changed since your preview. Run Preview axioms again to review against the latest commit.',
      );
      return true;
    }
    return false;
  }

  async function runPreview() {
    if (applying) return;
    const spec = buildSpec();
    if (!spec) return;
    const snapTarget = target;
    const snapSel: Selection = { graph: sel.graph, imports: sel.imports };
    const snapSig = inputSignature;
    const current = run.claim('preview');
    run.claim('dry');
    readBusy = 'preview';
    error = null;
    notice = null;
    dryPreview = null;
    const req: PatternPreviewRequest = {
      table: { data: tableData, format },
      spec,
      declare: declareEntities,
      limit: 500,
    };
    try {
      const res = await previewPatterns(snapTarget, snapSel, req, {
        signal: run.signal('preview'),
      });
      if (current() && inputSignature === snapSig) {
        preview = res;
        previewSignature = snapSig;
        previewPinned = { target: snapTarget, sel: snapSel, req };
      }
    } catch (e) {
      if (current() && !isAbort(e)) error = e;
    } finally {
      if (current() && readBusy === 'preview') readBusy = null;
    }
  }

  async function runDryRun() {
    if (applying || !preview || !previewPinned || previewSignature !== inputSignature) return;
    const pinned = previewPinned;
    const pinnedEtag = preview.etag ?? etag;
    const snapSig = inputSignature;
    const snapPolicy = onError;
    const current = run.claim('dry');
    readBusy = 'dry';
    error = null;
    notice = null;
    try {
      const res = await dryRunPatterns(
        pinned.target,
        pinned.sel,
        {
          table: pinned.req.table,
          spec: pinned.req.spec,
          declare: pinned.req.declare,
          message: commitMessage.trim() || 'Generate axioms from pattern',
          onError: snapPolicy,
        },
        { etag: pinnedEtag, changes: 50, signal: run.signal('dry') },
      );
      if (current() && inputSignature === snapSig && onError === snapPolicy) {
        dryPreview = res;
      }
    } catch (e) {
      if (current() && !isAbort(e) && !handlePreconditionError(e)) error = e;
    } finally {
      if (current() && readBusy === 'dry') readBusy = null;
    }
  }

  async function commitApply() {
    if (applying || !preview || !previewPinned || previewSignature !== inputSignature) return;
    const pinned = previewPinned;
    const pinnedEtag = preview.etag ?? etag;
    const submittedPolicy = onError;
    const submittedMessage = commitMessage.trim() || 'Generate axioms from pattern';
    // Cancel any in-flight read-only preview/dry-run before committing, but keep the
    // commit itself un-aborted so authoring edits cannot hide its outcome.
    run.claim('preview');
    run.claim('dry');
    readBusy = null;
    applying = true;
    error = null;
    notice = null;
    try {
      const res = await applyPatterns(
        pinned.target,
        pinned.sel,
        {
          table: pinned.req.table,
          spec: pinned.req.spec,
          declare: pinned.req.declare,
          message: submittedMessage,
          onError: submittedPolicy,
        },
        { etag: pinnedEtag },
      );
      if (destroyed) return;
      dryPreview = null;
      notice = res.committed
        ? `Committed ${fmtInt(res.added)} new axiom${res.added === 1 ? '' : 's'} (${fmtInt(res.inserted)} triple${res.inserted === 1 ? '' : 's'}) from ${fmtInt(res.pattern.rowsProcessed)} row${res.pattern.rowsProcessed === 1 ? '' : 's'}.`
        : `No new axioms to commit (${fmtInt(res.present)} already present).`;
      const seq = res.receipt?.commit?.seq ?? null;
      oncommitted?.(seq);
      // Clear preview against the old head so the user previews again if re-applying.
      preview = null;
      previewSignature = null;
      previewPinned = null;
    } catch (e) {
      if (!destroyed && !handlePreconditionError(e)) error = e;
    } finally {
      if (!destroyed) applying = false;
    }
  }

  const canApply = $derived(
    !readonly &&
      !anyBusy &&
      preview != null &&
      previewPinned != null &&
      previewSignature === inputSignature &&
      tableData.trim().length > 0 &&
      (preview.valid || onError === 'skip'),
  );
</script>

<div class="patterns">
  <div class="presets" role="group" aria-label="Starter presets">
    <span class="small faint">Starter presets:</span>
    {#each PATTERN_PRESETS as p (p.id)}
      <button
        type="button"
        class="btn sm"
        class:primary={activePreset === p.id}
        title={p.description}
        onclick={() => loadPreset(p)}>{p.label}</button
      >
    {/each}
  </div>

  {#if error}<div class="error-box" role="alert">{api.errorMessage(error)}</div>{/if}
  {#if applying}
    <div class="notice" role="status"><span class="spinner"></span> Committing pattern axioms…</div>
  {:else if notice}
    <div class="notice" role="status">{notice}</div>
  {/if}

  <div class="grid">
    <section class="card" aria-label="Spreadsheet input">
      <div class="card-head">
        <h3>1. Spreadsheet data (CSV / TSV)</h3>
        <div class="row">
          <label class="small">
            Format
            <select class="input sm" bind:value={format} aria-label="Table format">
              <option value="auto">Auto</option>
              <option value="csv">CSV</option>
              <option value="tsv">TSV</option>
            </select>
          </label>
          <label class="btn sm">
            Upload CSV/TSV
            <input
              type="file"
              accept=".csv,.tsv,.txt,text/csv,text/tab-separated-values"
              hidden
              onchange={onFileUpload}
            />
          </label>
        </div>
      </div>
      {#if fileName}
        <p class="small faint">Loaded file: <span class="mono">{fileName}</span></p>
      {/if}
      <textarea
        class="input mono table-input"
        rows="9"
        spellcheck="false"
        aria-label="Spreadsheet CSV or TSV rows"
        placeholder="ID,Label,Parent&#10;ID,LABEL@en,SC %&#10;ex:Heart,Heart,ex:Organ"
        bind:value={tableData}></textarea>
      {#if headers.length > 0}
        <div class="chips" aria-label="Detected columns">
          <span class="small faint">Columns:</span>
          {#each headers as h, i (`${i}:${h}`)}
            <button
              type="button"
              class="chip mono"
              title={mode === 'robot' ? `Bind column ${h}` : `Insert \${${h}}`}
              onclick={() => insertPlaceholder(h)}>{h}</button
            >
          {/each}
        </div>
      {/if}
    </section>

    <section class="card" aria-label="Template and pattern specification">
      <div class="card-head">
        <h3>2. Template / pattern rules</h3>
        <div class="segmented" role="radiogroup" aria-label="Template mode">
          <button
            type="button"
            class="seg"
            role="radio"
            aria-checked={mode === 'robot'}
            onclick={() => (mode = 'robot')}>ROBOT template</button
          >
          <button
            type="button"
            class="seg"
            role="radio"
            aria-checked={mode === 'pattern'}
            onclick={() => (mode = 'pattern')}>DOSDP / Cellfie patterns</button
          >
        </div>
      </div>

      {#if mode === 'robot'}
        <p class="small faint">
          Directives: <span class="mono">ID</span>, <span class="mono">LABEL[@lang]</span>,
          <span class="mono">TYPE</span>, <span class="mono">SC %</span>,
          <span class="mono">EC %</span>, <span class="mono">DC %</span>,
          <span class="mono">C %</span>, <span class="mono">I %</span>,
          <span class="mono">A &lt;prop&gt;</span>, <span class="mono">AI &lt;prop&gt;</span>,
          <span class="mono">&gt;A &lt;prop&gt;</span>, and <span class="mono">SPLIT=|</span>.
        </p>
        <label class="check-row">
          <input type="checkbox" bind:checked={headerTemplateRow} />
          <span>Read directives from row 2 of the spreadsheet</span>
        </label>
        <div class="subhead">
          <span class="small">Explicit column directives (optional)</span>
          <button type="button" class="btn sm" onclick={() => addRobotColumn(headers[0] ?? '')}
            >Add directive</button
          >
        </div>
        {#each robotColumns as col, i (i)}
          <div class="rule-row">
            <input
              class="input sm mono"
              placeholder="Column header"
              aria-label="Column header"
              bind:value={col.column}
            />
            <input
              class="input sm mono grow"
              placeholder="SC % or A rdfs:comment"
              aria-label="ROBOT directive"
              bind:value={col.directive}
            />
            <input
              class="input sm mono split-in"
              placeholder="Split"
              aria-label="Split delimiter"
              bind:value={col.split}
            />
            <button
              type="button"
              class="btn sm ghost"
              aria-label="Remove directive"
              onclick={() => removeRobotColumn(i)}>Remove</button
            >
          </div>
        {/each}
      {:else}
        <p class="small faint">
          Placeholders: <span class="mono">${'{col}'}</span>,
          <span class="mono">${'{col|iri}'}</span>, <span class="mono">${'{col|slug}'}</span>,
          <span class="mono">${'{col|str}'}</span>, <span class="mono">${'{col|lang(en)}'}</span>,
          <span class="mono">${'{col|type(xsd:integer)}'}</span>.
        </p>
        <div class="subhead">
          <span class="small">Axiom rules</span>
          <button type="button" class="btn sm" onclick={addRule}>Add rule</button>
        </div>
        {#each rules as r, i (i)}
          <div class="pattern-rule">
            <div class="rule-row">
              <input
                class="input sm"
                placeholder="Rule name"
                aria-label="Rule name"
                bind:value={r.name}
              />
              <select class="input sm" aria-label="Rule syntax" bind:value={r.syntax}>
                <option value="manchester">Manchester</option>
                <option value="functional">Functional (OFN)</option>
              </select>
              <label class="check-row small">
                <input type="checkbox" bind:checked={r.skipIfEmpty} />
                <span>Skip if empty</span>
              </label>
              <button
                type="button"
                class="btn sm ghost"
                aria-label="Remove rule"
                onclick={() => removeRule(i)}>Remove</button
              >
            </div>
            <input
              class="input sm mono full"
              placeholder="${'{class|iri}'} SubClassOf ${'{parent|iri}'}"
              aria-label="Axiom template"
              bind:value={r.template}
            />
          </div>
        {/each}

        <div class="subhead">
          <span class="small">Entity declarations (optional)</span>
          <button type="button" class="btn sm" onclick={addDeclaration}>Add declaration</button>
        </div>
        {#each declarations as d, i (i)}
          <div class="rule-row">
            <input
              class="input sm mono"
              placeholder="Column"
              aria-label="Declaration column"
              bind:value={d.column}
            />
            <select class="input sm" aria-label="Entity kind" bind:value={d.kind}>
              {#each ENTITY_KINDS as k (k)}
                <option value={k}>{k}</option>
              {/each}
            </select>
            <input
              class="input sm mono split-in"
              placeholder="Split"
              aria-label="Declaration split"
              bind:value={d.split}
            />
            <button
              type="button"
              class="btn sm ghost"
              aria-label="Remove declaration"
              onclick={() => removeDeclaration(i)}>Remove</button
            >
          </div>
        {/each}
      {/if}

      <div class="options-grid">
        <label class="field">
          <span class="small">Base IRI (optional)</span>
          <input
            class="input sm mono"
            placeholder="http://example.org/onto#"
            bind:value={baseIri}
          />
        </label>
        <label class="field">
          <span class="small"
            >Extra prefixes (one <span class="mono">prefix=iri</span> per line)</span
          >
          <textarea
            class="input sm mono"
            rows="2"
            spellcheck="false"
            placeholder="ex=http://example.org/onto#"
            bind:value={prefixText}></textarea>
        </label>
      </div>
      {#if parsedPrefixes.error}
        <p class="small warn-text">{parsedPrefixes.error}</p>
      {/if}

      <div class="row options-bar">
        <label class="check-row small">
          <input type="checkbox" bind:checked={declareEntities} />
          <span>Auto-declare referenced entities</span>
        </label>
        <label class="small row">
          <span>On row error</span>
          <select class="input sm" bind:value={onError} aria-label="On row error">
            <option value="abort">Abort commit</option>
            <option value="skip">Skip bad rows</option>
          </select>
        </label>
        <button
          type="button"
          class="btn primary"
          disabled={anyBusy || !tableData.trim()}
          onclick={runPreview}
        >
          {#if readBusy === 'preview'}<span class="spinner"></span>{/if}
          Preview axioms
        </button>
      </div>
    </section>
  </div>

  {#if preview}
    <section class="card preview-card" aria-label="Generated axioms preview">
      <div class="card-head">
        <h3>3. Preview &amp; atomic commit</h3>
        <div class="stats" role="status">
          <span class="stat">{fmtInt(preview.rowsProcessed)} rows</span>
          <span class="stat ok">{fmtInt(preview.newAxioms)} new</span>
          <span class="stat">{fmtInt(preview.existingAxioms)} present</span>
          <span class="stat">{fmtInt(preview.autoDeclarations.length)} auto-decl</span>
          <span class="stat">~{fmtInt(preview.estimatedTriples)} triples</span>
          {#if preview.rowsWithErrors > 0}
            <span class="stat err">{fmtInt(preview.rowsWithErrors)} row error(s)</span>
          {/if}
        </div>
      </div>

      {#if preview.diagnostics.length > 0}
        <div class="diag-box" aria-label="Row diagnostics">
          <h4>
            Diagnostics ({fmtInt(preview.diagnostics.length)}{preview.diagnosticsTruncated
              ? '+'
              : ''})
          </h4>
          <div class="table-wrap">
            <table class="table">
              <thead>
                <tr
                  ><th>Line</th><th>Column / Rule</th><th>Severity</th><th>Code</th><th>Message</th
                  ></tr
                >
              </thead>
              <tbody>
                {#each preview.diagnostics as d, i (i)}
                  <tr>
                    <td class="mono">{d.row}</td>
                    <td class="mono">{d.column ?? '—'}</td>
                    <td>
                      <span
                        class="badge"
                        class:err={d.severity === 'error'}
                        class:warn={d.severity === 'warning'}>{d.severity}</span
                      >
                    </td>
                    <td class="mono small">{d.code}</td>
                    <td>{d.message}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </div>
      {/if}

      <div class="filter-bar">
        <div class="segmented" role="radiogroup" aria-label="Filter axioms by status">
          <button
            type="button"
            class="seg"
            role="radio"
            aria-checked={statusFilter === 'all'}
            onclick={() => (statusFilter = 'all')}>All ({fmtInt(preview.axioms.length)})</button
          >
          <button
            type="button"
            class="seg"
            role="radio"
            aria-checked={statusFilter === 'new'}
            onclick={() => (statusFilter = 'new')}>New ({fmtInt(preview.newAxioms)})</button
          >
          <button
            type="button"
            class="seg"
            role="radio"
            aria-checked={statusFilter === 'existing'}
            onclick={() => (statusFilter = 'existing')}
            >Present ({fmtInt(preview.existingAxioms)})</button
          >
        </div>
        <input
          class="input sm grow"
          placeholder="Filter generated axioms…"
          aria-label="Filter generated axioms"
          bind:value={searchFilter}
        />
      </div>

      {#if preview.axiomsTruncated}
        <p class="small faint">
          Showing the first {fmtInt(preview.axioms.length)} of {fmtInt(preview.generated)} generated axioms.
          All {fmtInt(preview.generated)} will be evaluated on apply.
        </p>
      {/if}

      {#if filteredAxioms.length > 0}
        <div class="table-wrap axioms-table">
          <table class="table" aria-label="Generated axioms">
            <thead>
              <tr><th>Status</th><th>Type</th><th>Axiom</th><th>Line(s)</th></tr>
            </thead>
            <tbody>
              {#each filteredAxioms as ax, i (i)}
                <tr>
                  <td>
                    <span class="badge" class:ok={ax.status === 'new'}
                      >{ax.status === 'new' ? 'New' : 'Present'}</span
                    >
                  </td>
                  <td class="small">{ax.type}</td>
                  <td class="mono small">
                    {#if ax.manchester}
                      <div>{ax.manchester}</div>
                      <div class="faint">{ax.ofn}</div>
                    {:else}
                      <div>{ax.ofn}</div>
                    {/if}
                  </td>
                  <td class="mono small">{ax.rows.join(', ')}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else}
        <p class="faint">No axioms match the current filter.</p>
      {/if}

      {#if dryPreview}
        <div class="dry-box" aria-label="Dry-run triple diff">
          <h4>
            Dry-run triple diff (+{fmtInt(dryPreview.commit?.inserted ?? 0)} / −{fmtInt(
              dryPreview.commit?.deleted ?? 0,
            )} triples)
          </h4>
          {#if dryPreview.changes?.quads?.length}
            <DiffLines
              quads={dryPreview.changes.quads}
              total={dryPreview.changes.total}
              ds={target}
            />
          {:else if (dryPreview.commit?.inserted ?? 0) > 0 || (dryPreview.commit?.deleted ?? 0) > 0}
            <p class="small faint">Triple sample not included in dry-run response.</p>
          {:else}
            <p class="small faint">No triple changes would be written.</p>
          {/if}
        </div>
      {/if}

      <div class="commit-bar">
        {#if readonly}
          <p class="small faint">
            Read-only or protected branch: preview is available, but committing requires write
            access on an unprotected branch.
          </p>
        {:else}
          <input
            class="input sm grow"
            placeholder="Commit message"
            aria-label="Commit message"
            bind:value={commitMessage}
          />
          <button type="button" class="btn" disabled={!canApply} onclick={runDryRun}>
            {#if readBusy === 'dry'}<span class="spinner"></span>{/if}
            Check dry-run diff
          </button>
          <button type="button" class="btn primary" disabled={!canApply} onclick={commitApply}>
            {#if applying}<span class="spinner"></span>{/if}
            Apply as one commit
          </button>
        {/if}
      </div>
    </section>
  {/if}
</div>

<style>
  .patterns {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: 1rem;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.9rem;
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
  .card-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .card-head h3 {
    margin: 0;
    font-size: 0.95rem;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .table-input {
    width: 100%;
    resize: vertical;
    font-size: 0.82rem;
    line-height: 1.4;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.35rem;
  }
  .chip {
    border: 1px solid var(--border);
    background: var(--bg-subtle, transparent);
    border-radius: 999px;
    padding: 0.15rem 0.55rem;
    font-size: 0.78rem;
    cursor: pointer;
  }
  .chip:hover {
    border-color: var(--accent);
  }
  .subhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .rule-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
  }
  .pattern-rule {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .grow {
    flex: 1 1 12rem;
  }
  .full {
    width: 100%;
  }
  .split-in {
    width: 4.5rem;
  }
  .options-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 0.5rem;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .check-row {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
  }
  .options-bar {
    justify-content: space-between;
    padding-top: 0.25rem;
  }
  .stats {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .stat {
    font-size: 0.8rem;
    padding: 0.15rem 0.5rem;
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }
  .stat.ok {
    border-color: var(--success, #2e7d32);
  }
  .stat.err {
    border-color: var(--danger, #c62828);
  }
  .filter-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .axioms-table {
    max-height: 24rem;
    overflow: auto;
  }
  .diag-box,
  .dry-box {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.65rem;
  }
  .diag-box h4,
  .dry-box h4 {
    margin: 0 0 0.4rem 0;
    font-size: 0.85rem;
  }
  .commit-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
    padding-top: 0.35rem;
    border-top: 1px solid var(--border);
  }
  .badge.ok {
    background: color-mix(in srgb, var(--success, #2e7d32) 15%, transparent);
  }
  .badge.err {
    background: color-mix(in srgb, var(--danger, #c62828) 18%, transparent);
  }
  .warn-text {
    color: var(--danger, #c62828);
  }
</style>
