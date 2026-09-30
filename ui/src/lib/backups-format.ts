// Formatting and validation for the Backups area: dedup ratios, retention sentences,
// name templates, `*` globs, schedule presets and restore warnings. Pure functions.

import { fmtBytes, fmtRelative } from './format';
import type { BackupSummary, Repository, RepositoryConfig, Retention } from './backups';

/** Repository and policy names. */
export const REPO_NAME = /^[a-z0-9][a-z0-9_-]{0,63}$/;
export const POLICY_NAME = REPO_NAME;
/** Backup names (the snapshot-name grammar). */
export const BACKUP_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
/** Dataset names a restore may create (the clone rule). */
export const DATASET_NAME = /^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,63}$/;

// --- ratios and sizes ----------------------------------------------------------------

/** `98×`, `2.4×`, `∞` (nothing new stored), `—` (unknown). */
export function fmtRatio(r: number | null | undefined): string {
  if (r == null || Number.isNaN(r) || r <= 0) return '—';
  if (!Number.isFinite(r)) return '∞';
  return r >= 9.95 ? `${Math.round(r)}×` : `${r.toFixed(1)}×`;
}

/** Logical over added bytes: how much of a backup was already in the repository. */
export function dedupRatio(logical: number, added: number): number | null {
  if (!(logical > 0)) return null;
  return added > 0 ? logical / added : Infinity;
}

export const fmtDedup = (b: Pick<BackupSummary, 'logicalBytes' | 'addedBytes'>) =>
  fmtRatio(dedupRatio(b.logicalBytes, b.addedBytes));

/** A bandwidth limit: `100 MiB/s`, or `unlimited`. */
export const fmtBytesPerSec = (n: number | null | undefined) =>
  n ? `${fmtBytes(n)}/s` : 'unlimited';

/** `#42 · 3h ago` */
export const fmtCommitAge = (c: { seq: number; timestamp: string }, now = Date.now()) =>
  `#${c.seq} · ${fmtRelative(c.timestamp, now)}`;

/** Where a repository lives: the path, or `s3://bucket/prefix`. */
export function repoLocation(r: Pick<RepositoryConfig, 'type' | 'path' | 'bucket' | 'prefix'>) {
  if (r.type === 'fs') return r.path ?? '—';
  const scheme = { s3: 's3', gcs: 'gs', azure: 'az' }[r.type];
  const prefix = r.prefix?.replace(/^\/+|\/+$/g, '');
  return `${scheme}://${r.bucket ?? '?'}${prefix ? `/${prefix}` : ''}`;
}

/** The endpoint is plain http (allowed with `allowHttp`, worth a warning). */
export const insecureEndpoint = (r: Pick<Repository, 'endpoint'>) =>
  !!r.endpoint && /^http:\/\//i.test(r.endpoint);

// --- durations and retention ---------------------------------------------------------------

const UNITS: Record<string, [seconds: number, word: string]> = {
  s: [1, 'second'],
  sec: [1, 'second'],
  secs: [1, 'second'],
  second: [1, 'second'],
  seconds: [1, 'second'],
  m: [60, 'minute'],
  min: [60, 'minute'],
  mins: [60, 'minute'],
  minute: [60, 'minute'],
  minutes: [60, 'minute'],
  h: [3600, 'hour'],
  hr: [3600, 'hour'],
  hrs: [3600, 'hour'],
  hour: [3600, 'hour'],
  hours: [3600, 'hour'],
  d: [86400, 'day'],
  day: [86400, 'day'],
  days: [86400, 'day'],
  w: [604800, 'week'],
  week: [604800, 'week'],
  weeks: [604800, 'week'],
};

/**
 * A duration like `30d`, `12h`, `1w`, `90m` or `1d 12h`: its length and its words
 * (`30 days`, `1 day 12 hours`). Null when it does not parse.
 */
export function parseDuration(s: string | null | undefined): {
  seconds: number;
  words: string;
} | null {
  const src = (s ?? '').trim().toLowerCase();
  if (!src) return null;
  const re = /(\d+)\s*([a-z]+)\s*/y;
  let seconds = 0;
  const words: string[] = [];
  let at = 0;
  while (at < src.length) {
    re.lastIndex = at;
    const m = re.exec(src);
    const unit = m && UNITS[m[2]];
    if (!m || !unit) return null;
    const n = Number(m[1]);
    seconds += n * unit[0];
    words.push(`${n} ${unit[1]}${n === 1 ? '' : 's'}`);
    at = re.lastIndex;
  }
  return { seconds, words: words.join(' ') };
}

/**
 * What a policy's retention does, per dataset: "Keeps at least 7; deletes backups older
 * than 30 days, and any beyond 60." The first `minCount` are always kept, so a
 * `maxCount` below it acts as `minCount`.
 */
export function retentionSentence(r: Retention): string {
  const min = Math.max(0, r.minCount);
  const max = r.maxCount == null ? null : Math.max(min, r.maxCount);
  const expire = r.expireAfter ? (parseDuration(r.expireAfter)?.words ?? r.expireAfter) : null;
  if (!expire && max == null) return 'Keeps every backup.';
  if (!expire && max === min)
    return min === 0 ? 'Deletes every backup.' : `Keeps the newest ${min} only.`;
  const drops: string[] = [];
  if (expire) drops.push(`backups older than ${expire}`);
  if (max != null) drops.push(expire ? `and any beyond ${max}` : `any beyond ${max}`);
  const deletes = `deletes ${drops.join(', ')}`;
  const text = min > 0 ? `keeps at least ${min}; ${deletes}` : deletes;
  return `${text[0].toUpperCase()}${text.slice(1)}.`;
}

// --- name templates ------------------------------------------------------------------------

export type NameContext = {
  policy: string;
  dataset: string;
  /** The head commit. */
  seq: number;
  /** The run id (its first 8 hex digits are used). */
  run: string;
  /** The scheduled instant. */
  time: Date;
  /** IANA time zone of `{date:…}`. */
  timezone: string;
};

const pad = (n: number, w = 2) => String(n).padStart(w, '0');

type Parts = { Y: number; m: number; d: number; H: number; M: number; S: number };

function zonedParts(t: Date, timeZone: string): Parts {
  const f = new Intl.DateTimeFormat('en-US', {
    timeZone,
    hourCycle: 'h23',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
  const p: Record<string, string> = {};
  for (const x of f.formatToParts(t)) p[x.type] = x.value;
  return { Y: +p.year, m: +p.month, d: +p.day, H: +p.hour % 24, M: +p.minute, S: +p.second };
}

const DAY_MS = 86_400_000;

function dayOfYear(p: Parts) {
  return Math.round((Date.UTC(p.Y, p.m - 1, p.d) - Date.UTC(p.Y, 0, 1)) / DAY_MS) + 1;
}

/** ISO 8601 week number (weeks start on Monday; week 1 holds the first Thursday). */
export function isoWeek(y: number, m: number, d: number): number {
  const t = new Date(Date.UTC(y, m - 1, d));
  const dow = (t.getUTCDay() + 6) % 7;
  t.setUTCDate(t.getUTCDate() - dow + 3); // the Thursday of this week
  const firstThursday = new Date(Date.UTC(t.getUTCFullYear(), 0, 4));
  const shift = (firstThursday.getUTCDay() + 6) % 7;
  return 1 + Math.round(((t.getTime() - firstThursday.getTime()) / DAY_MS - 3 + shift) / 7);
}

function strftime(fmt: string, p: Parts): string {
  let out = '';
  for (let i = 0; i < fmt.length; i++) {
    if (fmt[i] !== '%') {
      out += fmt[i];
      continue;
    }
    const c = fmt[++i];
    switch (c) {
      case 'Y':
        out += pad(p.Y, 4);
        break;
      case 'm':
        out += pad(p.m);
        break;
      case 'd':
        out += pad(p.d);
        break;
      case 'H':
        out += pad(p.H);
        break;
      case 'M':
        out += pad(p.M);
        break;
      case 'S':
        out += pad(p.S);
        break;
      case 'j':
        out += pad(dayOfYear(p), 3);
        break;
      case 'V':
        out += pad(isoWeek(p.Y, p.m, p.d));
        break;
      default:
        throw new Error(
          c === undefined
            ? 'a date format ends with “%”'
            : `“%${c}” is not supported (use %Y %m %d %H %M %S %j %V)`,
        );
    }
  }
  return out;
}

/** `{time}`: the instant in UTC, `20260930t140311z`. */
export function fmtTimeToken(t: Date): string {
  return (
    `${pad(t.getUTCFullYear(), 4)}${pad(t.getUTCMonth() + 1)}${pad(t.getUTCDate())}` +
    `t${pad(t.getUTCHours())}${pad(t.getUTCMinutes())}${pad(t.getUTCSeconds())}z`
  );
}

export const DEFAULT_NAME_TEMPLATE = '{policy}-{dataset}-{time}';

/**
 * Renders a policy's name template: `{policy}`, `{dataset}`, `{seq}`, `{run}`, `{time}`
 * and `{date:FMT}` (a strftime subset in the policy's time zone). The result must be a
 * valid backup name.
 */
export function renderNameTemplate(
  tpl: string,
  ctx: NameContext,
): { name: string; error?: undefined } | { name?: undefined; error: string } {
  let out = '';
  try {
    for (let i = 0; i < tpl.length; i++) {
      if (tpl[i] !== '{') {
        out += tpl[i];
        continue;
      }
      const end = tpl.indexOf('}', i);
      if (end < 0) return { error: `“{” at ${i + 1} is not closed` };
      const token = tpl.slice(i + 1, end);
      i = end;
      if (token === 'policy') out += ctx.policy;
      else if (token === 'dataset') out += ctx.dataset;
      else if (token === 'seq') out += String(ctx.seq);
      else if (token === 'run') out += ctx.run.replace(/-/g, '').slice(0, 8);
      else if (token === 'time') out += fmtTimeToken(ctx.time);
      else if (token.startsWith('date:')) {
        out += strftime(token.slice(5), zonedParts(ctx.time, ctx.timezone));
      } else return { error: `unknown placeholder {${token}}` };
    }
  } catch (e) {
    return {
      error:
        e instanceof RangeError
          ? `unknown time zone “${ctx.timezone}”`
          : String((e as Error).message),
    };
  }
  if (!BACKUP_NAME.test(out))
    return {
      error: `“${out}” is not a valid backup name: up to 64 letters, digits, “.”, “_” or “-”, starting with a letter or digit`,
    };
  return { name: out };
}

// --- dataset globs ---------------------------------------------------------------------------

/** `*`-only glob, as in grants and policy dataset lists: `wiki-*`, `team-*-prod`, `*`. */
export function globMatch(pattern: string, name: string): boolean {
  let i = 0;
  let j = 0;
  let star: [number, number] | null = null;
  while (j < name.length) {
    if (i < pattern.length && pattern[i] === '*') {
      star = [i, j];
      i++;
    } else if (i < pattern.length && pattern[i] === name[j]) {
      i++;
      j++;
    } else if (star) {
      i = star[0] + 1;
      j = star[1] + 1;
      star = [star[0], star[1] + 1];
    } else {
      return false;
    }
  }
  for (; i < pattern.length; i++) if (pattern[i] !== '*') return false;
  return true;
}

/** The names any pattern matches, in their order. */
export const matchDatasets = (patterns: string[], names: string[]) =>
  names.filter((n) => patterns.some((p) => globMatch(p, n)));

/** Splits a comma or whitespace separated pattern list. */
export const parsePatterns = (s: string) =>
  s
    .split(/[\s,]+/)
    .map((x) => x.trim())
    .filter(Boolean);

// --- schedule presets ------------------------------------------------------------------------

export type SchedulePreset =
  | { kind: 'hourly'; minute: number }
  | { kind: 'daily'; hour: number; minute: number }
  /** `day`: 0 = Sunday … 6 = Saturday, as in cron. */
  | { kind: 'weekly'; day: number; hour: number; minute: number }
  | { kind: 'custom'; expr: string };

export const WEEKDAYS = [
  'Sunday',
  'Monday',
  'Tuesday',
  'Wednesday',
  'Thursday',
  'Friday',
  'Saturday',
];

/** The cron expression of a preset (custom ones are passed through). */
export function presetToSchedule(p: SchedulePreset): string {
  switch (p.kind) {
    case 'hourly':
      return `${p.minute} * * * *`;
    case 'daily':
      return `${p.minute} ${p.hour} * * *`;
    case 'weekly':
      return `${p.minute} ${p.hour} * * ${p.day}`;
    case 'custom':
      return p.expr.trim();
  }
}

const int = (s: string, lo: number, hi: number) =>
  /^\d{1,2}$/.test(s) && +s >= lo && +s <= hi ? +s : null;

/** The preset a schedule is, or `custom`. */
export function scheduleToPreset(s: string): SchedulePreset {
  const f = s.trim().split(/\s+/);
  if (f.length === 5 && f[2] === '*' && f[3] === '*') {
    const minute = int(f[0], 0, 59);
    if (minute != null && f[1] === '*' && f[4] === '*') return { kind: 'hourly', minute };
    const hour = int(f[1], 0, 23);
    if (minute != null && hour != null) {
      if (f[4] === '*') return { kind: 'daily', hour, minute };
      const day = int(f[4], 0, 7);
      if (day != null) return { kind: 'weekly', day: day % 7, hour, minute };
    }
  }
  return { kind: 'custom', expr: s.trim() };
}

// --- restore -------------------------------------------------------------------------------

/**
 * What replacing a dataset at `head` with a backup at `seq` loses: `commits 43–57 will be
 * lost`. A different lineage (another dataset id) is replaced as a whole.
 */
export function restoreLoss(
  head: number | null | undefined,
  seq: number,
  sameLineage: boolean,
): { lost: number; text: string } | null {
  if (head == null) return null;
  if (!sameLineage)
    return {
      lost: head,
      text: `It is a different lineage: all of its current data (head commit ${head}) will be replaced`,
    };
  if (head <= seq) return { lost: 0, text: 'No commits will be lost' };
  const lost = head - seq;
  return {
    lost,
    text: lost === 1 ? `Commit ${head} will be lost` : `Commits ${seq + 1}–${head} will be lost`,
  };
}

/** `{ds}-restored-{yyyymmdd}`, made unique against `taken` and kept to 64 characters. */
export function defaultRestoreName(ds: string, taken: Iterable<string>, now = new Date()): string {
  const used = new Set(taken);
  const date = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}`;
  const tail = `-restored-${date}`;
  const base = `${ds.slice(0, 64 - tail.length - 3)}${tail}`;
  if (!used.has(base)) return base;
  for (let n = 2; ; n++) if (!used.has(`${base}-${n}`)) return `${base}-${n}`;
}
