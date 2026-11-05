<script lang="ts">
  import { iriForm } from '$lib/iri-form.svelte';
  import { languageOrder, type EntityDisplay } from '$lib/rendering';
  import Modal from './Modal.svelte';
  import Icon from './Icon.svelte';
  let open = $state(false);
  let mode = $state<EntityDisplay>('label');
  let languages = $state('');
  const parsed = $derived(languageOrder(languages));
  function show() {
    mode = iriForm.mode;
    languages = iriForm.languageText;
    open = true;
  }
  function apply() {
    if (!parsed) return;
    iriForm.update(mode, parsed);
    open = false;
  }
</script>

<button
  class="btn ghost icon"
  aria-label="Rendering preferences"
  title="Rendering preferences"
  onclick={show}><Icon name="sliders" size={16} /></button
>
<Modal bind:open title="Rendering preferences" width={440}>
  <form
    onsubmit={(e) => {
      e.preventDefault();
      apply();
    }}
  >
    <label
      >Show entities by
      <select class="select" bind:value={mode} aria-label="Show entities by">
        <option value="label">Label</option>
        <option value="prefixed">Prefixed name</option>
        <option value="iri">Full IRI</option>
      </select>
    </label>
    <label
      >Language order
      <input
        class="input"
        bind:value={languages}
        aria-label="Language order"
        placeholder="fr, en"
        aria-invalid={!parsed}
        aria-describedby="render-language-help"
      />
    </label>
    <p id="render-language-help" class="muted">
      Use comma-separated language tags, most preferred first. Leave empty to use your browser's
      languages. Labels fall back to untagged values, English, then another language.
    </p>
    {#if !parsed}<p class="err" role="alert">Enter language tags such as fr, en-GB.</p>{/if}
    <p class="muted">
      Missing labels use a prefixed name, or the full IRI when no prefix matches. These preferences
      are saved in this browser.
    </p>
    <div class="actions">
      <button type="button" class="btn" onclick={() => (open = false)}>Cancel</button><button
        class="btn primary"
        disabled={!parsed}>Apply</button
      >
    </div>
  </form>
</Modal>

<style>
  form {
    display: grid;
    gap: var(--sp-3);
  }
  label {
    display: grid;
    gap: var(--sp-2);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }
</style>
