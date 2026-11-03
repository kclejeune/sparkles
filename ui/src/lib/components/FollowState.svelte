<script lang="ts">
  // Whether an editor follows the change feed, as a state of the page's own actions: a
  // quiet label while it follows, and a button that follows again after the stream stopped.
  // It is not a dot, so it does not read as the top bar's server status.
  import Icon from './Icon.svelte';

  let { live, onretry }: { live: 'off' | 'on' | 'stopped'; onretry: () => void } = $props();
</script>

{#if live === 'on'}
  <span class="follow" title="Commits by other writers refresh this page as they land">
    <Icon name="cycle" size={13} /><span class="lbl">Following changes</span>
  </span>
{:else if live === 'stopped'}
  <button
    class="btn ghost"
    onclick={onretry}
    title="The change feed stopped. Follow it again to see other writers' commits"
    ><Icon name="cycle" size={13} /><span class="lbl">Follow changes</span></button
  >
{/if}

<style>
  .follow {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--ctl-h);
    padding: 0 var(--sp-2);
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
</style>
