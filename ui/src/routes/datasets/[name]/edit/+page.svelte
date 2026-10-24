<script lang="ts">
  // The editing form of one resource on its own page (docs/specs/U02-shape-driven-forms.md
  // §2.1): `/datasets/{name}/edit?iri=…&branch=…`, for links from other tools.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { app } from '$lib/app.svelte';
  import { branchParam } from '$lib/branches';
  import { displayIri } from '$lib/rdf';
  import Icon from '$components/Icon.svelte';
  import ShapeForm from '$components/ShapeForm.svelte';
  import type { FormSaveAnswer } from '$lib/forms-api';

  const name = $derived(page.params.name ?? '');
  const iri = $derived(page.url.searchParams.get('iri')?.trim() ?? '');
  /** `?class=`: the form of a new instance of the class. */
  const cls = $derived(page.url.searchParams.get('class')?.trim() ?? '');
  const prefixes = $derived(app.prefixes(name));
  let branch = $state<string | null>(branchParam(page.url.searchParams.get('branch')));

  $effect(() => {
    if (name) void app.loadPrefixes(name);
  });

  /** After a new instance's first save, edit the resource it made. */
  function saved(a: FormSaveAnswer) {
    if (!cls || !a.focus) return;
    const q = new URLSearchParams({ iri: a.focus });
    if (a.branch && a.branch !== 'main') q.set('branch', a.branch);
    goto(`${resolve('/datasets/[name]/edit', { name })}?${q}`, { replaceState: true });
  }

  /** Keep the branch the form moved to in the URL, so a reload opens the same one. */
  $effect(() => {
    const b = branch;
    // a new instance's save moves to the resource with its branch (see `saved`)
    if (cls) return;
    const q = new URLSearchParams(page.url.searchParams);
    if ((q.get('branch') ?? null) === b) return;
    if (b) q.set('branch', b);
    else q.delete('branch');
    goto(`${resolve('/datasets/[name]/edit', { name })}?${q}`, {
      replaceState: true,
      keepFocus: true,
      noScroll: true,
    });
  });
</script>

<svelte:head
  ><title
    >{cls
      ? `New ${displayIri(cls, prefixes)}`
      : `Edit ${iri ? displayIri(iri, prefixes) : 'a resource'}`}
    | Sparkles</title
  ></svelte:head
>

<div class="page page-container">
  <nav class="crumbs">
    <a href={resolve('/datasets')}>Datasets</a><Icon name="chevron" size={12} /><a
      href={resolve('/datasets/[name]', { name })}>{name}</a
    ><Icon name="chevron" size={12} /><span>Edit</span>
  </nav>
  {#if !iri && !cls}
    <p class="empty">
      Name the resource to edit with <span class="mono">?iri=</span>, or the class of a new instance
      with <span class="mono">?class=</span>.
    </p>
  {:else}
    <div class="form-wrap">
      {#key `${name}|${iri}|${cls}`}
        <ShapeForm
          ds={name}
          iri={cls ? '' : iri}
          cls={cls || undefined}
          bind:branch
          {prefixes}
          onsaved={saved}
        />
      {/key}
    </div>
  {/if}
</div>

<style>
  .page {
    padding: 18px 28px 40px;
    gap: 16px;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .crumbs a {
    color: var(--text-2);
  }
  .form-wrap {
    max-width: 760px;
    min-width: 0;
  }
  @media (max-width: 600px) {
    .page {
      padding: 12px 16px 32px;
    }
  }
</style>
