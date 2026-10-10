// C18 Phase 3 in the mock: the review inbox `GET /$/memory/{ds}/inbox`, the branch
// review `GET /$/memory/{ds}/review/{branch}`, the reviewer's actions (promote, reject,
// relink and edit) and the ingest settings and profiles of `/$/ingest/{ds}/…`. The data
// is canned for the `org` dataset and changes as the actions run. Nothing is checked
// against the store: the server's own tests cover the semantics.

import { isAdmin } from './memory.mjs';

const RES = 'http://example.org/resource/';
const EX = 'http://example.org/ontology#';
const NOTES = 'https://example.org/notes/';
export const INGEST_BRANCH = 'ingest.standup-2026-10-08-1';
const RENDITION = 'urn:x-sparkles:rendition:standup-2026-10-08';
const STANDUP =
  '# Stand-up\n\nAna moved to the payments team this week. Kai is out until Friday. ' +
  'The checkout redesign ships on 14 October.\n';

const iri = (v) => `<${v}>`;
/** The span of a passage of the stand-up notes, in code points. */
const spanOf = (passage) => {
  const start = Array.from(STANDUP.slice(0, STANDUP.indexOf(passage))).length;
  return { rendition: iri(RENDITION), start, end: start + Array.from(passage).length };
};
const shortIri = (v) =>
  v.startsWith(RES)
    ? `res:${v.slice(RES.length)}`
    : v.startsWith(EX)
      ? `ex:${v.slice(EX.length)}`
      : v;

function fact(s, p, o, graph, extra = {}) {
  const isIri = !o.startsWith('"');
  return {
    s: iri(s),
    p: iri(p),
    o: isIri ? iri(o) : o,
    graph: iri(graph),
    shown: { s: shortIri(s), p: shortIri(p), o: isIri ? shortIri(o) : o, graph },
    status: 'unreviewed',
    reifiers: [`<urn:uuid:${Math.random().toString(16).slice(2)}>`],
    ...extra,
  };
}

const pass = { span: 'pass', link: 'pass', guard: 'pass', corroboration: 'none' };

function seed() {
  const session = `${NOTES}2026-10-09`;
  return {
    keepText: true,
    profiles: {
      people: {
        classes: [`${EX}Person`, `${EX}Team`],
        predicates: [`${EX}memberOf`],
        language: 'en',
      },
    },
    target: 'https://example.org/memory/consolidated',
    sessions: [
      {
        graph: iri(session),
        shown: session,
        by: ['agent-7'],
        first: '2026-10-09T10:02:11Z',
        last: '2026-10-09T10:04:40Z',
        facts: [
          fact(`${RES}kai`, `${EX}worksOn`, `${RES}proj-17`, session, {
            sLabel: 'Kai Ito',
            oLabel: 'Checkout redesign',
            confidence: '0.92',
            quote: 'Kai picks up the checkout redesign.',
            signals: pass,
            passes: true,
          }),
          fact(`${RES}lea`, `${EX}worksOn`, `${RES}proj-17`, session, {
            sLabel: 'Lea Brandt',
            oLabel: 'Checkout redesign',
            confidence: '0.97',
            signals: { span: 'pass', link: 'fail', guard: 'pass', corroboration: 'pass' },
            candidates: [
              { iri: `${RES}lea`, shown: 'res:lea', label: 'Lea Brandt' },
              { iri: `${RES}lea-b`, shown: 'res:lea-b', label: 'Lea Brandt' },
            ],
          }),
          fact(`${RES}ana`, `${EX}startDate`, '"2025-11-17"', session, {
            sLabel: 'Ana Lima',
            confidence: '0.55',
            signals: { span: 'none', link: 'pass', guard: 'pass', corroboration: 'none' },
          }),
        ],
      },
    ],
    branches: [
      {
        name: INGEST_BRANCH,
        kind: 'ingest',
        ahead: 2,
        behind: 0,
        note: 'Stand-up notes of 2026-10-08',
        creator: 'agent-7',
      },
    ],
    review: {
      [INGEST_BRANCH]: {
        kind: 'ingest',
        note: 'Stand-up notes of 2026-10-08',
        creator: 'agent-7',
        rejected: 0,
        facts: [
          fact(`${RES}ana`, `${EX}memberOf`, `${RES}payments-new`, `${NOTES}2026-10-08`, {
            sLabel: 'Ana Lima',
            oLabel: 'Payments',
            status: 'proposed',
            confidence: '0.90',
            quote: 'Ana moved to the payments team this week.',
            span: spanOf('Ana moved to the payments team this week.'),
            signals: pass,
          }),
          fact(`${RES}proj-17`, `${EX}dueDate`, '"14 October"', `${NOTES}2026-10-08`, {
            sLabel: 'Checkout redesign',
            status: 'proposed',
            confidence: '0.70',
            quote: 'The checkout redesign ships on 14 October.',
            span: spanOf('The checkout redesign ships on 14 October.'),
            signals: { span: 'pass', link: 'pass', guard: 'fail', corroboration: 'none' },
            notes: ['ex:dueDate needs an xsd:date.'],
          }),
        ],
        retracts: [
          fact(`${RES}ana`, `${EX}memberOf`, `${RES}platform`, `${NOTES}2026-10-01`, {
            sLabel: 'Ana Lima',
            oLabel: 'Platform team',
            status: 'reviewed',
          }),
        ],
        entities: [
          {
            iri: `${RES}payments-new`,
            shown: 'res:payments-new',
            label: 'Payments',
            types: ['ex:Team'],
            candidates: [{ iri: `${RES}payments`, shown: 'res:payments', label: 'Payments team' }],
          },
        ],
        sources: [
          {
            rendition: iri(RENDITION),
            source: `${NOTES}2026-10-08`,
            title: 'Stand-up notes of 2026-10-08',
            format: 'text/markdown',
            length: Array.from(STANDUP).length,
            text: STANDUP,
          },
        ],
      },
    },
  };
}

const state = new Map();
const of = (name) => {
  if (!state.has(name)) state.set(name, name === 'org' ? seed() : emptyState());
  return state.get(name);
};
const emptyState = () => ({
  keepText: true,
  profiles: {},
  sessions: [],
  branches: [],
  review: {},
});

const same = (a, b) => a.s === b.s && a.p === b.p && a.o === b.o && a.graph === b.graph;

function inbox(name, st) {
  const sessions = st.sessions.filter((s) => s.facts.length);
  const branches = st.branches.map((b) => {
    const r = st.review[b.name];
    return r ? { ...b, facts: r.facts.length, retracts: r.retracts.length } : b;
  });
  return {
    dataset: name,
    ...(st.target ? { target: st.target } : {}),
    agentGraphs: [`${NOTES}*`],
    sessions,
    branches,
    open: sessions.reduce((n, s) => n + s.facts.length, 0) + branches.length,
    truncated: false,
  };
}

let reviewSeq = 0;

// --- ingestion tasks (C18 Phase 4) -------------------------------------------------------
//
// The mock decides a task's course from its file name: `scanned` in a PDF's name needs
// OCR on page 3, `big` in a document's name waits for a confirmation, and a CSV gets a
// mapping draft. Tasks end at once, so the UI's polling sees their final state.

/** The report a PDF ingestion registers: three pages, a fact on page 2. */
const REPORT =
  '<!-- Page 1 -->\n\nQuarterly report of the engineering teams.\n\n' +
  '<!-- Page 2 -->\n\nKai Berg leads the platform team.\n\n' +
  '<!-- Page 4 -->\n\nAcme Corp has three teams.\n';
const REPORT_RENDITION = 'urn:x-sparkles:rendition:report';

function pages(text) {
  const out = [];
  let offset = 0;
  for (const line of text.split(/(?<=\n)/)) {
    const m = /^<!-- Page (\d+) -->/.exec(line);
    if (m) out.push({ page: Number(m[1]), start: offset });
    offset += Array.from(line).length;
  }
  return out;
}

/** The file name and the text fields of a multipart body. */
function multipart(buf, contentType) {
  const boundary = /boundary=(?:"([^"]+)"|([^;]+))/.exec(contentType ?? '');
  const out = { fields: {}, file: null };
  if (!boundary) return out;
  const b = `--${boundary[1] ?? boundary[2]}`;
  for (const part of buf.toString('latin1').split(b)) {
    const [head, ...rest] = part.split('\r\n\r\n');
    const name = /name="([^"]*)"/.exec(head ?? '')?.[1];
    if (!name) continue;
    const body = rest.join('\r\n\r\n').replace(/\r\n$/, '');
    const file = /filename="([^"]*)"/.exec(head)?.[1];
    if (file != null) out.file = { name: file, size: body.length };
    else out.fields[name] = body;
  }
  return out;
}

let taskSeq = 0;

function newTask(name, input, status, extra = {}) {
  const now = new Date().toISOString();
  return {
    id: `task${(++taskSeq).toString().padStart(12, '0')}`,
    dataset: name,
    status,
    progress: status === 'done' ? 1 : 0.2,
    createdAt: now,
    updatedAt: now,
    input,
    usage: { modelCalls: 1, inputTokens: 900, outputTokens: 120 },
    ...extra,
  };
}

function reportReview(st, branch, omitted) {
  const quote = 'Kai Berg leads the platform team.';
  const start = Array.from(REPORT.slice(0, REPORT.indexOf(quote))).length;
  st.branches.push({ name: branch, kind: 'ingest', ahead: 2, behind: 0, note: 'Report' });
  st.review[branch] = {
    kind: 'ingest',
    note: 'Ingestion of report.pdf',
    rejected: 0,
    facts: [
      fact(`${RES}kai-berg`, `${EX}memberOf`, `${RES}platform`, 'urn:x-sparkles:report', {
        sLabel: 'Kai Berg',
        oLabel: 'Platform team',
        status: 'proposed',
        confidence: '0.90',
        quote,
        span: { rendition: iri(REPORT_RENDITION), start, end: start + Array.from(quote).length },
        signals: pass,
      }),
    ],
    retracts: [],
    entities: [],
    sources: [
      {
        rendition: iri(REPORT_RENDITION),
        source: 'urn:x-sparkles:report',
        title: 'report.pdf',
        format: 'application/pdf',
        length: Array.from(REPORT).length,
        text: REPORT,
        pages: pages(REPORT),
        ...(omitted ? { omittedPages: [3] } : {}),
      },
    ],
  };
}

async function handleTasks(
  req,
  res,
  name,
  st,
  id,
  action,
  { send, fail, readBody, createBranch, ds },
) {
  st.tasks ??= [];
  if (!id) {
    if (req.method === 'GET')
      return (
        send(res, 200, {
          dataset: name,
          tasks: st.tasks.slice().reverse(),
          capabilities: { pdf: true, ocr: false },
        }),
        true
      );
    if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
    const { fields, file } = multipart(await readBody(req), req.headers['content-type']);
    if (!file) return (fail(res, 400, 'send a file part', { code: 'bad-argument' }), true);
    const mode = fields.mode ?? 'branch';
    const input = { name: file.name, bytes: file.size, mode };
    const lower = file.name.toLowerCase();
    let t;
    if (lower.endsWith('.csv')) {
      t = newTask(name, input, 'done', {
        result: {
          outcome: 'mapping-draft',
          format: 'csv',
          file: file.name,
          base: 'http://example.org/people/',
          rows: 3,
          triples: 6,
          columns: [
            { name: 'id', title: 'id' },
            { name: 'team', title: 'team' },
          ],
          mapping: {
            '@context': 'http://www.w3.org/ns/csvw',
            tableSchema: { aboutUrl: 'http://example.org/people/{id}', columns: [] },
          },
          preview: { rows: 3, triples: ['<http://example.org/people/1> <x> <y> .'] },
          drafted: 'model',
        },
      });
    } else if (
      lower.endsWith('.pdf') &&
      lower.includes('scanned') &&
      fields.allowPartial !== 'true'
    ) {
      t = newTask(name, input, 'failed', {
        message:
          "1 of the PDF's 4 pages has no usable text: OCR is needed, and this server has none",
        error: {
          code: 'needs-ocr',
          message:
            "1 of the PDF's 4 pages has no usable text: OCR is needed, and this server has none",
          pages: [{ page: 3, reasons: ['scanned'] }],
        },
      });
    } else if (lower.includes('big') && fields.confirm !== 'true') {
      t = newTask(name, input, 'awaiting-confirmation', {
        message: "the estimate is above the dataset's threshold: confirm to go on",
        estimate: {
          chunks: 40,
          inputTokens: 240000,
          outputTokens: 30000,
          tokens: 270000,
          estimatedCost: 0.3,
          threshold: 200000,
          needsConfirmation: true,
        },
      });
    } else {
      const pdf = lower.endsWith('.pdf');
      const branch = pdf ? `ingest.report-${st.tasks.length + 1}` : INGEST_BRANCH;
      if (pdf) {
        reportReview(st, branch, lower.includes('scanned'));
        createBranch?.(ds, { name: branch, note: 'Ingestion of report.pdf' });
      }
      const result = {
        outcome: mode === 'preview' ? 'preview' : 'proposed',
        mode,
        format: pdf ? 'pdf' : 'markdown',
        proposed: pdf ? 1 : 2,
        entities: { linked: 1, new: 1, ambiguous: 0 },
        failed: [],
        ...(mode === 'preview' ? {} : { branch }),
        ...(pdf ? { pages: pages(REPORT) } : {}),
        ...(pdf && lower.includes('scanned') ? { omittedPages: [3] } : {}),
      };
      t = newTask(name, input, mode === 'preview' ? 'awaiting-approval' : 'done', { result });
    }
    st.tasks.push(t);
    return (send(res, 202, t), true);
  }
  const t = st.tasks.find((x) => x.id === id);
  if (!t) return (fail(res, 404, `no ingestion task "${id}"`, { code: 'unknown-task' }), true);
  if (req.method === 'GET' && !action) return (send(res, 200, t), true);
  if (req.method === 'DELETE' && !action) {
    t.status = 'cancelled';
    return (send(res, 202, t), true);
  }
  if (req.method === 'POST' && action === 'confirm') {
    if (t.status !== 'awaiting-confirmation')
      return (fail(res, 409, 'not awaiting confirmation', { code: 'not-waiting' }), true);
    Object.assign(t, {
      status: 'done',
      progress: 1,
      message: undefined,
      result: {
        outcome: 'proposed',
        format: 'markdown',
        branch: INGEST_BRANCH,
        proposed: 2,
        entities: { linked: 1, new: 1, ambiguous: 0 },
        failed: [],
      },
    });
    return (send(res, 200, t), true);
  }
  if (req.method === 'POST' && action === 'approve') {
    if (t.status !== 'awaiting-approval')
      return (fail(res, 409, 'not a preview', { code: 'not-waiting' }), true);
    t.status = 'done';
    t.result = { ...t.result, outcome: 'approved' };
    return (send(res, 200, t), true);
  }
  return (fail(res, 405, 'method not allowed'), true);
}

/**
 * Handle `/$/memory/{ds}/{inbox,review,promote,reject,relink,edit}` and
 * `/$/ingest/{ds}/…`. Returns true when the request was answered.
 */
export async function handleReview(
  req,
  res,
  seg,
  { datasets, send, fail, readBody, createBranch },
) {
  if (seg[0] !== '$') return false;
  const [, what, name, action, arg] = seg;
  const memoryRoute =
    what === 'memory' &&
    ['inbox', 'review', 'promote', 'reject', 'relink', 'edit'].includes(action);
  if (!memoryRoute && what !== 'ingest') return false;
  const ds = datasets.get(name);
  if (!ds) return (fail(res, 404, `No such dataset: ${name}`), true);
  const st = of(name);
  const body = async () => {
    try {
      return JSON.parse((await readBody(req)).toString('utf8') || '{}');
    } catch {
      return null;
    }
  };

  if (what === 'ingest' && !['profiles', 'settings'].includes(action))
    return handleTasks(req, res, name, st, action, arg, { send, fail, readBody, createBranch, ds });
  if (what === 'ingest') {
    if (req.method !== 'GET' && !isAdmin(req)) return (fail(res, 403, 'admin access needed'), true);
    if (action === 'profiles' && !arg && req.method === 'GET')
      return (
        send(res, 200, { dataset: name, keepText: st.keepText, profiles: st.profiles }), true
      );
    if (action === 'settings' && req.method === 'PUT') {
      const b = await body();
      if (typeof b?.keepText !== 'boolean')
        return (fail(res, 400, 'expected `keepText`', { code: 'bad-request' }), true);
      st.keepText = b.keepText;
      return (send(res, 200, { keepText: st.keepText }), true);
    }
    if (action === 'profiles' && arg) {
      if (req.method === 'GET') return (send(res, 200, st.profiles[arg] ?? {}), true);
      if (req.method === 'PUT') {
        const b = await body();
        if (!b) return (fail(res, 400, 'expected a profile', { code: 'bad-request' }), true);
        st.profiles[arg] = b;
        return (send(res, 200, b), true);
      }
      if (req.method === 'DELETE') {
        delete st.profiles[arg];
        res.writeHead(204, { 'Access-Control-Allow-Origin': '*' });
        res.end();
        return true;
      }
    }
    return (fail(res, 405, 'method not allowed'), true);
  }

  if (req.method === 'GET' && action === 'inbox') return (send(res, 200, inbox(name, st)), true);
  if (req.method === 'GET' && action === 'review' && arg) {
    const r = st.review[arg];
    if (!r && !ds.branchSet?.has(arg))
      return (fail(res, 404, `no such branch: ${arg}`, { code: 'no-such-branch' }), true);
    const b = st.branches.find((x) => x.name === arg);
    return (
      send(res, 200, {
        dataset: name,
        branch: arg,
        ...(b ? { ahead: b.ahead, behind: b.behind } : {}),
        ...(r ?? { facts: [], retracts: [], entities: [], sources: [] }),
      }),
      true
    );
  }
  if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
  if (!isAdmin(req)) return (fail(res, 403, 'write access needed'), true);
  const b = await body();
  if (!b) return (fail(res, 400, 'expected a JSON body', { code: 'bad-request' }), true);

  if (action === 'promote') {
    const target = b.target ?? st.target;
    if (!target) return (fail(res, 400, 'no target graph', { code: 'no-target' }), true);
    const facts = b.facts ?? [];
    const branch = `review.local.20261010-${++reviewSeq}`;
    createBranch?.(ds, { name: branch, note: 'Promoted by local' });
    for (const s of st.sessions) s.facts = s.facts.filter((f) => !facts.some((x) => same(f, x)));
    st.branches.push({ name: branch, kind: 'review', ahead: 1, behind: 0, creator: 'local' });
    return (
      send(res, 200, {
        dataset: name,
        branch,
        target,
        promoted: facts.length,
        committed: true,
        commit: 1,
      }),
      true
    );
  }
  if (action === 'reject') {
    const facts = b.facts ?? [];
    if (b.branch) {
      const r = st.review[b.branch];
      if (r) {
        r.facts = r.facts.filter((f) => !facts.some((x) => same(f, x)));
        r.rejected = (r.rejected ?? 0) + facts.length;
      }
    } else {
      for (const s of st.sessions) s.facts = s.facts.filter((f) => !facts.some((x) => same(f, x)));
    }
    return (
      send(res, 200, {
        dataset: name,
        branch: b.branch ?? 'main',
        rejected: facts.length,
        commits: [],
      }),
      true
    );
  }
  if (action === 'relink') {
    const r = st.review[b.branch];
    if (!r)
      return (fail(res, 404, `no such branch: ${b.branch}`, { code: 'no-such-branch' }), true);
    const ent = r.entities.find((e) => e.iri === b.from);
    r.entities = r.entities.filter((e) => e.iri !== b.from);
    for (const f of r.facts) {
      if (f.o === iri(b.from)) {
        f.o = iri(b.to);
        f.shown = { ...f.shown, o: shortIri(b.to) };
        f.oLabel = ent?.candidates.find((c) => c.iri === b.to)?.label ?? f.oLabel;
      }
    }
    return (send(res, 200, { dataset: name, branch: b.branch, committed: true }), true);
  }
  if (action === 'edit') {
    const r = b.branch ? st.review[b.branch] : null;
    const list = r ? r.facts : st.sessions.flatMap((s) => s.facts);
    const f = list.find((x) => same(x, b.fact ?? {}));
    if (!f) return (fail(res, 404, 'no such fact', { code: 'no-such-fact' }), true);
    f.o = b.o;
    f.shown = { ...f.shown, o: b.o };
    f.signals = { ...f.signals, guard: 'pass' };
    delete f.notes;
    return (send(res, 200, { dataset: name, branch: b.branch, committed: true }), true);
  }
  return (fail(res, 404, 'not found'), true);
}
