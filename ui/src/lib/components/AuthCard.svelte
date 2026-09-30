<script lang="ts">
  // A centered card for the sign-in and CLI approval pages.
  import type { Snippet } from 'svelte';

  let { title, subtitle, children }: { title: string; subtitle?: Snippet; children: Snippet } =
    $props();
</script>

<div class="wrap">
  <div class="card">
    <div class="brand">
      <svg viewBox="0 0 32 32" width="26" height="26" aria-hidden="true">
        <path d="M16 3l2.9 8.1L27 14l-8.1 2.9L16 25l-2.9-8.1L5 14l8.1-2.9z" fill="var(--spark)" />
        <circle cx="25.5" cy="6.5" r="2.2" fill="var(--spark)" />
        <circle cx="7" cy="25" r="1.6" fill="var(--spark)" opacity="0.6" />
      </svg>
      <span>Sparkles</span>
    </div>
    <h1>{title}</h1>
    {#if subtitle}<p class="sub muted">{@render subtitle()}</p>{/if}
    {@render children()}
  </div>
</div>

<style>
  .wrap {
    min-height: 100%;
    display: grid;
    place-items: center;
    padding: 32px 16px;
  }
  .card {
    width: min(420px, 100%);
    display: grid;
    gap: 14px;
    padding: 28px 26px 24px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 650;
    font-size: 17px;
    letter-spacing: -0.02em;
  }
  h1 {
    margin: 4px 0 0;
    font-size: 19px;
    letter-spacing: -0.01em;
  }
  .sub {
    margin: -6px 0 0;
    font-size: var(--fs-sm);
    line-height: 1.5;
  }
</style>
