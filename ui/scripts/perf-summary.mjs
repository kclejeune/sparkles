#!/usr/bin/env node
// Turns the JSON of tests/perf runs into Markdown tables of medians (ROADMAP item 725).
//
//   node scripts/perf-summary.mjs DIR [DIR ...]
//
// Each DIR holds the *.repN.json files of one build (a PERF_LABEL directory). With several
// DIRs the tables put the builds side by side, in the order given.

import { readdirSync, readFileSync } from 'node:fs';
import { basename, join } from 'node:path';

const dirs = process.argv.slice(2);
if (!dirs.length) {
  console.error('usage: perf-summary.mjs DIR [DIR ...]');
  process.exit(2);
}
const med = (xs) => {
  const s = xs.filter((x) => x != null && Number.isFinite(x)).sort((a, b) => a - b);
  if (!s.length) return null;
  return s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};
const fmt = (x, d = 0) =>
  x == null ? 'n/a' : x.toLocaleString('en-US', { maximumFractionDigits: d });
const mb = (b) => (b == null ? null : b / 1048576);

const runs = new Map(); // name -> label -> [json]
for (const d of dirs) {
  for (const f of readdirSync(d).filter((f) => f.endsWith('.json'))) {
    const j = JSON.parse(readFileSync(join(d, f), 'utf8'));
    const label = j.meta?.label ?? basename(d);
    if (!runs.has(j.name)) runs.set(j.name, new Map());
    const m = runs.get(j.name);
    m.set(label, [...(m.get(label) ?? []), j]);
  }
}
const labels = [...new Set([...runs.values()].flatMap((m) => [...m.keys()]))];
const names = [...runs.keys()].sort();

const get = (j, path) => path.split('.').reduce((o, k) => (o == null ? o : o[k]), j);
const row = (name, title, path, scale = (x) => x, d = 0) =>
  `| ${title} | ${labels
    .map((l) => {
      const rs = (runs.get(name)?.get(l) ?? []).filter((r) => r.status === 'ok');
      const failed = (runs.get(name)?.get(l) ?? []).length - rs.length;
      const v = med(rs.map((r) => scale(get(r, path))));
      return fmt(v, d) + (failed ? ` (${failed} failed)` : '');
    })
    .join(' | ')} |`;
const head = (title) =>
  `\n#### ${title}\n\n| Measure | ${labels.join(' | ')} |\n|---|${labels.map(() => '---').join('|')}|`;

console.log('# Scale measurements\n');
for (const l of labels) {
  const j = [...runs.values()].map((m) => m.get(l)?.[0]).find(Boolean);
  if (j)
    console.log(
      `- ${l}: commit ${j.meta.commit}, UI ${j.meta.uiDir}, ${j.meta.host}, ${j.meta.browser}, WebGL ${j.meta.webgl}, ${j.meta.date}${j.meta.note ? `, ${j.meta.note}` : ''}`,
    );
}
for (const name of names) {
  const any = [...runs.get(name).values()][0][0];
  console.log(head(name));
  if (any.firstRowMs !== undefined || any.partial) {
    console.log(row(name, 'First row visible (ms)', 'firstRowMs'));
    console.log(row(name, 'Interactive (ms)', 'interactiveMs'));
    console.log(row(name, 'Network quiet at (ms)', 'quietMs'));
    console.log(row(name, 'API requests', 'network.api.count'));
    console.log(row(name, 'API bytes, decoded (MB)', 'network.api.decodedBytes', mb, 2));
    console.log(row(name, 'API bytes, transferred (MB)', 'network.api.encodedBytes', mb, 2));
    console.log(row(name, 'Static requests', 'network.groups.static.count'));
    console.log(row(name, 'Long tasks (count)', 'longTasks.count'));
    console.log(row(name, 'Long tasks (total ms)', 'longTasks.totalMs'));
    console.log(row(name, 'Longest task (ms)', 'longTasks.maxMs'));
    console.log(row(name, 'JS heap after load (MB)', 'heap.jsHeapUsedMB', (x) => x, 1));
    console.log(row(name, 'DOM nodes', 'heap.domNodes'));
    console.log(row(name, 'Rows loaded', 'rowsLoaded'));
    console.log(row(name, 'Rows in DOM', 'rowsInDom'));
    console.log(row(name, 'Twisty click latency (ms)', 'toggleLatencyMs'));
    const groups = new Set();
    for (const m of runs.get(name).values())
      for (const r of m) for (const g of Object.keys(r.network?.groups ?? {})) groups.add(g);
    for (const g of [...groups].filter((g) => g !== 'static').sort())
      console.log(
        row(
          name,
          `${g}: requests / MB decoded`.replace(/ \/ MB decoded/, ' requests'),
          `network.groups.${g}.count`,
        ) +
          '\n' +
          row(name, `${g}: MB decoded`, `network.groups.${g}.decodedBytes`, mb, 2),
      );
  } else if (name.startsWith('scroll')) {
    console.log(row(name, 'Rows loaded', 'rowsLoaded'));
    console.log(row(name, 'Rows in DOM before scroll', 'rowsInDomBefore'));
    console.log(row(name, 'Rows in DOM after scroll', 'rowsInDomAfter'));
    console.log(row(name, 'Scroll height (px)', 'scrollHeightPx'));
    for (const dir of ['scrollDown', 'scrollUp']) {
      console.log(row(name, `${dir}: frames`, `${dir}.frames`));
      console.log(row(name, `${dir}: frame p50 (ms)`, `${dir}.p50`, (x) => x, 1));
      console.log(row(name, `${dir}: frame p95 (ms)`, `${dir}.p95`, (x) => x, 1));
      console.log(row(name, `${dir}: frame max (ms)`, `${dir}.max`, (x) => x, 1));
    }
    console.log(row(name, 'JS heap before scroll (MB)', 'heapBefore.jsHeapUsedMB', (x) => x, 1));
    console.log(row(name, 'JS heap after scroll (MB)', 'heapAfter.jsHeapUsedMB', (x) => x, 1));
    console.log(row(name, 'DOM nodes after scroll', 'heapAfter.domNodes'));
  } else if (name.startsWith('explore')) {
    console.log(row(name, 'Nodes explored', 'nodes'));
    console.log(row(name, 'Nodes drawn', 'drawn'));
    console.log(row(name, 'Blocked at (nodes)', 'blockedAt'));
    console.log(row(name, 'First graph (ms)', 'firstGraphMs'));
    console.log(row(name, 'Expansions', 'expansions'));
    console.log(row(name, 'Expand latency, median (ms)', 'expandLatencyMs.median'));
    console.log(row(name, 'Expand latency, max (ms)', 'expandLatencyMs.max'));
    console.log(row(name, 'Pan and zoom frame p50 (ms)', 'panZoom.p50', (x) => x, 1));
    console.log(row(name, 'Pan and zoom frame p95 (ms)', 'panZoom.p95', (x) => x, 1));
    console.log(row(name, 'Pan and zoom frame max (ms)', 'panZoom.max', (x) => x, 1));
    console.log(row(name, 'Pan and zoom frames in 5 s', 'panZoom.frames'));
    console.log(row(name, 'Idle frame p50 (ms)', 'idle.p50', (x) => x, 1));
    console.log(row(name, 'API requests', 'network.count'));
    console.log(row(name, 'Long tasks (total ms)', 'longTasks.totalMs'));
    console.log(row(name, 'JS heap (MB)', 'heap.jsHeapUsedMB', (x) => x, 1));
    console.log(row(name, 'DOM nodes', 'heap.domNodes'));
  }
}
