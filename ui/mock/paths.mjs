// The mock's `SERVICE path:search` (docs/specs/F07-path-search.md, with the skipped
// predicates and the hub limit of docs/specs/U09-path-finder.md). oxigraph does not know
// the service, so the block's parameters are read here and the search runs over the
// default graph: a breadth-first search for the shortest paths, and an enumeration of the
// simple paths within the length for the k shortest, which the mock's small graphs allow.
// With `path:weight` (U09 Phase 3) every simple path within the length is enumerated and
// ordered by its cost, the sum of the smallest weight on a reifier of each edge, else 1.
// Only the parameters the path finder writes are understood.

import ox from 'oxigraph';

const NS = 'urn:x-sparkles:path#';

/** The block of a `SERVICE path:search { … }` call, or null. */
function block(query) {
  const m = /SERVICE\s+(?:path:search|<urn:x-sparkles:path#search>)\s*\{/i.exec(query);
  if (!m) return null;
  let depth = 1;
  let i = m.index + m[0].length;
  const start = i;
  for (; i < query.length && depth; i++) {
    if (query[i] === '{') depth++;
    else if (query[i] === '}') depth--;
  }
  return query.slice(start, i - 1);
}

/** The terms of a parameter: `<iri>`, `"literal"`, `path:name`, a number or `?var`. */
function values(src, name) {
  const re = new RegExp(
    `path:${name}\\s+((?:(?:<[^>]*>|"(?:[^"\\\\]|\\\\.)*"|path:\\w+|-?\\d+|\\?\\w+)\\s*,?\\s*)+)`,
    'g',
  );
  const out = [];
  for (const m of src.matchAll(re)) {
    for (const t of m[1].matchAll(/<([^>]*)>|"((?:[^"\\]|\\.)*)"|path:(\w+)|(-?\d+)|\?(\w+)/g)) {
      if (t[1] != null) out.push({ kind: 'iri', value: t[1] });
      else if (t[2] != null) out.push({ kind: 'literal', value: JSON.parse(`"${t[2]}"`) });
      else if (t[3] != null) out.push({ kind: 'path', value: t[3] });
      else if (t[4] != null) out.push({ kind: 'int', value: Number(t[4]) });
      else out.push({ kind: 'var', value: t[5] });
    }
  }
  return out;
}

export class PathError extends Error {}

function parse(src) {
  const one = (name) => values(src, name)[0];
  const known =
    /^(source|target|algorithm|k|minLength|maxLength|direction|maxDegree|excludePredicate|predicate|maxVisited|limit|pathIndex|edgeIndex|edgeSubject|edgePredicate|edgeObject|length|cost|weight)$/;
  for (const m of src.matchAll(/path:(\w+)\s/g))
    if (
      !known.test(m[1]) &&
      !/^(shortest|allShortest|kShortest|all|forward|backward|both)$/.test(m[1])
    )
      throw new PathError(`path:search: unknown parameter path:${m[1]}`);
  const source = one('source');
  const target = one('target');
  if (!source || !target) throw new PathError('path:search: missing path:source and path:target');
  const excluded = values(src, 'excludePredicate').map((t) => t.value);
  const only = values(src, 'predicate').map((t) => t.value);
  if (excluded.length && only.length)
    throw new PathError(
      'path:search: path:excludePredicate cannot be combined with path:predicate; name the predicates to follow or the ones to skip',
    );
  const maxDegree = one('maxDegree')?.value ?? null;
  if (maxDegree === 0) throw new PathError('path:search: path:maxDegree must be positive');
  const out = {};
  for (const n of [
    'pathIndex',
    'edgeIndex',
    'edgeSubject',
    'edgePredicate',
    'edgeObject',
    'length',
    'cost',
  ])
    out[n] = one(n)?.value;
  return {
    source,
    target,
    algorithm: one('algorithm')?.value ?? 'shortest',
    k: one('k')?.value ?? 1,
    minLength: one('minLength')?.value ?? 1,
    maxLength: one('maxLength')?.value ?? Infinity,
    direction: one('direction')?.value ?? 'forward',
    maxDegree,
    excluded: new Set(excluded),
    only: only.length ? new Set(only) : null,
    maxVisited: one('maxVisited')?.value ?? 10_000_000,
    limit: one('limit')?.value ?? Infinity,
    weight: one('weight')?.value ?? null,
    out,
  };
}

const keyOf = (t) => (t.termType === 'Literal' ? `"${t.value}"` : `<${t.value}>`);

function termOf(v) {
  return v.kind === 'iri' ? ox.namedNode(v.value) : ox.literal(v.value);
}

/** The successors of each node in path order: [next node, stored quad]. */
function adjacency(store, c) {
  const succ = new Map();
  const add = (k, v) => {
    let l = succ.get(k);
    if (!l) succ.set(k, (l = []));
    l.push(v);
  };
  for (const q of store.match(null, null, null, ox.defaultGraph())) {
    // the triple terms of reifiers are not nodes of the mock's searches
    if (q.object.termType === 'Quad') continue;
    const p = q.predicate.value;
    if (c.excluded.has(p) || (c.only && !c.only.has(p))) continue;
    const s = keyOf(q.subject);
    const o = keyOf(q.object);
    if (c.direction !== 'backward') add(s, [q.object, q]);
    // a literal has no edges of its own
    if (
      c.direction !== 'forward' &&
      q.object.termType !== 'Literal' &&
      !(c.direction === 'both' && s === o)
    )
      add(o, [q.subject, q]);
  }
  return succ;
}

/** Run the search of a query; null when the query has none. */
export function pathSearch(store, query) {
  const src = block(query);
  if (src == null) return null;
  const c = parse(src);
  const succ = adjacency(store, c);
  const s = termOf(c.source);
  const t = termOf(c.target);
  const sk = keyOf(s);
  const tk = keyOf(t);
  let visited = 0;
  let skipped = 0;
  const visit = () => {
    if (++visited > c.maxVisited)
      throw new PathError(
        `path:search: a search visited more than ${c.maxVisited} nodes; raise path:maxVisited, lower path:maxLength or add predicates`,
      );
  };
  // a hub is not passed through: only the source is expanded whatever its degree
  const next = (k) => {
    const l = succ.get(k) ?? [];
    if (k !== sk && c.maxDegree != null && l.length > c.maxDegree) {
      skipped++;
      return [];
    }
    return l;
  };
  let paths = [];
  /** The cost of each path, by its position in `paths`, with weights. */
  let costs = null;
  // the shortest path from `from` that avoids banned nodes and edges, as [node key, quad]
  // steps, by breadth-first search within `max` edges
  const bfs = (from, bannedNodes, bannedEdges, max) => {
    const parent = new Map([[from, null]]);
    let frontier = [from];
    for (let d = 0; d < max && frontier.length; d++) {
      const nextLevel = [];
      for (const k of frontier) {
        for (const [n, q] of next(k)) {
          const nk = keyOf(n);
          if (parent.has(nk) || bannedNodes.has(nk) || bannedEdges.has(`${k}|${q}`)) continue;
          visit();
          parent.set(nk, [k, q]);
          if (nk === tk) {
            const steps = [];
            for (let x = tk; parent.get(x); x = parent.get(x)[0])
              steps.unshift([x, parent.get(x)[1], parent.get(x)[0]]);
            return steps;
          }
          nextLevel.push(nk);
        }
      }
      frontier = nextLevel;
    }
    return null;
  };
  if (c.weight) {
    const W = ox.namedNode(c.weight);
    const REIFIES = ox.namedNode('http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies');
    const weightOf = (q) => {
      let w = Infinity;
      const t = ox.triple(q.subject, q.predicate, q.object);
      for (const r of store.match(null, REIFIES, t, null))
        for (const v of store.match(r.subject, W, null, null)) {
          const n = Number(v.object.value);
          if (v.object.termType === 'Literal' && Number.isFinite(n) && n >= 0) w = Math.min(w, n);
        }
      return Number.isFinite(w) ? w : 1;
    };
    // every simple path within the length, by depth-first search
    const all = [];
    const onPath = new Set([sk]);
    const walk = (k, steps, cost) => {
      if (k === tk && steps.length) {
        all.push({ steps: [...steps], cost });
        return;
      }
      if (steps.length >= Math.min(c.maxLength, 8)) return;
      for (const [n, q] of next(k)) {
        const nk = keyOf(n);
        if (onPath.has(nk) && nk !== tk) continue;
        if (nk === tk && onPath.has(nk)) continue;
        visit();
        onPath.add(nk);
        steps.push(q);
        walk(nk, steps, cost + weightOf(q));
        steps.pop();
        onPath.delete(nk);
      }
    };
    walk(sk, [], 0);
    all.sort((a, b) => a.cost - b.cost || a.steps.length - b.steps.length);
    let chosen = all.filter((p) => p.steps.length >= c.minLength);
    if (c.algorithm === 'shortest') chosen = chosen.slice(0, 1);
    else if (c.algorithm === 'allShortest')
      chosen = chosen.filter((p) => p.cost === chosen[0].cost);
    else chosen = chosen.slice(0, c.k);
    paths = chosen.map((p) => p.steps);
    costs = chosen.map((p) => p.cost);
  } else if (c.algorithm === 'kShortest') {
    // Yen's algorithm over the breadth-first search; a step is [node, quad, previous node]
    const first = bfs(sk, new Set(), new Set(), c.maxLength);
    const found = first ? [first] : [];
    const cands = [];
    const seen = new Set(first ? [first.map((x) => `${x[2]}|${x[1]}`).join(' ')] : []);
    while (found.length && found.length < c.k + 20) {
      const last = found[found.length - 1];
      for (let i = 0; i < last.length; i++) {
        const root = last.slice(0, i);
        const spur = i === 0 ? sk : root[i - 1][0];
        const bannedEdges = new Set();
        for (const p of found)
          if (
            p.length > i &&
            p
              .slice(0, i)
              .every(
                (x, j) =>
                  x === root[j] || (String(x[1]) === String(root[j][1]) && x[2] === root[j][2]),
              )
          )
            bannedEdges.add(`${p[i][2]}|${p[i][1]}`);
        const bannedNodes = new Set([sk, ...root.map((x) => x[0])]);
        bannedNodes.delete(spur);
        const tail = bfs(spur, bannedNodes, bannedEdges, c.maxLength - i);
        if (!tail) continue;
        const path = [...root, ...tail];
        const id = path.map((x) => `${x[2]}|${x[1]}`).join(' ');
        if (!seen.has(id)) {
          seen.add(id);
          cands.push(path);
        }
      }
      if (!cands.length) break;
      cands.sort((a, b) => a.length - b.length);
      found.push(cands.shift());
    }
    paths = found
      .filter((p) => p.length >= c.minLength)
      .slice(0, c.k)
      .map((p) => p.map((x) => x[1]));
  } else {
    // breadth first, with every parent of the same level
    const level = new Map([[sk, 0]]);
    const parents = new Map();
    let frontier = [sk];
    let d = 0;
    while (frontier.length && !level.has(tk) && d < c.maxLength) {
      d++;
      const nextLevel = [];
      for (const k of frontier) {
        for (const [n, q] of next(k)) {
          const nk = keyOf(n);
          if (nk === sk) continue;
          if (!level.has(nk)) {
            visit();
            level.set(nk, d);
            nextLevel.push(nk);
          }
          if (level.get(nk) === d) {
            if (!parents.has(nk)) parents.set(nk, []);
            parents.get(nk).push([k, q]);
          }
        }
      }
      frontier = nextLevel;
    }
    if (level.has(tk)) {
      const all = c.algorithm === 'allShortest';
      const back = (k) => {
        if (k === sk) return [[]];
        const out = [];
        for (const [pk, q] of parents.get(k) ?? []) {
          for (const p of back(pk)) {
            out.push([...p, q]);
            if (!all) return out;
          }
        }
        return out;
      };
      paths = back(tk);
    }
  }
  paths = paths.slice(0, Math.min(c.limit, c.algorithm === 'shortest' ? 1 : Infinity));
  const vars = Object.values(c.out).filter(Boolean);
  const rows = [];
  paths.forEach((p, n) => {
    p.forEach((q, i) => {
      const row = {
        [c.out.pathIndex]: ox.literal(
          String(n),
          ox.namedNode('http://www.w3.org/2001/XMLSchema#integer'),
        ),
        [c.out.edgeIndex]: ox.literal(
          String(i),
          ox.namedNode('http://www.w3.org/2001/XMLSchema#integer'),
        ),
        [c.out.edgeSubject]: q.subject,
        [c.out.edgePredicate]: q.predicate,
        [c.out.edgeObject]: q.object,
        [c.out.length]: ox.literal(
          String(p.length),
          ox.namedNode('http://www.w3.org/2001/XMLSchema#integer'),
        ),
        [c.out.cost]: ox.literal(
          String(costs ? costs[n] : p.length),
          ox.namedNode('http://www.w3.org/2001/XMLSchema#double'),
        ),
      };
      rows.push(vars.map((v) => row[v] ?? null));
    });
  });
  return {
    vars,
    rows,
    counters: {
      searches: 1,
      visited,
      paths: paths.length,
      ...(c.maxDegree != null ? { skippedHubs: skipped } : {}),
    },
  };
}
