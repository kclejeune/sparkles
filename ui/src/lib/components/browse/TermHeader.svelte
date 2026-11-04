<script lang="ts">
  // The head of an entity's page: breadcrumbs, the title, the short form, the kind, the IRI
  // with a copy button, the definition as a lede with its sources, and for a deprecated
  // entity the line that names its replacement. The page passes its actions.
  import type { Snippet } from 'svelte';
  import { toasts } from '$lib/app.svelte';
  import type { Linker, Name, Sourced } from '$lib/browse';
  import Icon from '$components/Icon.svelte';
  import PageActions from '$components/PageActions.svelte';
  import LinkedText from './LinkedText.svelte';
  import { shortFormRepeats, valueText } from '$lib/browse';
  import { displayIri, type PrefixMap } from '$lib/rdf';

  let {
    name,
    title,
    kind,
    crumbs = [],
    definition = null,
    replacedBy = [],
    consider = [],
    linker,
    curieIri,
    prefixes = {},
    actions,
  }: {
    name: Pick<Name, 'iri' | 'shortForm' | 'deprecated'>;
    title: string;
    /** The kind in words, such as "class". */
    kind: string;
    crumbs?: { label: string; href: string | null }[];
    definition?: Sourced | null;
    replacedBy?: string[];
    consider?: string[];
    linker: Linker;
    curieIri?: (curie: string) => string | null;
    prefixes?: PrefixMap;
    actions?: Snippet;
  } = $props();

  // as the Full/Prefixed switch writes it, with the prefix the rest of the page shows
  const shownIri = $derived(displayIri(name.iri, prefixes));

  async function copy() {
    try {
      await navigator.clipboard.writeText(name.iri);
      toasts.push('success', 'Copied the IRI', undefined, 1400);
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }
</script>

<!-- the shared page head: the crumbs above, the title with the actions level with it, and
     the IRI row and notes under the title (ROADMAP item 633) -->
<header class="page-head term-head">
  {#if crumbs.length}
    <nav class="crumbs" aria-label="Breadcrumbs">
      {#each crumbs as c, i (i)}
        {#if i > 0}<span aria-hidden="true">›</span>{/if}
        {#if c.href}<a href={c.href}>{c.label}</a>{:else}<span>{c.label}</span>{/if}
      {/each}
    </nav>
  {/if}
  <h1 class="term-title" class:struck={name.deprecated}>{title}</h1>
  {#if actions}<PageActions small>{@render actions()}</PageActions>{/if}
  <div class="ids">
    {#if name.shortForm && name.shortForm !== title && !shortFormRepeats(name.shortForm, name.iri, shownIri)}<span
        class="sf">{name.shortForm}</span
      >{/if}
    <span class="badge iri">{kind}</span>
    {#if name.deprecated}<span class="badge danger">deprecated</span>{/if}
    <!-- the prefixed form is for the eye, and a screen reader hears the full IRI that it copies -->
    {#if shownIri === name.iri}<span class="iri-text" title={name.iri}>{name.iri}</span>{:else}<span
        class="iri-text"
        title={name.iri}
        ><span class="sr-only">{name.iri}</span><span aria-hidden="true">{shownIri}</span></span
      >{/if}
    <button
      class="btn icon sm ghost copy"
      type="button"
      title="Copy the IRI"
      aria-label="Copy the IRI"
      onclick={copy}><Icon name="copy" size={14} /></button
    >
  </div>
  {#if name.deprecated}
    <p class="depr" role="note">
      This {kind} is deprecated.
      {#if replacedBy.length}
        Use {#each replacedBy as r, i (r)}{#if i > 0},
          {/if}{#if linker.href(r)}<a href={linker.href(r)}>{linker.name(r)}</a>{:else}{linker.name(
              r,
            )}{/if}{/each} instead.
      {/if}
      {#if consider.length}
        Consider {#each consider as r, i (r)}{#if i > 0},
          {/if}{#if linker.href(r)}<a href={linker.href(r)}>{linker.name(r)}</a>{:else}{linker.name(
              r,
            )}{/if}{/each}.
      {/if}
    </p>
  {/if}
  {#if definition}
    <p class="lede">
      <LinkedText text={valueText(definition.value)} {linker} {curieIri} />
      {#if definition.sources.length}<span class="src">{definition.sources.join(', ')}</span>{/if}
    </p>
  {/if}
</header>

<style>
  .term-head {
    min-width: 0;
  }
  .crumbs {
    font-size: var(--fs-sm);
    color: var(--text-3);
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  /* the small actions sit level with the middle of the large title's first line */
  @media (min-width: 640px) {
    .term-head > :global(.page-actions) {
      margin-top: calc((var(--fs-display) * 1.12 - var(--ctl-h-sm)) / 2);
    }
  }
  .crumbs a {
    color: var(--text-2);
  }
  .term-title {
    font-size: var(--fs-display);
    line-height: 1.12;
    letter-spacing: -0.025em;
    font-weight: 600;
    margin: 0;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .struck {
    text-decoration: line-through;
    text-decoration-thickness: 2px;
    text-decoration-color: var(--danger);
  }
  .ids {
    display: flex;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    flex-wrap: wrap;
    min-width: 0;
  }
  .sf {
    font: 500 12px var(--font-mono);
    color: var(--text-2);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 1px 6px;
  }
  .iri-text {
    font: 12px var(--font-mono);
    color: var(--iri);
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .copy {
    color: var(--text-3);
  }
  .lede {
    margin-top: var(--sp-5);
    font-size: 15px;
    line-height: 1.6;
    max-width: 68ch;
    color: var(--text);
  }
  .lede :global(a) {
    color: var(--iri);
  }
  .src {
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .depr {
    margin-top: var(--sp-4);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r);
    background: var(--danger-soft);
    color: var(--danger);
    font-weight: 500;
  }
  .depr a {
    color: inherit;
    text-decoration: underline;
  }
  @container browse (max-width: 639px) {
    .lede {
      font-size: 14px;
    }
  }
</style>
