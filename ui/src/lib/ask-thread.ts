// The Ask thread (C18 §6.2): the turns of one conversation with the server's assistant,
// what each turn shows of the work behind its answer, the context a page lends a
// question, and the threads of the server's history. The state that the drawer and the
// page share lives in `ask-thread.svelte.ts`; this module is its pure part.

import type {
  AskAbout,
  AskClarify,
  AskEvent,
  AskGlobal,
  AskRecord,
  AskResult,
  AskStoredAnswer,
  AskSummary,
  AskUsage,
  CheckResult,
  Diagnosis,
  Feedback,
} from './ask-api';
import { askFailure, MAX_TURNS, nextStep, type Turn } from './ask';
import { fmtInt } from './format';

/** What a page lends the questions asked from it, shown as a removable chip. */
/**
 * What a page lends a question: the resource or query it shows, and the branch and state
 * (`at`) it reads, which the question reads too.
 */
export type AskContextChip = { branch?: string | null; at?: string | null } & (
  | {
      kind: 'entity';
      iri: string;
      label: string;
      /** class, property, individual, concept or resource, for the suggestions */
      type?: 'class' | 'property' | 'individual' | 'concept' | 'resource';
    }
  | { kind: 'query'; label: string; question?: string; query: string }
  | { kind: 'commit'; label: string; commit: number }
);

/** One step of the work as it lands, for the rail of a running turn. */
export type TrailStep = { key: string; name: string; detail: string; ok: boolean };

export type TurnStatus = 'running' | 'done' | 'failed' | 'stopped' | 'stored';

export type ThreadTurn = {
  /** A key for the list, stable before the server's id arrives. */
  key: string;
  /** The server's ask id, from the usage event or the record. */
  id?: string;
  question: string;
  chip?: AskContextChip;
  /** The branch the turn read, when it was not main. */
  branch?: string;
  status: TurnStatus;
  /** The step the server is on while the turn runs. */
  step?: string;
  trail: TrailStep[];
  result?: AskResult;
  check?: CheckResult;
  summary?: AskSummary;
  global?: AskGlobal;
  clarify?: AskClarify;
  usage?: AskUsage;
  diagnosis?: Partial<Diagnosis> & { text?: string };
  failure?: string;
  mode?: 'local' | 'global';
  feedback?: Feedback;
  /** The linked entities and the communities of the ground event. */
  entities?: string[];
  communities?: string[];
  /** When the turn was asked, RFC 3339. */
  at?: string;
  /** The record's commit, for Show rows. */
  commit?: number;
  /** The record's answer was withheld from this reader (C18 §6.4). */
  withheld?: boolean;
  /** The turn's rows were asked for again, from its stored query. */
  rerun?: 'loading' | 'done' | 'failed';
  /** The rows were left out when the thread was kept for a reload. */
  rowsDropped?: boolean;
};

export type Thread = { id?: string; turns: ThreadTurn[] };

let seq = 0;
export const turnKey = () => `t${Date.now().toString(36)}${(seq++).toString(36)}`;

/** The `about` of a request for a chip: only an entity names a resource. */
export function chipAbout(c: AskContextChip | null | undefined): AskAbout | undefined {
  if (c?.kind !== 'entity') return undefined;
  return { iri: c.iri, label: c.label.slice(0, 200), ...(c.type ? { kind: c.type } : {}) };
}

/** The chip's words: `About mitochondrion`, `About the query in this tab`, `As of commit 18`. */
export function chipText(c: AskContextChip): string {
  switch (c.kind) {
    case 'entity':
      return `About ${c.label}`;
    case 'query':
      return `About ${c.label}`;
    case 'commit':
      return `As of commit ${fmtInt(c.commit)}`;
  }
}

/** The questions a chip suggests, at most three. */
export function chipSuggestions(c: AskContextChip | null | undefined): string[] {
  if (!c) return [];
  if (c.kind === 'query') return ['Explain this query', 'Add the labels', 'Count the results'];
  if (c.kind === 'commit') return [];
  switch (c.type) {
    case 'class':
      return ['What are its subclasses?', 'What is it part of?', 'Which instances does it have?'];
    case 'property':
      return ['Which entities use it?', 'What values does it take?'];
    case 'concept':
      return ['Which concepts are narrower?', 'Which concepts are related?'];
    default:
      return ['What is connected to it?', 'Summarize what is known about it'];
  }
}

/**
 * The earlier turns a question carries: the chip's query when the chip is one, then the
 * thread's answered turns with their queries, the last five.
 */
export function threadContext(turns: ThreadTurn[], chip?: AskContextChip | null): Turn[] {
  const out: Turn[] = [];
  if (chip?.kind === 'query' && chip.query.trim())
    out.push({ question: chip.question || 'The query on screen', query: chip.query });
  for (const t of turns) {
    const q = t.result?.query;
    if (!q?.trim() || t.status === 'running') continue;
    // the chip's query may be a turn of the thread opened in a tab
    const same = out.findIndex((x) => x.query === q);
    if (same >= 0) out.splice(same, 1);
    out.push({ question: t.question, query: q });
  }
  return out.slice(-MAX_TURNS);
}

const plural = (n: number, one: string, many = `${one}s`) => `${fmtInt(n)} ${n === 1 ? one : many}`;

/** Fold one event into a running turn: its parts, its trail and the step it is on. */
export function reduceTurn(t: ThreadTurn, e: AskEvent, o: { run: boolean; summary: boolean }) {
  const step = nextStep(e, o);
  if (step) t.step = step;
  const land = (key: string, name: string, detail: string, ok = true) => {
    const i = t.trail.findIndex((s) => s.key === key);
    const s = { key, name, detail, ok };
    if (i >= 0) t.trail[i] = s;
    else t.trail.push(s);
  };
  switch (e.event) {
    case 'ground': {
      const ents = Array.isArray(e.data.entities) ? (e.data.entities as GroundEntity[]) : [];
      const exact = ents.filter((x) => x.verdict === 'exact');
      t.entities = exact.map((x) => x.candidates?.[0]?.iri).filter((x): x is string => !!x);
      if (Array.isArray(e.data.communities))
        t.communities = (e.data.communities as unknown[]).filter(
          (x): x is string => typeof x === 'string',
        );
      const names = exact
        .map((x) => x.candidates?.[0]?.label || x.text)
        .filter(Boolean)
        .slice(0, 3);
      land(
        'ground',
        'Entities',
        names.length
          ? `${names.join(', ')}${exact.length > names.length ? ' and more' : ''}, linked`
          : ents.length
            ? plural(ents.length, 'mention') + ', none linked exactly'
            : 'none named',
      );
      break;
    }
    case 'escalate':
      land(`escalate-${t.trail.length}`, 'Escalated', `${e.data.role} to ${e.data.to.model}`);
      break;
    case 'draft':
      land(
        'query',
        'Query',
        `drafted by ${e.data.provider} · ${e.data.model}${e.data.attempt > 1 ? `, attempt ${e.data.attempt}` : ''}`,
      );
      t.result = {
        ...(t.result ?? {}),
        query: e.data.query,
        explanation: e.data.explanation,
        assumptions: e.data.assumptions,
        graph: e.data.graph,
      };
      break;
    case 'check':
      t.check = e.data;
      land('query', 'Query', checkWords(e.data), e.data.ok);
      break;
    case 'run':
      land(
        'rows',
        'Rows',
        e.data.error ? 'the run failed' : plural(e.data.rows ?? 0, 'row'),
        !e.data.error,
      );
      break;
    case 'diagnosis':
      t.diagnosis = e.data;
      land('diagnosis', 'Diagnosis', e.data.text ?? e.data.message ?? 'why nothing matched');
      break;
    case 'clarify':
      t.clarify = e.data;
      land('clarify', 'Clarify', 'a choice is needed');
      break;
    case 'result':
      t.result = { ...(t.result ?? {}), ...e.data };
      if (e.data.results) land('rows', 'Rows', plural(e.data.results.rows?.length ?? 0, 'row'));
      break;
    case 'summary':
      t.summary = e.data;
      land('summary', 'Summary', `${e.data.provider ?? ''} · ${e.data.model ?? ''}`.trim());
      t.step = 'Finishing';
      break;
    case 'reports':
      land('reports', 'Reports', plural(e.data.reports.length, 'community report'));
      break;
    case 'global':
      t.global = e.data;
      t.mode = 'global';
      break;
    case 'usage':
      t.id = e.data.askId ?? t.id;
      t.usage = e.data;
      if (!t.global && e.data.outcome !== 'error') t.mode = 'local';
      break;
    case 'error':
      t.failure = askFailure(e.data.code, e.data.message, e.data.resetAt);
      if (e.data.result) t.result = { ...(t.result ?? {}), ...e.data.result };
      break;
  }
}

type GroundEntity = {
  text?: string;
  verdict?: string;
  candidates?: { iri?: string; label?: string }[];
};

function checkWords(c: CheckResult): string {
  const errors = c.issues.filter((i) => i.severity === 'error').length;
  const warnings = c.issues.length - errors;
  if (!errors && !warnings) return 'checked, no issues';
  return [errors ? plural(errors, 'error') : '', warnings ? plural(warnings, 'warning') : '']
    .filter(Boolean)
    .join(', ');
}

/** The answer's text: the summary, the explanation of a preview, or nothing. */
export function answerText(t: ThreadTurn): string | null {
  return t.summary?.text ?? null;
}

/** The one-line summaries of a finished turn's folds. */
export function foldLines(t: ThreadTurn): {
  query: string | null;
  rows: string | null;
  sources: string | null;
  steps: string;
} {
  const q = t.result?.query;
  const rows = t.result?.results;
  const nRows = rows?.rows?.length ?? rows?.triples?.length;
  const cited = new Set(t.summary?.citations ?? []).size;
  const reports = t.global?.reports.length ?? 0;
  const src: string[] = [];
  if (cited) src.push(`${plural(cited, 'cited row')}`);
  if (reports) src.push(plural(reports, 'community report'));
  else if (t.communities?.length) src.push(plural(t.communities.length, 'community'));
  if (t.entities?.length) src.push(plural(t.entities.length, 'linked entity', 'linked entities'));
  const steps = t.usage?.steps ?? [];
  const ms = steps.reduce((n, s) => n + (s.latencyMs ?? 0), 0);
  const roles = [...new Set(steps.map((s) => s.role))];
  return {
    query: q ? (t.check ? checkWords(t.check) : 'the final query') : null,
    rows:
      nRows != null
        ? `${plural(nRows, 'row')}${t.result?.commit != null ? ` at commit ${fmtInt(t.result.commit)}` : ''}`
        : q && (t.status === 'stored' || t.rowsDropped)
          ? t.commit != null
            ? `not stored, run again at commit ${fmtInt(t.commit)}`
            : 'not stored, run again'
          : q && t.status === 'done'
            ? 'not run, preview only'
            : null,
    sources: src.length ? src.join(', ') : null,
    steps: steps.length
      ? `${roles.join(', ')}${ms ? ` in ${(ms / 1000).toFixed(1)} s` : ''}`
      : t.trail.length
        ? plural(t.trail.length, 'step')
        : 'no steps recorded',
  };
}

// --- the server's history ----------------------------------------------------------

/** The `about` suffix that a request renders into the question, for old records. */
const ABOUT_SUFFIX = / \(about <[^<>\s]+>\)$/;

/** A record as a stored turn of a reopened thread. */
export function turnFromRecord(r: AskRecord): ThreadTurn {
  const a: AskStoredAnswer = r.answer ?? {};
  const t: ThreadTurn = {
    key: r.id,
    id: r.id,
    question: r.question.replace(ABOUT_SUFFIX, ''),
    status: 'stored',
    trail: [],
    at: r.at,
    commit: r.commit,
    ...(r.branch ? { branch: r.branch } : {}),
    mode: r.mode === 'global' ? 'global' : 'local',
    withheld: r.answerWithheld === true,
    feedback: r.outcome === 'none' ? undefined : r.outcome,
    entities: a.entities,
    communities: a.communities,
    result: {
      query: r.query,
      explanation: a.explanation,
      assumptions: a.assumptions,
      graph: a.graph ?? null,
      commit: r.commit,
      verdict: a.verdict,
    },
  };
  if (r.about) t.chip = { kind: 'entity', iri: r.about.iri, label: r.about.label ?? r.about.iri };
  if (a.summary)
    t.summary = {
      text: a.summary.text,
      citations: a.summary.citations ?? [],
      rowsSent: a.summary.rowsSent,
      provider: a.summary.provider,
      model: a.summary.model,
      uncited: a.summary.uncited,
    };
  if (a.global)
    t.global = {
      points: (a.global.points ?? []).map((p) => ({
        n: p.n,
        text: p.text,
        score: 0,
        reports: p.reports ?? [],
        findings: [],
      })),
      reports: (
        a.global.reports ?? (r.reports ?? []).map((iri) => ({ iri, title: undefined }))
      ).map(({ iri, title }) => ({
        iri,
        title: title ?? iri,
        level: 0,
        status: '',
        findings: [],
      })),
      global: {},
    };
  if (a.diagnosis) t.diagnosis = { text: a.diagnosis, verdict: a.verdict };
  if (a.clarify) t.clarify = { id: '', ...a.clarify };
  if (a.error) t.failure = askFailure(a.error.code ?? '', a.error.message);
  const routing = r.routing as { steps?: AskUsage['steps']; answeredBy?: AskUsage['answeredBy'] };
  t.usage = {
    askId: r.id,
    steps: routing?.steps ?? [],
    answeredBy: routing?.answeredBy,
    notes: a.notes,
    outcome: r.result,
  };
  return t;
}

/** A thread of the history list: its first question, its turns and when it was last asked. */
export type ThreadSummary = {
  id: string;
  title: string;
  turns: number;
  at: string;
  /** The last feedback given in the thread, or `global` for an answer from reports. */
  note?: string;
};

/** The history's records grouped into threads, the most recent thread first. */
export function threadsFromRecords(records: AskRecord[]): ThreadSummary[] {
  const by = new Map<string, AskRecord[]>();
  // the history comes newest first, and records of one second keep the server's order
  for (const r of [...records].reverse()) {
    const id = r.thread ?? r.id;
    const list = by.get(id);
    if (list) list.push(r);
    else by.set(id, [r]);
  }
  const out: ThreadSummary[] = [];
  for (const [id, list] of by) {
    list.sort((a, b) => a.at.localeCompare(b.at));
    const first = list[0];
    const last = list[list.length - 1];
    const fb = [...list].reverse().find((r) => r.outcome !== 'none')?.outcome;
    out.push({
      id,
      title: first.question.replace(ABOUT_SUFFIX, ''),
      turns: list.length,
      at: last.at,
      note:
        fb === 'accepted'
          ? 'right'
          : fb === 'edited'
            ? 'edited'
            : fb === 'rejected'
              ? 'wrong'
              : list.some((r) => r.mode === 'global')
                ? 'from reports'
                : undefined,
    });
  }
  return out.reverse().sort((a, b) => b.at.localeCompare(a.at));
}

/** The day group of a thread in the list: Today, Yesterday or Earlier. */
export function dayGroup(at: string, now = new Date()): 'Today' | 'Yesterday' | 'Earlier' {
  const d = new Date(at);
  if (isNaN(d.getTime())) return 'Earlier';
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(now) - day(d)) / 86_400_000);
  return diff <= 0 ? 'Today' : diff === 1 ? 'Yesterday' : 'Earlier';
}

/** The line under a thread in the list: `2 questions, right`. */
export function threadLine(t: ThreadSummary): string {
  return [plural(t.turns, 'question'), t.note].filter(Boolean).join(', ');
}

/** The IRIs of a result's cited rows (1-based citations), at most `max`. */
export function citedIris(t: ThreadTurn, max = 10): string[] {
  const rows = t.result?.results?.rows;
  const out: string[] = [];
  if (!rows) return out;
  for (const n of t.summary?.citations ?? []) {
    for (const cell of rows[n - 1] ?? []) {
      if (cell?.type === 'uri' && !out.includes(cell.value)) out.push(cell.value);
      if (out.length >= max) return out;
    }
  }
  return out;
}

/** The entity Open in Explore focuses: a cited row's, a linked one, or the first IRI cell. */
export function exploreTarget(t: ThreadTurn): string | null {
  const cited = citedIris(t, 1)[0];
  if (cited) return cited;
  if (t.entities?.[0]) return t.entities[0];
  for (const row of t.result?.results?.rows ?? [])
    for (const cell of row) if (cell?.type === 'uri') return cell.value;
  return null;
}

/** The kind a chip suggests questions for, from an entity's kinds (lib/browse.ts). */
export function chipTypeOf(
  kinds: readonly string[],
): 'class' | 'property' | 'individual' | 'concept' | 'resource' {
  if (kinds.includes('class')) return 'class';
  if (kinds.some((k) => k === 'property' || k.endsWith('Property'))) return 'property';
  if (kinds.includes('concept')) return 'concept';
  if (kinds.includes('individual')) return 'individual';
  return 'resource';
}
