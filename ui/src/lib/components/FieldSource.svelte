<script lang="ts">
  // Where a settings field's value comes from (C19 §10): a label for a value of the
  // server's configuration, a lock for a field the operator locked, a marker and a reset
  // for a value changed at runtime, and a warning for a runtime value a lock ignores. A
  // runtime value that replaces a declared one says that it overrides the server config,
  // and its reset is "Use server config". Any layered settings form uses it, for a
  // dataset's kinds or the server's.
  import { resetText, sourceText, type FieldInfo } from '$lib/settings';
  import Icon from './Icon.svelte';

  let {
    info,
    label,
    canEdit = false,
    busy = false,
    onreset,
  }: {
    info: FieldInfo;
    /** The field's name, for the reset button's accessible name. */
    label: string;
    /** The caller may change the settings (a reset is offered). */
    canEdit?: boolean;
    busy?: boolean;
    /** Remove the field's runtime value. */
    onreset?: () => void;
  } = $props();

  const text = $derived(sourceText(info));
  const reset = $derived(resetText(info));
  const resetName = $derived(
    info.overridden
      ? `Remove the ignored change of ${label}`
      : info.hasDeclared
        ? `Use server config for ${label}`
        : `Reset ${label} to default`,
  );
  const ignored = $derived(
    info.ignored === undefined ? '' : ` (${JSON.stringify(info.ignored)})`.slice(0, 120),
  );
</script>

<span class="sources" data-source={info.locked ? 'locked' : info.source}>
  {#if text}
    <span
      class="src {info.locked ? 'locked' : info.source}"
      class:overrides={info.overrides !== undefined}
      title={text.title}
    >
      {#if info.locked}<Icon name="lock" size={11} />{:else if info.source === 'runtime'}<i
          class="dot"
        ></i>{/if}
      {text.label}
    </span>
  {/if}
  {#if info.partlyLocked}
    <span
      class="src locked"
      title="The operator locked some members of this field in the server's settings file. Changes to them are refused."
      ><Icon name="lock" size={11} /> partly locked</span
    >
  {/if}
  {#if info.overridden}
    <span
      class="src overridden"
      title="A value changed at runtime{ignored} is stored, but it is ignored because the operator locked the field. Reset it to remove it."
      ><Icon name="alert" size={11} /> change ignored</span
    >
  {/if}
  {#if info.runtime && canEdit && onreset}
    <button
      type="button"
      class="btn ghost sm reset"
      disabled={busy}
      aria-label={resetName}
      title={reset.title}
      onclick={onreset}><Icon name="refresh" size={11} /> {reset.text}</button
    >
  {/if}
</span>

<style>
  .sources {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 6px;
    min-width: 0;
  }
  .src {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-xs);
    font-weight: 500;
    color: var(--text-3);
    white-space: nowrap;
    cursor: help;
  }
  .src.runtime {
    color: var(--spark-ink);
  }
  .src.overrides {
    color: var(--warn);
  }
  .src.overrides .dot {
    background: var(--warn);
  }
  .src.locked {
    color: var(--text-2);
  }
  .src.overridden {
    color: var(--warn);
  }
  .dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--spark);
  }
  .reset {
    height: 20px;
    padding: 0 6px;
    gap: 4px;
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
</style>
