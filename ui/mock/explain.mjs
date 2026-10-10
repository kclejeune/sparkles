// C18 Phase 2b in the mock: `POST /{ds}/sparql/explain` with the notes and the template
// description of §6.6 computed from the given plan, and a query that a timeout stops
// (`# mock:timeout` in its text) with its partial plan in the error body.

/** The nodes of a plan in tree order, with ids, own time and parents. */
function flatten(plan) {
  const out = [];
  const walk = (n, id, parent) => {
    const node =
      n.operator && typeof n.operator === 'object' ? { ...n.operator, complete: n.complete } : n;
    const kids = n.children ?? [];
    const at = out.length;
    out.push({ id, node, parent, kids: [] });
    let childMs = 0;
    kids.forEach((c, i) => {
      const k = walk(c, `${id}.${i}`, at);
      out[at].kids.push(k);
      childMs += out[k].node.timeMs || 0;
    });
    out[at].self = Math.max(0, (node.timeMs || 0) - childMs);
    return at;
  };
  walk(plan, '0', null);
  return out;
}

const ms = (x) =>
  x >= 1000 ? `${(x / 1000).toFixed(x >= 10000 ? 0 : 1)} s` : `${x.toFixed(x >= 10 ? 0 : 1)} ms`;
const rows = (x) => Math.round(x).toLocaleString('en-US');
const label = (n) =>
  `${n.operator} ${String(n.description ?? '').replace(/\s*\[.*?\]/g, '')}`.trim();

/** The notes and template sentences of a plan, as the server computes them. */
export function explainPlan(plan, error) {
  const nodes = flatten(plan);
  const executed = nodes.some((n) => (n.node.actualRows ?? -1) >= 0);
  const total = nodes[0].node.timeMs || 0;
  const failed = !!error;
  const partial = (n) =>
    failed && n.node.complete === false && !n.node.stoppedEarly && !n.node.skipped;
  const notes = [];
  if (error) {
    const secs = error.timeoutSeconds;
    notes.push({
      node: null,
      code: 'budget',
      severity: 'high',
      text: `The ${secs != null ? `${secs} s ` : ''}timeout stopped the query after ${ms(total)}.`,
      source: 'explain',
    });
  }
  if (executed && total > 0) {
    const top = [...nodes].sort((a, b) => b.self - a.self)[0];
    if (top.self / total >= 0.2)
      notes.push({
        node: top.id,
        code: 'dominant',
        severity: error || top.self >= 100 ? 'high' : 'info',
        text: `${label(top.node)} took ${ms(top.self)} of the ${ms(total)}.`,
        source: 'explain',
      });
  }
  for (const n of nodes) {
    const e = n.node.estimatedRows;
    const a = n.node.actualRows;
    if (e >= 0 && a >= 0 && !partial(n) && !n.node.stoppedEarly) {
      const r = Math.max(a, 1) / Math.max(e, 1);
      if (r >= 10 || r <= 0.1)
        notes.push({
          node: n.id,
          code: 'misestimate',
          severity: 'warning',
          text: `${label(n.node)} was estimated at ${rows(e)} rows and produced ${rows(a)}.`,
          source: 'explain',
        });
    }
  }
  const scans = nodes.filter((n) => n.node.operator === 'IndexScan');
  const asks = [];
  if (scans.length)
    asks.push({
      text: `Finds ${scans.map((s) => String(s.node.description).split(' ').slice(1).join(' ')).join(' and ')}.`,
      nodes: scans.map((s) => s.id),
    });
  const filters = nodes.filter((n) => n.node.operator === 'Filter');
  if (filters.length)
    asks.push({
      text: `Keeping those where ${filters.map((f) => f.node.description).join(' and ')}.`,
      nodes: filters.map((f) => f.id),
    });
  if (!asks.length) asks.push({ text: 'Reads the values the query gives.', nodes: ['0'] });
  const facts = nodes.map((n) => ({
    id: n.id,
    operator: n.node.operator,
    description: n.node.description ?? '',
    estimatedRows: n.node.estimatedRows >= 0 ? n.node.estimatedRows : null,
    actualRows: n.node.actualRows >= 0 ? n.node.actualRows : null,
    timeMs: n.node.timeMs || 0,
    selfMs: n.self,
    complete: n.node.complete !== false,
    partial: partial(n),
  }));
  return { executed, notes, asks, nodes: facts };
}

/** The partial plan of a query that a 2-second timeout stopped. */
export function timedOutPlan(full) {
  const mark = (n) => ({ ...n, complete: false, children: (n.children ?? []).map((c) => mark(c)) });
  const plan = mark(full);
  // the filter spent the time
  const walk = (n) => {
    if (n.operator === 'Filter') n.timeMs = 1900;
    n.children.forEach(walk);
  };
  walk(plan);
  plan.timeMs = 2000;
  return plan;
}

export async function handleExplain(req, res, url, seg, { datasets, send, fail, readBody }) {
  if (seg[1] !== 'sparql' || seg[2] !== 'explain' || seg.length !== 3) return false;
  const ds = datasets.get(seg[0] ?? '');
  if (!ds) return (fail(res, 404, `No such dataset: ${seg[0] ?? ''}`), true);
  if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
  let body;
  try {
    body = JSON.parse((await readBody(req)).toString('utf8'));
  } catch {
    return (fail(res, 400, 'the body is not JSON', { code: 'bad-argument' }), true);
  }
  if (typeof body?.query !== 'string')
    return (fail(res, 400, 'expected `query`', { code: 'bad-argument' }), true);
  if (body.profile !== 'given' || !body.plan)
    return (fail(res, 400, 'the mock explains given plans only', { code: 'bad-argument' }), true);
  const e = explainPlan(body.plan, body.error);
  const events = [
    [
      'plan',
      {
        dataset: seg[0],
        commit: body.commit ?? null,
        queryType: 'SELECT',
        profile: 'given',
        executed: e.executed,
        plan: body.plan,
        warnings: [],
      },
    ],
    [
      'notes',
      {
        nodes: e.nodes,
        notes: e.notes,
        shownNotes: Math.min(12, e.notes.length),
        hiddenEstimates: false,
      },
    ],
    ['explanation', { source: 'template', asks: e.asks, notes: e.notes }],
  ];
  if (
    String(req.headers.accept ?? '').includes('application/json') &&
    !String(req.headers.accept).includes('text/event-stream')
  ) {
    const out = {};
    for (const [k, v] of events) Object.assign(out, k === 'explanation' ? { explanation: v } : v);
    return (send(res, 200, out), true);
  }
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache',
    'Access-Control-Allow-Origin': '*',
  });
  for (const [k, v] of events) res.write(`event: ${k}\ndata: ${JSON.stringify(v)}\n\n`);
  res.end();
  return true;
}
