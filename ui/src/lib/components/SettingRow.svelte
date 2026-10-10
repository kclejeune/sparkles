<script lang="ts">
  // One row of a layered settings form: the field's name and source on the left, its
  // control and a line of help on the right. A field that a refused write named is
  // highlighted.
  // A runtime value that replaces a value of the server's configuration shows that value
  // under the control.
  import type { Snippet } from 'svelte';
  import { valueText, type FieldInfo } from '$lib/settings';
  import FieldSource from './FieldSource.svelte';

  let {
    id,
    label,
    help,
    info,
    conflict = false,
    dirty = false,
    error,
    canEdit = false,
    busy = false,
    onreset,
    children,
  }: {
    /** The id of the control, which the label names. */
    id: string;
    label: string;
    help?: string;
    info: FieldInfo;
    /** A write was refused because the operator locked this field. */
    conflict?: boolean;
    /** The form holds an unsaved change of the field. */
    dirty?: boolean;
    /** Why the field's text does not read. */
    error?: string;
    canEdit?: boolean;
    busy?: boolean;
    onreset?: () => void;
    children: Snippet;
  } = $props();
</script>

<div
  class="setting"
  class:conflict
  class:locked={info.locked}
  data-field={info.path}
  data-source={info.locked ? 'locked' : info.source}
  data-overrides={info.overrides !== undefined ? '' : undefined}
>
  <div class="name">
    <label for={id}>{label}</label>
    {#if dirty}<span class="unsaved" title="Not saved yet">unsaved</span>{/if}
    <FieldSource {info} {label} {canEdit} {busy} {onreset} />
  </div>
  <div class="control">
    {@render children()}
    {#if info.overrides !== undefined}
      <p class="note declared" title={JSON.stringify(info.overrides, null, 2)}>
        server config: <code>{valueText(info.overrides)}</code>
      </p>
    {/if}
    {#if conflict}
      <p class="note bad">
        The operator locked this field in the server's settings file, so the change was refused.
      </p>
    {/if}
    {#if error}<p class="note bad">{error}</p>{/if}
    {#if help}<p class="note">{help}</p>{/if}
  </div>
</div>

<style>
  .setting {
    display: grid;
    grid-template-columns: minmax(0, 220px) minmax(0, 1fr);
    gap: 4px 16px;
    padding: 5px 10px;
    border-radius: var(--r);
    border: 1px solid transparent;
  }
  .setting.conflict {
    border-color: color-mix(in srgb, var(--danger) 50%, transparent);
    background: var(--danger-soft);
  }
  .name {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 8px;
    min-width: 0;
    padding-top: 5px;
    align-self: start;
  }
  .name label {
    font-size: var(--fs-sm);
    font-weight: 500;
    color: var(--text);
  }
  .locked .name label {
    color: var(--text-2);
  }
  .unsaved {
    font-size: var(--fs-xs);
    color: var(--iri);
    font-weight: 500;
  }
  .control {
    display: grid;
    gap: 4px;
    min-width: 0;
    align-content: start;
  }
  .note {
    max-width: 72ch;
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .note.bad {
    color: var(--danger);
  }
  .note.declared {
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .note.declared code {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    color: var(--text);
  }
  @media (max-width: 760px) {
    .setting {
      grid-template-columns: minmax(0, 1fr);
    }
    .name {
      padding-top: 0;
    }
  }
</style>
