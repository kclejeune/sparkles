<script lang="ts">
  // An editing form generated from SHACL shapes (docs/specs/U02-shape-driven-forms.md):
  // the fields the server's form model lists, grouped and ordered by the shapes, checked
  // in the browser on each change, previewed with the server's check and saved as one
  // commit on a branch or on main. A save sends the values the form loaded as the
  // expected values of the fields it changes, and a conflict shows them beside the stored
  // values and the person's own.
  import { resolve } from '$app/paths';
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { MAIN } from '$lib/branches';
  import { LABEL_IRIS } from '$lib/explore';
  import { fmtInt } from '$lib/format';
  import {
    blocking,
    changeCount,
    changeSet,
    checkField,
    entryOf,
    fieldDomId,
    grouped,
    resultText,
    savePlan,
    widgetOf,
    type Entry,
    type FieldResult,
    type FormField as Field,
    type FormModel,
    type Problem,
  } from '$lib/forms';
  import {
    checkForm,
    getForm,
    saveForm,
    type FieldConflict,
    type FormCheckAnswer,
    type FormSaveAnswer,
  } from '$lib/forms-api';
  import { displayIri, displayTerm, type PrefixMap } from '$lib/rdf';
  import DiffLines from './DiffLines.svelte';
  import FormField from './FormField.svelte';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    ds,
    iri,
    branch = $bindable(null),
    prefixes,
    title,
    oncancel,
    onsaved,
    ondraft,
    cls,
  }: {
    /** The dataset (its main). */
    ds: string;
    /** The resource the form edits (empty for a new instance). */
    iri: string;
    /** A new instance of this class instead of `iri` (U02 Phase 2). */
    cls?: string;
    /** The branch the form is open on; a save to a new session branch moves it there. */
    branch?: string | null;
    prefixes: PrefixMap;
    /** The resource's name in the header. */
    title?: string;
    oncancel?: () => void;
    onsaved?: (a: FormSaveAnswer) => void;
    /** Open the dialog that drafts shapes from the data (offered to admins). */
    ondraft?: () => void;
  } = $props();

  const lang = typeof navigator === 'undefined' ? 'en' : (navigator.language || 'en').slice(0, 2);

  let model = $state<FormModel | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(false);
  let entries = $state<Record<string, Entry[]>>({});
  let message = $state('');
  let check = $state<{ answer: FormCheckAnswer; key: string } | null>(null);
  let checking = $state(false);
  let checkError = $state<string | null>(null);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let conflicts = $state<FieldConflict[] | null>(null);
  let summaryShown = $state(false);
  /** The change set the person was asked to confirm saving with problems. */
  let confirmed = $state<string | null>(null);
  let blockedWhy = $state<string | null>(null);
  let branchList = $state<api.Branch[]>([]);
  let summaryEl = $state<HTMLElement>();
  let ctl: AbortController | undefined;

  const STATE_MSG = 'The form shows a state you cannot save.';

  /** The widget of each field, by id. */
  const widgets = $derived(
    Object.fromEntries((model?.fields ?? []).map((f) => [f.id, widgetOf(f)])),
  );

  function entriesOf(f: Field, m: FormModel): Entry[] {
    const w = widgetOf(f);
    const out = f.values.map((v) => entryOf(v.term, w, v.label));
    const list = (w === 'radio' || w === 'select') && (f.constraints.in?.length ?? 0) > 0;
    if (!out.length && f.editable && m.editable && !list) {
      const e = entryOf(null, w);
      if (w === 'text-lang') e.lang = f.constraints.languageIn?.[0] ?? lang;
      out.push(e);
    }
    return out;
  }

  async function load() {
    ctl?.abort();
    const c = (ctl = new AbortController());
    loading = true;
    loadError = null;
    try {
      const m = await getForm(ds, iri, {
        branch,
        lang,
        signal: c.signal,
        ...(cls ? { class: cls } : {}),
      });
      if (c !== ctl) return;
      model = m;
      entries = Object.fromEntries(m.fields.map((f) => [f.id, entriesOf(f, m)]));
      check = null;
      conflicts = null;
      summaryShown = false;
      confirmed = null;
      blockedWhy = null;
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      if (c === ctl) loadError = api.errorMessage(e);
    } finally {
      if (c === ctl) loading = false;
    }
  }

  $effect(() => {
    void ds;
    void iri;
    void cls;
    untrack(() => load());
    return () => ctl?.abort();
  });

  /** The dataset's full-text index is ready: the entity pickers search with it. */
  let fullText = $state(false);
  $effect(() => {
    const name = ds;
    api.textStatus(name).then(
      (t) => {
        if (name === ds) fullText = t?.state === 'ready';
      },
      () => (fullText = false),
    );
  });

  $effect(() => {
    const name = ds;
    api.branches(name).then(
      (l) => {
        if (name === ds) branchList = l.branches;
      },
      () => (branchList = []),
    );
  });

  const editable = $derived(!!model?.editable);
  /** The terms of each field now. */
  const current = $derived(
    Object.fromEntries(Object.entries(entries).map(([id, es]) => [id, es.map((e) => e.term)])),
  );
  /** The browser's problems, by field (editable fields only: the others do not change). */
  const problems = $derived.by(() => {
    const out: Record<string, Problem[]> = {};
    for (const f of model?.fields ?? []) {
      if (!f.editable || !editable) continue;
      const ps = checkField(f, current[f.id] ?? []);
      (entries[f.id] ?? []).forEach((e, i) => {
        if (e.error) ps.push({ index: i, message: e.error });
      });
      if (ps.length) out[f.id] = ps;
    }
    return out;
  });
  const problemCount = $derived(Object.values(problems).reduce((n, p) => n + p.length, 0));
  const changes = $derived(model ? changeSet(model, current) : { changes: [], expect: {} });
  const nChanges = $derived(changeCount(changes));
  const changeKey = $derived(JSON.stringify(changes));
  const checkCurrent = $derived(!!check && check.key === changeKey);
  const results = $derived(checkCurrent ? (check?.answer.validation.results ?? []) : []);
  const resultsByField = $derived.by(() => {
    const out: Record<string, FieldResult[]> = {};
    for (const r of results) if (r.field) (out[r.field] ??= []).push(r);
    return out;
  });
  const otherResults = $derived(results.filter((r) => !r.field));
  const rejected = $derived(checkCurrent && check?.answer.preview.outcome === 'rejected');
  const plan = $derived(
    model
      ? savePlan(
          model.guard,
          problemCount,
          checkCurrent && check
            ? {
                conforms: check.answer.validation.conforms,
                rejected: check.answer.preview.outcome === 'rejected',
                current: true,
              }
            : null,
        )
      : 'blocked',
  );
  const confirming = $derived(confirmed !== null && confirmed === changeKey);
  /** The resource's name: the given title, its label in the form, or its IRI. */
  const name = $derived.by(() => {
    if (title) return title;
    const f = model?.fields.find((x) => LABEL_IRIS.includes(x.path) && x.values.length);
    const v = f?.values.find((x) => x.term.type === 'literal');
    return v && v.term.type === 'literal' ? v.term.value : displayIri(iri, prefixes);
  });
  const heading = $derived(cls ? `New ${displayIri(cls, prefixes)}` : `Edit ${name}`);
  const openBranch = $derived(model?.branch ?? branch ?? MAIN);
  const branchInfo = $derived(branchList.find((b) => b.name === openBranch));

  function body() {
    return {
      ...(cls ? { class: cls } : { focus: iri }),
      ...(openBranch !== MAIN ? { branch: openBranch } : {}),
      changes: changes.changes,
      expect: changes.expect,
      ...(message.trim() ? { message: message.trim() } : {}),
    };
  }

  async function runCheck(): Promise<FormCheckAnswer | null> {
    if (!nChanges) return null;
    const key = changeKey;
    checking = true;
    checkError = null;
    try {
      const answer = await checkForm(ds, body());
      check = { answer, key };
      if (answer.conflicts?.length) {
        conflicts = answer.conflicts;
        showSummary('Another commit changed some of these fields since the form loaded them.');
      }
      return answer;
    } catch (e) {
      checkError = api.errorMessage(e);
      return null;
    } finally {
      checking = false;
    }
  }

  function showSummary(why: string | null = null) {
    blockedWhy = why;
    summaryShown = true;
    queueMicrotask(() => summaryEl?.focus());
  }

  async function onSave() {
    if (!model || !nChanges || saving) return;
    if (!editable) return showSummary(STATE_MSG);
    if (conflicts?.length)
      return showSummary('Choose a value for each field another commit changed.');
    let p = plan;
    if (p === 'check') {
      const a = await runCheck();
      if (!a) return showSummary(checkError ?? 'The changes could not be checked.');
      if (a.conflicts?.length) return;
      p = a.preview.outcome === 'rejected' ? 'blocked' : 'save';
    }
    if (p === 'blocked')
      return showSummary(
        'The write-time guard would reject this change. Fix the problems below and save again.',
      );
    if (p === 'confirm' && !confirming) {
      confirmed = changeKey;
      return showSummary(null);
    }
    await doSave();
  }

  async function doSave() {
    saving = true;
    saveError = null;
    confirmed = null;
    try {
      const r = await saveForm(ds, body());
      if ('conflict' in r) {
        conflicts = r.conflict;
        showSummary('Another commit changed some of these fields since the form loaded them.');
        return;
      }
      const a = r.saved;
      const where = a.branch === MAIN ? 'main' : `branch ${a.branch}`;
      toasts.push(
        'success',
        a.committed ? `Saved commit ${a.commit.seq} to ${where}` : 'Nothing changed',
        a.branchCreated ? `The branch ${a.branch} was created for your edits.` : undefined,
      );
      message = '';
      if (a.branch !== openBranch) branch = a.branch === MAIN ? null : a.branch;
      onsaved?.(a);
      // a new instance's page moves to the resource it made
      if (cls) return;
      await load();
      api.branches(ds).then(
        (l) => (branchList = l.branches),
        () => {},
      );
    } catch (e) {
      saveError = api.errorMessage(e);
      showSummary(saveError);
    } finally {
      saving = false;
    }
  }

  /** Resolve a conflict: the stored values become the loaded ones, and `take` copies them. */
  function resolveConflict(c: FieldConflict, take: boolean) {
    const f = model?.fields.find((x) => x.id === c.id);
    if (!f || !model) return;
    f.values = c.now.map((term) => ({ term, graph: model!.graph }));
    if (take) entries[f.id] = entriesOf(f, model);
    conflicts = (conflicts ?? []).filter((x) => x.id !== c.id);
    if (!conflicts.length) conflicts = null;
  }

  function changeBranch(b: string) {
    branch = b === MAIN ? null : b;
    load();
  }

  function focusField(id: string, ev: Event) {
    ev.preventDefault();
    const el = document.getElementById(fieldDomId(id));
    el?.scrollIntoView({ block: 'center' });
    el?.focus();
  }

  const fieldOf = (id: string) => model?.fields.find((f) => f.id === id);
  const valuesText = (ts: Term[]) =>
    ts.length ? ts.map((t) => displayTerm(t, prefixes)).join(', ') : 'no value';
  const mine = (id: string) => valuesText((current[id] ?? []).filter((t): t is Term => t !== null));

  const OUTCOME: Record<string, string> = {
    commit: 'The save would commit.',
    'no-change': 'The save would change nothing.',
    rejected: 'The write-time guard would reject the save.',
    'storage-refused': 'The storage limit would refuse the save.',
    'precondition-failed': 'A precondition would fail.',
  };
  const summaryCount = $derived(
    problemCount +
      results.filter(blocking).length +
      (conflicts?.length ?? 0) +
      (blockedWhy ? 1 : 0),
  );
</script>

<section class="sf" aria-label={heading}>
  <header class="sf-head">
    <div class="sf-title">
      <h2>
        <Icon name="pencil" size={14} />
        {heading}
      </h2>
      {#if model?.types.length}
        <span class="faint small">{model.types.map((t) => displayIri(t, prefixes)).join(', ')}</span
        >
      {/if}
    </div>
    {#if branchList.length > 1}
      <label class="branch-pick small">
        <span class="faint">Branch</span>
        <select
          class="select"
          value={openBranch}
          onchange={(e) => changeBranch(e.currentTarget.value)}
          disabled={saving}
        >
          {#each branchList as b (b.name)}<option value={b.name}>{b.name}</option>{/each}
          {#if !branchList.some((b) => b.name === openBranch)}<option value={openBranch}
              >{openBranch}</option
            >{/if}
        </select>
      </label>
    {/if}
  </header>

  {#if loading && !model}
    <p class="faint row"><span class="spinner"></span> Reading the form…</p>
  {:else if loadError}
    <div class="error-box" role="alert">{loadError}</div>
  {:else if model}
    {@const m = model}
    <div class="sf-meta small">
      <span>
        Shapes: {m.shapes.source === 'guard'
          ? 'write-time guard'
          : m.shapes.source === 'graphs'
            ? 'shapes graphs'
            : 'drafted from the data'}
        ({m.shapes.nodeShapes.length}
        {m.shapes.nodeShapes.length === 1 ? 'shape' : 'shapes'})
      </span>
      <details class="view-shapes">
        <summary>View shapes</summary>
        <ul class="mono">
          {#each m.shapes.nodeShapes as s (s)}<li>{displayIri(s, prefixes)}</li>{/each}
        </ul>
        {#if m.shapes.unknownHints.length}
          <p class="faint">
            Editors and viewers the form does not know, shown with the default widget:
            {m.shapes.unknownHints.map((h) => displayIri(h, prefixes)).join(', ')}
          </p>
        {/if}
        {#each m.fields.filter((f) => f.sources.length > 1) as f (f.id)}
          <p class="faint">
            {f.label} is constrained by {f.sources
              .map((s) => displayIri(s, prefixes))
              .join(' and ')}.
          </p>
        {/each}
      </details>
    </div>

    {#if m.shapes.source === 'drafted'}
      <p class="banner warn small" role="note">Shapes drafted from the data, not enforced.</p>
    {/if}
    {#if openBranch !== MAIN}
      <p class="banner small" role="status">
        <Icon name="branch" size={12} />
        <span
          >Editing on branch <strong>{openBranch}</strong>{branchInfo
            ? `: ${fmtInt(branchInfo.ahead)} ${branchInfo.ahead === 1 ? 'commit' : 'commits'}`
            : ''}.</span
        >
        <a
          class="btn sm"
          href="{resolve('/datasets/[name]/merge', { name: ds })}?source={encodeURIComponent(
            openBranch,
          )}">Review and merge</a
        >
      </p>
    {:else if m.save.to === 'branch' && editable}
      <p class="banner small" role="note">
        <Icon name="branch" size={12} />
        {m.save.mainProtected ? 'main is protected, so saves' : 'Saves'} go to branch
        <strong>{m.save.branch}</strong>{m.save.exists ? '' : ', which the first save creates'}.
      </p>
    {/if}
    {#if !editable}
      <p class="faint small">
        This form is read-only: you may not save changes here, or it shows a past state.
      </p>
    {/if}

    {#if m.shapes.missing}
      <p class="faint">{m.shapes.missing}</p>
    {/if}
    {#if m.fields.length === 0}
      <div class="empty-form">
        <p>No shape describes this resource.</p>
        {#if auth.can(ds, 'admin')}
          {#if ondraft}
            <button type="button" class="btn sm" onclick={ondraft}>Draft shapes</button>
          {:else}
            <a class="btn sm" href="{resolve('/explore')}?tab=schema">Draft shapes in Explore</a>
          {/if}
        {/if}
      </div>
    {/if}

    {#if summaryShown && summaryCount > 0}
      <div class="summary" role="alert" tabindex="-1" bind:this={summaryEl}>
        <h3>
          {summaryCount === 1 ? 'There is 1 problem' : `There are ${fmtInt(summaryCount)} problems`}
        </h3>
        {#if blockedWhy}<p>{blockedWhy}</p>{/if}
        <ul>
          {#each Object.entries(problems) as [id, ps] (id)}
            {#each ps as p, i (i)}
              <li>
                <a href="#{fieldDomId(id)}" onclick={(e) => focusField(id, e)}
                  >{fieldOf(id)?.label}: {p.message}</a
                >
              </li>
            {/each}
          {/each}
          {#each results.filter((r) => r.field) as r, i (i)}
            <li>
              <a href="#{fieldDomId(r.field ?? '')}" onclick={(e) => focusField(r.field ?? '', e)}
                >{fieldOf(r.field ?? '')?.label}: {resultText(r)}</a
              >
            </li>
          {/each}
          {#each otherResults as r, i (i)}
            <li>{displayTerm(r.focusNode, prefixes)}: {resultText(r)}</li>
          {/each}
          {#each conflicts ?? [] as c (c.id)}
            <li>
              <a href="#conflict-{fieldDomId(c.id)}"
                >{fieldOf(c.id)?.label}: changed by another commit</a
              >
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    {#if conflicts?.length}
      <section class="conflicts" aria-label="Conflicting changes">
        <h3>Changed by another commit</h3>
        {#each conflicts as c (c.id)}
          {@const f = fieldOf(c.id)}
          <div class="conflict" id="conflict-{fieldDomId(c.id)}" tabindex="-1">
            <strong>{f?.label ?? c.id}</strong>
            <dl>
              <dt>You loaded</dt>
              <dd>{valuesText(f?.values.map((v) => v.term) ?? [])}</dd>
              <dt>Stored now</dt>
              <dd>{valuesText(c.now)}</dd>
              <dt>Yours</dt>
              <dd>{mine(c.id)}</dd>
            </dl>
            <div class="row">
              <button type="button" class="btn sm" onclick={() => resolveConflict(c, false)}
                >Keep mine</button
              >
              <button type="button" class="btn sm" onclick={() => resolveConflict(c, true)}
                >Take the stored values</button
              >
            </div>
          </div>
        {/each}
      </section>
    {/if}

    <form
      class="sf-body"
      novalidate
      onsubmit={(e) => {
        e.preventDefault();
        onSave();
      }}
    >
      {#each grouped(m) as g (g.group?.iri ?? '')}
        <fieldset class="group">
          {#if g.group}<legend>{g.group.label}</legend>{:else}<legend class="sr">Fields</legend
            >{/if}
          {#each g.fields as f (f.id)}
            <FormField
              field={f}
              widget={widgets[f.id] ?? 'term'}
              bind:entries={entries[f.id]}
              editable={editable && f.editable}
              problems={problems[f.id] ?? []}
              results={resultsByField[f.id] ?? []}
              {ds}
              {prefixes}
              {lang}
              {fullText}
            />
          {/each}
        </fieldset>
      {/each}

      {#if m.other.length}
        <fieldset class="group other">
          <legend>Other properties <span class="faint">not in the shapes</span></legend>
          {#if m.closed}
            <p class="problem small">
              A shape of this resource is closed, so these properties violate it.
            </p>
          {/if}
          <table class="props">
            <tbody>
              {#each m.other as o (o.predicate)}
                <tr>
                  <th title={o.predicate}>{displayIri(o.predicate, prefixes)}</th>
                  <td>
                    {#each o.values as v, i (i)}<div>
                        <TermView term={v.term} {prefixes} />
                      </div>{/each}
                  </td>
                  <td class="faint small">read-only</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </fieldset>
      {/if}

      {#if checkCurrent && check}
        {@const a = check.answer}
        <section class="preview small" aria-label="Preview" aria-live="polite">
          <p>
            <strong>{OUTCOME[a.preview.outcome] ?? a.preview.outcome}</strong>
            {#if a.preview.commit}
              {fmtInt(a.preview.commit.inserted)} added, {fmtInt(a.preview.commit.deleted)} removed.
            {/if}
            {#if a.branch !== MAIN}<span class="faint">On branch {a.branch}.</span>{/if}
          </p>
          {#if a.preview.changes?.quads.length}
            <DiffLines
              quads={a.preview.changes.quads}
              total={a.preview.changes.total}
              label="Changes the save would make"
              maxHeight={180}
            />
          {/if}
          {#if a.validation.withheld}
            <p class="faint">
              Your access hides part of the dataset, so the validation results are withheld.
            </p>
          {:else if a.validation.timedOut}
            <p class="warn-text">
              The shapes could not be checked within 10 s. Save follows the guard's mode.
            </p>
          {:else if a.validation.conforms}
            <p class="ok-text">The resource conforms to its shapes after the change.</p>
          {:else if a.validation.results}
            <p class="warn-text">
              {fmtInt(a.validation.results.length)}
              {a.validation.results.length === 1 ? 'result' : 'results'} against the shapes, shown under
              their fields.
            </p>
            {#if otherResults.length}
              <ul>
                {#each otherResults as r, i (i)}
                  <li>
                    {displayTerm(r.focusNode, prefixes)}: {resultText(r)}
                  </li>
                {/each}
              </ul>
            {/if}
          {/if}
        </section>
      {/if}
      {#if checkError}<p class="problem small" role="alert">{checkError}</p>{/if}

      {#if editable && m.fields.some((f) => f.editable)}
        <div class="bar">
          <span class="counts small" aria-live="polite">
            {problemCount === 1 ? '1 problem' : `${fmtInt(problemCount)} problems`} · {nChanges ===
            1
              ? '1 change'
              : `${fmtInt(nChanges)} changes`}
            {#if rejected}<span class="problem"> · the guard would reject it</span>{/if}
          </span>
          <label class="msg small">
            <span>Message</span>
            <input
              class="input"
              bind:value={message}
              placeholder="What changed and why"
              maxlength="2000"
            />
          </label>
          <div class="actions">
            <button
              type="button"
              class="btn sm"
              disabled={!nChanges || checking}
              onclick={runCheck}
            >
              {#if checking}<span class="spinner"></span>{/if} Preview
            </button>
            {#if oncancel}
              <button type="button" class="btn sm ghost" onclick={oncancel}>Cancel</button>
            {/if}
            {#if confirming}
              <button type="button" class="btn sm danger" onclick={doSave} disabled={saving}
                >Save with problems</button
              >
            {:else}
              <button type="submit" class="btn sm primary" disabled={!nChanges || saving}>
                {#if saving}<span class="spinner"></span>{/if} Save
              </button>
            {/if}
          </div>
        </div>
        {#if confirming}
          <p class="small warn-text" role="status">
            The change has problems that the dataset's guard does not block. Save it anyway, or fix
            them first.
          </p>
        {/if}
        {#if saveError && !summaryShown}<p class="problem small">{saveError}</p>{/if}
      {/if}
    </form>
  {/if}
</section>

<style>
  .sf {
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  .sf-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    justify-content: space-between;
  }
  .sf-title {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .sf-title h2 {
    font-size: var(--fs-md);
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    overflow-wrap: anywhere;
  }
  .branch-pick {
    display: inline-flex;
    gap: 6px;
    align-items: center;
  }
  .sf-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    align-items: baseline;
    color: var(--text-2);
  }
  .view-shapes summary {
    cursor: pointer;
    color: var(--iri);
  }
  .view-shapes ul {
    margin: 4px 0;
    padding-left: 18px;
  }
  .banner {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 6px 10px;
    border-radius: var(--r);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .banner.warn {
    background: color-mix(in srgb, var(--warn) 10%, var(--surface));
  }
  .empty-form {
    display: grid;
    gap: 6px;
    justify-items: start;
  }
  .summary {
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    background: var(--danger-soft);
    border-radius: var(--r);
    padding: 8px 12px;
  }
  .summary:focus {
    outline: 2px solid var(--danger);
    outline-offset: 2px;
  }
  .summary h3 {
    font-size: var(--fs-sm);
    margin: 0 0 4px;
  }
  .summary p {
    margin: 0 0 4px;
    font-size: var(--fs-sm);
  }
  .summary ul {
    margin: 0;
    padding-left: 18px;
    font-size: var(--fs-sm);
  }
  .summary a {
    color: var(--danger);
  }
  .conflicts {
    display: grid;
    gap: 6px;
  }
  .conflicts h3 {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .conflict {
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 8px 10px;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .conflict dl {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 2px 10px;
    margin: 0;
  }
  .conflict dt {
    color: var(--text-2);
  }
  .conflict dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .sf-body {
    display: grid;
    gap: 8px;
    min-width: 0;
  }
  .group {
    border: 0;
    border-top: 1px solid var(--border);
    margin: 0;
    padding: 6px 0 0;
    min-width: 0;
  }
  .group legend {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-2);
    padding: 0 6px 0 0;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .props {
    width: 100%;
    font-size: var(--fs-sm);
    border-collapse: collapse;
  }
  .props th {
    text-align: left;
    font-weight: 500;
    color: var(--iri);
    padding: 2px 8px 2px 0;
    vertical-align: top;
    overflow-wrap: anywhere;
  }
  .props td {
    padding: 2px 0;
    vertical-align: top;
  }
  .preview {
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 8px 10px;
    display: grid;
    gap: 4px;
    background: var(--surface-2);
  }
  .preview p {
    margin: 0;
  }
  .bar {
    position: sticky;
    bottom: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px;
    padding: 8px 0 4px;
    background: var(--surface);
    border-top: 1px solid var(--border);
  }
  .counts {
    color: var(--text-2);
    flex: 1 1 100%;
  }
  .msg {
    display: grid;
    gap: 2px;
    flex: 1 1 180px;
    color: var(--text-2);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .problem {
    color: var(--danger);
    margin: 0;
  }
  .warn-text {
    color: var(--warn);
  }
  .ok-text {
    color: var(--ok);
  }
  .small {
    font-size: var(--fs-xs);
  }
</style>
