<script lang="ts">
  import type { Term } from '$lib/api';
  import { displayTerm, iri, WELL_KNOWN, type PrefixMap } from '$lib/rdf';
  import { FIELDS, id, listKind, RDF, same, SH, ShapeGraph } from '$lib/shape-editor';
  import ShapeValue from './ShapeValue.svelte';
  import ShapePathField from './ShapePathField.svelte';
  let {
    graph,
    subject,
    prefixes,
    readonly = false,
    onchange,
    onpending,
    onselect,
    onnewproperty,
  }: {
    graph: ShapeGraph;
    subject: Term;
    prefixes: PrefixMap;
    readonly?: boolean;
    onchange: (graph: ShapeGraph) => void;
    onpending: (key: string, pending: boolean) => void;
    onselect: (term: Term) => void;
    onnewproperty: () => void;
  } = $props();
  let adding = $state<string | null>(null);
  let addFields = $state<Record<string, string>>({
    Targets: 'targetClass',
    Description: 'name',
    Constraints: 'class',
  });
  let attach = $state('');
  let removing = $state(false);
  let error = $state<string | null>(null);
  const property = $derived(graph.property(subject));
  const known = new Set([
    RDF + 'type',
    SH + 'path',
    SH + 'property',
    ...FIELDS.map((f) => SH + f.name),
  ]);
  const unknown = $derived(
    graph.statements(subject).filter(([, p]) => !known.has(String(p.value))),
  );
  const implicit = $derived(
    graph.values(subject, RDF + 'type').some((t) => same(t, iri(WELL_KNOWN.rdfs + 'Class'))),
  );
  const shapeOptions = $derived(graph.shapes());
  function change(g: ShapeGraph) {
    error = null;
    onchange(g);
  }
  function safely(fn: () => ShapeGraph) {
    try {
      change(fn());
      return true;
    } catch (e) {
      error = (e as Error).message;
      return false;
    }
  }
</script>

<div class="definition">
  <header>
    <div>
      <span class="kind">{property ? 'Property shape' : 'Node shape'}</span>
      <h2>{displayTerm(subject, prefixes)}</h2>
    </div>
    <button class="btn ghost" disabled={readonly} onclick={() => (removing = !removing)}
      >Delete shape</button
    >
  </header>
  {#if removing}
    <div class="delete-box">
      <p>
        Delete this shape's definition? Referenced shapes and unrelated statements stay. Remove
        incoming links first.
      </p>
      <button class="btn" disabled={readonly} onclick={() => safely(() => graph.remove(subject))}
        >Delete definition</button
      >
      <button class="btn ghost" onclick={() => (removing = false)}>Cancel deletion</button>
    </div>
  {/if}
  {#if property}
    <section aria-label="Property path">
      <h3>Path</h3>
      {#key graph
        .values(subject, SH + 'path')
        .map(id)
        .join('|')}
        <ShapePathField
          {graph}
          {subject}
          {prefixes}
          {readonly}
          onchange={change}
          onpending={(p) => onpending('path', p)}
        />
      {/key}
    </section>
  {/if}
  {#each ['Targets', 'Description', 'Constraints'] as group (group)}
    <section aria-label={group}>
      <h3>{group}</h3>
      {#if group === 'Targets'}
        <label class="check"
          ><input
            type="checkbox"
            checked={implicit}
            disabled={readonly}
            onchange={(e) =>
              safely(() =>
                graph.replaceValue(
                  subject,
                  RDF + 'type',
                  implicit ? iri(WELL_KNOWN.rdfs + 'Class') : null,
                  e.currentTarget.checked ? iri(WELL_KNOWN.rdfs + 'Class') : null,
                ),
              )}
          />Also a class with an implicit class target</label
        >
      {/if}
      {#each FIELDS.filter((f) => f.group === group) as field (field.name)}
        {@const values = graph.values(subject, SH + field.name)}
        {#if values.length || adding === field.name}
          <div class="constraint" data-field={field.name}>
            <h4>sh:{field.label}</h4>
            {#if field.propertyOnly && !property}<p class="warn-text">
                This parameter applies to property shapes. Its stored RDF is preserved.
              </p>{/if}
            {#if !field.multiple && values.length > 1}<p class="warn-text">
                This parameter has several values. Remove or replace them explicitly.
              </p>{/if}
            {#each values as value (id(value))}
              <ShapeValue
                {field}
                {value}
                {graph}
                {prefixes}
                {readonly}
                onpending={(p) => onpending(field.name + id(value), p)}
                onapply={(vs) =>
                  safely(() =>
                    listKind(field.kind)
                      ? graph.setList(subject, SH + field.name, value, vs)
                      : graph.replaceValue(subject, SH + field.name, value, vs[0]),
                  )}
                onremove={() =>
                  safely(() =>
                    listKind(field.kind)
                      ? graph.setList(subject, SH + field.name, value, null)
                      : graph.replaceValue(subject, SH + field.name, value, null),
                  )}
              />
            {/each}
            {#if adding === field.name}
              <ShapeValue
                {field}
                {graph}
                {prefixes}
                {readonly}
                onpending={(p) => onpending('new-' + field.name, p)}
                onapply={(vs) => {
                  const applied = safely(() =>
                    listKind(field.kind)
                      ? graph.setList(subject, SH + field.name, null, vs)
                      : graph.replaceValue(subject, SH + field.name, null, vs[0]),
                  );
                  if (applied) adding = null;
                  return applied;
                }}
              />
              <button
                class="btn ghost"
                onclick={() => {
                  onpending('new-' + field.name, false);
                  adding = null;
                }}>Cancel new parameter</button
              >
            {/if}
          </div>
        {/if}
      {/each}
      <div class="add-row">
        <select
          class="select"
          aria-label={`Add ${group.toLowerCase()} parameter`}
          bind:value={addFields[group]}
          disabled={readonly}
        >
          <option value="" disabled>Choose a parameter</option>
          {#each FIELDS.filter((f) => f.group === group && (!f.propertyOnly || property) && (f.multiple || !graph.values(subject, SH + f.name).length)) as field (field.name)}<option
              value={field.name}>sh:{field.label}</option
            >{/each}
        </select>
        <button
          class="btn"
          disabled={readonly || !!adding}
          onclick={() => {
            const options = FIELDS.filter(
              (f) =>
                f.group === group &&
                (!f.propertyOnly || property) &&
                (f.multiple || !graph.values(subject, SH + f.name).length),
            );
            const f = options.find((f) => f.name === addFields[group]) ?? options[0];
            if (f) adding = f.name;
          }}
          >Add {group === 'Constraints'
            ? 'constraint'
            : group === 'Targets'
              ? 'target'
              : 'description'}</button
        >
      </div>
    </section>
  {/each}
  <section aria-label="Property shapes">
    <h3>Property shapes</h3>
    {#each graph.values(subject, SH + 'property') as child (id(child))}
      <div class="link-row">
        <button class="btn ghost" onclick={() => onselect(child)}
          >{displayTerm(child, prefixes)}</button
        ><button
          class="btn ghost"
          disabled={readonly}
          aria-label={`Unlink ${displayTerm(child, prefixes)}`}
          onclick={() => safely(() => graph.replaceValue(subject, SH + 'property', child, null))}
          >Unlink</button
        >
      </div>
    {/each}
    <div class="add-row">
      <button class="btn" disabled={readonly} onclick={onnewproperty}>New property shape</button>
      <select
        class="select"
        aria-label="Existing property shape"
        bind:value={attach}
        disabled={readonly}
        ><option value="">Choose existing</option
        >{#each shapeOptions.filter((s) => graph.property(s)) as shape (id(shape))}<option
            value={id(shape)}>{displayTerm(shape, prefixes)}</option
          >{/each}</select
      >
      <button
        class="btn"
        disabled={readonly || !attach}
        onclick={() => {
          const term = shapeOptions.find((t) => id(t) === attach);
          if (term) safely(() => graph.replaceValue(subject, SH + 'property', null, term));
        }}>Attach</button
      >
    </div>
  </section>
  {#if graph.links(subject).some((l) => l.property !== 'property')}
    <section aria-label="Referenced shapes">
      <h3>Referenced shapes</h3>
      {#each graph.links(subject).filter((l) => l.property !== 'property') as link, i (i)}<button
          class="btn ghost"
          onclick={() => onselect(link.term)}
          >sh:{link.property} → {displayTerm(link.term, prefixes)}</button
        >{/each}
    </section>
  {/if}
  <section aria-label="Additional RDF">
    <h3>Additional RDF</h3>
    <p class="help">
      Custom constraints, rules and annotations remain unchanged. The text validator can check the
      complete draft.
    </p>
    {#if unknown.length}<ul>
        {#each unknown as triple, i (i)}<li>
            <code>{triple.map((t) => displayTerm(t, prefixes)).join(' ')}</code>
          </li>{/each}
      </ul>{:else}<p class="faint">No additional statements on this shape.</p>{/if}
  </section>
  <datalist id="shape-references"
    >{#each shapeOptions as shape (id(shape))}<option
        value={shape.type === 'bnode' ? '_:' + shape.value : displayTerm(shape, prefixes)}
      ></option>{/each}</datalist
  >
  {#if error}<p class="error-box" role="alert">{error}</p>{/if}
</div>

<style>
  .definition {
    display: grid;
    gap: 18px;
    min-width: 0;
  }
  header {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: 10px;
  }
  h2 {
    font-size: var(--fs-lg);
    overflow-wrap: anywhere;
    margin: 4px 0 0;
  }
  .kind,
  .help,
  .check {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    margin: 0 0 10px;
  }
  h4 {
    font-size: var(--fs-sm);
    font-family: var(--font-mono);
    font-weight: 500;
    margin: 0 0 8px;
  }
  section {
    border-top: 1px solid var(--border);
    padding-top: 14px;
    min-width: 0;
  }
  .constraint {
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 10px;
    margin-bottom: 10px;
    display: grid;
    gap: 6px;
  }
  .add-row,
  .link-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    margin-top: 10px;
  }
  .add-row select {
    flex: 1 1 180px;
    min-width: 0;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-bottom: 12px;
  }
  .delete-box {
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
  }
  p {
    font-size: var(--fs-sm);
    margin: 0 0 8px;
  }
  ul {
    margin: 0;
    padding-left: 18px;
  }
  code {
    overflow-wrap: anywhere;
    font-size: var(--fs-xs);
  }
</style>
