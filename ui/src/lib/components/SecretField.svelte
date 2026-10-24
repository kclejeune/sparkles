<script lang="ts" module>
  /** What the field shows about the key a name holds. */
  export type SecretView = { set: boolean; label: string; title: string; locked?: boolean };
</script>

<script lang="ts">
  // A settings field that names a secret, such as a provider's API key: the name, which
  // suggests the secrets the server knows, and the state of the key it names, masked,
  // since the server never sends a key back. Set key opens a password input whose key is
  // sent once and is not kept in the page.
  import Icon from './Icon.svelte';

  let {
    value,
    id,
    disabled = false,
    placeholder,
    names = [],
    keyState,
    onstore,
    onchange,
  }: {
    value: unknown;
    id: string;
    disabled?: boolean;
    placeholder?: string;
    /** The secrets the server knows. */
    names?: readonly string[];
    /** The state of the key a name holds, or null when no secret has the name. */
    keyState?: (name: string) => SecretView | null;
    /** Store a key under a name. True when it was stored. */
    onstore?: (name: string, key: string) => Promise<boolean>;
    onchange: (v: string) => void;
  } = $props();

  const name = $derived(typeof value === 'string' ? value.trim() : '');
  const view = $derived(name && keyState ? keyState(name) : null);
  let setting = $state(false);
  let busy = $state(false);

  async function store(e: SubmitEvent | MouseEvent, input: HTMLInputElement | null) {
    e.preventDefault();
    if (!input?.value || !onstore || !name) return;
    busy = true;
    try {
      if (await onstore(name, input.value)) setting = false;
    } finally {
      input.value = '';
      busy = false;
    }
  }
  let keyInput = $state<HTMLInputElement | null>(null);
</script>

<div class="secret">
  <input
    {id}
    class="input mono"
    value={typeof value === 'string' ? value : ''}
    {placeholder}
    list="{id}-names"
    {disabled}
    autocomplete="off"
    spellcheck="false"
    aria-describedby="{id}-state"
    oninput={(e) => onchange(e.currentTarget.value)}
  />
  <datalist id="{id}-names">
    {#each names as n (n)}<option value={n}></option>{/each}
  </datalist>
  {#if name}
    <div class="state" id="{id}-state">
      {#if view?.set}
        <span class="badge ok"><Icon name="key" size={11} /> key set</span>
        <span class="mask" aria-label="The key is hidden">••••••••</span>
        <span class="faint" title={view.title}>{view.label}</span>
      {:else}
        <span class="badge danger"><Icon name="key" size={11} /> no key</span>
        <span class="faint">{view ? view.title : `No key saved for ${name} yet.`}</span>
      {/if}
      {#if onstore && !disabled && !view?.locked}
        <button
          type="button"
          class="btn sm"
          aria-expanded={setting}
          aria-label="{view?.set ? 'Replace' : 'Set'} the key for {name}"
          onclick={() => (setting = !setting)}>{view?.set ? 'Replace key' : 'Set key'}</button
        >
      {/if}
    </div>
    {#if setting}
      <div class="set">
        <input
          bind:this={keyInput}
          class="input mono"
          type="password"
          autocomplete="off"
          spellcheck="false"
          aria-label="New key for {name}"
          placeholder="Paste the key"
          onkeydown={(e) => {
            if (e.key === 'Enter') void store(e as unknown as MouseEvent, keyInput);
          }}
        />
        <button type="button" class="btn sm" disabled={busy} onclick={() => (setting = false)}
          >Cancel</button
        >
        <button
          type="button"
          class="btn primary sm"
          disabled={busy}
          onclick={(e) => store(e, keyInput)}>Save key</button
        >
      </div>
      <p class="faint small">
        The key is sent once and is not shown again. API keys below lists it with the others.
      </p>
    {/if}
  {/if}
</div>

<style>
  /* the editor fits the width of its row, however long its entries */
  .secret > :global(*) {
    min-width: 0;
  }
  .secret {
    display: grid;
    gap: 4px;
    max-width: 420px;
    min-width: 0;
  }
  .input {
    width: 100%;
  }
  .state,
  .set {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    font-size: var(--fs-sm);
  }
  .set .input {
    flex: 1 1 200px;
    width: auto;
  }
  .mask {
    font-family: var(--font-mono);
    letter-spacing: 1px;
    color: var(--text-3);
  }
  .small {
    margin: 0;
    font-size: var(--fs-xs);
  }
</style>
