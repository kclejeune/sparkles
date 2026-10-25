<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { onBranch } from '$lib/branches';
  import type { PrefixMap } from '$lib/rdf';
  import { removalPatch } from '$lib/support-patch';
  import Modal from './Modal.svelte';
  import TermView from './TermView.svelte';
  import EntailmentDiffView from './EntailmentDiffView.svelte';
  let {
    name,
    source,
    commit,
    fact,
    prefixes,
    initialProfile = '',
    onclose,
  }: {
    name: string;
    source: string;
    commit: number;
    fact: api.SupportFact;
    prefixes: PrefixMap;
    initialProfile?: string;
    onclose: () => void;
  } = $props();
  let open = $state(true);
  let profile = $state(untrack(() => initialProfile));
  let draft = $state<api.Branch | null>(null);
  let preview = $state<api.WritePreview | null>(null);
  let receipt = $state<api.Receipt | null>(null);
  let patch = $state('');
  let busy = $state(false);
  let error = $state('');
  let controller: AbortController | null = null;
  let generation = 0;
  onDestroy(() => {
    generation++;
    controller?.abort();
  });
  $effect(() => {
    void profile;
    preview = null;
    error = '';
  });
  const href = $derived(
    draft
      ? `${resolve('/datasets/[name]', { name })}?branch=${encodeURIComponent(draft.name)}`
      : '',
  );
  async function prepare() {
    controller?.abort();
    controller = new AbortController();
    const current = ++generation;
    busy = true;
    error = '';
    preview = null;
    try {
      let branch = draft;
      if (!branch) {
        const branchName = `support-${crypto.randomUUID().slice(0, 12)}`;
        branch = await api.createBranch(name, {
          name: branchName,
          from: source,
          at: `commit:${commit}`,
        });
        if (branch.name !== branchName || branch.name === 'main')
          throw new Error('The server did not create the requested draft branch.');
        draft = branch;
        patch = removalPatch(fact, branch.id, branch.head);
      }
      const query = new URLSearchParams({ dryRun: 'true', entailments: 'true', changes: '1' });
      if (profile) query.set('profile', profile === 'asserted' ? 'rules' : profile);
      if (profile === 'asserted') query.set('rules', '');
      const result = await api.json<api.WritePreview>(
        `/${encodeURIComponent(onBranch(name, branch.name))}/patch?${query}`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/rdf-patch' },
          body: patch,
          signal: controller.signal,
        },
      );
      if (generation === current) preview = result;
    } catch (failure) {
      if (
        generation === current &&
        !(failure instanceof DOMException && failure.name === 'AbortError')
      ) {
        error = api.errorMessage(failure);
        if (failure instanceof api.ApiError && failure.body?.dryRun === true)
          preview = failure.body as unknown as api.WritePreview;
      }
    } finally {
      if (generation === current) busy = false;
    }
  }
  async function apply() {
    if (
      !draft ||
      !preview?.wouldCommit ||
      preview.outcome !== 'commit' ||
      preview.changes?.total !== 1
    )
      return;
    controller?.abort();
    controller = new AbortController();
    const current = ++generation;
    busy = true;
    error = '';
    try {
      const result = await api.json<api.Receipt>(
        `/${encodeURIComponent(onBranch(name, draft.name))}/patch?receipt=true`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/rdf-patch' },
          body: patch,
          signal: controller.signal,
        },
      );
      if (generation === current) {
        receipt = result;
        preview = null;
      }
    } catch (failure) {
      if (
        generation === current &&
        !(failure instanceof DOMException && failure.name === 'AbortError')
      ) {
        error = api.errorMessage(failure);
        preview = null;
      }
    } finally {
      if (generation === current) busy = false;
    }
  }
</script>

<Modal bind:open title="Remove a support fact on a branch" width={760} {onclose}>
  <p>
    This creates a draft branch from <strong>{source}</strong> at commit {commit}. Preview the
    removal, then apply it to that branch.
  </p>
  <p class="mono">
    {#each fact.triple as term, index (index)}<TermView {term} {prefixes} />{' '}{/each}
  </p>
  <label
    >Program <select class="select" bind:value={profile} disabled={busy || !!receipt}>
      <option value="">Recorded program</option><option value="asserted">Asserted data only</option
      ><option value="rdfs-simple">RDFS simple</option><option value="rdfs">RDFS</option><option
        value="owl-rl">OWL 2 RL</option
      >
    </select></label
  >
  {#if error}<p class="error-box">{error}</p>{/if}
  {#if draft}<p class="faint">
      Draft branch <a {href}>{draft.name}</a> remains available after closing this preview.
    </p>{/if}
  {#if receipt}<p class="success-box">
      Removed on <a {href}>{draft?.name}</a> in commit {receipt.commit.seq}.
    </p>{/if}
  {#if preview}
    <p>Preview outcome: {preview.outcome}. {preview.changes?.total ?? 0} changed quad.</p>
    {#if preview.validation}<p class="faint">
        Validation: {preview.validation.status ?? 'checked'}
      </p>{/if}
    {#if preview.entailments}<EntailmentDiffView
        diff={preview.entailments}
        {prefixes}
      />{:else if preview.entailmentsUnavailable}<p class="faint">
        {preview.entailmentsUnavailable}
      </p>{/if}
  {/if}
  {#snippet actions()}
    <button
      class="btn ghost"
      onclick={() => {
        open = false;
        onclose();
      }}>Close</button
    >
    {#if !receipt}<button class="btn" onclick={prepare} disabled={busy}
        >{busy ? 'Working…' : draft ? 'Preview removal again' : 'Create branch and preview'}</button
      >
      <button
        class="btn primary"
        onclick={apply}
        disabled={busy ||
          !preview?.wouldCommit ||
          preview.outcome !== 'commit' ||
          preview.changes?.total !== 1}>Apply removal on branch</button
      >{/if}
  {/snippet}
</Modal>
