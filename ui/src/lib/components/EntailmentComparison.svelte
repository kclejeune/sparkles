<script lang="ts">
  import { onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import type { PrefixMap } from '$lib/rdf';
  import EntailmentDiffView from './EntailmentDiffView.svelte';
  let { ds, from, to, prefixes }: { ds: string; from: string; to: string; prefixes: PrefixMap } =
    $props();
  let profile = $state('');
  let result = $state<api.EntailmentDiff | null>(null);
  let busy = $state(false);
  let error = $state('');
  let controller: AbortController | null = null;
  let generation = 0;
  onDestroy(() => controller?.abort());
  $effect(() => {
    void ds;
    void from;
    void to;
    void profile;
    controller?.abort();
    generation++;
    result = null;
    error = '';
    busy = false;
  });
  async function compare() {
    controller?.abort();
    controller = new AbortController();
    const current = ++generation;
    busy = true;
    error = '';
    try {
      const diff = await api.entailmentDiff(ds, from, to, {
        profile: profile || undefined,
        signal: controller.signal,
      });
      if (generation === current) result = diff;
    } catch (failure) {
      if (
        generation === current &&
        !(failure instanceof DOMException && failure.name === 'AbortError')
      )
        error = api.errorMessage(failure);
    } finally {
      if (generation === current) busy = false;
    }
  }
</script>

<section class="panel comparison" aria-label="Reasoning changes">
  <div class="panel-head"><h3>Reasoning changes</h3></div>
  <div class="panel-body">
    <p class="faint">Compare <code>{from}</code> with <code>{to}</code>.</p>
    <div class="row">
      <label
        >Program <select class="select" bind:value={profile}
          ><option value="">Recorded program</option><option value="rdfs-simple">RDFS simple</option
          ><option value="rdfs">RDFS</option><option value="owl-rl">OWL 2 RL</option></select
        ></label
      ><button class="btn sm" onclick={compare} disabled={busy}
        >{busy ? 'Computing…' : 'Compare entailments'}</button
      >
    </div>
    {#if error}<p class="error-box">{error}</p>{/if}
    {#if result}<EntailmentDiffView diff={result} {prefixes} />{/if}
  </div>
</section>

<style>
  .panel-body {
    display: grid;
    gap: 12px;
  }
  label {
    display: flex;
    gap: 8px;
    align-items: center;
  }
</style>
