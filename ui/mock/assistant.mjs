// C18 Phase 2 in the mock: `POST /{ds}/ask` over server-sent events with a scripted
// pipeline, the assistant settings of `/$/assistant/{ds}`, the caller's history under
// `/$/asks/{ds}` and feedback. Only the `org` dataset has an assistant. Its draft list is
// `mock · small` then `mock · large`.
//
// The pipeline is scripted by words in the question:
// - "Ana" alone ends with a `clarify` event, until a clarification comes.
// - "repair" makes the small model draft an unknown class, so the check fails, the
//   diagnosis starts a repair, and the repair escalates to the large model.
// - "connected" drafts a SELECT with graph variables.
// - "provider" ends with `provider-unavailable`, and "budget" is refused with `429`.
// - anything else drafts the members of the payments team.
//
// A request body with the mock-only field `hold: "summary"` stops before the summary
// until the client goes away, so that the step indicator stays on Summarizing (the
// screenshots of `mise run docs:screenshots` use it).

import { randomUUID } from 'node:crypto';
import { checkQuery, mockPrincipal, isAdmin } from './memory.mjs';

const PREFIXES = `PREFIX ex: <http://example.org/ontology#>
PREFIX res: <http://example.org/resource/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
`;
const SMALL = { provider: 'mock', model: 'small' };
const LARGE = { provider: 'mock', model: 'large' };
const DRAFT = [SMALL, LARGE];

/** The settings of datasets that changed them, by name. */
const settings = {};
/** The history of each dataset, oldest first. */
const history = {};
/** The draft pair of each ask, for Try harder. */
const drafted = {};

const who = (req) => mockPrincipal(req) ?? 'local';

function settingsOf(name) {
  const s = settings[name] ?? (name === 'org' ? { enabled: true, send: 'rows' } : {});
  const enabled = !!s.enabled;
  const days = s.historyDays ?? 30;
  return {
    ...s,
    status: enabled
      ? {
          models: true,
          historyDays: days,
          ask: s.ask !== false,
          draft: DRAFT,
          summary: s.send !== 'schema',
        }
      : {
          models: true,
          historyDays: days,
          ask: false,
          reason: 'the dataset has no assistant (PUT /$/assistant/{ds} with enabled)',
        },
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function draftFor(question, pair, attempt, given) {
  if (given)
    return {
      query: given,
      explanation: 'The query as you edited it.',
      assumptions: [],
    };
  if (/repair/i.test(question) && pair === SMALL)
    return {
      query: `${PREFIXES}SELECT ?person WHERE { ?person a ex:Teem ; ex:memberOf res:payments }`,
      explanation: 'Finds the people of the payments team.',
      assumptions: [],
    };
  if (/connected/i.test(question))
    return {
      query: `${PREFIXES}SELECT ?person ?team WHERE { ?person ex:memberOf ?team }`,
      explanation: 'Draws who is a member of which team.',
      assumptions: [],
      graph: { subject: '?person', predicate: '', object: '?team' },
    };
  return {
    query: `${PREFIXES}SELECT ?person ?name WHERE { ?person ex:memberOf res:payments ; foaf:name ?name }`,
    explanation: `Finds the members of the payments team${attempt > 1 ? ', with the right class' : ''}.`,
    assumptions: ['"payments team" = res:payments'],
  };
}

async function runAsk(req, res, ds, body, { sparklesResult }) {
  const question = String(body.question ?? '').trim();
  const me = who(req);
  const id = randomUUID().replace(/-/g, '').slice(0, 16);
  const run = body.run !== false;
  const s = settingsOf(ds.name);
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    'Access-Control-Allow-Origin': '*',
  });
  let closed = false;
  res.on('close', () => (closed = true));
  const emit = async (event, data) => {
    if (closed) return;
    res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
    await sleep(40);
  };
  const steps = [];
  const escalations = [];
  let pos = 0;
  if (body.tryHarder) {
    const prev = drafted[body.tryHarder];
    pos = Math.min((prev ?? 0) + 1, DRAFT.length - 1);
    const h = (history[ds.name] ??= []).find((r) => r.id === body.tryHarder);
    if (h) h.outcome = 'rejected';
    escalations.push({
      role: 'draft',
      from: DRAFT[prev ?? 0],
      to: DRAFT[pos],
      signal: 'try-harder',
    });
    await emit('escalate', escalations.at(-1));
  }
  const finish = async (outcome, result) => {
    const pair = DRAFT[pos];
    drafted[id] = pos;
    const usage = {
      outcome,
      askId: id,
      inputTokens: 1200 * steps.length,
      outputTokens: 80 * steps.length,
      steps,
      escalations,
      draftPair: pos,
      tryHarder: !body.query && pos + 1 < DRAFT.length,
      ...(result?.query ? { answeredBy: { role: steps.at(-1)?.role ?? 'draft', ...pair } } : {}),
    };
    if (s.historyDays !== 0 && question)
      (history[ds.name] ??= []).push({
        id,
        principal: me,
        at: new Date().toISOString(),
        question,
        ...(result?.query ? { query: result.query } : {}),
        result: outcome,
        outcome: 'none',
        routing: { steps, escalations },
      });
    await emit('usage', usage);
    res.end();
  };
  if (!body.query) {
    await emit('ground', {
      examples: [],
      entities: [],
      classes: 2,
      predicates: 3,
    });
    if (/provider/i.test(question)) {
      steps.push({
        role: 'draft',
        ...SMALL,
        outcome: 'provider-unavailable',
        latencyMs: 5000,
      });
      await emit('error', {
        code: 'provider-unavailable',
        message: 'the provider did not answer',
      });
      return finish('error');
    }
    if (/\bAna\b/.test(question) && !/Lima|Souza/.test(question) && !body.clarification) {
      await emit('clarify', {
        id: 'mention',
        question: 'Which "Ana" do you mean?',
        choices: [
          {
            label: 'Ana Lima · ex:Person',
            value: 'http://example.org/resource/ana',
          },
          {
            label: 'Ana Souza · ex:Person',
            value: 'http://example.org/resource/ana-souza',
          },
        ],
      });
      return finish('clarify');
    }
  }
  let role = 'draft';
  let rolePos = pos;
  for (let attempt = 1; attempt <= 3; attempt++) {
    const pair = DRAFT[rolePos];
    const d = draftFor(question, pair, attempt, body.query);
    steps.push({
      role,
      ...pair,
      outcome: 'ok',
      latencyMs: 300 + 100 * attempt,
      inputTokens: 1200,
      outputTokens: 80,
    });
    if (!body.query)
      await emit('draft', {
        attempt,
        role,
        ...pair,
        level: 'json-schema',
        graph: null,
        ...d,
      });
    const check = checkQuery(ds, { query: d.query, terms: true });
    const bad = /ex:Teem/.test(d.query);
    if (bad) {
      check.ok = false;
      check.issues = [
        {
          code: 'unknown-class',
          severity: 'error',
          message: 'ex:Teem is not a class of this dataset.',
          term: 'ex:Teem',
          suggestions: [{ term: 'ex:Team', label: 'Team', count: 3, why: 'similar name' }],
        },
      ];
    }
    await emit('check', check);
    if (bad) {
      await emit('diagnosis', {
        attempt,
        kind: 'check',
        text: 'ex:Teem is not a class; did you mean ex:Team?',
      });
      if (role === 'repair' && rolePos + 1 < DRAFT.length) {
        escalations.push({
          role,
          from: DRAFT[rolePos],
          to: DRAFT[rolePos + 1],
          signal: 'check-failed',
        });
        await emit('escalate', escalations.at(-1));
        rolePos++;
        pos = rolePos;
      }
      role = 'repair';
      continue;
    }
    const result = {
      query: d.query,
      explanation: d.explanation,
      assumptions: d.assumptions,
      terms: check.terms ?? [],
      issues: check.issues,
      graph: d.graph ?? null,
      commit: check.commit,
      attempt,
      limitAdded: false,
    };
    if (!run) {
      await emit('result', result);
      return finish('checked', result);
    }
    const doc = sparklesResult(ds, d.query, Number(body.maxRows ?? 1000));
    const rows = doc.rows?.length ?? doc.triples?.length ?? 0;
    await emit('run', {
      attempt,
      commit: check.commit,
      rows,
      truncated: false,
      elapsedMs: 3,
    });
    await emit('result', { ...result, results: doc });
    if (rows && s.send !== 'schema' && body.summary !== false) {
      if (body.hold === 'summary') while (!closed) await sleep(100);
      steps.push({
        role: 'summarize',
        ...SMALL,
        outcome: 'ok',
        latencyMs: 200,
      });
      const first = doc.rows?.[0]?.find((t) => t?.type === 'literal')?.value ?? 'The first row';
      await emit('summary', {
        text: `${first} is in the answer [1]. There are ${rows} rows in all [1–${rows}].`,
        citations: Array.from({ length: rows }, (_, i) => i + 1),
        rowsSent: rows,
        ...SMALL,
      });
    }
    return finish(rows ? 'answered' : 'empty', result);
  }
  await emit('error', {
    code: 'no-valid-query',
    message: 'Sparkles could not write a valid query for this question.',
  });
  return finish('failed');
}

/** Handles the routes of C18 Phase 2; false when the request is not one of them. */
export async function handleAssistant(req, res, url, seg, ctx) {
  const { datasets, send, fail, readBody } = ctx;
  const jsonBody = async () => {
    try {
      return JSON.parse((await readBody(req)).toString('utf8') || '{}');
    } catch {
      return null;
    }
  };
  if (seg[0] === '$' && seg[1] === 'assistant' && seg[2]) {
    const ds = datasets.get(seg[2]);
    if (!ds) return (fail(res, 404, `No such dataset: ${seg[2]}`), true);
    if (req.method === 'GET') return (send(res, 200, settingsOf(ds.name)), true);
    if (req.method === 'PUT') {
      if (!isAdmin(req)) return (fail(res, 403, 'admin access needed'), true);
      const b = await jsonBody();
      if (!b) return (fail(res, 400, 'invalid JSON', { code: 'bad-settings' }), true);
      delete b.status;
      settings[ds.name] = b;
      return (send(res, 200, settingsOf(ds.name)), true);
    }
    return (fail(res, 405, 'method not allowed'), true);
  }
  if (seg[0] === '$' && seg[1] === 'asks' && seg[2]) {
    const ds = datasets.get(seg[2]);
    if (!ds) return (fail(res, 404, `No such dataset: ${seg[2]}`), true);
    const me = who(req);
    const list = (history[ds.name] ??= []);
    if (seg[3] && seg[4] === 'feedback' && req.method === 'POST') {
      const b = await jsonBody();
      if (!b || !['accepted', 'edited', 'rejected'].includes(b.outcome))
        return (
          fail(res, 400, 'outcome must be accepted, edited or rejected', {
            code: 'bad-argument',
          }),
          true
        );
      const r = list.find((x) => x.id === seg[3] && x.principal === me);
      if (!r)
        return (
          fail(res, 404, `no ask "${seg[3]}" of yours`, {
            code: 'unknown-ask',
          }),
          true
        );
      r.outcome = b.outcome;
      return (send(res, 204, ''), true);
    }
    if (seg[3]) return false;
    if (req.method === 'GET') {
      const mine = list.filter((r) => r.principal === me).reverse();
      const days = settingsOf(ds.name).status.historyDays;
      return (
        send(res, 200, {
          dataset: ds.name,
          historyDays: days,
          asks: days ? mine : [],
        }),
        true
      );
    }
    if (req.method === 'DELETE') {
      const id = url.searchParams.get('id');
      const before = list.length;
      history[ds.name] = list.filter((r) => r.principal !== me || (id != null && r.id !== id));
      if (id && history[ds.name].length === before)
        return (
          fail(res, 404, `no ask "${id}" in your history`, {
            code: 'unknown-ask',
          }),
          true
        );
      return (send(res, 204, ''), true);
    }
    return (fail(res, 405, 'method not allowed'), true);
  }
  if (seg.length === 2 && seg[1] === 'ask' && seg[0] !== '$') {
    const ds = datasets.get(seg[0]);
    if (!ds) return (fail(res, 404, `No such dataset: ${seg[0]}`), true);
    if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
    const b = await jsonBody();
    if (!b || typeof b.question !== 'string' || !b.question.trim())
      return (
        fail(res, 400, 'question must hold 1 to 2000 characters', {
          code: 'bad-argument',
        }),
        true
      );
    const s = settingsOf(ds.name);
    if (!s.status.ask) return (fail(res, 404, s.status.reason, { code: 'no-assistant' }), true);
    if (/budget/i.test(b.question)) {
      const reset = new Date(Date.now() + 86_400_000);
      reset.setUTCHours(0, 0, 0, 0);
      return (
        fail(res, 429, "this dataset's question budget for today is used up", {
          code: 'budget-exceeded',
          resetAt: reset.toISOString(),
        }),
        true
      );
    }
    if (b.tryHarder) {
      const prev = drafted[b.tryHarder];
      if (prev == null)
        return (fail(res, 404, 'no recent ask of yours', { code: 'unknown-ask' }), true);
      if (prev + 1 >= DRAFT.length)
        return (
          fail(res, 409, 'the draft role has no pair after the one that answered', {
            code: 'no-later-pair',
          }),
          true
        );
    }
    await runAsk(req, res, ds, b, ctx);
    return true;
  }
  return false;
}
