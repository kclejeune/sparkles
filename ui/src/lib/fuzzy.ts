// The command palette's ranking of short lists on the client: commands, pages, datasets,
// branches and saved queries (ROADMAP item 631). Entities keep the server's typo-tolerant
// ranking. The match is deliberately strict, so that a few letters do not pull in rows that
// merely contain them somewhere:
//
// - the whole query as the text (1000), its prefix (900), a substring at the start of a word
//   (800, less for later words) or anywhere else (600, less further in);
// - every query word as the prefix of a word of the text, in any order (500);
// - initials: each query letter continues the previous match or starts a word (400), so
//   `fq` finds "Format query";
// - one typo (a substitution, insertion, deletion or swap) in a query of 5 or more letters
//   against the start of a word (200).
//
// Keywords are matched only at the start of a word, at 0.8 of the score and without a
// highlight.

export interface FuzzyMatch {
  score: number;
  /** Ranges of the text to highlight, in UTF-16 code units, end exclusive. */
  ranges: [number, number][];
}

/** The start offsets of the words of a lowercased text. */
function wordStarts(t: string): number[] {
  const out: number[] = [];
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (!/[\p{L}\p{N}]/u.test(c)) continue;
    const prev = i ? t[i - 1] : ' ';
    const upperStart = i > 0 && /\p{Lu}/u.test(c) && /\p{Ll}/u.test(prev);
    if (!/[\p{L}\p{N}]/u.test(prev) || upperStart) out.push(i);
  }
  return out;
}

/** The optimal string alignment distance of `a` and `b`, or `max + 1` once it exceeds `max`. */
export function editDistance(a: string, b: string, max = 1): number {
  if (Math.abs(a.length - b.length) > max) return max + 1;
  const rows: number[][] = [];
  for (let i = 0; i <= a.length; i++) {
    rows.push(new Array<number>(b.length + 1).fill(0));
    rows[i][0] = i;
  }
  for (let j = 0; j <= b.length; j++) rows[0][j] = j;
  for (let i = 1; i <= a.length; i++) {
    let best = Infinity;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      let v = Math.min(rows[i - 1][j] + 1, rows[i][j - 1] + 1, rows[i - 1][j - 1] + cost);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1])
        v = Math.min(v, rows[i - 2][j - 2] + 1);
      rows[i][j] = v;
      best = Math.min(best, v);
    }
    if (best > max) return max + 1;
  }
  return rows[a.length][b.length];
}

/** Each query letter continues the previous match or starts a word of the text. */
function initials(q: string, t: string, starts: Set<number>): [number, number][] | null {
  const ranges: [number, number][] = [];
  let at = 0;
  let prev = -2;
  for (const c of q) {
    if (c === ' ') continue;
    let found = -1;
    if (prev >= 0 && t[prev + 1] === c) found = prev + 1;
    else {
      for (let i = at; i < t.length; i++)
        if (t[i] === c && starts.has(i)) {
          found = i;
          break;
        }
    }
    if (found < 0) return null;
    const last = ranges[ranges.length - 1];
    if (last && last[1] === found) last[1] = found + 1;
    else ranges.push([found, found + 1]);
    prev = found;
    at = found + 1;
  }
  return ranges;
}

function matchText(q: string, text: string): FuzzyMatch | null {
  const t = text.toLowerCase();
  if (!q) return { score: 0, ranges: [] };
  if (t === q) return { score: 1000, ranges: [[0, text.length]] };
  if (t.startsWith(q)) return { score: 900, ranges: [[0, q.length]] };
  const startList = wordStarts(text);
  const starts = new Set(startList);
  // a substring: at the start of a word first, then anywhere
  let best: FuzzyMatch | null = null;
  for (let i = t.indexOf(q); i >= 0; i = t.indexOf(q, i + 1)) {
    const w = startList.indexOf(i);
    const score = w >= 0 ? 800 - Math.min(w, 50) : q.length >= 3 ? 600 - Math.min(i, 100) : -1;
    if (score > (best?.score ?? -1)) best = { score, ranges: [[i, i + q.length]] };
  }
  if (best) return best;
  // every query word as the prefix of a word
  const qWords = q.split(/\s+/).filter(Boolean);
  if (qWords.length > 1) {
    const ranges: [number, number][] = [];
    const ok = qWords.every((w) => {
      const s = startList.find((i) => t.startsWith(w, i) && !ranges.some((r) => r[0] === i));
      if (s === undefined) return false;
      ranges.push([s, s + w.length]);
      return true;
    });
    if (ok) return { score: 500, ranges: ranges.sort((a, b) => a[0] - b[0]) };
  }
  // initials
  const ini = initials(q, t, starts);
  if (ini) return { score: 400 - ini.length * 10, ranges: ini };
  // one typo, for a query of 5 or more letters, against the start of a word
  if (q.length >= 5 && qWords.length === 1) {
    for (const s of startList) {
      for (const len of [q.length, q.length - 1, q.length + 1]) {
        if (s + len > t.length) continue;
        if (editDistance(q, t.slice(s, s + len)) <= 1)
          return { score: 200 - Math.min(s, 100), ranges: [[s, s + len]] };
      }
    }
  }
  return null;
}

/** Whether every query word starts a word of the keyword. */
function matchKeyword(q: string, keyword: string): number | null {
  const k = keyword.toLowerCase();
  const starts = wordStarts(keyword);
  const words = q.split(/\s+/).filter(Boolean);
  if (!words.every((w) => starts.some((s) => k.startsWith(w, s)))) return null;
  return k === q ? 1000 : starts[0] === 0 && k.startsWith(q) ? 900 : 800;
}

/** How well `query` matches `text` (and its keywords), or null when it does not. */
export function fuzzy(query: string, text: string, keywords: string[] = []): FuzzyMatch | null {
  const q = query.trim().toLowerCase().replace(/\s+/g, ' ');
  const m = matchText(q, text);
  let kw: number | null = null;
  for (const k of keywords) {
    const s = matchKeyword(q, k);
    if (s != null && (kw == null || s > kw)) kw = s;
  }
  const kwScore = kw == null ? null : Math.round(kw * 0.8);
  if (m && (kwScore == null || m.score >= kwScore)) return m;
  if (kwScore != null) return { score: kwScore, ranges: [] };
  return m;
}

/**
 * The items of `list` that match `query`, best first. Ties go to a recent item (`recent`
 * holds their ids), then the shorter label, then the label.
 */
export function rank<T>(
  list: readonly T[],
  query: string,
  get: (x: T) => { id: string; title: string; keywords?: string[] },
  recent: ReadonlySet<string> = new Set(),
): { item: T; match: FuzzyMatch }[] {
  const out: { item: T; match: FuzzyMatch; title: string; bonus: number }[] = [];
  for (const item of list) {
    const g = get(item);
    const match = fuzzy(query, g.title, g.keywords);
    if (match) out.push({ item, match, title: g.title, bonus: recent.has(g.id) ? 50 : 0 });
  }
  if (!query.trim()) return out.map(({ item, match }) => ({ item, match }));
  out.sort(
    (a, b) =>
      b.match.score + b.bonus - (a.match.score + a.bonus) ||
      a.title.length - b.title.length ||
      a.title.localeCompare(b.title),
  );
  return out.map(({ item, match }) => ({ item, match }));
}
