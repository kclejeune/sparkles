<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    open = $bindable(false),
    title,
    width = 440,
    onclose,
    closeDisabled = false,
    children,
    actions,
  }: {
    open?: boolean;
    title: string;
    width?: number;
    onclose?: () => void;
    closeDisabled?: boolean;
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
    if (closeDisabled) return;
    open = false;
    onclose?.();
  }
</script>

<dialog
  bind:this={dialog}
  style:width="{width}px"
  onclose={() => open && close()}
  oncancel={(e) => {
    if (closeDisabled) e.preventDefault();
  }}
  onclick={(e) => e.target === dialog && close()}
  aria-labelledby="modal-title"
>
  {#if open}
    <div class="head">
      <h2 id="modal-title">{title}</h2>
      <button class="btn ghost icon sm" onclick={close} aria-label="Close" disabled={closeDisabled}
        ><Icon name="x" size={14} /></button
      >
    </div>
    <div class="body">{@render children()}</div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  {/if}
</dialog>

<style>
  /* A centred card; on a phone (760 px and narrower) a sheet docked at the bottom of the
     screen, full width, whose body scrolls under a fixed head and footer, so the actions stay
     within reach of a thumb and above the on-screen keyboard. */
  dialog {
    max-width: calc(100vw - 2 * var(--sp-4));
    max-height: calc(100dvh - 2 * var(--sp-6));
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-overlay);
    flex-direction: column;
  }
  dialog[open] {
    display: flex;
  }
  dialog::backdrop {
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }
  @media (prefers-reduced-motion: no-preference) {
    dialog[open] {
      animation: pop 0.14s ease-out;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(6px) scale(0.985);
    }
  }
  @keyframes rise {
    from {
      transform: translateY(24px);
      opacity: 0.6;
    }
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    flex: none;
    padding: 14px var(--sp-4) 6px;
  }
  .head h2 {
    font-size: var(--fs-md);
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    padding: var(--sp-2) var(--sp-4) var(--sp-4);
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    align-content: start;
    gap: var(--sp-3);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: var(--sp-2);
    flex: none;
    padding: var(--sp-3) var(--sp-4);
    border-top: 1px solid var(--border);
    background: var(--surface-2);
    border-radius: 0 0 var(--r-lg) var(--r-lg);
  }
  @media (max-width: 760px) {
    dialog {
      width: 100% !important;
      max-width: none;
      max-height: 90dvh;
      margin: auto 0 0;
      border-width: 1px 0 0;
      border-radius: var(--r-lg) var(--r-lg) 0 0;
    }
    @media (prefers-reduced-motion: no-preference) {
      dialog[open] {
        animation: rise 0.18s ease-out;
      }
    }
    .head {
      padding-top: var(--sp-4);
    }
    .actions {
      padding-bottom: calc(var(--sp-3) + env(safe-area-inset-bottom));
      border-radius: 0;
    }
  }
</style>
