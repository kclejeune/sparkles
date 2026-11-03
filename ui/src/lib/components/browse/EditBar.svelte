<script lang="ts">
  // The bar of the ontology browser's edit mode (ROADMAP item 630): the one signal that the
  // page is in edit mode, with the branch the edits go to, Undo and Redo of the session's
  // commits, Start session, the full editor and Done. On a large or medium screen it sits at
  // the top of the page, sticky under the top bar, and on a phone at the bottom of the
  // screen in thumb reach (`variant="bottom"`). A protected branch asks for a session.
  import Icon from '$components/Icon.svelte';

  let {
    variant,
    branch,
    protectedBranch,
    canSession,
    undoLabel,
    redoLabel,
    notice,
    error,
    editorHref,
    editorName,
    onundo,
    onredo,
    onsession,
    ondone,
  }: {
    variant: 'top' | 'bottom';
    branch: string;
    protectedBranch: boolean;
    /** Whether Start session applies: the page is on the main branch. */
    canSession: boolean;
    /** The label of the commit Undo takes back, or null when there is none. */
    undoLabel: string | null;
    redoLabel: string | null;
    /** Another writer's commit, as a sentence. */
    notice: string | null;
    /** Why a session could not start. */
    error: string | null;
    editorHref: string;
    /** "Ontology" or "Thesaurus". */
    editorName: string;
    onundo: () => void;
    onredo: () => void;
    onsession: () => void;
    ondone: () => void;
  } = $props();

  const mod = typeof navigator !== 'undefined' && /Mac|iP(hone|ad)/.test(navigator.platform);
  const undoKey = mod ? '⌘Z' : 'Ctrl+Z';
  const redoKey = mod ? '⇧⌘Z' : 'Ctrl+Shift+Z';
  const hint = $derived(
    error ??
      notice ??
      (protectedBranch
        ? `${branch} is protected. Start a session to edit on a branch of your own.`
        : `Each change is previewed, then committed to ${branch}.`),
  );
  let menu: HTMLDetailsElement | undefined = $state();
</script>

{#if variant === 'top'}
  <div class="ebar" class:warn={protectedBranch || !!error} role="toolbar" aria-label="Edit mode">
    <span class="state">Editing</span>
    <span class="brn" title="The branch the edits go to">{branch}</span>
    <span class="hint" role="status">{hint}</span>
    <span class="grp">
      {#if !protectedBranch}
        <button
          type="button"
          class="btn ghost sm"
          disabled={!undoLabel}
          title={undoLabel ? `Undo: ${undoLabel} (${undoKey})` : 'Nothing to undo'}
          onclick={onundo}><Icon name="stepBack" size={14} />Undo</button
        >
        <button
          type="button"
          class="btn ghost sm"
          disabled={!redoLabel}
          title={redoLabel ? `Redo: ${redoLabel} (${redoKey})` : 'Nothing to redo'}
          onclick={onredo}><Icon name="stepForward" size={14} />Redo</button
        >
        <span class="vsep" aria-hidden="true"></span>
      {/if}
      {#if canSession}
        <button
          type="button"
          class="btn sm"
          class:primary={protectedBranch}
          class:ghost={!protectedBranch}
          title="Edit on a branch of your own, then merge it"
          onclick={onsession}><Icon name="branch" size={14} />Start session</button
        >
      {/if}
      <a class="btn ghost sm" href={editorHref}
        ><Icon name={editorName === 'Thesaurus' ? 'layers' : 'tree'} size={14} />{editorName} editor</a
      >
      <button type="button" class="btn sm" onclick={ondone}>Done <span class="k">Esc</span></button>
    </span>
  </div>
{:else}
  <div class="pbar" role="toolbar" aria-label="Edit mode">
    <span class="state"
      ><span class="dot" aria-hidden="true"></span><span class="lines"
        ><span>Editing</span><span class="brn">{branch}</span></span
      ></span
    >
    {#if protectedBranch || error || notice}<span class="sr-only" role="status">{hint}</span>{/if}
    <span class="r">
      {#if !protectedBranch}
        <button type="button" class="btn" aria-label="Undo" disabled={!undoLabel} onclick={onundo}
          ><Icon name="stepBack" size={16} /></button
        >
        <button type="button" class="btn" aria-label="Redo" disabled={!redoLabel} onclick={onredo}
          ><Icon name="stepForward" size={16} /></button
        >
      {/if}
      <details class="more" bind:this={menu}>
        <summary class="btn" aria-label="More edit actions"><Icon name="menu" size={16} /></summary>
        <div class="menu" role="menu">
          {#if canSession}
            <button
              type="button"
              role="menuitem"
              onclick={() => {
                if (menu) menu.open = false;
                onsession();
              }}><Icon name="branch" size={14} />Start session</button
            >
          {/if}
          <a role="menuitem" href={editorHref}
            ><Icon name={editorName === 'Thesaurus' ? 'layers' : 'tree'} size={14} />Open in {editorName}
            editor</a
          >
        </div>
      </details>
      <button type="button" class="btn" onclick={ondone}>Done</button>
    </span>
  </div>
  {#if protectedBranch || error}
    <div class="pnote" role="note">{hint}</div>
  {/if}
{/if}

<style>
  .ebar {
    position: sticky;
    top: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    min-height: 44px;
    max-width: calc(1360px - 2 * var(--gutter));
    margin: var(--sp-3) var(--gutter) 0;
    padding: 6px 6px 6px 12px;
    background: color-mix(in srgb, var(--surface) 94%, transparent);
    backdrop-filter: blur(6px);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
    white-space: nowrap;
  }
  .ebar .state::before,
  .dot {
    content: '';
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--spark);
    box-shadow: 0 0 0 3px var(--spark-soft);
    flex: none;
  }
  .ebar.warn {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
  }
  .ebar.warn .state::before {
    background: var(--warn);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 18%, transparent);
  }
  .brn {
    font: 500 12px var(--font-mono);
    color: var(--text-2);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 1px 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 22ch;
  }
  .hint {
    flex: 1 1 12em;
    min-width: 0;
    color: var(--text-3);
    font-size: var(--fs-sm);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ebar.warn .hint {
    color: var(--warn);
  }
  .grp {
    margin-left: auto;
    display: flex;
    gap: 4px;
    align-items: center;
    flex-wrap: wrap;
  }
  .vsep {
    width: 1px;
    height: 20px;
    background: var(--border);
    margin: 0 4px;
  }
  .k {
    font: 500 10px var(--font-mono);
    color: var(--text-3);
    margin-left: 2px;
  }
  .pbar,
  .pnote {
    display: none;
  }
  @media (max-width: 1199px) {
    .ebar:not(.warn) .hint {
      display: none;
    }
    .ebar.warn .hint {
      order: 9;
      flex-basis: 100%;
      white-space: normal;
    }
  }
  @media (max-width: 760px) {
    .ebar {
      display: none;
    }
    .pbar {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      z-index: 30;
      display: flex;
      align-items: center;
      gap: 6px;
      min-height: 60px;
      padding: 8px 12px calc(8px + env(safe-area-inset-bottom));
      background: var(--surface);
      border-top: 1px solid var(--border);
      box-shadow: var(--shadow-pop);
    }
    .pnote {
      display: block;
      position: fixed;
      left: 0;
      right: 0;
      bottom: calc(60px + env(safe-area-inset-bottom));
      z-index: 30;
      padding: 8px 16px;
      background: var(--surface-2);
      border-top: 1px solid var(--border);
      color: var(--warn);
      font-size: var(--fs-sm);
    }
    .pbar .state {
      min-width: 0;
      font-size: var(--fs-sm);
    }
    .lines {
      display: grid;
      min-width: 0;
    }
    .pbar .brn {
      background: none;
      border: 0;
      padding: 0;
      font-size: 11px;
      max-width: 40vw;
    }
    .r {
      margin-left: auto;
      display: flex;
      gap: 4px;
      align-items: center;
    }
    .pbar .btn {
      height: 40px;
      min-width: 40px;
      justify-content: center;
    }
    .more {
      position: relative;
    }
    .more summary {
      list-style: none;
    }
    .more summary::-webkit-details-marker {
      display: none;
    }
    .menu {
      position: absolute;
      right: 0;
      bottom: calc(100% + 8px);
      min-width: 220px;
      display: grid;
      gap: 2px;
      padding: 4px;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: var(--r-lg);
      box-shadow: var(--shadow-pop);
    }
    .menu > * {
      display: flex;
      align-items: center;
      gap: 8px;
      min-height: 40px;
      padding: 0 10px;
      border: 0;
      background: none;
      border-radius: var(--r);
      color: var(--text);
      font: inherit;
      text-align: left;
      text-decoration: none;
      cursor: pointer;
    }
    .menu > *:hover {
      background: var(--hover);
    }
  }
</style>
