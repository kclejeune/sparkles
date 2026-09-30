<script lang="ts" module>
  // Minimal stroke icon set (24×24, 1.75 stroke), drawn for this UI.
  const PATHS: Record<string, string> = {
    query: 'M4 6h16M4 12h10M4 18h7M17 15l3 3-3 3',
    explore:
      'M6 6m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0M18 8m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0M10 18m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0 -5 0M8.3 7l7.2 1M7 8.3l2 7.4M16.4 10l-4.6 6',
    database:
      'M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3',
    server: 'M4 4h16v6H4zM4 14h16v6H4zM8 7h.01M8 17h.01',
    play: 'M7 4.5v15l12-7.5z',
    plus: 'M12 5v14M5 12h14',
    x: 'M6 6l12 12M18 6L6 18',
    trash: 'M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 11v6M14 11v6',
    upload: 'M12 16V4M7 9l5-5 5 5M4 16v4h16v-4',
    download: 'M12 4v12M7 11l5 5 5-5M4 20h16',
    copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
    refresh: 'M20 11a8 8 0 1 0-2.3 5.7M20 5v6h-6',
    cycle: 'M5 11a7 7 0 0 1 12.5-4.3M19 13a7 7 0 0 1-12.5 4.3M18 3v4h-4M6 21v-4h4',
    sun: 'M12 12m-4 0a4 4 0 1 0 8 0a4 4 0 1 0 -8 0M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4',
    moon: 'M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z',
    monitor: 'M3 4h18v12H3zM8 20h8M12 16v4',
    search: 'M11 11m-7 0a7 7 0 1 0 14 0a7 7 0 1 0 -14 0M20 20l-4-4',
    chevron: 'M9 6l6 6-6 6',
    chevronDown: 'M6 9l6 6 6-6',
    table: 'M3 5h18v14H3zM3 10h18M3 15h18M9 5v14',
    graph:
      'M5 12m-2 0a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M19 6m-2 0a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M19 18m-2 0a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M7 11l10-4M7 13l10 4',
    plan: 'M4 5h8M8 10h8M8 15h6M12 20h8M6 5v10h2M10 10v10h2',
    code: 'M8 7l-5 5 5 5M16 7l5 5-5 5',
    zap: 'M13 3L5 14h6l-1 7 8-11h-6z',
    layers: 'M12 3l9 5-9 5-9-5zM3 13l9 5 9-5',
    archive: 'M3 4h18v5H3zM5 9v11h14V9M10 13h4',
    brain:
      'M12 4v16M8 4a3 3 0 0 0-3 3 3 3 0 0 0-1 5 3 3 0 0 0 2 5 3 3 0 0 0 5 1M16 4a3 3 0 0 1 3 3 3 3 0 0 1 1 5 3 3 0 0 1-2 5 3 3 0 0 1-5 1',
    external: 'M14 4h6v6M20 4l-9 9M18 14v6H4V6h6',
    info: 'M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0 -18 0M12 11v5M12 8h.01',
    expand: 'M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5',
    target: 'M12 12m-8 0a8 8 0 1 0 16 0a8 8 0 1 0 -16 0M12 12m-3 0a3 3 0 1 0 6 0a3 3 0 1 0 -6 0',
    tree: 'M5 4v16M5 8h6M5 14h6M11 6h8v4h-8zM11 12h8v4h-8z',
    check: 'M5 12l5 5 9-10',
    alert: 'M12 4l9 16H3zM12 10v4M12 17h.01',
    clock: 'M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0 -18 0M12 7v5l3 2',
    sparkle: 'M12 3l2 6.5L20.5 12 14 14l-2 7-2-7-6.5-2L10 9.5z',
    filter: 'M4 5h16l-6 7v6l-4 2v-8z',
    fit: 'M4 4h6M4 4v6M20 4h-6M20 4v6M4 20h6M4 20v-6M20 20h-6M20 20v-6M9 12h6',
    wand: 'M4 20L15 9M14 4v2M18 8h2M17 5l1.5-1.5M10 5l.5 1M19 12l1 .5',
  };
</script>

<script lang="ts">
  let {
    name,
    size = 16,
    class: cls = '',
  }: { name: string; size?: number; class?: string } = $props();
</script>

<svg
  class="icon {cls}"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill={name === 'play' ? 'currentColor' : 'none'}
  stroke="currentColor"
  stroke-width="1.75"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={PATHS[name] ?? PATHS.info} />
</svg>

<style>
  .icon {
    flex: none;
    display: block;
  }
</style>
