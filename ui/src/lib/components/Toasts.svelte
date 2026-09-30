<script lang="ts">
  import { toasts } from '$lib/app.svelte';
  import Icon from './Icon.svelte';
</script>

<div class="toasts" role="status" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div class="toast {t.kind}">
      <Icon name={t.kind === 'error' ? 'alert' : t.kind === 'success' ? 'check' : 'info'} size={15} />
      <div class="text">
        <div>{t.text}</div>
        {#if t.detail}<div class="detail">{t.detail}</div>{/if}
      </div>
      <button class="btn ghost icon sm" aria-label="Dismiss" onclick={() => toasts.dismiss(t.id)}>
        <Icon name="x" size={12} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 16px;
    bottom: 16px;
    display: grid;
    gap: 8px;
    z-index: 100;
    width: min(380px, calc(100vw - 32px));
  }
  .toast {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 10px 8px 10px 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r);
    box-shadow: var(--shadow-pop);
    animation: slide 0.16s ease-out;
  }
  .toast :global(.icon) {
    margin-top: 1px;
  }
  .toast.error :global(.icon:first-child) {
    color: var(--danger);
  }
  .toast.success :global(.icon:first-child) {
    color: var(--ok);
  }
  .text {
    flex: 1;
    min-width: 0;
  }
  .detail {
    color: var(--text-2);
    font-size: var(--fs-sm);
    word-break: break-word;
    margin-top: 2px;
  }
  @keyframes slide {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
  }
</style>
