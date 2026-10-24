<script lang="ts">
  /**
   * Where the validation lens's shapes come from (docs/specs/U07-data-quality-and-schema-diagram.md
   * §11.2), shown beside the badge channel in the lens menu: the dataset's write-time guard,
   * a shapes graph of the dataset named by its IRI, or the shapes the Validate panel last
   * validated with.
   */
  import type { GuardProbe } from '$lib/validation-overlay';

  let {
    probe,
    editor = false,
    kind = $bindable('guard'),
    graphIri = $bindable(''),
  }: {
    /** What the overlay found out about the dataset's guard; null while it looks. */
    probe: GuardProbe | null;
    /** The Validate panel has shapes. */
    editor?: boolean;
    kind?: 'guard' | 'graph' | 'editor';
    graphIri?: string;
  } = $props();

  let draft = $state(graphIri);
  const guard = $derived(probe?.state === 'shacl' || probe?.state === 'shex');

  function use(e: SubmitEvent) {
    e.preventDefault();
    graphIri = draft.trim();
  }
</script>

<div class="vsrc" role="group" aria-label="Validation lens">
  <label class="row small">
    Shapes
    <select class="select sm" bind:value={kind} aria-label="Validation source">
      {#if guard}
        <option value="guard"
          >Write-time guard ({probe?.state === 'shex' ? 'ShEx' : 'SHACL'})</option
        >
      {/if}
      <option value="graph">A shapes graph</option>
      {#if editor}<option value="editor">The Validate panel's shapes</option>{/if}
    </select>
  </label>
  {#if kind === 'graph'}
    <form class="row small" onsubmit={use}>
      <input
        class="mono"
        placeholder="http://example.org/shapes"
        aria-label="Shapes graph IRI"
        bind:value={draft}
      />
      <button class="btn sm" type="submit">Use</button>
    </form>
  {/if}
</div>

<style>
  .vsrc {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2, 8px);
    align-items: center;
    margin-top: var(--space-2, 8px);
  }
  .vsrc input {
    min-width: 0;
    width: 16rem;
    max-width: 100%;
  }
</style>
