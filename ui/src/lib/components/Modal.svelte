<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    open = $bindable(false),
    title,
    width = 440,
    onclose,
    children,
    actions,
  }: {
    open?: boolean;
    title: string;
    width?: number;
    onclose?: () => void;
    children: Snippet;
    actions?: Snippet;
  } = $props();

  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  });

  function close() {
    open = false;
    onclose?.();
  }
</script>

<dialog
  bind:this={dialog}
  style:width="{width}px"
  onclose={() => open && close()}
  onclick={(e) => e.target === dialog && close()}
  aria-labelledby="modal-title"
>
  {#if open}
    <div class="head">
      <h2 id="modal-title">{title}</h2>
      <button class="btn ghost icon sm" onclick={close} aria-label="Close"
        ><Icon name="x" size={14} /></button
      >
    </div>
    <div class="body">{@render children()}</div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  {/if}
</dialog>

<style>
  dialog {
    max-width: calc(100vw - 32px);
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-pop);
  }
  dialog[open] {
    animation: pop 0.14s ease-out;
  }
  dialog::backdrop {
    background: rgba(10, 12, 18, 0.45);
    backdrop-filter: blur(2px);
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(6px) scale(0.985);
    }
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px 6px;
  }
  .head h2 {
    font-size: var(--fs-md);
  }
  .body {
    padding: 8px 16px 16px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
    background: var(--surface-2);
    border-radius: 0 0 var(--r-lg) var(--r-lg);
  }
</style>
