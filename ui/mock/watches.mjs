// Mock of watches on entities (ROADMAP item 619, docs/API.md "Watches"): `GET` and
// `POST /$/watches`, `DELETE /$/watches/{id}`, `GET /$/watches/{id}/matches` and
// `POST /$/watches/{id}/seen`, in the shapes of docs/openapi.json (`Watch`, `WatchList`,
// `WatchMatches`). Watches live in memory and belong to everyone who calls the mock.
// `recordWatchMatches` runs after each commit of a write: a change in the watched graph
// that names the entity matches an entity watch, and one that names the entity or a class
// below it through rdfs:subClassOf in the store after the commit matches a subtree watch.
// That is coarser than the server, which also walks the hierarchy before the commit.
// `DELETE /$/watches` clears every watch, which the tests use to start again. The mail
// status says that bob@example.org receives mail, as a server with an email channel that
// carries watches does.

import { randomUUID } from 'node:crypto';

const SUBCLASS = 'http://www.w3.org/2000/01/rdf-schema#subClassOf';
const MAX_MATCHES = 200;

/** @type {Map<string, any>} */
const watches = new Map();

const view = (w) => {
  const unread = w.matches.filter((m) => m.commit > w.seen).length;
  const j = {
    id: w.id,
    dataset: w.dataset,
    branch: w.branch,
    entity: w.entity,
    scope: w.scope,
    created: w.created,
    since: w.since,
    seen: w.seen,
    unread,
    overflow: w.overflow,
    truncated: false,
  };
  if (w.graph) j.graph = w.graph;
  if (w.depth) j.depth = w.depth;
  if (w.matches.length) j.latest = w.matches.at(-1).commit;
  return j;
};

/** The IRIs a line of N-Quads names in its first three terms, and its graph (`null`: the default graph). */
export function parse(line) {
  const terms = line
    .replace(/"(?:[^"\\]|\\.)*"(?:@[\w-]+|\^\^<[^>]*>)?/g, '"L"')
    .trim()
    .replace(/\s*\.$/, '')
    .split(/\s+/);
  const iri = (t) => (t && t.startsWith('<') ? t.slice(1, -1) : null);
  return {
    named: terms.slice(0, 3).map(iri).filter(Boolean),
    graph: iri(terms[3]),
  };
}

/** The superclasses of `iri` in `graph` of `store`, up to `depth` levels (every level without). */
function ancestors(store, ox, iri, graph, depth) {
  const out = new Map([[iri, 0]]);
  let frontier = [iri];
  for (let level = 1; frontier.length && (!depth || level <= depth); level++) {
    const next = [];
    for (const c of frontier) {
      const g = graph ? ox.namedNode(graph) : ox.defaultGraph();
      for (const q of store.match(ox.namedNode(c), ox.namedNode(SUBCLASS), null, g)) {
        if (q.object.termType !== 'NamedNode' || out.has(q.object.value)) continue;
        out.set(q.object.value, level);
        next.push(q.object.value);
      }
    }
    frontier = next;
  }
  return out;
}

/**
 * Record the matches of a commit on `ds` (a dataset or branch object): `added` and `removed`
 * are its N-Quads lines.
 */
export function recordWatchMatches(ds, commit, added, removed, ox) {
  if (!watches.size) return;
  const changes = [...added, ...removed].map(parse);
  for (const w of watches.values()) {
    if (w.dataset !== ds.name || w.branch !== (ds.branchName ?? 'main')) continue;
    if (commit.seq <= w.since || w.matches.some((m) => m.commit === commit.seq)) continue;
    const members = new Set();
    for (const c of changes) {
      if ((c.graph ?? null) !== (w.graph ?? null)) continue;
      for (const iri of c.named) {
        if (iri === w.entity) members.add(iri);
        else if (
          w.scope === 'subtree' &&
          ancestors(ds.store, ox, iri, w.graph, w.depth).has(w.entity)
        )
          members.add(iri);
      }
    }
    if (!members.size) continue;
    const listed = [...members].sort();
    w.matches.push({
      commit: commit.seq,
      time: commit.timestamp,
      author: commit.author,
      members: listed.slice(0, 20),
      count: listed.length,
      exact: true,
    });
    while (w.matches.length > MAX_MATCHES) {
      w.matches.shift();
      w.overflow = true;
    }
  }
}

/** Seed a watch with matches, for the screenshots. */
export function seedWatch(w) {
  const id = randomUUID();
  watches.set(id, {
    id,
    created: new Date(Date.now() - 6 * 86_400_000).toISOString(),
    overflow: false,
    matches: [],
    ...w,
  });
  return id;
}

export async function handleWatches(req, res, url, seg, ctx) {
  const { send, fail, readBody, datasets, headSeq } = ctx;
  if (seg[0] !== '$' || seg[1] !== 'watches') return false;
  const id = seg[2] ? decodeURIComponent(seg[2]) : null;
  if (!id) {
    if (req.method === 'GET') {
      const p = url.searchParams;
      const graph = p.get('graph');
      const ws = [...watches.values()]
        .filter((w) => !p.get('dataset') || w.dataset === p.get('dataset'))
        .filter((w) => !p.get('entity') || w.entity === p.get('entity'))
        .filter((w) => graph == null || (graph === 'default' ? !w.graph : w.graph === graph))
        .map(view);
      send(res, 200, {
        watches: ws,
        unread: ws.reduce((n, w) => n + w.unread, 0),
        mail: { channels: true, address: 'bob@example.org' },
      });
      return true;
    }
    if (req.method === 'DELETE') {
      watches.clear();
      send(res, 204, '');
      return true;
    }
    if (req.method !== 'POST') return (fail(res, 405, 'method not allowed'), true);
    let b;
    try {
      b = JSON.parse(String(await readBody(req)));
    } catch {
      return (fail(res, 400, 'the body is not JSON', { code: 'bad-request' }), true);
    }
    const ds = datasets.get(b.dataset);
    if (!ds || typeof b.entity !== 'string')
      return (fail(res, 404, `no such dataset: /${b.dataset}`, { code: 'not-found' }), true);
    const branch = b.branch ?? 'main';
    const scope = b.scope ?? 'entity';
    const graph = b.graph && b.graph !== 'default' ? b.graph : undefined;
    if (b.depth != null && (scope !== 'subtree' || !(b.depth >= 1 && b.depth <= 1000)))
      return (
        fail(res, 400, 'depth: from 1 to 1000, and only for the subtree scope', {
          code: 'bad-request',
        }),
        true
      );
    const same = [...watches.values()].find(
      (w) =>
        w.dataset === b.dataset &&
        w.branch === branch &&
        w.graph === graph &&
        w.entity === b.entity &&
        w.scope === scope &&
        w.depth === b.depth,
    );
    if (same) return (send(res, 200, view(same)), true);
    const bobj = branch === 'main' ? ds : ds.branchSet?.get(branch);
    if (!bobj) return (fail(res, 404, `no such branch: ${branch}`, { code: 'not-found' }), true);
    const head = headSeq(bobj);
    const nid = randomUUID();
    const w = {
      id: nid,
      dataset: b.dataset,
      branch,
      graph,
      entity: b.entity,
      scope,
      depth: b.depth,
      created: new Date().toISOString(),
      since: head,
      seen: head,
      overflow: false,
      matches: [],
    };
    watches.set(nid, w);
    send(res, 201, view(w));
    return true;
  }
  const w = watches.get(id);
  if (!w) return (fail(res, 404, 'no such watch', { code: 'no-such-watch' }), true);
  if (seg.length === 3) {
    if (req.method !== 'DELETE') return (fail(res, 405, 'method not allowed'), true);
    watches.delete(id);
    send(res, 204, '');
    return true;
  }
  if (seg[3] === 'matches' && req.method === 'GET') {
    const p = url.searchParams;
    const after = p.get('after') ? Number(p.get('after')) : null;
    const before = p.get('before') ? Number(p.get('before')) : null;
    const limit = Number(p.get('limit') ?? 50);
    const all = [...w.matches]
      .reverse()
      .filter((m) => after == null || m.commit > after)
      .filter((m) => before == null || m.commit < before);
    send(res, 200, {
      watch: view(w),
      matches: all.slice(0, limit).map((m) => ({ ...m, unread: m.commit > w.seen })),
      more: all.length > limit,
    });
    return true;
  }
  if (seg[3] === 'seen' && req.method === 'POST') {
    const b = JSON.parse(String(await readBody(req)) || '{}');
    if (!Number.isInteger(b.commit))
      return (fail(res, 400, 'commit: a number', { code: 'bad-request' }), true);
    w.seen = Math.max(w.seen, b.commit);
    send(res, 200, view(w));
    return true;
  }
  fail(res, 405, 'method not allowed');
  return true;
}
