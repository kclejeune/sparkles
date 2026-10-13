// The model behind the time slider (docs/specs/U01-time-slider.md): the track of commits a
// slider moves over, the mapping between a pixel and a commit, buckets for long histories,
// named-snapshot and merge marks, and the steps of Play. Nothing here touches the DOM or
// the network, so Vitest covers it.
import type { Commit } from './api';
import { commitKindLabel, fmtDelta } from './commits';
import { fmtBytes, fmtInt } from './format';
import { diffSummary, readable } from './history';

/** A readable or unreadable stretch of commits, both ends included. */
export type Range = { from: number; to: number };

/** One stop of the slider: a commit, or the head when no commit of the track is the head. */
export type Stop = {
  seq: number;
  /** Milliseconds since the epoch. */
  time: number;
  timestamp: string;
  kind: string;
  message?: string;
  /** Who made the commit, when the view knows (the change log has authors, the catalog does not). */
  author?: string;
  inserted?: number;
  deleted?: number;
  /** A point-in-time read can still see this state. */
  readable: boolean;
  snapshots: string[];
  merge: boolean;
  /** The commit the branch was forked from. */
  fork: boolean;
  /** The live state, which the view follows when the slider rests on it. */
  head: boolean;
};

export type Track = {
  /** Ascending by sequence number. */
  stops: Stop[];
  /** The dataset's head (or the branch's), which may not be a stop of a filtered track. */
  head: number;
  /** The oldest readable stop, or null when none is. */
  oldestReadable: number | null;
  /** Runs of unreadable stops. */
  gone: Range[];
};

export type TrackOptions = {
  /** The head of the dataset or branch. Defaults to the newest commit given. */
  head?: number;
  /** The commit the branch was forked from. */
  fork?: number | null;
  /** Who made each commit, by sequence number. */
  authors?: ReadonlyMap<number, string>;
  /** The ranges a point-in-time read can see, from `/$/commits` or `history-gone`. */
  readable?: readonly Range[];
};

const inRanges = (seq: number, ranges: readonly Range[]) =>
  ranges.some((r) => seq >= r.from && seq <= r.to);

/** Build the track from commits in any order. A head stop is added when none is the head. */
export function buildTrack(commits: readonly Commit[], opts: TrackOptions = {}): Track {
  const sorted = [...commits].sort((a, b) => a.seq - b.seq);
  const head = opts.head ?? sorted.at(-1)?.seq ?? 0;
  const stops: Stop[] = sorted.map((c) => {
    const time = Date.parse(c.timestamp);
    return {
      seq: c.seq,
      time: Number.isNaN(time) ? 0 : time,
      timestamp: c.timestamp,
      kind: c.kind,
      message: c.message,
      author: opts.authors?.get(c.seq),
      inserted: c.inserted,
      deleted: c.deleted,
      readable: opts.readable ? inRanges(c.seq, opts.readable) : readable(c),
      snapshots: c.snapshots ?? [],
      merge: c.kind === 'merge' || !!c.mergedFrom,
      fork: opts.fork != null && c.seq === opts.fork,
      head: c.seq === head,
    };
  });
  if (!stops.some((s) => s.head)) {
    // the head is the live state: always readable
    const last = stops.at(-1);
    stops.push({
      seq: head,
      time: last ? last.time : 0,
      timestamp: last ? last.timestamp : '',
      kind: 'head',
      readable: true,
      snapshots: [],
      merge: false,
      fork: false,
      head: true,
    });
  }
  return { stops, head, oldestReadable: oldestReadable(stops), gone: goneRanges(stops) };
}

function oldestReadable(stops: readonly Stop[]): number | null {
  return stops.find((s) => s.readable)?.seq ?? null;
}

/** Runs of unreadable stops, as sequence ranges. */
export function goneRanges(stops: readonly Stop[]): Range[] {
  const out: Range[] = [];
  let open: Range | null = null;
  for (const s of stops) {
    if (s.readable) {
      open = null;
    } else if (open) {
      open.to = s.seq;
    } else {
      open = { from: s.seq, to: s.seq };
      out.push(open);
    }
  }
  return out;
}

/**
 * The track after a read answered `history-gone`: the stops outside `ranges` become
 * unreadable (`ranges` are the readable ranges of the error body). A body without ranges
 * only rules out the stop that failed.
 */
export function markGone(track: Track, seq: number, ranges?: readonly Range[]): Track {
  const stops = track.stops.map((s) => ({
    ...s,
    readable: ranges
      ? s.readable && (s.head || inRanges(s.seq, ranges))
      : s.readable && s.seq !== seq,
  }));
  return { ...track, stops, oldestReadable: oldestReadable(stops), gone: goneRanges(stops) };
}

/** Index of the stop with this sequence number, or -1. */
export function indexOf(track: Track, seq: number): number {
  return track.stops.findIndex((s) => s.seq === seq);
}

/** The stop at `seq`, or the nearest one at or before it. */
export function stopAtOrBefore(track: Track, seq: number): Stop | undefined {
  let best: Stop | undefined;
  for (const s of track.stops) {
    if (s.seq <= seq) best = s;
    else break;
  }
  return best ?? track.stops[0];
}

// --- selectors ---------------------------------------------------------------------------

/** The `at` selector for a stop: empty for the head, otherwise `commit:N`. */
export function selectorOf(track: Track, seq: number): string {
  return seq === track.head ? '' : `commit:${seq}`;
}

/**
 * The stop an `at` selector names: the head for an empty selector or `head`, `commit:N`
 * as is, `snapshot:NAME` through the stops' snapshot names and `time:T` as the last stop at
 * or before the instant. Null when the selector names nothing on the track.
 */
export function seqOfSelector(track: Track, selector: string | null | undefined): number | null {
  const s = (selector ?? '').trim();
  if (!s || s === 'head') return track.head;
  const commit = /^(?:commit:)?(\d{1,20})$/.exec(s);
  if (commit) {
    const n = Number(commit[1]);
    return track.stops.some((x) => x.seq === n) ? n : null;
  }
  const snap = /^snapshot:(.+)$/.exec(s);
  if (snap) return track.stops.find((x) => x.snapshots.includes(snap[1]))?.seq ?? null;
  const time = /^time:(.+)$/.exec(s);
  if (time) {
    const t = Date.parse(time[1]);
    if (Number.isNaN(t)) return null;
    let found: number | null = null;
    for (const x of track.stops) if (x.time <= t) found = x.seq;
    return found;
  }
  return null;
}

// --- stepping ------------------------------------------------------------------------------

export type Direction = 1 | -1;

/**
 * The stop `count` stops away from `seq` in `dir`, skipping stops a read cannot see, or
 * `seq` itself at the end of the track.
 */
export function step(track: Track, seq: number, dir: Direction, count = 1): number {
  const open = track.stops.filter((s) => s.readable);
  if (!open.length) return seq;
  // the last readable stop at or before `seq`
  let i = -1;
  for (let k = 0; k < open.length; k++) if (open[k].seq <= seq) i = k;
  const exact = i >= 0 && open[i].seq === seq;
  const target = dir === 1 ? i + count : (exact ? i : i + 1) - count;
  return open[Math.min(open.length - 1, Math.max(0, target))].seq;
}

/** The readable stop nearest to `seq` by position on the track, or `seq` when none is. */
export function nearestReadable(track: Track, seq: number): number {
  const i = indexOf(track, seq);
  if (i < 0) return seq;
  for (let d = 0; d < track.stops.length; d++) {
    if (track.stops[i - d]?.readable) return track.stops[i - d].seq;
    if (track.stops[i + d]?.readable) return track.stops[i + d].seq;
  }
  return seq;
}

/** The oldest readable stop. */
export function first(track: Track): number {
  return track.oldestReadable ?? track.head;
}

/** The next (dir 1) or previous (dir -1) stop that carries a named snapshot, or `seq`. */
export function snapshotStep(track: Track, seq: number, dir: Direction): number {
  const marked = track.stops.filter((s) => s.snapshots.length && s.readable);
  const next =
    dir === 1 ? marked.find((s) => s.seq > seq) : [...marked].reverse().find((s) => s.seq < seq);
  return next?.seq ?? seq;
}

/**
 * The next stop of Play, or null when it should stop: at the head, or in front of a stretch
 * that cannot be read. `count` is the number of stops one step covers.
 */
export function playNext(track: Track, seq: number, count = 1): number | null {
  const i = indexOf(track, seq);
  if (i < 0) return null;
  const target = Math.min(track.stops.length - 1, i + Math.max(1, count));
  if (target === i) return null;
  for (let k = i + 1; k <= target; k++) if (!track.stops[k].readable) return null;
  return track.stops[target].seq;
}

/**
 * The wait before Play's next step: the speed's own interval, but never less than 1.5
 * times the last read took, so playing never queues requests.
 */
export function playDelay(speed: 1 | 2 | 4, lastReadMs: number): number {
  return Math.max(Math.round(1000 / speed), Math.round(lastReadMs * 1.5));
}

// --- the track on screen -----------------------------------------------------------------

export type Axis = 'sequence' | 'time';

export type View = {
  /** Index range of the stops in view, both ends included. */
  from: number;
  to: number;
};

/** The whole track in view. */
export function fullView(track: Track): View {
  return { from: 0, to: Math.max(0, track.stops.length - 1) };
}

/**
 * The view after zooming by `factor` (above 1 narrows) around the stop at `centre`, at least
 * `minStops` stops wide.
 */
export function zoomView(
  track: Track,
  view: View,
  factor: number,
  centre: number,
  minStops = 8,
): View {
  const n = track.stops.length;
  if (n <= minStops) return fullView(track);
  const width = view.to - view.from + 1;
  const next = Math.min(n, Math.max(minStops, Math.round(width / factor)));
  if (next >= n) return fullView(track);
  const c = Math.min(Math.max(centre, view.from), view.to);
  const share = width > 1 ? (c - view.from) / (width - 1) : 0.5;
  let from = Math.round(c - share * (next - 1));
  from = Math.min(Math.max(0, from), n - next);
  return { from, to: from + next - 1 };
}

/** The domain value of a stop on the chosen axis. */
function valueOf(s: Stop, axis: Axis): number {
  return axis === 'time' ? s.time : s.seq;
}

/**
 * Horizontal positions of the stops in `view` inside a track `width` pixels wide with `pad`
 * pixels at each end. By sequence, equal gaps between numbers show as equal distances; by
 * time, as equal durations. A single stop sits in the middle.
 */
export function positions(
  track: Track,
  view: View,
  axis: Axis,
  width: number,
  pad = 14,
): Map<number, number> {
  const stops = track.stops.slice(view.from, view.to + 1);
  const out = new Map<number, number>();
  if (!stops.length) return out;
  const lo = valueOf(stops[0], axis);
  const hi = valueOf(stops[stops.length - 1], axis);
  const span = hi - lo;
  const inner = Math.max(1, width - 2 * pad);
  for (const s of stops) {
    out.set(s.seq, span > 0 ? pad + ((valueOf(s, axis) - lo) / span) * inner : pad + inner / 2);
  }
  return out;
}

/**
 * The stop nearest to pixel `x`. With `readableOnly` an unreadable stop gives way to the
 * nearest readable one, so a handle dragged into a hatched stretch rests at its edge.
 */
export function stopAt(
  track: Track,
  view: View,
  axis: Axis,
  width: number,
  x: number,
  readableOnly = true,
  pad = 14,
): number {
  const pos = positions(track, view, axis, width, pad);
  let best: number | null = null;
  let bestD = Infinity;
  for (const s of track.stops.slice(view.from, view.to + 1)) {
    if (readableOnly && !s.readable) continue;
    const d = Math.abs((pos.get(s.seq) ?? 0) - x);
    if (d < bestD) {
      best = s.seq;
      bestD = d;
    }
  }
  return best ?? track.head;
}

export type Bucket = {
  /** The sequence numbers of its first and last stop. */
  first: number;
  last: number;
  /** Where it sits, in pixels. */
  x: number;
  commits: number;
  /** Net changes, counted over the commits that report them. */
  inserted: number;
  deleted: number;
  snapshots: string[];
  merges: number;
  /** Whether every commit in it is readable. */
  readable: boolean;
};

/**
 * Group the stops in view into buckets at least `minGap` pixels apart. A track with room
 * for every stop gives one bucket per stop.
 */
export function bucketize(
  track: Track,
  view: View,
  axis: Axis,
  width: number,
  minGap = 7,
  pad = 14,
): Bucket[] {
  const pos = positions(track, view, axis, width, pad);
  const out: Bucket[] = [];
  let cur: Bucket | null = null;
  let curCol = -1;
  let sum = 0;
  let n = 0;
  for (const s of track.stops.slice(view.from, view.to + 1)) {
    const x = pos.get(s.seq) ?? 0;
    const col = Math.floor(x / minGap);
    if (!cur || col !== curCol) {
      cur = {
        first: s.seq,
        last: s.seq,
        x,
        commits: 0,
        inserted: 0,
        deleted: 0,
        snapshots: [],
        merges: 0,
        readable: true,
      };
      curCol = col;
      sum = 0;
      n = 0;
      out.push(cur);
    }
    cur.last = s.seq;
    sum += x;
    n++;
    // the bucket sits at the mean of its stops
    cur.x = sum / n;
    // the synthetic head stop is a position, not a commit
    if (s.kind !== 'head') cur.commits++;
    cur.inserted += s.inserted ?? 0;
    cur.deleted += s.deleted ?? 0;
    cur.snapshots.push(...s.snapshots);
    if (s.merge) cur.merges++;
    if (!s.readable) cur.readable = false;
  }
  return out;
}

// --- text ----------------------------------------------------------------------------------

/** "1 October 2026 14:03" in the viewer's locale. */
export function fmtMoment(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const date = d.toLocaleDateString(undefined, { day: 'numeric', month: 'long', year: 'numeric' });
  const time = d.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
  return `${date} ${time}`;
}

/** The caption under the track: "42 · update by ana · “fix titles” · +3 −1". */
export function caption(s: Stop): string {
  if (s.kind === 'head' && !s.timestamp) return `${fmtInt(s.seq)} · head`;
  const parts = [fmtInt(s.seq)];
  const kind = commitKindLabel(s.kind);
  parts.push(s.author ? `${kind} by ${s.author}` : kind);
  if (s.message) parts.push(`“${s.message}”`);
  if (s.inserted != null && s.deleted != null)
    parts.push(fmtDelta({ inserted: s.inserted, deleted: s.deleted }));
  if (s.snapshots.length) parts.push(s.snapshots.map((n) => `snapshot:${n}`).join(', '));
  return parts.join(' · ');
}

/** The slider's `aria-valuetext`: "commit 42, 1 October 2026 14:03, update by ana, 3 added, 1 removed". */
export function valueText(s: Stop): string {
  const parts = [`commit ${s.seq}`];
  if (s.timestamp) parts.push(fmtMoment(s.timestamp));
  if (s.kind !== 'head')
    parts.push(s.author ? `${commitKindLabel(s.kind)} by ${s.author}` : commitKindLabel(s.kind));
  if (s.head) parts.push('head');
  if (s.inserted != null && s.deleted != null)
    parts.push(`${fmtInt(s.inserted)} added, ${fmtInt(s.deleted)} removed`);
  return parts.join(', ');
}

/** The tooltip over a bucket: "commits 40–52, 13 commits, +120 −4". */
export function bucketTitle(b: Bucket): string {
  const range = b.first === b.last ? `commit ${b.first}` : `commits ${b.first}–${b.last}`;
  const count = b.commits > 1 ? `, ${fmtInt(b.commits)} commits` : '';
  const net =
    b.inserted || b.deleted ? `, ${diffSummary({ added: b.inserted, removed: b.deleted })}` : '';
  const snaps = b.snapshots.length ? `, ${b.snapshots.map((n) => `snapshot:${n}`).join(', ')}` : '';
  return `${range}${count}${net}${snaps}`;
}

/** The tooltip over a hatched stretch. */
export function goneTitle(track: Track, g: Range): string {
  const range = g.from === g.to ? `Commit ${g.from}` : `Commits ${g.from}–${g.to}`;
  const oldest =
    track.oldestReadable != null
      ? `The oldest commit that can be read is ${track.oldestReadable}.`
      : 'No commit of this track can be read.';
  return `${range} can no longer be read. ${oldest}`;
}

// --- failed reads ------------------------------------------------------------------------------

export type TimeFailure = {
  /** The commit that could not be read, when the server names it. */
  seq?: number;
  /** The ranges a read can still see, from the error body. */
  readable?: Range[];
  title: string;
  detail?: string;
};

const asRanges = (v: unknown): Range[] | undefined =>
  Array.isArray(v)
    ? v.filter(
        (r): r is Range =>
          !!r &&
          typeof r === 'object' &&
          typeof (r as Range).from === 'number' &&
          typeof (r as Range).to === 'number',
      )
    : undefined;

/** "40–57", or "40" for a range of one. */
export function rangeText(r: Range): string {
  return r.from === r.to ? `${r.from}` : `${r.from}–${r.to}`;
}

/**
 * What a refused point-in-time read means for the slider: `410 history-gone` (the state is
 * no longer kept) and `507` from the history cache (it would need too much memory). Other
 * errors are the view's own and give null.
 */
export function timeFailure(e: unknown): TimeFailure | null {
  if (!e || typeof e !== 'object') return null;
  const err = e as { status?: unknown; body?: Record<string, unknown>; code?: unknown };
  const body = err.body ?? {};
  if (err.status === 410 && (err.code === 'history-gone' || body.code === 'history-gone')) {
    const seq = typeof body.commit === 'number' ? body.commit : undefined;
    const readable = asRanges(body.reconstructable);
    const oldest =
      typeof body.oldestReconstructable === 'number' ? body.oldestReconstructable : null;
    const detail = readable?.length
      ? `Readable commits: ${readable.map(rangeText).join(', ')}.`
      : oldest != null
        ? `The oldest readable commit is ${oldest}.`
        : undefined;
    return {
      seq,
      readable,
      title:
        seq != null ? `Commit ${seq} can no longer be read` : 'That commit can no longer be read',
      detail,
    };
  }
  if (err.status === 507 && (body.budget === 'history-memory' || body.budget === 'memory')) {
    const limit = typeof body.limit === 'number' ? body.limit : null;
    return {
      title:
        body.budget === 'history-memory'
          ? 'Reading this state needs more history cache than the server allows'
          : 'Reading this state needs more memory than the server allows',
      detail: limit != null ? `The limit is ${fmtBytes(limit)}.` : undefined,
    };
  }
  return null;
}
