<script lang="ts">
  /**
   * The legend of the active lenses (docs/specs/U10-canvas-lenses.md §2.1 and §2.6). It docks
   * in the canvas's corner and lists each encoding with its text, so that no lens is colour
   * alone. A click on an entry, or Enter on it, selects its nodes, and again clears them. At
   * 390 px it is a strip above the graph that shows the active encodings in one line and
   * opens on a tap.
   */
  import { base } from '$app/paths';
  import { CHANNELS, NO_VALUE, OTHER, sizeFactor, type LegendEntry, type Results } from '$lib/lens';
  import { reportHref } from '$lib/graphrag';

  let {
    results,
    ds = '',
    selected = null,
    onselect,
  }: {
    results: Results;
    /** The dataset, for the links of GraphRAG communities to their reports. */
    ds?: string;
    /** The key of the entry whose nodes are selected. */
    selected?: string | null;
    onselect?: (key: string | null, ids: string[]) => void;
  } = $props();

  const active = $derived(CHANNELS.flatMap((ch) => (results[ch] ? [results[ch]!] : [])));
  /** Open on wide screens, a one-line strip on narrow ones until tapped. */
  let open = $state(
    typeof matchMedia === 'function' ? !matchMedia('(max-width: 640px)').matches : true,
  );
  const line = $derived(active.map((r) => r.title).join(' · '));

  const swatch = (i: number | undefined) =>
    i == null ? 'transparent' : i === OTHER ? 'var(--lens-other)' : `var(--lens-${i + 1})`;

  function pick(channel: string, e: LegendEntry) {
    const key = `${channel}:${e.key}`;
    if (!e.ids.length) return;
    onselect?.(selected === key ? null : key, selected === key ? [] : e.ids);
  }
</script>

{#if active.length}
  <section class="legend" class:open aria-label="Lens legend" data-testid="lens-legend">
    <button
      class="toggle"
      aria-expanded={open}
      onclick={() => (open = !open)}
      title={open ? 'Hide the legend' : 'Show the legend'}
    >
      <span class="line">{open ? 'Legend' : line}</span>
      <span aria-hidden="true" class="chev">{open ? '▾' : '▸'}</span>
    </button>
    {#if open}
      {#each active as r (r.channel)}
        <div class="group">
          <p class="title">{r.title}</p>
          <ul>
            {#each r.legend as e (e.key)}
              {@const key = `${r.channel}:${e.key}`}
              <li>
                <button
                  class="entry"
                  aria-pressed={selected === key}
                  disabled={!e.ids.length}
                  onclick={() => pick(r.channel, e)}
                  title={e.ids.length ? 'Select these nodes' : undefined}
                >
                  {#if r.channel === 'colour'}
                    <span class="sw" style:background={swatch(e.swatch)} aria-hidden="true"></span>
                    <span class="txt"
                      >{e.swatch != null && e.swatch !== OTHER
                        ? `colour ${e.swatch + 1} is `
                        : ''}{e.text}</span
                    >
                  {:else if r.channel === 'size'}
                    {@const d = Math.round(7 * sizeFactor(e.step))}
                    <span class="sz" aria-hidden="true"
                      ><span
                        class="dot"
                        class:none={e.step === NO_VALUE}
                        style:width={`${d}px`}
                        style:height={`${d}px`}
                      ></span></span
                    >
                    <span class="txt">{e.text}</span>
                  {:else if r.channel === 'badge'}
                    <!-- the validation lens's entries name their severity's mark in the text -->
                    {#if r.lens !== 'validation'}<span class="pill" aria-hidden="true">i</span>{/if}
                    <span class="txt">{e.text}</span>
                  {:else}
                    <svg class="dash" width="26" height="10" aria-hidden="true"
                      ><line
                        x1="1"
                        y1="5"
                        x2="25"
                        y2="5"
                        stroke-dasharray={e.key === 'inferred' ? '6 4' : ''}
                      /></svg
                    >
                    <span class="txt">{e.text}</span>
                  {/if}
                  <span class="n faint">{e.count.toLocaleString('en-US')}</span>
                </button>
                {#if e.community && ds}
                  <a
                    class="report"
                    href={reportHref(`${base}/datasets/${encodeURIComponent(ds)}`, e.community)}
                    title="Open this community's report in the Communities tab">report</a
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/each}
      <p class="foot faint">Computed for this picture, not saved.</p>
    {/if}
  </section>
{/if}

<style>
  .legend {
    position: absolute;
    left: 10px;
    bottom: 10px;
    z-index: 5;
    max-width: min(320px, calc(100% - 20px));
    max-height: calc(100% - 60px);
    overflow-y: auto;
    background: color-mix(in srgb, var(--surface) 94%, transparent);
    border: 1px solid var(--border);
    border-radius: var(--r);
    box-shadow: var(--shadow-pop);
    font-size: var(--fs-xs);
    padding: 4px 8px 6px;
  }
  .toggle {
    display: flex;
    width: 100%;
    justify-content: space-between;
    gap: 8px;
    align-items: center;
    background: none;
    border: 0;
    padding: 2px 0;
    color: var(--text-2);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    text-align: left;
  }
  .line {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .group {
    margin-top: 4px;
  }
  .title {
    margin: 2px 0;
    color: var(--text);
    font-weight: 600;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .report {
    font-size: var(--fs-xs);
    flex: none;
  }
  .entry {
    display: flex;
    gap: 6px;
    align-items: center;
    width: 100%;
    background: none;
    border: 0;
    border-radius: var(--r-sm);
    padding: 2px 4px;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .entry:hover:not(:disabled) {
    background: var(--hover);
  }
  .entry:disabled {
    cursor: default;
    color: var(--text-2);
  }
  .entry[aria-pressed='true'] {
    background: var(--spark-soft);
  }
  .txt {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .n {
    font-variant-numeric: tabular-nums;
  }
  .sw {
    width: 11px;
    height: 11px;
    border-radius: 50%;
    flex: none;
  }
  .sz {
    width: 22px;
    display: grid;
    place-items: center;
    flex: none;
  }
  .dot {
    border-radius: 50%;
    background: var(--iri);
  }
  .dot.none {
    background: transparent;
    border: 1px dashed var(--text-3);
  }
  .pill {
    display: inline-grid;
    place-items: center;
    min-width: 18px;
    height: 13px;
    border-radius: 7px;
    background: var(--spark);
    color: var(--on-spark);
    font-weight: 700;
    font-size: 9px;
    flex: none;
  }
  .dash {
    flex: none;
    stroke: var(--text-2);
    stroke-width: 1.6;
  }
  .foot {
    margin: 4px 0 0;
  }
  /* the 390 px layout: a collapsible strip above the graph (§2.6) */
  @media (max-width: 640px) {
    .legend {
      left: 0;
      right: 0;
      top: 0;
      bottom: auto;
      max-width: none;
      max-height: 60%;
      border-radius: 0;
      border-width: 0 0 1px;
      box-shadow: none;
    }
  }
</style>
