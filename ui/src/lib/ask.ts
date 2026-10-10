// The question a query tab can carry (C18 §6.1): the header's lines, the parameters that
// Save as example proposes for constant entities, the local Asked history and the words
// for an empty result.

import type {
  AskClarify,
  AskEvent,
  AskGraph,
  AskRecord,
  AskSummary,
  AskUsage,
  CheckIssue,
  CheckResult,
  CheckTerm,
  Diagnosis,
  Feedback,
} from './ask-api';
import { compactIri, localName, type Prefixes } from './compact';
import { fmtInt } from './format';
import { load, save } from './storage';
import { queryVariables, RESERVED } from './stored-queries';

/** An earlier question of a conversation, with the query that answered it. */
export type Turn = { question: string; query: string };

/** The question of a tab, with what the agent said about its query. */
export type TabQuestion = {
  question?: string;
  explanation?: string;
  assumptions?: string[];
  /** The query as the link or the history gave it; an edit makes the tab's query differ. */
  original: string;
  // what an ask of the server adds (Phase 2)
  /** The server's id of the ask, for feedback and Try harder. */
  askId?: string;
  /** The earlier turns this question was asked with. */
  context?: Turn[];
  summary?: AskSummary;
  usage?: AskUsage;
  clarify?: AskClarify;
  /** Why the ask ended without an answer, in words. */
  failure?: string;
  graph?: AskGraph | null;
  /** The feedback sent for the answer. */
  feedback?: Feedback;
};

/** The last check of a tab's query, kept with the text it checked. */
export type TabCheck = {
  text: string;
  result?: CheckResult;
  error?: string;
};

/** Whether a tab has anything for the header to show. */
export const hasQuestion = (q: TabQuestion | undefined): q is TabQuestion =>
  !!q && !!(q.question || q.explanation || q.assumptions?.length);

/** A tab's title from its question: the first words, at most 32 characters. */
export function questionTitle(question: string): string {
  const q = question.trim().replace(/\s+/g, ' ');
  return q.length <= 32 ? q : `${q.slice(0, 31).trimEnd()}…`;
}

// --- the header's terms list ------------------------------------------------------

const plural = (n: number, one: string, many = `${one}s`) => `${fmtInt(n)} ${n === 1 ? one : many}`;

/**
 * What the terms list says about a term after its name and label, such as
 * `property · 214 triples` or `entity · ex:Team`.
 */
export function termDetail(t: CheckTerm): string {
  const parts: string[] = [t.kind];
  if (t.kind === 'property' && t.count != null) parts.push(plural(t.count, 'triple'));
  if (t.kind === 'class' && t.count != null) parts.push(plural(t.count, 'instance'));
  if (t.kind === 'entity' && t.types?.length) parts.push(t.types.join(', '));
  if (!t.occurs) parts.push('not in the data you can read');
  return parts.join(' · ');
}

/** One line of the terms list: `ex:memberOf "member of" · property · 214 triples`. */
export function termLine(t: CheckTerm): string {
  return `${t.term}${t.label ? ` "${t.label}"` : ''} · ${termDetail(t)}`;
}

const errors = (issues: CheckIssue[]) => issues.filter((i) => i.severity === 'error');

/** The Checked line: the issues, or none, and the estimated rows. */
export function checkedLine(r: CheckResult): string {
  const e = errors(r.issues).length;
  const w = r.issues.length - e;
  const parts: string[] = [];
  if (!e && !w) parts.push('no issues');
  if (e) parts.push(plural(e, 'error'));
  if (w) parts.push(plural(w, 'warning'));
  if (r.estimatedRows != null) parts.push(`estimated ${plural(r.estimatedRows, 'row')}`);
  return parts.join(' · ');
}

/** Whether a check refused the text as an update. */
export const isNotAQuery = (r: CheckResult | undefined) =>
  !!r?.issues.some((i) => i.code === 'not-a-query');

export const UPDATE_REFUSAL = 'This link contains an update. Links can only open queries.';

// --- Save as example ----------------------------------------------------------------

/** A parameter proposed for a constant entity of the query. */
export type Proposal = {
  name: string;
  /** The entity, as a full IRI (the parameter's default). */
  iri: string;
  /** The entity as the check named it, such as `res:payments`. */
  term: string;
  label?: string;
  /** The type the name comes from, compact. */
  type: string;
};

/** A local name in lower camel case: `Team` -> `team`, `OrganizationalUnit` -> `organizationalUnit`. */
export function lowerCamel(name: string): string {
  const words = name.split(/[^A-Za-z0-9]+/).filter(Boolean);
  if (!words.length) return '';
  const first = words[0];
  // a leading run of capitals is an acronym: `HTTPServer` -> `httpServer`
  const run = /^[A-Z]+(?=[A-Z][a-z]|$|\d)/.exec(first)?.[0] ?? first[0];
  const head = run.toLowerCase() + first.slice(run.length);
  const tail = words.slice(1).map((w) => w[0].toUpperCase() + w.slice(1));
  let out = [head, ...tail].join('');
  if (/^\d/.test(out)) out = `p${out}`;
  return out;
}

/**
 * Parameters for the constant entities of a checked query: one per entity term with a
 * type, named after the type's local name, made unique against the query's variables
 * and each other with a numeric suffix.
 */
export function proposeParameters(
  query: string,
  terms: CheckTerm[] | undefined,
  prefixes: Prefixes,
): Proposal[] {
  const taken = new Set(queryVariables(query));
  const out: Proposal[] = [];
  for (const t of terms ?? []) {
    if (t.kind !== 'entity' || !t.types?.length) continue;
    const type = t.types[0];
    const typeIri = compactIri(type, prefixes) ?? type;
    const base = lowerCamel(localName(typeIri)) || 'value';
    let name = base;
    for (let n = 2; taken.has(name) || RESERVED.has(name); n++) name = `${base}${n}`;
    taken.add(name);
    out.push({ name, iri: t.iri, term: t.term, label: t.label, type });
  }
  return out;
}

/** The prefixes a query declares, by name. */
function declared(text: string): Prefixes {
  const out: Prefixes = {};
  for (const m of text.matchAll(/\bPREFIX\s+([A-Za-z][\w.-]*)?:\s*<([^>\s]*)>/gi))
    out[m[1] ?? ''] = m[2];
  return out;
}

/** The spans of the query where a constant IRI is written, outside declarations. */
export function iriSpans(
  text: string,
  prefixes: Prefixes,
): { from: number; to: number; iri: string }[] {
  const known = { ...prefixes, ...declared(text) };
  const out: { from: number; to: number; iri: string }[] = [];
  const n = text.length;
  let i = 0;
  while (i < n) {
    const c = text[i];
    if (c === '#') {
      while (i < n && text[i] !== '\n') i++;
    } else if (c === '"' || c === "'") {
      const long = text.startsWith(c.repeat(3), i);
      const close = long ? c.repeat(3) : c;
      i += close.length;
      while (i < n && !text.startsWith(close, i)) i += text[i] === '\\' ? 2 : 1;
      i += close.length;
    } else if (c === '<') {
      const end = text.indexOf('>', i + 1);
      const iri = end < 0 ? '' : text.slice(i + 1, end);
      if (end > 0 && !/\s/.test(iri)) {
        // `PREFIX ex: <…>` and `BASE <…>` declare, they do not use
        const before = text.slice(Math.max(0, i - 80), i);
        if (!/\b(PREFIX\s+[\w.-]*:|BASE)\s*$/i.test(before))
          out.push({ from: i, to: end + 1, iri });
        i = end + 1;
      } else i++;
    } else if (c === '?' || c === '$') {
      const m = /^[A-Za-z0-9_·À-￿]+/.exec(text.slice(i + 1));
      i += 1 + (m ? m[0].length : 0);
    } else if (/[A-Za-z:]/.test(c) && (i === 0 || !/[\w?$:.\-@^]/.test(text[i - 1]))) {
      const m = /^([A-Za-z][\w.-]*)?:((?:[\w\-À-￿%]|\\.|\.(?=[\w\-À-￿%:]))*)/.exec(text.slice(i));
      if (m) {
        const ns = known[m[1] ?? ''];
        const before = text.slice(Math.max(0, i - 20), i);
        const isDecl = /\bPREFIX\s+$/i.test(before);
        if (ns != null && !isDecl) {
          const local = m[2].replace(/\\(.)/g, '$1');
          out.push({ from: i, to: i + m[0].length, iri: ns + local });
        }
        i += Math.max(m[0].length, 1);
      } else {
        const w = /^[A-Za-z_]\w*/.exec(text.slice(i));
        i += w ? w[0].length : 1;
      }
    } else i++;
  }
  return out;
}

/** The query with each proposal's entity replaced by its variable. */
export function applyProposals(text: string, proposals: Proposal[], prefixes: Prefixes): string {
  const by = new Map(proposals.map((p) => [p.iri, p.name]));
  if (!by.size) return text;
  let out = '';
  let last = 0;
  for (const s of iriSpans(text, prefixes)) {
    const name = by.get(s.iri);
    if (!name) continue;
    out += text.slice(last, s.from) + `?${name}`;
    last = s.to;
  }
  return out + text.slice(last);
}

/** A stored-query name from a question: `Who is on the payments team?` -> `who-is-on-the-payments-team`. */
export function nameFromQuestion(question: string): string {
  const slug = question
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[̀-ͯ]/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
    .replace(/-+$/, '');
  return slug || 'example';
}

// --- the Asked history --------------------------------------------------------------

/** A question asked in this browser, with its query. */
export type Asked = {
  question: string;
  query: string;
  explanation?: string;
  assumptions?: string[];
  /** When it was asked, RFC 3339. */
  at: string;
};

export const ASKED_MAX = 50;

/** The storage key of a dataset's history for a principal (`local` without sign-in). */
export const askedKey = (ds: string, principal: string | null | undefined) =>
  `sparkles.asked.${principal || 'local'}.${ds}`;

/** The list with `entry` first; the same question and query are kept once, at most 50. */
export function addAsked(list: Asked[], entry: Asked, max = ASKED_MAX): Asked[] {
  const rest = list.filter((a) => !(a.question === entry.question && a.query === entry.query));
  return [entry, ...rest].slice(0, max);
}

export function loadAsked(ds: string, principal: string | null | undefined): Asked[] {
  const v = load<unknown>(askedKey(ds, principal), []);
  if (!Array.isArray(v)) return [];
  return v
    .filter(
      (a): a is Asked =>
        !!a && typeof a.question === 'string' && typeof a.query === 'string' && !!a.question,
    )
    .slice(0, ASKED_MAX);
}

export function rememberAsked(
  ds: string,
  principal: string | null | undefined,
  entry: Asked,
): Asked[] {
  const list = addAsked(loadAsked(ds, principal), entry);
  save(askedKey(ds, principal), list);
  return list;
}

// --- an empty result ------------------------------------------------------------------

export const EMPTY_HEADLINE = 'No data you can read matched.';

/** What the result area says about an empty result, from its diagnosis. */
export type EmptyExplanation = {
  headline: string;
  message: string;
  first?: {
    kind: string;
    text: string;
    /** The element's constants that do not occur in the view. */
    missing: string[];
    issues: string[];
  };
};

export function emptyExplanation(d: Diagnosis | null | undefined): EmptyExplanation | null {
  if (!d || !d.empty) return null;
  const out: EmptyExplanation = { headline: EMPTY_HEADLINE, message: d.message };
  if (d.first) {
    out.first = {
      kind: d.first.kind,
      text: d.first.text,
      missing: d.first.constants.filter((c) => !c.occurs).map((c) => c.term),
      issues: d.first.issues.map((i) => i.message),
    };
  }
  return out;
}

/** Whether a finished result is empty: no rows, no triples, or ASK false. */
export function isEmptyResult(r: {
  queryType: string;
  rows?: unknown[];
  boolean?: boolean;
}): boolean {
  if (r.queryType === 'ASK') return r.boolean === false;
  if (r.queryType === 'SELECT') return (r.rows?.length ?? 0) === 0;
  return false;
}

// --- the Ask bar (Phase 2) ------------------------------------------------------------

/** How an ask ends: with the checked query to read first, or with rows and a summary. */
export type RunMode = 'preview' | 'run';

/** What the Ask bar remembers per principal. */
export type AskPrefs = { mode: RunMode; summaryCollapsed: boolean };

export const askPrefsKey = (principal: string | null | undefined) =>
  `sparkles.askPrefs.${principal || 'local'}`;

export function loadAskPrefs(principal: string | null | undefined): AskPrefs {
  const v = load<Partial<AskPrefs> | null>(askPrefsKey(principal), null);
  return {
    mode: v?.mode === 'run' ? 'run' : 'preview',
    summaryCollapsed: v?.summaryCollapsed === true,
  };
}

export function saveAskPrefs(principal: string | null | undefined, p: AskPrefs): void {
  save(askPrefsKey(principal), p);
}

/** The most earlier turns a follow-up carries. */
export const MAX_TURNS = 5;

/**
 * The context of a follow-up asked in a tab: the tab's own earlier turns and its answered
 * question, the last 5. A tab without an answered question gives none.
 */
export function followUpContext(q: TabQuestion | undefined, query: string): Turn[] {
  if (!q?.question || !q.askId || !query.trim()) return [];
  return [...(q.context ?? []), { question: q.question, query }].slice(-MAX_TURNS);
}

/** What the step indicator says while the server works, after `e` arrived. */
export function nextStep(e: AskEvent, o: { run: boolean; summary: boolean }): string | null {
  switch (e.event) {
    case 'ground':
      return 'Writing a query';
    case 'escalate':
      return `Asking ${e.data.to.model}`;
    case 'draft':
      return 'Checking';
    case 'check':
      return e.data.ok ? (o.run ? 'Running' : 'Finishing') : 'Repairing';
    case 'run':
      return e.data.error || !e.data.rows ? 'Looking for what matched nothing' : 'Finishing';
    case 'diagnosis':
      return `Repairing (${Math.min(e.data.attempt ?? 1, 2)} of 2)`;
    case 'result':
      return o.run && o.summary && e.data.results ? 'Summarizing' : 'Finishing';
    default:
      return null;
  }
}

/** The step indicator's first words, before any event arrives. */
export const FIRST_STEP = 'Finding entities';

/** A piece of a summary's text: plain text, or a marker that cites rows (1-based). */
export type SummaryPart = { text: string; rows?: number[] };

/**
 * The summary's text cut at its `[n]` markers. A marker may name one row, a range such as
 * `[1–4]` or a list such as `[1, 3]`. Only the rows in `citations` are kept, and a marker
 * with none left stays plain text.
 */
export function summaryParts(text: string, citations: number[]): SummaryPart[] {
  const cited = new Set(citations);
  const out: SummaryPart[] = [];
  let last = 0;
  for (const m of text.matchAll(/\[(\d+(?:\s*[-–]\s*\d+)?(?:\s*,\s*\d+(?:\s*[-–]\s*\d+)?)*)\]/g)) {
    const rows: number[] = [];
    for (const part of m[1].split(',')) {
      const [a, b] = part.split(/[-–]/).map((x) => parseInt(x.trim(), 10));
      const hi = b != null && b >= a && b - a < 1000 ? b : a;
      for (let n = a; n <= hi; n++) if (cited.has(n)) rows.push(n);
    }
    if (!rows.length) continue;
    if (m.index! > last) out.push({ text: text.slice(last, m.index) });
    out.push({ text: m[0], rows });
    last = m.index! + m[0].length;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}

/** The label under a summary: `generated · cites 4 of 12 rows`. */
export function summaryLabel(s: AskSummary, rows: number): string {
  const n = new Set(s.citations).size;
  if (!n) return 'generated · cites no row';
  return `generated · cites ${fmtInt(n)} of ${plural(rows, 'row')}`;
}

/** The words the UI shows for an ask that ended without an answer (§6.5). */
export function askFailure(code: string, message?: string, resetAt?: string): string {
  switch (code) {
    case 'provider-unavailable':
    case 'provider-auth':
    case 'provider-rejected':
    case 'timeout':
    case 'deadline':
      return 'The model provider is not responding.';
    case 'budget-exceeded': {
      const at = resetAt ? new Date(resetAt) : null;
      const when = at && !isNaN(at.getTime()) ? ` It resets at ${at.toLocaleString()}.` : '';
      return `This dataset's question budget for today is used up.${when}`;
    }
    case 'no-valid-query':
    case 'invalid-output':
      return 'Sparkles could not write a valid query for this question.';
    case 'unanswerable':
      return 'The data does not seem to describe this.';
    case 'no-assistant':
      return message ?? 'This dataset has no assistant.';
    case 'no-later-pair':
      return 'No stronger model is configured for this dataset.';
    case 'cancelled':
      return 'The question was stopped.';
    default:
      return message ?? 'The question could not be answered.';
  }
}

/** Graph pickers from a draft's `graph` variables, when the result has those columns. */
export function graphColumns(
  g: AskGraph | null | undefined,
  vars: string[],
): { s: string; p: string; o: string } | null {
  if (!g) return null;
  const v = (x: string | undefined) => (x ?? '').replace(/^[?$]/, '');
  const s = v(g.subject);
  const o = v(g.object);
  const p = v(g.predicate);
  if (!vars.includes(s) || !vars.includes(o)) return null;
  return { s, p: vars.includes(p) ? p : '', o };
}

/** Who wrote the final query, such as `local · qwen3:8b`. */
export function answeredBy(u: AskUsage | undefined): string | null {
  const a = u?.answeredBy;
  return a ? `${a.provider} · ${a.model}` : null;
}

/** The routing record in lines, for the tooltip of the model name. */
export function routingLines(u: AskUsage | undefined): string[] {
  const out: string[] = [];
  for (const s of u?.steps ?? []) {
    const tail = [s.outcome, s.latencyMs != null ? `${fmtInt(s.latencyMs)} ms` : '', s.signal ?? '']
      .filter(Boolean)
      .join(', ');
    out.push(`${s.role}: ${s.provider} · ${s.model} (${tail})`);
  }
  for (const e of u?.escalations ?? [])
    out.push(`${e.role} moved from ${e.from.model} to ${e.to.model} (${e.signal})`);
  return out;
}

/** The server-side history in the form of the local Asked list. */
export function askedFromRecords(records: AskRecord[]): (Asked & { id: string })[] {
  return records
    .filter((r) => r.question && r.query)
    .map((r) => ({ id: r.id, question: r.question, query: r.query!, at: r.at }));
}
