<script lang="ts">
  import '@fontsource-variable/instrument-sans';
  import '@fontsource-variable/jetbrains-mono';
  import '../app.css';
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { watchTables } from '$lib/tables';
  import { watchTabStrips } from '$lib/tabstrip';
  import { app } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { poll } from '$lib/poll';
  import { reviewCounts } from '$lib/review-count.svelte';
  import NavLinks from '$components/NavLinks.svelte';
  import Toasts from '$components/Toasts.svelte';
  import TopBar from '$components/TopBar.svelte';
  import AskDrawer from '$components/ask/AskDrawer.svelte';

  let { children } = $props();

  // sign-in and CLI approval pages render without the shell
  const bare = $derived(/^\/ui\/(login|cli)(\/|$)/.test(page.url.pathname));

  // the open review items of the current dataset, on the Memory link
  $effect(() => {
    const ds = app.current;
    if (!ds || bare) return;
    const p = poll(() => reviewCounts.refresh(ds), { interval: 60_000 });
    return () => p.stop();
  });

  onMount(() => watchTabStrips());
  onMount(() => watchTables());

  onMount(() => {
    void auth.ensure().then(() => auth.guard());
    // While the page is visible: the connection status every 30 s, and the dataset list
    // (other clients write too) every 30 s while it changes, backing off to 5 min while
    // it does not. Pages refresh it at once after their own changes.
    const pinger = poll(() => app.ping(), { interval: 30_000 });
    const datasets = poll(() => app.refreshDatasets(), {
      interval: 30_000,
      maxIdle: 300_000,
    });
    return () => {
      pinger.stop();
      datasets.stop();
    };
  });
</script>

{#if bare}
  <main class="bare">
    {@render children()}
  </main>
{:else}
  <a class="skip" href="#main">Skip to content</a>
  <div class="shell">
    <TopBar />
    <aside class="rail">
      <NavLinks variant="rail" />
    </aside>
    <main id="main" tabindex="-1">
      {@render children()}
    </main>
    <AskDrawer />
  </div>
{/if}

<Toasts />

<style>
  /* The shell: the top bar across, the rail beside the page. The rail is --rail-w wide,
     with labels from 1200 px and icons below; on a phone (760 px) it goes, and the top
     bar's menu opens the pages in a drawer. */
  .shell {
    display: grid;
    /* the Ask drawer docks in the last column, which is empty while it is closed */
    grid-template-areas:
      'top top top'
      'rail main ask';
    grid-template-columns: var(--rail-w) minmax(0, 1fr) auto;
    grid-template-rows: var(--topbar-h) minmax(0, 1fr);
    height: 100vh;
    height: 100dvh;
  }
  .rail {
    grid-area: rail;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-3) var(--sp-2);
    background: var(--surface-2);
    border-right: 1px solid var(--border);
  }
  main {
    grid-area: main;
    min-width: 0;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
  }
  main:focus {
    outline: none;
  }
  main.bare {
    height: 100vh;
    height: 100dvh;
  }
  .skip {
    position: absolute;
    left: var(--sp-2);
    top: var(--sp-2);
    transform: translateY(-200%);
    z-index: 100;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r);
    background: var(--surface);
    box-shadow: var(--shadow-pop);
  }
  .skip:focus {
    transform: none;
  }
  @media (max-width: 760px) {
    .shell {
      grid-template-areas:
        'top'
        'main';
      grid-template-columns: minmax(0, 1fr);
    }
    .rail {
      display: none;
    }
  }
</style>
