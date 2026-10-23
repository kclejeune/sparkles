<script lang="ts">
  // A combobox over Explore's label search that also takes a pasted IRI or prefixed name,
  // and with `literals` a quoted literal (the WAI-ARIA combobox pattern with a listbox).
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import * as ex from '$lib/explore';
  import { shortLabel } from '$lib/graph';
  import { displayIri } from '$lib/rdf';

  export type Picked = { term: Term; label: string };

  let {
    id,
    label,
    ds,
    prefixes,
    value = $bindable(null),
    literals = false,
    onpicked,
  }: {
    id: string;
    label: string;
    ds: string;
    prefixes: Record<string, string>;
    value?: Picked | null;
    /** Offer a typed `"…"` as a literal (a path may end at one). */
    literals?: boolean;
    onpicked?: () => void;
  } = $props();

  let text = $state('');
  let open = $state(false);
  let active = $state(0);
  let hits = $state<ex.SearchHit[]>([]);
  let searching = $state(false);
  let err = $state<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let blurTimer: ReturnType<typeof setTimeout> | undefined;
  let ctl: AbortController | undefined;
  let input: HTMLInputElement | undefined = $state();

  /** The term the text names directly: an IRI, a prefixed name, or a quoted literal. */
  const direct = $derived.by((): Picked | null => {
    const q = text.trim();
    if (!q) return null;
    const bare = q.replace(/^<(.*)>$/, '$1');
    if (/^(https?|urn|mailto|file):/i.test(bare))
      return { term: { type: 'uri', value: bare }, label: shortLabel(bare, prefixes) };
    const pn = /^([A-Za-z][\w.-]*)?:(\S*)$/.exec(q);
    if (pn && prefixes[pn[1] ?? ''] != null) {
      const iri = prefixes[pn[1] ?? ''] + pn[2];
      return { term: { type: 'uri', value: iri }, label: shortLabel(iri, prefixes) };
    }
    const lit = /^"(.*)"$/.exec(q);
    if (literals && lit) return { term: { type: 'literal', value: lit[1] }, label: q };
    return null;
  });

  const options = $derived<(Picked & { direct?: boolean; iri?: string })[]>([
    ...(direct ? [{ ...direct, direct: true }] : []),
    ...hits
      .filter((h) => !(direct?.term.type === 'uri' && direct.term.value === h.iri))
      .map((h) => ({
        term: { type: 'uri' as const, value: h.iri },
        label: h.label ?? shortLabel(h.iri, prefixes),
        iri: h.iri,
      })),
  ]);

  function onInput() {
    clearTimeout(timer);
    clearTimeout(blurTimer);
    open = true;
    active = 0;
    const q = text.trim();
    if (q.length < 2 || direct) {
      hits = [];
      searching = false;
      return;
    }
    searching = true;
    timer = setTimeout(async () => {
      ctl?.abort();
      const c = (ctl = new AbortController());
      err = null;
      try {
        hits = await ex.search(ds, q, c.signal);
      } catch (e) {
        if (!(e instanceof DOMException && e.name === 'AbortError')) err = api.errorMessage(e);
      } finally {
        if (ctl === c) searching = false;
      }
    }, 250);
  }

  function pick(o: Picked) {
    value = { term: o.term, label: o.label };
    text = '';
    hits = [];
    open = false;
    onpicked?.();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      open = true;
      active = Math.min(active + 1, options.length - 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      active = Math.max(active - 1, 0);
    } else if (e.key === 'Enter' && open && options[active]) {
      // Enter picks an option; with the list closed it finds paths (the panel's form)
      e.preventDefault();
      e.stopPropagation();
      pick(options[active]);
    } else if (e.key === 'Escape' && open && text) {
      e.stopPropagation();
      open = false;
    } else if (e.key === 'Backspace' && !text && value) {
      value = null;
    }
  }

  export function focus() {
    input?.focus();
  }

  const listId = $derived(`${id}-list`);
  const shown = $derived(open && text.trim().length >= 2);
</script>

<div class="picker">
  <label class="lbl" for={id}>{label}</label>
  <div class="box">
    {#if value}
      <span class="chip" title={value.term.type === 'uri' ? value.term.value : value.label}>
        <span class="chip-label">{value.label}</span>
        <button
          type="button"
          class="chip-x"
          aria-label="Clear {label}"
          onclick={() => {
            value = null;
            input?.focus();
          }}>×</button
        >
      </span>
    {/if}
    <input
      {id}
      bind:this={input}
      bind:value={text}
      class="input"
      placeholder={value ? '' : literals ? 'Label, IRI or "literal"' : 'Label or IRI'}
      autocomplete="off"
      role="combobox"
      aria-expanded={shown && options.length > 0}
      aria-controls={listId}
      aria-autocomplete="list"
      aria-activedescendant={shown && options[active] ? `${listId}-${active}` : undefined}
      oninput={onInput}
      onkeydown={onKey}
      onfocus={() => {
        clearTimeout(blurTimer);
        open = true;
      }}
      onblur={() => (blurTimer = setTimeout(() => (open = false), 150))}
    />
    {#if searching}<span class="spinner"></span>{/if}
  </div>
  {#if shown}
    <div class="results" id={listId} role="listbox" aria-label="{label} choices">
      {#each options as o, i (o.label + (o.direct ? '!' : '') + (o.iri ?? ''))}
        <div
          id="{listId}-{i}"
          role="option"
          tabindex="-1"
          aria-selected={i === active}
          class="hit"
          class:active={i === active}
          onmouseenter={() => (active = i)}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => pick(o)}
          onkeydown={() => {}}
        >
          {#if o.direct}
            <span
              >Use <span class="mono"
                >{o.term.type === 'uri' ? displayIri(o.term.value, prefixes) : o.label}</span
              ></span
            >
          {:else}
            <span class="hit-label">{o.label}</span>
            <span class="hit-iri mono">{displayIri(o.iri ?? '', prefixes)}</span>
          {/if}
        </div>
      {:else}
        {#if !searching}<div class="hit none">
            {err ?? `Nothing matches “${text.trim()}”.`}
          </div>{/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .picker {
    position: relative;
    min-width: 0;
    flex: 1 1 200px;
  }
  .lbl {
    display: block;
    font-size: var(--fs-xs);
    color: var(--text-2);
    margin-bottom: 3px;
  }
  .box {
    display: flex;
    align-items: center;
    gap: 4px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius, 6px);
    background: var(--surface);
    padding: 2px 6px;
    min-height: 32px;
  }
  .box:focus-within {
    outline: 2px solid var(--spark);
    outline-offset: -1px;
  }
  .input {
    flex: 1;
    min-width: 40px;
    border: 0;
    background: transparent;
    color: var(--text);
    padding: 4px 2px;
    outline: none;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 70%;
    padding: 1px 4px 1px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--spark) 14%, var(--surface));
    font-size: var(--fs-sm);
  }
  .chip-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-x {
    border: 0;
    background: transparent;
    color: var(--text-2);
    cursor: pointer;
    font-size: 15px;
    line-height: 1;
    padding: 0 3px;
  }
  .results {
    position: absolute;
    z-index: 30;
    left: 0;
    right: 0;
    top: 100%;
    margin-top: 2px;
    max-height: 260px;
    overflow: auto;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius, 6px);
    box-shadow: var(--shadow, 0 6px 18px rgb(0 0 0 / 0.15));
  }
  .hit {
    display: flex;
    flex-direction: column;
    padding: 6px 10px;
    cursor: pointer;
    min-width: 0;
  }
  .hit.active {
    background: color-mix(in srgb, var(--spark) 12%, var(--surface));
  }
  .hit-label {
    font-size: var(--fs-sm);
  }
  .hit-iri {
    font-size: var(--fs-xs);
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hit.none {
    color: var(--text-3);
    cursor: default;
  }
</style>
