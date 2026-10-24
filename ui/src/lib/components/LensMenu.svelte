<script lang="ts">
  /**
   * The lens menu of a graph toolbar (docs/specs/U10-canvas-lenses.md §2.1, §2.5 and §2.6):
   * one row per channel, each a combobox of its lenses, with the run's status and the note
   * that nothing is saved. Shift+L opens it, and Shift+1 to Shift+4 turn the size, colour,
   * badge and edge lens off and on again while `keys` is set. At 390 px it is a bottom sheet.
   * A polite live region announces each lens's result.
   */
  import { onMount, tick, type Snippet } from 'svelte';
  import { CHANNEL_NAMES, CHANNELS, type Channel } from '$lib/lens';
  import { LENS_OPTIONS, type LensId, type Lenses } from '$lib/lenses.svelte';
  import Icon from './Icon.svelte';

  let {
    lenses,
    short,
    keys = true,
    extra,
  }: {
    lenses: Lenses;
    /** A short form of an IRI, for the property picker. */
    short: (iri: string) => string;
    /** Whether the keyboard shortcuts apply, such as while the graph is on screen. */
    keys?: boolean;
    /** More controls for the panel, such as the validation lens's source. */
    extra?: Snippet;
  } = $props();

  let open = $state(false);
  let button: HTMLButtonElement | undefined = $state();
  let panel: HTMLDivElement | undefined = $state();
  /** A channel whose property lens waits for its property. */
  let picking = $state<Partial<Record<Channel, boolean>>>({});
  const uid = Math.random().toString(36).slice(2, 8);

  const count = $derived(CHANNELS.filter((c) => lenses.choice[c].lens !== 'none').length);

  const options = (ch: Channel) =>
    LENS_OPTIONS[ch].filter(
      (o) => (!o.reasoning || lenses.reasoning) && (!o.validation || lenses.validation),
    );

  const selectValue = (ch: Channel) =>
    picking[ch] && lenses.choice[ch].lens !== 'property' ? 'property' : lenses.choice[ch].lens;

  function choose(ch: Channel, id: LensId) {
    if (id === 'property') {
      picking[ch] = true;
      void lenses.loadProperties();
      return;
    }
    picking[ch] = false;
    lenses.set(ch, { lens: id });
  }

  function chooseProperty(ch: Channel, iri: string) {
    if (!iri) return;
    lenses.set(ch, { lens: 'property', property: iri });
  }

  const propsOf = (ch: Channel) =>
    ch === 'size' ? lenses.properties.filter((p) => p.numeric) : lenses.properties;

  /** Where the panel opens: under the button, kept on screen. It is fixed, so no clipping box of the toolbar cuts it. */
  let pos = $state({ top: 0, left: 0 });
  const WIDTH = 560;
  function place() {
    const r = button?.getBoundingClientRect();
    if (!r) return;
    const w = Math.min(WIDTH, window.innerWidth - 32);
    pos = {
      top: Math.round(r.bottom + 6),
      left: Math.round(Math.max(16, Math.min(r.right - w, window.innerWidth - w - 16))),
    };
  }

  async function show() {
    place();
    open = true;
    await tick();
    panel?.querySelector<HTMLElement>('select')?.focus();
  }

  function hide(refocus = true) {
    open = false;
    if (refocus) button?.focus();
  }

  function status(ch: Channel): string {
    const s = lenses.status[ch];
    const r = lenses.results[ch];
    if (s.note) return s.note;
    if (s.error) return s.error;
    if (s.busy) return 'reading…';
    if (!r) return '';
    const n = r.values.size;
    const what = r.channel === 'edge' ? (n === 1 ? 'edge' : 'edges') : n === 1 ? 'node' : 'nodes';
    const ms = s.ms != null && s.ms >= 50 ? ` · ${(s.ms / 1000).toFixed(1)} s` : '';
    return `${r.summary ?? `${n.toLocaleString('en-US')} ${what}`}${ms}`;
  }

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!keys || e.metaKey || e.ctrlKey || e.altKey || !e.shiftKey) return;
      if ((e.target as HTMLElement)?.closest?.('input, textarea, [contenteditable]')) return;
      if (e.code === 'KeyL') {
        e.preventDefault();
        if (open) hide();
        else void show();
        return;
      }
      const i = ['Digit1', 'Digit2', 'Digit3', 'Digit4'].indexOf(e.code);
      if (i >= 0) {
        e.preventDefault();
        lenses.toggle(CHANNELS[i]);
      }
    };
    const onDown = (e: MouseEvent) => {
      if (!open) return;
      const t = e.target as Node;
      if (panel?.contains(t) || button?.contains(t)) return;
      hide(false);
    };
    const onResize = () => open && place();
    document.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDown);
    window.addEventListener('resize', onResize);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('mousedown', onDown);
      window.removeEventListener('resize', onResize);
    };
  });
</script>

<div class="lens-menu">
  <button
    bind:this={button}
    class="btn sm"
    aria-haspopup="dialog"
    aria-expanded={open}
    aria-controls={`lens-panel-${uid}`}
    title="Size, colour, badge and edge lenses (Shift+L)"
    onclick={() => (open ? hide() : show())}
    ><Icon name="layers" size={13} /> Lenses{#if count}<span
        class="count"
        aria-label={`, ${count} on`}>{count}</span
      >{/if}</button
  >
  {#if open}
    <div
      bind:this={panel}
      class="panel"
      id={`lens-panel-${uid}`}
      role="dialog"
      aria-label="Lenses"
      tabindex="-1"
      style:--top={`${pos.top}px`}
      style:--left={`${pos.left}px`}
      onkeydown={(e) => {
        if (e.key === 'Escape') {
          e.stopPropagation();
          hide();
        }
      }}
    >
      <div class="head">
        <strong>Lenses</strong>
        <button class="btn sm ghost icon" aria-label="Close lenses" onclick={() => hide()}
          ><Icon name="x" size={13} /></button
        >
      </div>
      {#if lenses.blocked}<p class="note warn">{lenses.blocked}</p>{/if}
      <div class="grid" role="group" aria-label="Lens per channel">
        {#each CHANNELS as ch, i (ch)}
          <label class="ch" for={`lens-${uid}-${ch}`}>{CHANNEL_NAMES[ch]}</label>
          <div class="pick">
            <select
              id={`lens-${uid}-${ch}`}
              class="select sm"
              value={selectValue(ch)}
              aria-keyshortcuts={`Shift+${i + 1}`}
              onchange={(e) => choose(ch, (e.currentTarget as HTMLSelectElement).value as LensId)}
            >
              {#each options(ch) as o (o.id)}
                <option value={o.id}>{o.name}</option>
              {/each}
            </select>
            {#if selectValue(ch) === 'property'}
              <select
                class="select sm prop"
                aria-label={`${CHANNEL_NAMES[ch]} property`}
                value={lenses.choice[ch].property ?? ''}
                onchange={(e) => chooseProperty(ch, (e.currentTarget as HTMLSelectElement).value)}
              >
                <option value="" disabled>
                  {propsOf(ch).length
                    ? 'Choose a property'
                    : ch === 'size'
                      ? 'No numeric property'
                      : 'No property'}
                </option>
                {#each propsOf(ch) as p (p.iri)}
                  <option value={p.iri} title={p.iri}>{short(p.iri)} ({p.count})</option>
                {/each}
              </select>
            {/if}
          </div>
          <span
            class="st faint"
            class:err={!!lenses.status[ch].error}
            data-testid={`lens-status-${ch}`}
          >
            {#if lenses.status[ch].busy}<span class="spinner"></span>{/if}
            {status(ch)}
          </span>
        {/each}
      </div>
      {#if !lenses.reasoning}
        <p class="note faint">
          The dataset has no reasoning, so there are no inferred edges to show.
        </p>
      {/if}
      {#if lenses.validationNote}
        <p class="note faint" data-testid="lens-validation-note">{lenses.validationNote}</p>
      {/if}
      {#if extra}{@render extra()}{/if}
      <p class="note faint">These views are computed for this picture and are not saved.</p>
      <p class="note faint keys">Shift+1 to Shift+4 turn a channel off and on again.</p>
    </div>
  {/if}
  <div class="sr-only" aria-live="polite">{lenses.announcement}</div>
</div>

<style>
  .lens-menu {
    position: relative;
    display: inline-flex;
  }
  .count {
    display: inline-grid;
    place-items: center;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: 8px;
    background: var(--spark-soft);
    color: var(--spark-ink);
    font-size: var(--fs-xs);
    font-weight: 600;
    margin-left: 2px;
  }
  .panel {
    position: fixed;
    left: var(--left);
    top: var(--top);
    z-index: 30;
    width: min(560px, calc(100vw - 32px));
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
    padding: 10px 12px;
    display: grid;
    gap: 8px;
    text-align: left;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .grid {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) minmax(0, auto);
    gap: 6px 10px;
    align-items: center;
  }
  .ch {
    font-weight: 600;
    font-size: var(--fs-sm);
  }
  .pick {
    display: flex;
    gap: 6px;
    min-width: 0;
    flex-wrap: wrap;
  }
  .pick select {
    min-width: 0;
    max-width: 100%;
  }
  .prop {
    max-width: 220px;
  }
  .st {
    font-size: var(--fs-xs);
    display: inline-flex;
    gap: 4px;
    align-items: center;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .st.err {
    color: var(--danger);
  }
  .note {
    margin: 0;
    font-size: var(--fs-xs);
  }
  .note.warn {
    color: var(--warn);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  /* the 390 px layout: a bottom sheet with one row per channel (§2.6) */
  @media (max-width: 640px) {
    .panel {
      left: 0;
      right: 0;
      bottom: 0;
      top: auto;
      width: auto;
      max-height: 75vh;
      overflow-y: auto;
      border-radius: var(--r-lg) var(--r-lg) 0 0;
      padding: 12px 16px calc(12px + env(safe-area-inset-bottom));
    }
    .grid {
      grid-template-columns: minmax(0, 1fr);
      gap: 4px;
    }
    .ch {
      margin-top: 6px;
    }
    .keys {
      display: none;
    }
  }
</style>
