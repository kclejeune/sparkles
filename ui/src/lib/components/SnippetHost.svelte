<script lang="ts" generics="T">
  // Renders a snippet where a component has to be mounted by hand (a map popup).
  import type { Snippet } from 'svelte';

  let { snippet, arg }: { snippet: Snippet<[T]>; arg: T } = $props();
</script>

{@render snippet(arg)}
