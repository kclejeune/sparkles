// Point-in-time reads, named snapshots and the change log for the mock backend
// (docs/API.md, "Point-in-time reads and snapshots" and "History queries"). The mock keeps
// the quads of every commit it makes after start-up, so `at=` reads those commits; the
// seeded history of the demo datasets has no states and answers 410 `history-gone`, which
// shows the slider's hatched stretch.

import ox from 'oxigraph';
import { allCommits, changeJson, changesOf } from './branches.mjs';

/** commit -> the quads right after it, as N-Quads lines without the final " ." */
const statesOf = new WeakMap();
/** commit -> the same state in a store, built on first use */
const storeOf = new WeakMap();
/** branch dataset -> Map(name -> {seq, created, note}) */
const snapshotsOf = new WeakMap();

const snaps = (ds) => {
  let m = snapshotsOf.get(ds);
  if (!m) snapshotsOf.set(ds, (m = new Map()));
  return m;
};

/** Remember the state right after `commit`, which `ds.store` holds when this is called. */
export function recordState(ds, commit) {
  statesOf.set(
    commit,
    ds.store.match().map((q) => String(q)),
  );
}

const pinned = (ds, seq) => [...snaps(ds)].filter(([, s]) => s.seq === seq).map(([n]) => n);

/** Whether a read can see the state after `c`. An in-memory dataset keeps only pinned ones. */
function readable(ds, c, head) {
  if (c.seq === head) return true;
  if (!statesOf.has(c)) return false;
  return ds.type !== 'mem' || pinned(ds, c.seq).length > 0;
}

/** The ranges of commits a read can see. */
export function readableRanges(ds) {
  const list = allCommits(ds);
  const head = list.at(-1)?.seq ?? 0;
  const out = [];
  for (const c of list) {
    if (!readable(ds, c, head)) continue;
    const last = out.at(-1);
    if (last && last.to === c.seq - 1) last.to = c.seq;
    else out.push({ from: c.seq, to: c.seq });
  }
  return out;
}

/** The commit as the catalog lists it, with whether it is readable and the snapshots on it. */
export function decorate(ds, c) {
  const head = allCommits(ds).at(-1)?.seq ?? 0;
  const names = pinned(ds, c.seq);
  return {
    ...c,
    reconstructable: readable(ds, c, head),
    ...(names.length ? { snapshots: names } : {}),
  };
}

/** Fields of a commit listing about what can be read. */
export function readableFields(ds) {
  const ranges = readableRanges(ds);
  return { oldestReconstructable: ranges[0]?.from ?? null, reconstructable: ranges };
}

function stateStore(commit) {
  let st = storeOf.get(commit);
  if (!st) {
    st = new ox.Store();
    const lines = statesOf.get(commit) ?? [];
    if (lines.length)
      st.load(lines.map((l) => `${l} .`).join('\n'), { format: 'application/n-quads' });
    storeOf.set(commit, st);
  }
  return st;
}

const err = (status, error, extra = {}) => ({ error: [status, { error, ...extra }] });

/**
 * The state `at` names: `{seq, commit, head, live, store, headers}` or `{error: [status, body]}`.
 * `store` is set for a past state.
 */
export function resolveAt(ds, selector) {
  const list = allCommits(ds);
  const head = list.at(-1);
  const sel = String(selector ?? '').trim();
  let seq;
  let canonical;
  const num = /^(?:commit:)?(\d{1,20})$/.exec(sel);
  const snap = /^snapshot:(.+)$/.exec(sel);
  const time = /^time:(.+)$/.exec(sel);
  if (!sel || sel === 'head') {
    return { seq: head.seq, commit: head, head: head.seq, live: true, headers: {} };
  } else if (num) {
    seq = Number(num[1]);
    canonical = `commit:${seq}`;
  } else if (snap) {
    const s = snaps(ds).get(snap[1]);
    if (!s)
      return err(404, `no snapshot '${snap[1]}' in dataset ${ds.name}`, {
        code: 'no-such-snapshot',
      });
    seq = s.seq;
    canonical = `snapshot:${snap[1]}`;
  } else if (time) {
    const t = Date.parse(time[1]);
    if (Number.isNaN(t)) return err(400, `invalid at '${sel}'`, { code: 'invalid-at' });
    const found = [...list].reverse().find((c) => Date.parse(c.timestamp) <= t);
    if (!found) return err(404, `no commit at or before ${time[1]}`, { code: 'before-history' });
    seq = found.seq;
    canonical = `time:${time[1]}`;
  } else {
    return err(
      400,
      `invalid at '${sel}': expected head, N, commit:N, time:<RFC 3339> or snapshot:<name>`,
      { code: 'invalid-at' },
    );
  }
  if (seq > head.seq)
    return err(404, `no commit ${seq} in dataset ${ds.name} (head is ${head.seq})`, {
      code: 'no-such-commit',
    });
  const commit = list.find((c) => c.seq === seq);
  const headers = { 'Sparkles-At': canonical, 'Sparkles-Head': String(head.seq) };
  if (seq === head.seq) return { seq, commit: head, head: head.seq, live: true, headers };
  if (ds.type === 'mem' && !pinned(ds, seq).length)
    return err(501, 'point-in-time reads need a persistent dataset', {
      code: 'history-unsupported',
    });
  if (!commit || !readable(ds, commit, head.seq)) {
    const ranges = readableRanges(ds);
    return err(
      410,
      `commit ${seq} of dataset ${ds.name} is no longer reconstructable; the oldest reconstructable commit is ${ranges[0]?.from ?? head.seq}`,
      {
        code: 'history-gone',
        at: canonical,
        commit: seq,
        head: head.seq,
        oldestReconstructable: ranges[0]?.from ?? head.seq,
        reconstructable: ranges,
        metadata: commit ?? null,
      },
    );
  }
  return {
    seq,
    commit,
    head: head.seq,
    live: false,
    store: stateStore(commit),
    headers: {
      ...headers,
      'Memento-Datetime': new Date(commit.timestamp).toUTCString(),
      'Sparkles-Commit': String(seq),
    },
  };
}

/** A dataset that answers reads from a past state's store. */
export function pastView(ds, store) {
  return Object.assign(Object.create(ds), { store });
}

// ---------------------------------------------------------------------------
// named snapshots: GET and POST /$/snapshots/{ds}, DELETE /$/snapshots/{ds}/{name}

const snapshotJson = (ds, name, s) => {
  const list = allCommits(ds);
  const c = list.find((x) => x.seq === s.seq);
  return {
    name,
    ref: `snapshot:${name}`,
    seq: s.seq,
    commit: c ? decorate(ds, c) : null,
    created: s.created,
    expires: null,
    note: s.note ?? null,
    generation: null,
    reconstructable: true,
  };
};

export async function handleSnapshots(req, res, url, seg, ctx) {
  if (seg[0] !== '$' || seg[1] !== 'snapshots' || !seg[2]) return false;
  const ds = ctx.datasets.get(seg[2]);
  if (!ds) return (ctx.fail(res, 404, `No such dataset: ${seg[2]}`), true);
  const map = snaps(ds);
  const name = seg[3];
  const head = allCommits(ds).at(-1);
  if (req.method === 'GET' && !name) {
    const snapshots = [...map]
      .map(([n, s]) => snapshotJson(ds, n, s))
      .sort((a, b) => a.seq - b.seq || a.name.localeCompare(b.name));
    ctx.send(res, 200, { dataset: ds.name, datasetId: ds.id, head: head.seq, snapshots });
    return true;
  }
  if (req.method === 'GET' && name) {
    const s = map.get(name);
    if (!s) return (ctx.fail(res, 404, `no snapshot '${name}' in dataset ${ds.name}`), true);
    ctx.send(res, 200, snapshotJson(ds, name, s));
    return true;
  }
  if (req.method === 'POST' && !name) {
    let body = {};
    try {
      body = JSON.parse((await ctx.readBody(req)).toString() || '{}');
    } catch {
      return (ctx.fail(res, 400, 'invalid JSON body'), true);
    }
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(body.name ?? ''))
      return (ctx.fail(res, 400, `invalid snapshot name '${body.name ?? ''}'`), true);
    const at = resolveAt(ds, body.at ?? 'head');
    if (at.error) return (ctx.send(res, ...at.error), true);
    const had = map.get(body.name);
    if (had && had.seq !== at.seq)
      return (ctx.fail(res, 409, `snapshot '${body.name}' already pins commit ${had.seq}`), true);
    const s = had ?? { seq: at.seq, created: new Date().toISOString(), note: body.note };
    map.set(body.name, s);
    ctx.send(res, had ? 200 : 201, snapshotJson(ds, body.name, s), 'application/json', {
      Location: `/$/snapshots/${ds.name}/${body.name}`,
    });
    return true;
  }
  if (req.method === 'DELETE' && name) {
    if (!map.delete(name))
      return (ctx.fail(res, 404, `no snapshot '${name}' in dataset ${ds.name}`), true);
    ctx.send(res, 204, '');
    return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// the change log: GET /{ds}/history

/** A term in N-Triples, or a bare IRI. */
const asTerm = (v) => (v == null || v === '' ? null : /^[<_"]/.test(v) ? v : `<${v}>`);

export function handleHistoryChanges(req, res, url, seg, ctx) {
  if (seg[0] === '$' || seg[1] !== 'history' || seg.length !== 2 || req.method !== 'GET')
    return false;
  const ds = ctx.datasets.get(seg[0]);
  if (!ds) return false;
  const p = url.searchParams;
  const list = allCommits(ds);
  const head = list.at(-1).seq;
  const sel = (v, d) =>
    v == null || v === '' || v === 'head' ? d : Number(String(v).replace(/^commit:/, ''));
  const from = sel(p.get('from'), 0);
  const to = sel(p.get('to'), head);
  if (!Number.isInteger(from) || !Number.isInteger(to))
    return (ctx.fail(res, 400, 'the mock reads commit numbers only'), true);
  const want = {
    subject: asTerm(p.get('subject')),
    predicate: asTerm(p.get('predicate')),
    object: asTerm(p.get('object')),
    graph: p.get('graph'),
  };
  const op = p.get('op');
  const order = p.get('order') === 'desc' ? -1 : 1;
  const limit = Math.min(Number(p.get('limit') ?? 1000), 100000);
  const changes = [];
  const unrecorded = [];
  for (const c of list.filter((x) => x.seq >= from && x.seq <= to)) {
    const recorded = changesOf.get(c);
    if (!recorded) {
      if (c.kind !== 'create') {
        const last = unrecorded.at(-1);
        const reason = c.bulk ? 'bulk' : 'before-log';
        if (last && last.to === c.seq - 1 && last.reason === reason) last.to = c.seq;
        else unrecorded.push({ from: c.seq, to: c.seq, reason });
      }
      continue;
    }
    for (const ch of recorded) {
      const j = changeJson(ch);
      if (want.subject && j.subject !== want.subject) continue;
      if (want.predicate && j.predicate !== want.predicate) continue;
      if (want.object && j.object !== want.object) continue;
      if (
        want.graph &&
        (want.graph === 'default' ? j.graph !== null : j.graph !== `<${want.graph}>`)
      )
        continue;
      const kind = ch.op === '+' ? 'add' : 'remove';
      if (op && op !== kind) continue;
      changes.push({
        op: kind,
        subject: j.subject,
        predicate: j.predicate,
        object: j.object,
        graph: j.graph,
        commit: c.seq,
        timestamp: c.timestamp,
        kind: c.kind,
        ...(c.author ? { author: c.author } : {}),
        ...(c.message ? { message: c.message } : {}),
      });
    }
  }
  changes.sort((a, b) => (a.commit - b.commit) * order);
  ctx.send(res, 200, {
    dataset: ds.name,
    datasetId: ds.id,
    head,
    from,
    to,
    truncated: changes.length > limit,
    changes: changes.slice(0, limit),
    unrecorded,
  });
  return true;
}
