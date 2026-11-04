<script lang="ts">
  // A page's actions at the end of its head (ROADMAP item 633). The page marks its primary
  // actions with `data-primary`. From 640px, when the actions would leave the title column
  // under 60% of the head, the others fold, last first, into a "More" menu at the end, which
  // runs each folded action by clicking it, so a page's handlers and links stay its own.
  // A folded action keeps its label, icon and disabled state in the menu. Below 640px nothing
  // folds and the actions wrap on a row of their own.
  import { onMount, tick, type Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    children,
    label = 'More actions',
    small = false,
  }: {
    children: Snippet;
    label?: string;
    /** small buttons, as a term's head has */ small?: boolean;
  } = $props();

  type Item = {
    el: HTMLElement;
    text: string;
    icon: string;
    disabled: boolean;
    href: string | null;
  };

  let box: HTMLDivElement | undefined = $state();
  let moreBtn: HTMLButtonElement | undefined = $state();
  let menu: HTMLDivElement | undefined = $state();
  let items = $state<Item[]>([]);
  let open = $state(false);

  /** The share of the head the actions may take before they fold. */
  const SHARE = 0.4;
  /** The room the More button takes, measured once it shows. */
  let moreWidth = 76;

  const foldable = (el: HTMLElement) =>
    !el.hasAttribute('data-primary') &&
    !el.classList.contains('more') &&
    el.matches('a.btn, button.btn');

  function describe(el: HTMLElement): Item {
    const text = (el.getAttribute('aria-label') ?? el.textContent ?? '')
      .replace(/\s+/g, ' ')
      .trim();
    return {
      el,
      text,
      icon: el.querySelector('svg')?.outerHTML ?? '',
      disabled: el.matches(':disabled, [aria-disabled="true"]'),
      href: el instanceof HTMLAnchorElement ? el.href : null,
    };
  }

  let busy = false;
  function fit() {
    if (!box || busy) return;
    busy = true;
    const head = box.closest<HTMLElement>('.page-head') ?? box.parentElement;
    const kids = [...box.children].filter(
      (c): c is HTMLElement => c instanceof HTMLElement && !c.classList.contains('more'),
    );
    for (const k of kids) k.removeAttribute('data-folded');
    const wide = window.matchMedia('(min-width: 640px)').matches;
    const folded: HTMLElement[] = [];
    if (wide && head) {
      const gap = parseFloat(getComputedStyle(box).columnGap) || 0;
      const widths = new Map(kids.map((k) => [k, k.getBoundingClientRect().width]));
      const room = head.clientWidth * SHARE;
      const total = (shown: HTMLElement[], more: boolean) =>
        shown.reduce((n, k) => n + (widths.get(k) ?? 0), 0) +
        gap * Math.max(0, shown.length - 1 + (more ? 1 : 0)) +
        (more ? moreWidth : 0);
      let shown = kids.filter((k) => (widths.get(k) ?? 0) > 0);
      const order = shown.filter(foldable).reverse();
      for (const k of order) {
        if (total(shown, folded.length > 0) <= room) break;
        folded.unshift(k);
        shown = shown.filter((x) => x !== k);
      }
    }
    for (const k of folded) k.setAttribute('data-folded', '');
    items = folded.map(describe);
    if (!items.length) open = false;
    busy = false;
  }

  let frame = 0;
  const schedule = () => {
    if (!frame)
      frame = requestAnimationFrame(() => {
        frame = 0;
        fit();
      });
  };

  onMount(() => {
    const head = box?.closest<HTMLElement>('.page-head') ?? box?.parentElement;
    const ro = new ResizeObserver(schedule);
    if (head) ro.observe(head);
    // the page adds and removes actions, and changes their labels and disabled states
    const mo = new MutationObserver((records) => {
      if (records.some((r) => !(r.target instanceof Element && r.target.closest('.more'))))
        schedule();
    });
    if (box)
      mo.observe(box, {
        childList: true,
        subtree: true,
        characterData: true,
        attributes: true,
        attributeFilter: ['disabled', 'aria-disabled', 'aria-label', 'href'],
      });
    schedule();
    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
      mo.disconnect();
    };
  });

  $effect(() => {
    if (moreBtn && items.length) {
      const w = moreBtn.getBoundingClientRect().width;
      if (w && Math.abs(w - moreWidth) > 1) {
        moreWidth = w;
        schedule();
      }
    }
  });

  async function toggle() {
    open = !open;
    if (open) {
      await tick();
      menuItems()[0]?.focus();
    }
  }
  const menuItems = () => [...(menu?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
  function close(focus = true) {
    open = false;
    if (focus) moreBtn?.focus();
  }
  function onMenuKey(e: KeyboardEvent) {
    const list = menuItems().filter((x) => !x.hasAttribute('aria-disabled'));
    const i = list.indexOf(document.activeElement as HTMLElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      list[(i + step + list.length) % list.length]?.focus();
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      (e.key === 'Home' ? list[0] : list.at(-1))?.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === 'Tab') {
      close(false);
    }
  }
  function run(it: Item) {
    if (it.disabled) return;
    close();
    it.el.click();
  }

  $effect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!(e.target instanceof Node) || !box?.querySelector('.more')?.contains(e.target))
        open = false;
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  });
</script>

<div class="page-actions" bind:this={box}>
  {@render children()}
  {#if items.length}
    <div class="more">
      <button
        bind:this={moreBtn}
        type="button"
        class="btn"
        class:sm={small}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        onclick={toggle}
        onkeydown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            void toggle();
          }
        }}><Icon name="more" size={14} />More</button
      >
      {#if open}
        <div
          class="menu"
          role="menu"
          aria-label={label}
          bind:this={menu}
          tabindex="-1"
          onkeydown={onMenuKey}
        >
          {#each items as it (it.el)}
            {#if it.href && !it.disabled}
              <a
                role="menuitem"
                class="item"
                href={it.href}
                tabindex="-1"
                onclick={(e) => {
                  e.preventDefault();
                  run(it);
                }}
                >{#if it.icon}<!-- eslint-disable-next-line svelte/no-at-html-tags -->{@html it.icon}{/if}{it.text}</a
              >
            {:else}
              <button
                type="button"
                role="menuitem"
                class="item"
                tabindex="-1"
                aria-disabled={it.disabled ? 'true' : undefined}
                onclick={() => run(it)}
                >{#if it.icon}<!-- eslint-disable-next-line svelte/no-at-html-tags -->{@html it.icon}{/if}{it.text}</button
              >
            {/if}
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .more {
    position: relative;
    flex: none;
  }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 40;
    min-width: 200px;
    display: grid;
    padding: var(--sp-1);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
  }
  .item {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: var(--ctl-h);
    padding: 0 var(--sp-3);
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text);
    font: inherit;
    font-size: var(--fs-md);
    text-align: left;
    text-decoration: none;
    white-space: nowrap;
    cursor: pointer;
  }
  .item:hover,
  .item:focus-visible {
    background: var(--surface-3);
  }
  .item[aria-disabled='true'] {
    color: var(--text-3);
    cursor: default;
  }
  .item :global(svg) {
    flex: none;
    color: var(--text-2);
  }
</style>
