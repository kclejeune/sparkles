<script lang="ts">
  import {
    keptAvailability,
    readKeptClassification,
    validateKeptSelection,
    type KeptClassification,
  } from '$lib/classification-read';
  import type { InferredPins } from '$lib/inferred-read';

  let {
    target,
    graph,
    imports,
    mode = 'lenient',
    commit,
    pins,
    revision,
  }: {
    target: string;
    graph: string | null;
    imports: boolean;
    mode?: 'lenient' | 'strict';
    commit: number | null;
    pins?: InferredPins;
    revision: number;
  } = $props();
  let reply = $state<KeptClassification | null>(null);
  let failed = $state(false);

  $effect(() => {
    const view = {
      target,
      selection: { graph, imports },
      commit,
      mode,
      languages: [],
    };
    const resultId = pins?.classificationId;
    void revision;
    reply = null;
    failed = false;
    if (view.commit == null) return;
    const scope = { ...view, commit: view.commit };
    const controller = new AbortController();
    void readKeptClassification(scope, { resultId }, controller.signal)
      .then((answer) => {
        if (controller.signal.aborted) return;
        validateKeptSelection(answer, scope);
        if (resultId && answer.result && answer.result.resultId !== resultId)
          throw new Error('The kept read changed its selected result pin.');
        reply = answer;
      })
      .catch(() => {
        if (!controller.signal.aborted) failed = true;
      });
    return () => controller.abort();
  });
</script>

{#if reply}
  <p class="faint">
    {reply.status === 'available'
      ? 'The kept classification covers named classes only.'
      : keptAvailability(reply)}
    This view uses a matching fresh RDFS or OWL RL materialization when available.
  </p>
{:else if failed}
  <p class="faint">
    Could not read kept classification availability. Materialized reads use their own source.
  </p>
{/if}
