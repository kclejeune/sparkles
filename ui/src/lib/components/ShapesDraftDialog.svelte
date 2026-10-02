<script lang="ts">
  // The schema browser's "Draft shapes" dialog: SHACL shapes (in Turtle or SHACLC) or a
  // ShEx schema drafted from the data, the constraints that would reject current instances, and two ways to use the
  // draft (the dataset page's shapes editor, or a write-time guard in warn mode).
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { fmtInt } from '$lib/format';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import {
    componentLabel,
    draftText,
    exclusions,
    parseSupport,
    summaryLine,
    type DraftLang,
  } from '$lib/shapes-draft';
  import { shaclSyntaxKey } from '$lib/shacl';
  import { shexDraftKey, validateLangKey } from '$lib/shex';
  import { save } from '$lib/storage';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    ds,
    graph = 'default',
    reasoning = false,
    prefixes = {},
  }: {
    open?: boolean;
    ds: string;
    graph?: string;
    reasoning?: boolean;
    prefixes?: PrefixMap;
  } = $props();

  let supportText = $state('1');
  let closed = $state(false);
  let lang = $state<DraftLang>('shacl');
  let draft = $state<api.ShapesDraft | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let installing = $state(false);
  let confirming = $state(false);
  let ctl: AbortController | null = null;

  const support = $derived(parseSupport(supportText));
  const shexOffered = $derived(!!app.datasets.find((d) => d.name === ds)?.endpoints?.shex);
  const text = $derived(draft ? draftText(draft, lang) : '');
  const canInstall = $derived(auth.can(ds, 'admin'));

  // a new dataset or graph starts over
  $effect(() => {
    void ds;
    void graph;
    draft = null;
    error = null;
  });

  async function run() {
    if (support == null) return;
    ctl?.abort();
    ctl = new AbortController();
    loading = true;
    error = null;
    try {
      draft = await api.draftShapes(ds, {
        support,
        closed,
        graph,
        reasoning,
        signal: ctl.signal,
      });
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') return;
      error = api.errorMessage(e);
      draft = null;
    } finally {
      loading = false;
    }
  }

  function openInEditor() {
    if (!draft) return;
    if (lang === 'shex') save(shexDraftKey(ds), { schema: draft.shex, map: draft.shapeMap });
    else {
      save(`sparkles.shacl.${ds}`, lang === 'shaclc' ? draft.shaclc : draft.shacl);
      save(shaclSyntaxKey(ds), lang === 'shaclc' ? 'shaclc' : 'turtle');
    }
    save(validateLangKey(ds), lang === 'shex' ? 'shex' : 'shacl');
    open = false;
    void goto(`${resolve('/datasets/[name]', { name: ds })}#validate`);
  }

  async function install() {
    if (!draft) return;
    installing = true;
    try {
      await api.installWarnGuard(
        ds,
        // SHACL is installed from the Turtle draft, whichever syntax is shown
        lang === 'shex'
          ? { language: 'shex', schema: draft.shex, shapeMap: draft.shapeMap }
          : { language: 'shacl', shapes: draft.shacl },
      );
      confirming = false;
      toasts.push(
        'success',
        'Write-time validation installed in warn mode',
        `Writes to /${ds} are checked against the draft and commit with a warning when they do not conform.`,
      );
    } catch (e) {
      if (e instanceof api.ApiError && e.status === 403) {
        toasts.push(
          'error',
          'Installing a guard needs admin access',
          `${e.message}. Ask an admin of /${ds}, or open the draft in the shapes editor.`,
        );
      } else toasts.error('Could not install the guard', e);
    } finally {
      installing = false;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      toasts.push('success', 'Copied');
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }
</script>

<Modal bind:open title="Draft shapes from the data" width={760} onclose={() => ctl?.abort()}>
  <p class="muted small">
    One shape per class, from exact counts over the {graph === 'default'
      ? 'default graph'
      : graph === 'union'
        ? 'union of all graphs'
        : displayIri(graph, prefixes)}. A constraint is drafted when at least the support's share of
    the instances it applies to satisfy it; at support 1 the current data conforms.
  </p>
  <div class="opts">
    <label class="row small">
      <span class="faint">Support</span>
      <input
        class="input sm num"
        class:invalid={support == null}
        aria-invalid={support == null}
        inputmode="decimal"
        bind:value={supportText}
        onkeydown={(e) => e.key === 'Enter' && run()}
      />
    </label>
    <label class="row small">
      <input type="checkbox" bind:checked={closed} />
      <span>closed shapes</span>
    </label>
    <div class="tabs" role="tablist" aria-label="Shape language">
      {#each [['shacl', 'SHACL'], ['shaclc', 'SHACLC'], ['shex', 'ShEx']] as const as [l, title] (l)}
        {#if l !== 'shex' || shexOffered}
          <button class="tab" role="tab" aria-selected={lang === l} onclick={() => (lang = l)}
            >{title}</button
          >
        {/if}
      {/each}
    </div>
    <span class="spacer"></span>
    <button class="btn sm primary" onclick={run} disabled={loading || support == null}>
      {#if loading}<span class="spinner"></span>{:else}<Icon name="wand" size={13} />{/if}
      Draft
    </button>
  </div>

  {#if error}
    <p class="error small">{error}</p>
  {/if}

  {#if draft}
    <p class="small summary">{summaryLine(draft)}</p>
    <ul class="shapes">
      {#each draft.shapes as s (s.shape)}
        {@const ex = exclusions(s)}
        <li>
          <div class="shape-head">
            <span class="mono">{displayIri(s.class, prefixes)}</span>
            <span class="faint small">{fmtInt(s.instances)} instances</span>
          </div>
          {#if ex.length}
            <ul class="excl">
              {#each ex as x (x.path + x.component)}
                <li class="small">
                  <span class="mono">{displayIri(x.path, prefixes)}</span>
                  <span class="mono faint">{componentLabel(x.component)}</span>
                  <span class="warn">excludes {fmtInt(x.excluded)} of {fmtInt(x.applicable)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
    <pre class="draft mono" aria-label="Draft text">{text}</pre>
    {#if confirming}
      <div class="confirm small">
        Installing replaces any write-time validation of /{ds}. Writes still commit in warn mode;
        the receipt and the header report what does not conform.
        <div class="row">
          <button class="btn sm" onclick={() => (confirming = false)}>Cancel</button>
          <button class="btn sm primary" onclick={install} disabled={installing}>
            {#if installing}<span class="spinner"></span>{/if} Install in warn mode
          </button>
        </div>
      </div>
    {/if}
  {/if}

  {#snippet actions()}
    <button class="btn" onclick={copy} disabled={!draft}>
      <Icon name="copy" size={13} /> Copy
    </button>
    <button
      class="btn"
      onclick={() => (confirming = true)}
      disabled={!draft || !canInstall || confirming}
      title={canInstall ? undefined : `Requires admin access to /${ds}`}
    >
      <Icon name="check" size={13} /> Install as guard (warn)
    </button>
    <button class="btn primary" onclick={openInEditor} disabled={!draft}>
      <Icon name="external" size={13} /> Open in shapes editor
    </button>
  {/snippet}
</Modal>

<style>
  .opts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin: 8px 0;
  }
  .num {
    width: 64px;
  }
  .invalid {
    border-color: var(--danger);
  }
  .summary {
    margin: 8px 0 4px;
  }
  .shapes {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 180px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
  }
  .shapes > li {
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
  }
  .shapes > li:last-child {
    border-bottom: none;
  }
  .shape-head {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: baseline;
  }
  .shape-head .mono,
  .excl .mono {
    overflow-wrap: anywhere;
  }
  .excl {
    list-style: none;
    margin: 4px 0 0 12px;
    padding: 0;
  }
  .excl li {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .warn {
    color: var(--warn);
  }
  .draft {
    margin: 10px 0 0;
    max-height: 320px;
    overflow: auto;
    padding: 10px;
    font-size: 11.5px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    white-space: pre;
  }
  .confirm {
    margin-top: 10px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
  }
  .confirm .row {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    margin-top: 8px;
  }
  .error {
    color: var(--danger);
  }
</style>
