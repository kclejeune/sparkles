// C22 in the mock: the GraphRAG routes of `/$/graphrag/{ds}` for the `org` dataset, with
// two canned communities under one parent and their reports. Reviews change the canned
// reports. The mock does not run builds: the server's own tests cover them, and
// `POST …/build` and `…/refresh` answer 501.

import { isAdmin } from './memory.mjs';
import { memberships } from './algo.mjs';

const RES = 'http://example.org/resource/';
const GRAPH = 'urn:x-sparkles:graphrag:reports';
const C = (n) => `urn:x-sparkles:graphrag:org:${n}`;

function seed() {
  const report = (id, title, summary, rating, about) => ({
    iri: `urn:uuid:${id}`,
    title,
    summary,
    status: 'unreviewed',
    rating,
    failure: null,
    revisionOf: null,
    droppedCitations: 0,
    droppedUnsupported: 0,
    sourceCommit: 3,
    activity: {
      model: 'mock/small',
      inputTokens: 1840,
      outputTokens: 212,
      startedAt: '2026-10-20T09:00:00Z',
    },
    findings: [
      {
        text: `${about} works with the rest of the group every week.`,
        about: [`${RES}${about.toLowerCase()}`],
        cites: [
          `<<( <${RES}${about.toLowerCase()}> <http://example.org/ontology#memberOf> <${RES}payments> )>>`,
        ],
        unmatchedNames: [],
      },
    ],
  });
  return {
    [C('0:org')]: {
      iri: C('0:org'),
      level: 0,
      parent: null,
      members: ['ana', 'kai', 'lee', 'mo'],
      children: [C('1:pay'), C('1:plat')],
      report: report(
        'r0',
        'Engineering',
        'The payments and platform teams, joined by shared releases.',
        8,
        'Ana',
      ),
    },
    [C('1:pay')]: {
      iri: C('1:pay'),
      level: 1,
      parent: C('0:org'),
      members: ['ana', 'kai'],
      children: [],
      report: report(
        'r1',
        'The payments team',
        'Ana and Kai ship the checkout redesign.',
        7,
        'Kai',
      ),
    },
    [C('1:plat')]: {
      iri: C('1:plat'),
      level: 1,
      parent: C('0:org'),
      members: ['lee', 'mo'],
      children: [],
      report: report(
        'r2',
        'The platform team',
        'Lee and Mo run the build and the clusters.',
        5,
        'Lee',
      ),
    },
  };
}

let communities = seed();

const counts = () => {
  const out = { unreviewed: 0, reviewed: 0, rejected: 0, stale: 0, failed: 0 };
  for (const c of Object.values(communities)) if (c.report) out[c.report.status]++;
  return out;
};

// The links among the members, which the overview counts between communities.
const LINKS = [
  ['ana', 'kai'],
  ['kai', 'lee'],
  ['lee', 'mo'],
];
const KNOWS = 'http://xmlns.com/foaf/0.1/knows';

const groupId = (path, folded = false) =>
  'r1.' +
  Buffer.from(JSON.stringify({ g: GRAPH, l: 0, p: path, ...(folded ? { o: true } : {}) })).toString(
    'base64url',
  );

function parseId(id) {
  try {
    const j = JSON.parse(Buffer.from(id.slice(3), 'base64url').toString('utf8'));
    return Array.isArray(j.p) && j.p.length ? j : null;
  } catch {
    return null;
  }
}

/** The community at `level` that holds member `m`, if any. */
const holder = (m, level) =>
  Object.values(communities).find((c) => c.level === level && c.members.includes(m));

/**
 * `POST /$/graph/{ds}/groups` grouped by the GraphRAG communities (U06 with C22), for
 * `org`. Other requests to the route fall through to the mock's 404.
 */
async function handleGroups(req, res, ds, ctx) {
  const { send, fail, readBody } = ctx;
  let b;
  try {
    b = JSON.parse((await readBody(req)).toString('utf8') || '{}');
  } catch {
    return (fail(res, 400, 'invalid JSON'), true);
  }
  const id = b.within?.group;
  const isReports =
    (b.by && typeof b.by === 'object' && 'reports' in b.by) || id?.startsWith('r1.');
  if (!isReports) return false;
  const commit = ds.commits?.length ?? 1;
  if (ds.name !== 'org')
    return (
      send(res, 200, {
        commit,
        available: false,
        unavailable: 'none',
        reason: 'No GraphRAG communities were built for this dataset at this state.',
        within: { reports: { graph: GRAPH, level: 0 }, size: 0 },
        grouping: { kind: 'reports', predicate: null, level: 0 },
        communities: false,
        groups: [],
        edges: [],
        truncated: false,
      }),
      true
    );
  const levels = [
    { level: 0, communities: Object.values(communities).filter((c) => c.level === 0).length },
    { level: 1, communities: Object.values(communities).filter((c) => c.level === 1).length },
  ];
  const set = id ? parseId(id) : { g: GRAPH, l: 0, p: [] };
  if (!set) return (fail(res, 400, 'the group id is malformed'), true);
  const last = set.p.length ? communities[set.p[set.p.length - 1]] : null;
  if (set.p.length && !last)
    return (fail(res, 400, 'the group names communities the reports do not hold'), true);
  const label = (m) => m[0].toUpperCase() + m.slice(1);
  const top = (members) => members.slice(0, 3).map((m) => ({ iri: `${RES}${m}`, label: label(m) }));
  const within = { reports: { graph: GRAPH, level: 0 }, size: 0 };
  if (id) within.group = id;
  if (last)
    Object.assign(within, { community: last.iri, level: last.level, title: last.report?.title });
  const kids = last
    ? last.children.map((k) => communities[k])
    : Object.values(communities).filter((c) => c.level === 0);
  const members = last ? last.members : [...new Set(kids.flatMap((c) => c.members))];
  within.size = members.length;
  // the key of a node: its community at the next level, or the opened one's members
  const level = last ? last.level + 1 : 0;
  const edges = new Map();
  const add = (s, t) => {
    const k = `${s}\u0001${t}`;
    edges.set(k, (edges.get(k) ?? 0) + 1);
  };
  const out = (m) => {
    // where a member lies relative to the set: inside, or in a sibling on the path
    for (let i = 0; i < set.p.length; i++) {
      const h = holder(m, i);
      if (!h) return null;
      if (h.iri !== set.p[i]) return `g:${groupId([...set.p.slice(0, i), h.iri])}`;
    }
    return 'inside';
  };
  if (b.by === 'members' || !kids.length) {
    for (const [a, z] of LINKS) {
      const [oa, oz] = [out(a), out(z)];
      if (oa === 'inside' && oz === 'inside') add(`m:${RES}${a}`, `m:${RES}${z}`);
      else if (oa === 'inside' && oz) add(`m:${RES}${a}`, oz);
      else if (oz === 'inside' && oa) add(oa, `m:${RES}${z}`);
    }
    return (
      send(res, 200, {
        commit,
        available: true,
        within,
        grouping: { kind: 'members', predicate: null },
        communities: false,
        groups: [],
        members: members.map((m) => ({ iri: `${RES}${m}`, label: label(m) })),
        edges: [...edges].map(([k, count]) => {
          const [source, target] = k.split('\u0001');
          return { source, target, predicate: KNOWS, count };
        }),
        truncated: false,
        levels,
      }),
      true
    );
  }
  const key = (m) => {
    const o = out(m);
    if (o !== 'inside') return o;
    const h = holder(m, level);
    return h ? `g:${groupId([...set.p, h.iri])}` : null;
  };
  for (const [a, z] of LINKS) {
    const [ka, kz] = [key(a), key(z)];
    if (ka && kz && (out(a) === 'inside' || out(z) === 'inside')) add(ka, kz);
  }
  const groups = kids
    .sort((x, y) => y.members.length - x.members.length || x.iri.localeCompare(y.iri))
    .map((c) => ({
      id: groupId([...set.p, c.iri]),
      label: c.report?.title ?? c.iri,
      size: c.members.length,
      top: top(c.members),
      community: c.iri,
      level: c.level,
      children: c.children.length,
      report: c.report
        ? { iri: c.report.iri, title: c.report.title, status: c.report.status }
        : null,
    }));
  return (
    send(res, 200, {
      commit,
      available: true,
      within,
      grouping: { kind: 'reports', predicate: null, level },
      communities: false,
      groups,
      edges: [...edges].map(([k, count]) => {
        const [source, target] = k.split('\u0001');
        return { source, target, predicate: KNOWS, count };
      }),
      truncated: false,
      levels,
    }),
    true
  );
}

export async function handleGraphrag(req, res, url, seg, ctx) {
  const { datasets, send, fail, readBody } = ctx;
  if (seg[0] === '$' && seg[1] === 'graph' && seg[3] === 'groups' && req.method === 'POST') {
    const ds = datasets.get(seg[2]);
    if (!ds) return false;
    return handleGroups(req, res, ds, ctx);
  }
  if (seg[0] !== '$' || seg[1] !== 'graphrag' || !seg[2]) return false;
  const ds = datasets.get(seg[2]);
  if (!ds) return (fail(res, 404, `No such dataset: ${seg[2]}`), true);
  const enabled = ds.name === 'org';
  const action = seg[3] ?? '';
  if (req.method === 'GET' && action === '') {
    const s = {
      dataset: ds.name,
      enabled,
      reportsGraph: GRAPH,
      refresh: 'manual',
      available: true,
    };
    if (enabled) {
      Object.assign(s, {
        reports: counts(),
        levels: [
          { level: 0, communities: 1 },
          { level: 1, communities: 2 },
        ],
        lastBuild: {
          sourceCommit: 3,
          branch: 'main',
          finishedAt: '2026-10-20T09:01:00Z',
          tokens: 6156,
          pending: 0,
        },
      });
      if (isAdmin(req)) {
        s.budget = { tokensPerBuild: 2000000, tokensPerDay: 500000, usedToday: 6156 };
        s.anomalies = [];
      }
    }
    return (send(res, 200, s), true);
  }
  if (req.method === 'GET' && action === 'communities') {
    const list = enabled
      ? Object.values(communities).map((c) => ({
          iri: c.iri,
          level: c.level,
          parent: c.parent,
          memberCount: c.members.length,
          report: c.report && {
            iri: c.report.iri,
            title: c.report.title,
            status: c.report.status,
            rating: c.report.rating,
          },
        }))
      : [];
    return (send(res, 200, { available: true, reportsGraph: GRAPH, communities: list }), true);
  }
  if (req.method === 'GET' && action === 'community') {
    const c = enabled ? communities[url.searchParams.get('iri') ?? ''] : null;
    if (!c) return (fail(res, 404, 'no such community'), true);
    return (
      send(res, 200, {
        iri: c.iri,
        level: c.level,
        parent: c.parent,
        memberCount: c.members.length,
        factCount: c.members.length * 4,
        members: c.members.map((m) => ({
          iri: `${RES}${m}`,
          label: m[0].toUpperCase() + m.slice(1),
        })),
        children: c.children.map((k) => ({ iri: k, title: communities[k]?.report?.title ?? null })),
        report: c.report,
      }),
      true
    );
  }
  if (req.method === 'POST' && action === 'memberships') {
    let b;
    try {
      b = JSON.parse((await readBody(req)).toString('utf8') || '{}');
    } catch {
      return (fail(res, 400, 'invalid JSON'), true);
    }
    if (!Array.isArray(b.nodes)) return (fail(res, 400, 'nodes takes an array of IRIs'), true);
    if (ds.branchName && ds.branchName !== 'main')
      return (
        send(res, 200, {
          available: false,
          reason: 'GraphRAG communities are read on main',
          memberships: [],
          communities: [],
        }),
        true
      );
    const out = enabled
      ? memberships(communities, b.nodes, b.level ?? null)
      : { available: true, reportsGraph: GRAPH, memberships: [], communities: [] };
    return (send(res, 200, out), true);
  }
  if (req.method === 'POST' && action === 'review') {
    let b;
    try {
      b = JSON.parse((await readBody(req)).toString('utf8') || '{}');
    } catch {
      return (fail(res, 400, 'invalid JSON'), true);
    }
    const c = Object.values(communities).find((x) => x.report?.iri === b.report);
    if (!c) return (fail(res, 404, 'no such report'), true);
    if (b.title || b.summary) {
      c.report = {
        ...c.report,
        iri: `urn:uuid:${Math.random().toString(16).slice(2)}`,
        title: b.title ?? c.report.title,
        summary: b.summary ?? c.report.summary,
        status: 'reviewed',
        revisionOf: c.report.iri,
        activity: undefined,
      };
    } else if (b.status === 'reviewed' || b.status === 'rejected') {
      c.report.status = b.status;
    } else {
      return (fail(res, 400, 'status is reviewed or rejected'), true);
    }
    return (send(res, 200, { report: b.report, status: c.report.status, commit: null }), true);
  }
  if (req.method === 'POST' && ['build', 'refresh', 'confirm'].includes(action))
    return (fail(res, 501, 'the mock does not run GraphRAG builds'), true);
  return (fail(res, 404, 'no such route'), true);
}
