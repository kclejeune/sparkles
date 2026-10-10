<script lang="ts">
  // The count of open items in a dataset's memory review inbox, as a small badge. It
  // renders nothing while nothing waits or the server sends no count for the caller. Its
  // accessible name (role img) says the count in words.
  import { badgeCount, reviewLabel, type ReviewCounts } from '$lib/maintenance';

  let { counts, text = '' }: { counts: ReviewCounts | null; text?: string } = $props();
  const label = $derived(counts ? reviewLabel(counts.open, counts.oldest) : '');
</script>

{#if counts && counts.open > 0}
  <span class="badge spark review-badge" role="img" aria-label={label} title={label}
    >{badgeCount(counts.open)}{text}</span
  >
{/if}

<style>
  .review-badge {
    font-variant-numeric: tabular-nums;
  }
</style>
