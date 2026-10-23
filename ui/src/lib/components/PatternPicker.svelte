<script lang="ts">
  // A picker of the Pattern pane (U08 §2.2): a combobox in the WAI-ARIA pattern, whose
  // list is fed from the schema and narrowed by what is typed. Typed text the list lacks
  // is accepted as it is, so a full IRI or a prefixed name always works, and `note` says
  // why the list is empty when the schema did not load.
  import type { Choice } from '$lib/pattern-schema';

  let {
    label,
    name,
    value = $bindable(''),
    placeholder = '',
    choices,
    short,
    note = null,
    more = false,
    onmore,
    onpick,
    autofocus = false,
  }: {
    label: string;
    name: string;
    value?: string;
    placeholder?: string;
    /** the choices for what is typed, ranked */
    choices: (typed: string) => Choice[];
    /** how a choice is written into the field: a prefixed name, else `<iri>` */
    short: (iri: string) => string;
    note?: string | null;
    /** whether the schema has a further page to read */
    more?: boolean;
    onmore?: () => void;
    onpick?: (text: string) => void;
    autofocus?: boolean;
  } = $props();

  let open = $state(false);
  let active = $state(-1);
  let input: HTMLInputElement | undefined = $state();
  const id = `pp-${Math.random().toString(36).slice(2, 9)}`;
  const list = $derived(open ? choices(value) : []);

  function pick(c: Choice) {
    value = short(c.iri);
    open = false;
    active = -1;
    onpick?.(value);
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      open = true;
      active = Math.min(active + 1, list.length - 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      active = Math.max(active - 1, -1);
    } else if (e.key === 'Enter' && open && active >= 0 && list[active]) {
      e.preventDefault();
      pick(list[active]);
    } else if (e.key === 'Escape' && open) {
      // the pane's Escape closes the form; the list closes first
      e.preventDefault();
      e.stopPropagation();
      open = false;
      active = -1;
    }
  }

  export function focus() {
    input?.focus();
  }
</script>

<div class="picker">
  <input
    bind:this={input}
    {name}
    aria-label={label}
    {placeholder}
    role="combobox"
    aria-expanded={open && list.length > 0}
    aria-controls={`${id}-list`}
    aria-autocomplete="list"
    aria-activedescendant={active >= 0 ? `${id}-${active}` : undefined}
    autocomplete="off"
    spellcheck="false"
    bind:value
    onfocus={() => (open = true)}
    oninput={() => {
      open = true;
      active = -1;
    }}
    onblur={() =>
      setTimeout(() => {
        // a window that loses focus blurs the field too, which keeps the list open
        if (document.activeElement !== input) open = false;
      }, 150)}
    onkeydown={keydown}
    {...autofocus ? { autofocus: true } : {}}
  />
  {#if open && (list.length > 0 || note)}
    <div class="list" role="listbox" id={`${id}-list`} aria-label={`${label} choices`}>
      {#if note}<p class="note" role="note">{note}</p>{/if}
      {#each list as c, i (c.iri)}
        <div
          role="option"
          id={`${id}-${i}`}
          aria-selected={i === active}
          class:active={i === active}
          tabindex="-1"
          onmousedown={(e) => {
            e.preventDefault();
            pick(c);
          }}
        >
          <span class="mono">{short(c.iri)}</span>
          {#if c.count !== null}<span class="count">{c.count.toLocaleString()}</span>{/if}
          {#if c.hint}<span class="hint">{c.hint}</span>{/if}
        </div>
      {/each}
      {#if more}
        <button
          class="btn sm ghost"
          type="button"
          onmousedown={(e) => {
            e.preventDefault();
            onmore?.();
          }}>More from the schema</button
        >
      {/if}
    </div>
  {/if}
</div>

<style>
  .picker {
    position: relative;
    flex: 1 1 120px;
    min-width: 0;
  }
  input {
    width: 100%;
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .list {
    position: absolute;
    z-index: 5;
    left: 0;
    min-width: min(320px, 90vw);
    width: max-content;
    max-width: min(480px, 90vw);
    top: calc(100% + 2px);
    max-height: 240px;
    overflow: auto;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius, 6px);
    box-shadow: var(--shadow-md, 0 4px 16px rgb(0 0 0 / 0.2));
    font-size: 12px;
  }
  [role='option'] {
    display: flex;
    gap: 6px;
    align-items: baseline;
    padding: 4px 8px;
    cursor: pointer;
    min-height: 28px;
  }
  [role='option'].active,
  [role='option']:hover {
    background: var(--spark-soft);
  }
  .mono {
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }
  .count {
    margin-left: auto;
    flex: none;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .hint {
    flex: none;
    color: var(--text-3);
    font-size: 11px;
  }
  .note {
    margin: 0;
    padding: 6px 8px;
    color: var(--text-2);
  }
  @media (max-width: 600px) {
    [role='option'] {
      min-height: 44px;
    }
  }
</style>
