<script lang="ts">
  // The top of a hierarchy: the root terms an ontology names, or the roots the hierarchy
  // route computes, each with its definition and its number of children.
  import { countOf, treeWords, type Hierarchy, type TreeKind, type TreeNode } from '$lib/browse';

  let {
    title,
    roots,
    kind,
    definitions,
    href,
    from = null,
    deprecated = 0,
    more,
    busy = false,
  }: {
    title: string;
    roots: TreeNode[];
    kind: TreeKind;
    definitions: Map<string, string>;
    href: (iri: string) => string;
    from?: Hierarchy['roots'] | null;
    deprecated?: number;
    more?: () => void;
    busy?: boolean;
  } = $props();
  const words = $derived(treeWords(kind));
  const name = (n: TreeNode) => n.label ?? n.shortForm ?? n.iri;
</script>

<section class="term-sec" aria-labelledby="roots-h">
  <h2 id="roots-h">{title}</h2>
  {#if !roots.length}
    <p class="muted">This graph has none.</p>
  {:else}
    <ul class="roots">
      {#each roots as n (n.iri)}
        <li>
          <div class="t">
            <a href={href(n.iri)} class:dep={n.deprecated}>{name(n)}</a>
            {#if n.shortForm && n.shortForm !== name(n)}<span class="sfx">{n.shortForm}</span>{/if}
            {#if n.deprecated}<span class="badge danger">deprecated</span>{/if}
            <span class="spacer"></span>
            {#if n.children}<span class="kids"
                >{countOf(n.children, words.child, words.children)}</span
              >{/if}
          </div>
          {#if definitions.get(n.iri)}<p>{definitions.get(n.iri)}</p>{/if}
        </li>
      {/each}
    </ul>
    {#if more}
      <button class="more-btn" type="button" disabled={busy} onclick={more}
        >{busy ? 'Reading…' : 'Show more'}</button
      >
    {/if}
  {/if}
  {#if from === 'ontology'}
    <p class="faint foot">The ontology names these as its root terms.</p>
  {:else if from === 'scheme'}
    <p class="faint foot">These are the top concepts of the scheme.</p>
  {/if}
  {#if deprecated}
    <p class="faint foot">
      {countOf(deprecated, 'deprecated root is', 'deprecated roots are')} listed last.
    </p>
  {/if}
</section>

<style>
  .roots {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .roots li {
    padding: var(--sp-3) 0;
    border-top: 1px solid var(--border);
    min-width: 0;
  }
  .roots li:first-child {
    border-top: 0;
    padding-top: 0;
  }
  .t {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px var(--sp-2);
    font-size: var(--fs-md);
    font-weight: 600;
    min-width: 0;
  }
  .t a {
    color: var(--iri);
    overflow-wrap: anywhere;
  }
  .dep {
    text-decoration: line-through;
  }
  .sfx {
    font: 400 11px var(--font-mono);
    color: var(--text-3);
  }
  .kids {
    font-size: var(--fs-sm);
    font-weight: 400;
    color: var(--text-3);
  }
  p {
    color: var(--text-2);
    margin-top: 4px;
  }
  .foot {
    margin-top: var(--sp-3);
    font-size: var(--fs-sm);
  }
</style>
