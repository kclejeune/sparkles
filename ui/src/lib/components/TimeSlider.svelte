<script lang="ts" module>
  /** What the slider tells the view when a handle comes to rest. */
  export type TimeChange = {
    /** The `at` selector of the right handle: empty for the head. */
    at: string;
    /** The `at` selector of the left handle in compare mode, or empty. */
    from: string;
    compare: boolean;
    /** In compare mode, the branch whose head the left side is, instead of an earlier commit. */
    against: string;
  };
</script>

<script lang="ts">
  // The time slider (docs/specs/U01-time-slider.md): one control over the commits of a
  // branch. It writes the `at` selector the views already read, a second handle compares
  // two commits, and Play steps through the commits while the view renders each one.
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { changesFeedUnavailable, followCommits } from '$lib/changes-feed';
  import { mergeCommits, nextBefore } from '$lib/commits';
  import { fmtInt } from '$lib/format';
  import { LatestRun } from '$lib/supersede';
  import {
    bucketize,
    buildBucketTrack,
    buildTrack,
    caption,
    first,
    fullView,
    goneTitle,
    bucketTitle,
    indexOf,
    isCoarse,
    markGone,
    nearestReadable,
    playDelay,
    playNext,
    positions,
    refineTrack,
    rangeOfView,
    seqOfSelector,
    selectorOf,
    snapshotStep,
    step,
    stopAt,
    stopAtOrBefore,
    valueText,
    viewOfRange,
    zoomView,
    type Axis,
    type Range,
    type Track,
    type View,
  } from '$lib/timeline';
  import Icon from './Icon.svelte';

  let {
    ds,
    commits: given = undefined,
    head: givenHead = undefined,
    authors = undefined,
    fork = null,
    readable = undefined,
    memory = false,
    at = $bindable(''),
    from = $bindable(''),
    compare = $bindable(false),
    branches = [],
    against = $bindable(''),
    busy = false,
    lastReadMs = 0,
    paused = false,
    announcement = '',
    label = 'History',
    refreshKey = 0,
    canFollow = true,
    onchange,
    onclose,
    onplaying,
  }: {
    /** The dataset on its branch, as the API client takes it (`ds` or `ds@branch`). */
    ds: string;
    /** The commits to show, in place of the branch's own (an entity's history). */
    commits?: api.Commit[];
    /** The dataset's head, when `commits` is given. */
    head?: number;
    /** Who made each commit, by sequence number. */
    authors?: ReadonlyMap<number, string>;
    /** The commit the branch was forked from. */
    fork?: number | null;
    /** The ranges a read can see, when `commits` is given. */
    readable?: Range[];
    /** The dataset is in memory, where past states are kept only for snapshots and retention. */
    memory?: boolean;
    /** The `at` selector of the right handle. */
    at?: string;
    /** The `at` selector of the left handle while comparing. */
    from?: string;
    compare?: boolean;
    /** Other branches the view can compare this one with. */
    branches?: string[];
    /** The branch compared with: its head is the left side. Empty compares two commits. */
    against?: string;
    /** The view is reading a state. Play waits for it. */
    busy?: boolean;
    /** How long the last read took, so that Play never starts sooner than 1.5 times it. */
    lastReadMs?: number;
    /** Hold Play, for example while focus is in the view. */
    paused?: boolean;
    /** What the polite live region says after a read. */
    announcement?: string;
    label?: string;
    /** Bump to reload the commits. */
    refreshKey?: number;
    canFollow?: boolean;
    onchange?: (c: TimeChange) => void;
    onclose?: () => void;
    /** Told when playback starts and stops, so overlays can pause (U07 §11.5). */
    onplaying?: (on: boolean) => void;
  } = $props();

  const uid = $props.id();
  const MAX_COMMITS = 2000;
  const PAGE = 500;
  /** A range of at most this many commits is read commit by commit when zoomed to. */
  const DETAIL = 1000;
  const BUCKETS = 200;

  let track = $state<Track>(buildTrack([]));
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let truncated = $state(false);
  /** The track is made of buckets of commits (a history too long to list). */
  let inBuckets = $state(false);
  let total = $state(0);
  let timelineReadable: Range[] | undefined;
  let width = $state(640);
  let axis = $state<Axis>('sequence');
  let view = $state<View>({ from: 0, to: 0 });
  let zoomed = $state(false);
  let rightSeq = $state(0);
  let leftSeq = $state(0);
  let dragging = $state<'left' | 'right' | 'band' | null>(null);
  let playing = $state(false);
  $effect(() => onplaying?.(playing));
  let speed = $state<1 | 2 | 4>(1);
  let follow = $state(false);
  let followNote = $state<string | null>(null);
  /** The state the view last read successfully, which a refused read returns to. */
  let good = { right: 0, left: 0, compare: false, at: '', from: '' };
  const runs = new LatestRun();

  // --- loading the commits ---------------------------------------------------------

  async function load() {
    if (given) return;
    const owns = runs.claim('commits');
    loading = true;
    try {
      let page = await api.commits(ds, { limit: PAGE });
      // a history too long to list comes as buckets, which zooming refines
      if (page.head - page.firstRetained + 1 > MAX_COMMITS) {
        const tl = await api.commitTimeline(ds, { buckets: BUCKETS }).catch(() => null);
        if (!owns()) return;
        if (tl) {
          inBuckets = true;
          total = tl.commits;
          timelineReadable = tl.readable;
          truncated = false;
          zoomed = false;
          setTrack(buildBucketTrack(tl, { fork }));
          loadError = null;
          return;
        }
      }
      inBuckets = false;
      let shown = page.commits;
      let cut = false;
      while (page.next && owns()) {
        if (shown.length >= MAX_COMMITS) {
          cut = true;
          break;
        }
        const before = nextBefore(page, shown);
        if (before == null) break;
        page = await api.commits(ds, { limit: PAGE, before });
        shown = mergeCommits(shown, page.commits);
      }
      if (!owns()) return;
      truncated = cut;
      setTrack(
        buildTrack(shown, {
          head: page.head,
          fork,
          authors,
          readable: undefined,
        }),
      );
      loadError = null;
    } catch (e) {
      if (owns()) loadError = api.errorMessage(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  /** The finer stops of a range: each commit when there are few, else smaller buckets. */
  async function fineStops(r: Range) {
    if (r.to - r.from + 1 <= DETAIL) {
      let page = await api.commits(ds, { limit: PAGE, before: r.to + 1 });
      let shown = page.commits;
      while (page.next && shown.length && shown[shown.length - 1].seq > r.from) {
        const before = nextBefore(page, shown);
        if (before == null) break;
        page = await api.commits(ds, { limit: PAGE, before });
        shown = mergeCommits(shown, page.commits);
      }
      return buildTrack(
        shown.filter((c) => c.seq >= r.from),
        { head: track.head, fork, readable: timelineReadable },
      ).stops.filter((s) => s.kind !== 'head');
    }
    const tl = await api.commitTimeline(ds, { buckets: BUCKETS, from: r.from, to: r.to });
    return buildBucketTrack(tl, { fork, readable: timelineReadable }).stops.filter(
      (s) => s.kind !== 'head',
    );
  }

  /** Zoom a bucketed track: read the commits of the range that comes into view. */
  async function zoomBuckets(factor: number) {
    const centre = indexOf(track, rightSeq);
    const next = zoomView(track, view, factor, centre < 0 ? view.to : centre);
    if (factor <= 1 || !isCoarse(track, next)) {
      view = next;
      zoomed = !(view.from === 0 && view.to === track.stops.length - 1);
      return;
    }
    const owns = runs.claim('zoom');
    const range = rangeOfView(track, next);
    try {
      const fine = await fineStops(range);
      if (!owns()) return;
      track = refineTrack(track, range, fine);
      view = viewOfRange(track, range);
      zoomed = true;
    } catch (e) {
      if (owns()) loadError = api.errorMessage(e);
    }
  }

  function setTrack(t: Track) {
    track = t;
    view = zoomed ? clampView(view, t) : fullView(t);
    const r = seqOfSelector(t, at);
    rightSeq = r ?? numericSeq(at, t) ?? t.head;
    leftSeq = compare
      ? (seqOfSelector(t, from) ?? numericSeq(from, t) ?? step(t, rightSeq, -1))
      : 0;
    good = { right: rightSeq, left: leftSeq, compare, at, from };
  }

  /** A commit selector that is not a stop of this track: the stop at or before it. */
  function numericSeq(sel: string, t: Track): number | null {
    const m = /^(?:commit:)?(\d{1,20})$/.exec((sel ?? '').trim());
    return m ? (stopAtOrBefore(t, Number(m[1]))?.seq ?? null) : null;
  }

  function clampView(v: View, t: Track): View {
    const n = t.stops.length;
    if (v.to >= n || v.from >= n) return fullView(t);
    return v;
  }

  $effect(() => {
    void ds;
    void refreshKey;
    if (given) return;
    void load();
  });

  $effect(() => {
    if (!given) return;
    void authors;
    void readable;
    setTrack(buildTrack(given, { head: givenHead, fork, authors, readable }));
  });

  // the selector fields move the handles when they are typed into or set by a link
  $effect(() => {
    const sel = at;
    const f = from;
    const isCompare = compare;
    if (dragging || !track.stops.length) return;
    const r = seqOfSelector(track, sel) ?? numericSeq(sel, track);
    if (r != null && r !== rightSeq) rightSeq = r;
    if (isCompare) {
      const l = seqOfSelector(track, f) ?? numericSeq(f, track);
      if (l != null && l !== leftSeq) leftSeq = l;
    }
  });

  // --- derived geometry ---------------------------------------------------------------

  const pad = 14;
  const pos = $derived(positions(track, view, axis, width, pad));
  const buckets = $derived(bucketize(track, view, axis, width, 7, pad));
  const bucketed = $derived(buckets.length < view.to - view.from + 1);
  const maxChange = $derived(
    Math.max(1, ...buckets.map((b) => b.inserted + b.deleted)) /* bars scale to the biggest */,
  );
  const realStops = $derived(track.stops.filter((s) => s.kind !== 'head'));
  const oneCommit = $derived(realStops.length <= 1);
  const readableStops = $derived(track.stops.filter((s) => s.readable));
  const noPast = $derived(memory && readableStops.length <= 1);
  const canCompare = $derived(readableStops.length >= 2 || branches.length > 0);
  const rightStop = $derived(track.stops[indexOf(track, rightSeq)] ?? track.stops.at(-1));
  const leftStop = $derived(track.stops[indexOf(track, leftSeq)] ?? track.stops[0]);
  const firstSeq = $derived(track.stops[0]?.seq ?? 0);
  const xRight = $derived(pos.get(rightSeq) ?? pad);
  const xLeft = $derived(pos.get(leftSeq) ?? pad);
  const snapshotMarks = $derived(
    track.stops
      .filter((s) => s.snapshots.length && pos.has(s.seq))
      .map((s) => ({ x: pos.get(s.seq)!, names: s.snapshots, seq: s.seq })),
  );
  const snapshotLabels = $derived.by(() => {
    let lastX = -Infinity;
    return snapshotMarks.map((m) => {
      const show = m.x - lastX > 78;
      if (show) lastX = m.x;
      return { ...m, show };
    });
  });
  const goneBands = $derived(
    track.gone
      .map((g) => {
        const xs = track.stops
          .filter((s) => s.seq >= g.from && s.seq <= g.to && pos.has(s.seq))
          .map((s) => pos.get(s.seq)!);
        if (!xs.length) return null;
        const next = track.stops.find((s) => s.seq > g.to && pos.has(s.seq));
        const x0 = Math.max(0, Math.min(...xs) - (xs.length === 1 ? 5 : 3));
        const x1 = next ? (Math.max(...xs) + pos.get(next.seq)!) / 2 : Math.max(...xs) + 5;
        return { range: g, x0, x1: Math.max(x1, x0 + 6) };
      })
      .filter((g): g is { range: Range; x0: number; x1: number } => g !== null),
  );
  const forkX = $derived.by(() => {
    const f = track.stops.find((s) => s.fork);
    return f ? pos.get(f.seq) : undefined;
  });
  const perStep = $derived(
    bucketed ? Math.max(1, Math.round((view.to - view.from + 1) / buckets.length)) : 1,
  );

  // --- changing the state ------------------------------------------------------------------

  let timer: ReturnType<typeof setTimeout> | undefined;

  const fromSelector = (seq: number) => (seq === track.head ? 'head' : `commit:${seq}`);

  /** Write the selectors and tell the view. */
  function emit() {
    clearTimeout(timer);
    const nextAt = selectorOf(track, rightSeq);
    const nextFrom = compare && !against ? fromSelector(leftSeq) : '';
    at = nextAt;
    from = nextFrom;
    onchange?.({ at: nextAt, from: nextFrom, compare, against: compare ? against : '' });
  }

  /** Emit after the handle has rested for 250 ms (a drag). */
  function emitSoon() {
    clearTimeout(timer);
    timer = setTimeout(emit, 250);
  }

  /** Move a handle to a commit. */
  function moveTo(which: 'left' | 'right', seq: number, now: boolean) {
    if (which === 'right') {
      if (compare && seq <= leftSeq) seq = step(track, leftSeq, 1);
      if (seq === rightSeq && !now) return;
      rightSeq = seq;
    } else {
      if (seq >= rightSeq) seq = step(track, rightSeq, -1);
      if (seq === leftSeq && !now) return;
      leftSeq = seq;
    }
    ensureVisible(seq);
    if (now) emit();
    else emitSoon();
  }

  function ensureVisible(seq: number) {
    const i = indexOf(track, seq);
    if (i < 0 || (i >= view.from && i <= view.to)) return;
    const w = view.to - view.from;
    const from0 = Math.min(Math.max(0, i < view.from ? i : i - w), track.stops.length - 1 - w);
    view = { from: from0, to: from0 + w };
  }

  function setCompare(on: boolean) {
    if (on && !canCompare) return;
    compare = on;
    if (!on) against = '';
    if (on)
      leftSeq = step(track, rightSeq, -1) === rightSeq ? first(track) : step(track, rightSeq, -1);
    if (on && leftSeq >= rightSeq) {
      // sitting at the oldest readable commit: compare with the head instead
      leftSeq = rightSeq;
      rightSeq = track.head;
    }
    emit();
  }

  function setPlaying(on: boolean) {
    if (on && oneCommit) return;
    if (on && rightSeq === track.head) {
      // play from the start when there is nothing after the handle
      rightSeq = first(track);
      if (compare) leftSeq = rightSeq;
      emit();
    }
    playing = on;
  }

  /** The view refused a read: the handle returns to the last state that was read. */
  export function reject(info?: { seq?: number; readable?: Range[] }) {
    if (info?.seq != null) track = markGone(track, info.seq, info.readable);
    const open = (seq: number) => track.stops[indexOf(track, seq)]?.readable !== false;
    // a state that was never read (a link to a commit that is gone) gives way to the head
    const backRight = open(good.right) ? good.right : track.head;
    const backLeft = open(good.left) ? good.left : step(track, backRight, -1);
    rightSeq = backRight;
    leftSeq = backLeft;
    compare = good.compare && canCompare;
    at = selectorOf(track, backRight);
    from = compare ? fromSelector(backLeft) : '';
    playing = false;
    clearTimeout(timer);
  }

  /**
   * A read failed for a reason other than a state that cannot be read: Play stops and the
   * handle stays where it is, so that retrying reads the same state.
   */
  export function fail() {
    playing = false;
  }

  /** The view read the state the slider names. */
  export function accept() {
    good = { right: rightSeq, left: leftSeq, compare, at, from };
  }

  /** Read the commits again. */
  export function reload() {
    return load();
  }

  // --- Play ---------------------------------------------------------------------------------

  $effect(() => {
    if (!playing) return;
    // these are what makes the effect run again once the view has rendered a step
    void rightSeq;
    if (busy || paused) return;
    const delay = playDelay(speed, lastReadMs);
    const t = setTimeout(() => {
      if (busy || paused) return;
      const next = playNext(track, rightSeq, perStep);
      if (next == null) {
        playing = false;
        return;
      }
      moveTo('right', next, true);
    }, delay);
    return () => clearTimeout(t);
  });

  // --- Follow ---------------------------------------------------------------------------

  $effect(() => {
    if (!follow || rightSeq !== track.head || given) return;
    const ctl = new AbortController();
    followNote = null;
    // resume from the last commit seen, so that none made between a read and the connection,
    // or during the pause between two connections, is skipped
    let after = track.head;
    (async () => {
      while (!ctl.signal.aborted) {
        try {
          after = await followCommits(
            ds,
            after,
            (c) => {
              after = Math.max(after, c.seq);
              void load().then(() => {
                if (!ctl.signal.aborted && rightSeq !== track.head) rightSeq = track.head;
                emit();
              });
            },
            ctl.signal,
          );
        } catch (e) {
          if (ctl.signal.aborted) return;
          followNote = changesFeedUnavailable(e);
          follow = false;
          return;
        }
        // the server ends a stream after five minutes
        await new Promise((r) => setTimeout(r, 500));
      }
    })();
    return () => ctl.abort();
  });

  // --- pointer ------------------------------------------------------------------------

  let trackEl: HTMLDivElement | undefined = $state();
  let drag: {
    which: 'left' | 'right' | 'band';
    start: number;
    left0: number;
    right0: number;
  } | null = null;

  function xOf(e: PointerEvent): number {
    const r = trackEl!.getBoundingClientRect();
    return e.clientX - r.left;
  }

  function onpointerdown(e: PointerEvent) {
    if (e.button !== 0 || !trackEl) return;
    const x = xOf(e);
    const hit = (e.target as HTMLElement).closest('[data-handle]')?.getAttribute('data-handle');
    let which: 'left' | 'right' | 'band' = 'right';
    if (compare && !against) {
      if (hit === 'left' || hit === 'right' || hit === 'band') which = hit;
      else which = Math.abs(x - xLeft) < Math.abs(x - xRight) ? 'left' : 'right';
    }
    drag = {
      which,
      start: stopAt(track, view, axis, width, x, true, pad),
      left0: leftSeq,
      right0: rightSeq,
    };
    dragging = which;
    trackEl.setPointerCapture(e.pointerId);
    if (which !== 'band') {
      moveTo(which, drag.start, false);
      (trackEl.querySelector(`[data-handle="${which}"]`) as HTMLElement | null)?.focus();
    }
    e.preventDefault();
  }

  function onpointermove(e: PointerEvent) {
    if (!drag || !trackEl) return;
    const seq = stopAt(track, view, axis, width, xOf(e), true, pad);
    if (drag.which === 'band') {
      const open = readableStops.map((s) => s.seq);
      const idx = (s: number) => open.indexOf(s);
      let delta = idx(seq) - idx(drag.start);
      const lo = idx(drag.left0);
      const hi = idx(drag.right0);
      if (lo < 0 || hi < 0 || idx(seq) < 0) return;
      delta = Math.max(-lo, Math.min(open.length - 1 - hi, delta));
      const l = open[lo + delta];
      const r = open[hi + delta];
      if (l === leftSeq && r === rightSeq) return;
      leftSeq = l;
      rightSeq = r;
      emitSoon();
    } else {
      moveTo(drag.which, seq, false);
    }
  }

  function onpointerup(e: PointerEvent) {
    if (!drag) return;
    drag = null;
    dragging = null;
    trackEl?.releasePointerCapture?.(e.pointerId);
    emit();
  }

  function onwheel(e: WheelEvent) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    zoom(e.deltaY < 0 ? 1.5 : 1 / 1.5);
  }

  function zoom(factor: number) {
    if (inBuckets) return void zoomBuckets(factor);
    const centre = indexOf(track, rightSeq);
    view = zoomView(track, view, factor, centre < 0 ? view.to : centre);
    zoomed = !(view.from === 0 && view.to === track.stops.length - 1);
  }

  // --- keyboard -----------------------------------------------------------------------------

  /** The stop a PageUp or PageDown lands on when the track is bucketed: the end of the next bucket. */
  function bucketJump(seq: number, dir: 1 | -1): number {
    const i = buckets.findIndex((b) => seq >= b.first && seq <= b.last);
    const j = Math.min(buckets.length - 1, Math.max(0, i + dir));
    return nearestReadable(track, buckets[j]?.last ?? seq);
  }

  function onkeydown(e: KeyboardEvent, which: 'left' | 'right') {
    if (e.altKey || e.metaKey) return;
    const cur = which === 'right' ? rightSeq : leftSeq;
    const go = (seq: number) => {
      e.preventDefault();
      moveTo(which, seq, true);
    };
    switch (e.key) {
      case 'ArrowLeft':
      case 'ArrowDown':
        return go(step(track, cur, -1));
      case 'ArrowRight':
      case 'ArrowUp':
        return go(step(track, cur, 1));
      case 'PageUp':
        return go(bucketed ? bucketJump(cur, -1) : step(track, cur, -1, 10));
      case 'PageDown':
        return go(bucketed ? bucketJump(cur, 1) : step(track, cur, 1, 10));
      case 'Home':
        return go(first(track));
      case 'End':
        return go(track.head);
      case 'S':
      case 's':
        if (e.ctrlKey) return;
        return go(snapshotStep(track, cur, e.shiftKey ? -1 : 1));
      case ' ':
        e.preventDefault();
        return setPlaying(!playing);
      case 'c':
      case 'C':
        if (e.ctrlKey) return;
        e.preventDefault();
        return setCompare(!compare);
      case '+':
      case '=':
        e.preventDefault();
        return zoom(1.5);
      case '-':
      case '_':
        e.preventDefault();
        return zoom(1 / 1.5);
    }
  }

  function onrootkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    }
  }

  function close() {
    playing = false;
    clearTimeout(timer);
    onclose?.();
  }

  const bar = (b: { inserted: number; deleted: number }) =>
    4 + 14 * Math.sqrt((b.inserted + b.deleted) / maxChange);

  const rangeText = $derived(
    compare && against
      ? `from the head of ${against} to commit ${fmtInt(rightSeq)}`
      : compare
        ? `from commit ${fmtInt(leftSeq)} to commit ${fmtInt(rightSeq)}`
        : `commit ${fmtInt(rightSeq)}`,
  );
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="slider" aria-label="{label} time slider" onkeydown={onrootkeydown}>
  <div class="bar">
    <div class="group" role="group" aria-label="Move through commits">
      <button
        class="btn ghost icon sm"
        title="Oldest readable commit (Home)"
        aria-label="Oldest readable commit"
        disabled={oneCommit}
        onclick={() => moveTo(compare ? 'left' : 'right', first(track), true)}
        ><Icon name="skipBack" size={13} /></button
      >
      <button
        class="btn ghost icon sm"
        title="Previous commit (←)"
        aria-label="Previous commit"
        disabled={oneCommit}
        onclick={() => moveTo('right', step(track, rightSeq, -1), true)}
        ><Icon name="stepBack" size={14} /></button
      >
      <button
        class="btn ghost icon sm"
        title="Next commit (→)"
        aria-label="Next commit"
        disabled={oneCommit}
        onclick={() => moveTo('right', step(track, rightSeq, 1), true)}
        ><Icon name="stepForward" size={14} /></button
      >
      <button
        class="btn ghost icon sm"
        title="Head (End)"
        aria-label="Head"
        disabled={oneCommit}
        onclick={() => moveTo('right', track.head, true)}
        ><Icon name="skipForward" size={13} /></button
      >
    </div>
    <button
      class="btn sm"
      aria-pressed={playing}
      title={playing ? 'Pause (Space)' : 'Play through the commits (Space)'}
      disabled={oneCommit || noPast}
      onclick={() => setPlaying(!playing)}
    >
      <Icon name={playing ? 'pause' : 'play'} size={12} />
      {playing ? 'Pause' : 'Play'}
    </button>
    <select
      class="select sm"
      aria-label="Play speed"
      title="Play speed"
      value={String(speed)}
      onchange={(e) => (speed = Number(e.currentTarget.value) as 1 | 2 | 4)}
    >
      <option value="1">1×</option>
      <option value="2">2×</option>
      <option value="4">4×</option>
    </select>
    <button
      class="btn sm"
      aria-pressed={compare}
      title="Compare two commits (C)"
      disabled={!canCompare}
      onclick={() => setCompare(!compare)}
    >
      <Icon name="layers" size={13} /> Compare
    </button>
    {#if compare && branches.length}
      <select
        class="select sm"
        aria-label="Compare with"
        title="Compare with an earlier commit of this branch, or with the head of another branch"
        bind:value={against}
        onchange={emit}
      >
        <option value="">an earlier commit</option>
        {#each branches as b (b)}<option value={b}>head of {b}</option>{/each}
      </select>
    {/if}
    <span class="faint small branch mono" title="The branch the view reads">{ds}</span>
    <span class="spacer"></span>
    <select
      class="select sm"
      aria-label="Place commits by"
      title="Place commits by sequence number or by time"
      bind:value={axis}
    >
      <option value="sequence">By sequence</option>
      <option value="time">By time</option>
    </select>
    <button
      class="btn ghost icon sm"
      title="Zoom out (−)"
      aria-label="Zoom out"
      disabled={!zoomed}
      onclick={() => zoom(1 / 1.5)}><Icon name="zoomOut" size={13} /></button
    >
    <button
      class="btn ghost icon sm"
      title="Zoom in (+)"
      aria-label="Zoom in"
      disabled={track.stops.length <= 8}
      onclick={() => zoom(1.5)}><Icon name="zoomIn" size={13} /></button
    >
    {#if canFollow && !given}
      <label
        class="check small"
        title="Re-read the view when a commit is made, while it rests on the head"
      >
        <input type="checkbox" bind:checked={follow} /> Follow
      </label>
    {/if}
    <button
      class="btn ghost icon sm"
      title="Read the commits again"
      aria-label="Refresh"
      disabled={loading}
      onclick={() => (given ? emit() : load().then(emit))}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
    <button
      class="btn ghost icon sm"
      title="Close and read the head (Esc)"
      aria-label="Close the time slider"
      onclick={close}><Icon name="x" size={13} /></button
    >
  </div>

  {#if loadError}
    <div class="note error-text">Could not read the commits. {loadError}</div>
  {:else if noPast}
    <div class="note">
      Past states of this in-memory dataset are not kept. Set a retention window or pin snapshots in
      the
      <a href={resolve('/datasets/[name]', { name: ds.split('@')[0] })}>History panel</a>.
    </div>
  {:else}
    <div
      class="track"
      role="presentation"
      class:busy
      bind:this={trackEl}
      bind:clientWidth={width}
      {onpointerdown}
      {onpointermove}
      {onpointerup}
      onpointercancel={onpointerup}
      {onwheel}
    >
      <svg {width} height="58" viewBox="0 0 {width} 58" aria-hidden="true">
        <defs>
          <pattern
            id="hatch-{uid}"
            width="6"
            height="6"
            patternUnits="userSpaceOnUse"
            patternTransform="rotate(45)"
          >
            <line x1="0" y1="0" x2="0" y2="6" class="hatch" />
          </pattern>
        </defs>
        <line x1={pad - 6} x2={width - pad + 6} y1="40" y2="40" class="axis" />
        {#each goneBands as g (g.range.from)}
          <g>
            <title>{goneTitle(track, g.range)}</title>
            <rect x={g.x0} y="22" width={g.x1 - g.x0} height="22" fill="url(#hatch-{uid})" />
            {#if g.x1 - g.x0 > 34}
              <text x={(g.x0 + g.x1) / 2} y="56" class="gone-label" text-anchor="middle">gone</text>
            {/if}
          </g>
        {/each}
        {#if compare && !against}
          <rect
            x={Math.min(xLeft, xRight)}
            y="24"
            width={Math.abs(xRight - xLeft)}
            height="18"
            class="range"
          />
        {/if}
        {#if forkX != null}
          <g>
            <title>The commit this branch was forked from</title>
            <path d="M {forkX} 46 l -4 7 h 8 z" class="fork" />
          </g>
        {/if}
        {#each buckets as b (b.first)}
          <g>
            <title>{bucketTitle(b)}</title>
            {#if bucketed}
              <rect
                x={b.x - 2.5}
                y={40 - bar(b)}
                width="5"
                height={bar(b)}
                class="tick"
                class:unreadable={!b.readable}
                rx="1"
              />
            {:else}
              <line
                x1={b.x}
                x2={b.x}
                y1={40 - (b.inserted || b.deleted ? bar(b) / 1.6 : 6)}
                y2="40"
                class="tick-line"
                class:unreadable={!b.readable}
              />
            {/if}
            {#if b.merges}
              <circle cx={b.x} cy="40" r="3.6" class="merge" />
            {/if}
          </g>
        {/each}
        {#each snapshotLabels as m (m.seq)}
          <g>
            <title>{m.names.map((n) => `snapshot:${n}`).join(', ')}</title>
            <path d="M {m.x} 6 l 4.5 4.5 l -4.5 4.5 l -4.5 -4.5 z" class="diamond" />
            <line x1={m.x} x2={m.x} y1="15" y2="40" class="snap-line" />
            {#if m.show}
              <text x={m.x + 8} y="14" class="snap-label">{m.names[0]}</text>
            {/if}
          </g>
        {/each}
        <text x={width - pad + 6} y="56" class="head-label" text-anchor="end">head</text>
      </svg>
      {#if compare && !against}
        <div
          class="band"
          data-handle="band"
          style:left="{Math.min(xLeft, xRight)}px"
          style:width="{Math.abs(xRight - xLeft)}px"
          title="Drag to move both handles"
        ></div>
        <div
          class="handle left"
          class:dragging={dragging === 'left'}
          data-handle="left"
          role="slider"
          tabindex="0"
          aria-orientation="horizontal"
          aria-label="Compared from"
          aria-valuemin={firstSeq}
          aria-valuemax={track.head}
          aria-valuenow={leftSeq}
          aria-valuetext={valueText(leftStop)}
          style:left="{xLeft - 7}px"
          onkeydown={(e) => onkeydown(e, 'left')}
        ></div>
      {/if}
      <div
        class="handle right"
        class:dragging={dragging === 'right'}
        data-handle="right"
        role="slider"
        tabindex="0"
        aria-orientation="horizontal"
        aria-label={compare ? 'Compared to' : 'Commit'}
        aria-valuemin={firstSeq}
        aria-valuemax={track.head}
        aria-valuenow={rightSeq}
        aria-valuetext={valueText(rightStop)}
        style:left="{xRight - 7}px"
        onkeydown={(e) => onkeydown(e, 'right')}
      ></div>
    </div>
    <div class="caption">
      {#if oneCommit}
        <span class="faint">No earlier commits</span>
      {:else if compare && against}
        <span class="mono"><span class="from-tag">from</span> head of {against}</span>
        <span class="mono"><span class="to-tag">to</span> {caption(rightStop)}</span>
      {:else if compare}
        <span class="mono"><span class="from-tag">from</span> {caption(leftStop)}</span>
        <span class="mono"><span class="to-tag">to</span> {caption(rightStop)}</span>
      {:else}
        <span class="mono">{caption(rightStop)}</span>
      {/if}
      <span class="spacer"></span>
      <span class="faint small"
        >{rangeText}{rightStop?.timestamp
          ? ` · ${rightStop.timestamp.slice(0, 16).replace('T', ' ')}`
          : ''}</span
      >
      {#if inBuckets}
        <span class="faint small" title="Zoom in to see each commit"
          >{fmtInt(total)} commits in buckets</span
        >
      {/if}
      {#if truncated}
        <span class="faint small" title="Only the newest {MAX_COMMITS} commits are on the track"
          >newest {fmtInt(MAX_COMMITS)} commits</span
        >
      {/if}
      {#if followNote}<span class="faint small">{followNote}</span>{/if}
    </div>
  {/if}
  <div class="sr-only" role="status" aria-live="polite">{announcement}</div>
</section>

<style>
  .slider {
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    padding: 6px 12px 6px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 4px;
    min-width: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
  }
  .group {
    display: inline-flex;
    gap: 0;
  }
  .branch {
    margin-left: 4px;
  }
  .check {
    display: inline-flex;
    gap: 4px;
    align-items: center;
    cursor: pointer;
  }
  .note {
    padding: 8px 2px;
    color: var(--text-2);
  }
  .error-text {
    color: var(--danger);
  }
  .track {
    position: relative;
    height: 58px;
    touch-action: none;
    user-select: none;
    cursor: pointer;
  }
  .track.busy {
    cursor: progress;
  }
  svg {
    display: block;
    overflow: visible;
  }
  .axis {
    stroke: var(--border-strong);
    stroke-width: 1.5;
    stroke-linecap: round;
  }
  .hatch {
    stroke: var(--text-3);
    stroke-width: 1.4;
    opacity: 0.55;
  }
  .gone-label {
    fill: var(--text-3);
    font-size: 10px;
    font-style: italic;
  }
  .tick-line {
    stroke: var(--text-3);
    stroke-width: 1.5;
  }
  .tick {
    fill: var(--text-3);
    opacity: 0.8;
  }
  .unreadable {
    opacity: 0.3;
  }
  .merge {
    fill: var(--surface);
    stroke: var(--iri);
    stroke-width: 1.6;
  }
  .range {
    fill: var(--spark-soft);
  }
  .diamond {
    fill: var(--spark);
    stroke: var(--spark-ink);
    stroke-width: 1;
  }
  .snap-line {
    stroke: var(--spark);
    stroke-width: 1;
    stroke-dasharray: 2 2;
    opacity: 0.7;
  }
  .snap-label {
    fill: var(--text-2);
    font-size: 10.5px;
    font-family: var(--font-mono);
  }
  .fork {
    fill: var(--iri);
  }
  .head-label {
    fill: var(--text-3);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .band {
    position: absolute;
    top: 22px;
    height: 22px;
    cursor: grab;
  }
  .handle {
    position: absolute;
    top: 22px;
    width: 14px;
    height: 22px;
    border-radius: 5px;
    background: var(--spark);
    border: 2px solid var(--surface);
    box-shadow:
      0 0 0 1px var(--spark-ink),
      0 1px 3px rgba(0, 0, 0, 0.25);
    cursor: grab;
  }
  .handle.left {
    background: var(--iri);
    box-shadow:
      0 0 0 1px var(--iri),
      0 1px 3px rgba(0, 0, 0, 0.25);
  }
  .handle.dragging {
    cursor: grabbing;
  }
  .handle:focus-visible {
    outline: none;
    box-shadow: var(--focus);
  }
  .caption {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 14px;
    min-height: 18px;
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .caption .mono {
    overflow-wrap: anywhere;
  }
  .from-tag,
  .to-tag {
    font-family: var(--font-ui);
    font-weight: 600;
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .from-tag {
    color: var(--iri);
  }
  .to-tag {
    color: var(--spark-ink);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
    border: 0;
  }
  @media (prefers-reduced-motion: no-preference) {
    .handle {
      transition: box-shadow 0.12s;
    }
  }
</style>
