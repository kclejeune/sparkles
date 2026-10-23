// C22 in the mock: the GraphRAG routes of `/$/graphrag/{ds}` for the `org` dataset, with
// two canned communities under one parent and their reports. Reviews change the canned
// reports. The mock does not run builds: the server's own tests cover them, and
// `POST …/build` and `…/refresh` answer 501.

import { isAdmin } from './memory.mjs';

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

export async function handleGraphrag(req, res, url, seg, ctx) {
  const { datasets, send, fail, readBody } = ctx;
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
