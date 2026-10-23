/**
 * ForceAtlas2 in a web worker (spec U06 §4.2), so the page stays responsive while a large
 * picture settles. The page sends the nodes' starting positions, sizes and fixed flags
 * and the edges as index pairs. The worker iterates in slices of about 12 ms, answers a
 * `stop` between slices, and sends positions as a transferable `Float32Array` of x, y
 * pairs at most 15 times a second, then once more when it ends. With `final` it sends
 * only the last positions, which reduced motion asks for.
 */
import iterate from 'graphology-layout-forceatlas2/iterate.js';

export type LayoutStart = {
  type: 'start';
  id: number;
  x: Float32Array;
  y: Float32Array;
  size: Float32Array;
  fixed: Uint8Array;
  /** Index pairs. */
  edges: Uint32Array;
  /** One weight per edge pair. */
  weights: Float32Array;
  iterations: number;
  final: boolean;
};
export type LayoutMessage = LayoutStart | { type: 'stop' };
export type LayoutTick = {
  type: 'tick' | 'done';
  id: number;
  /** x, y pairs in node order. */
  positions: Float32Array;
  progress: number;
};

const PPN = 10;
const PPE = 3;

let current = 0;

function settings(n: number) {
  return {
    linLogMode: false,
    outboundAttractionDistribution: false,
    adjustSizes: false,
    edgeWeightInfluence: 0.5,
    scalingRatio: n > 2000 ? 10 : 6,
    strongGravityMode: false,
    gravity: 1,
    slowDown: 1 + Math.log(Math.max(1, n)),
    barnesHutOptimize: n > 2000,
    barnesHutTheta: 0.6,
  };
}

function positions(nodes: Float32Array, n: number): Float32Array {
  const out = new Float32Array(n * 2);
  for (let i = 0; i < n; i++) {
    out[i * 2] = nodes[i * PPN];
    out[i * 2 + 1] = nodes[i * PPN + 1];
  }
  return out;
}

function start(m: LayoutStart) {
  const id = m.id;
  current = id;
  const n = m.x.length;
  const nodes = new Float32Array(n * PPN);
  for (let i = 0; i < n; i++) {
    const j = i * PPN;
    nodes[j] = m.x[i];
    nodes[j + 1] = m.y[i];
    nodes[j + 6] = 1;
    nodes[j + 7] = 1;
    nodes[j + 8] = m.size[i] || 1;
    nodes[j + 9] = m.fixed[i] ? 1 : 0;
  }
  const e = m.edges.length / 2;
  const edges = new Float32Array(e * PPE);
  for (let k = 0; k < e; k++) {
    const s = m.edges[k * 2];
    const t = m.edges[k * 2 + 1];
    const w = m.weights[k] || 1;
    edges[k * PPE] = s * PPN;
    edges[k * PPE + 1] = t * PPN;
    edges[k * PPE + 2] = w;
    nodes[s * PPN + 6] += w;
    nodes[t * PPN + 6] += w;
  }
  const opts = settings(n);
  let done = 0;
  let sent = 0;
  const post = (type: 'tick' | 'done') => {
    const p = positions(nodes, n);
    const msg: LayoutTick = { type, id, positions: p, progress: done / m.iterations };
    (self as unknown as Worker).postMessage(msg, [p.buffer]);
  };
  const slice = () => {
    if (current !== id) return;
    const until = performance.now() + 12;
    while (done < m.iterations && performance.now() < until) {
      if (n > 0) iterate(opts, nodes, edges);
      done++;
    }
    if (done >= m.iterations) {
      post('done');
      return;
    }
    const now = performance.now();
    if (!m.final && now - sent >= 66) {
      sent = now;
      post('tick');
    }
    setTimeout(slice, 0);
  };
  slice();
}

self.onmessage = (ev: MessageEvent<LayoutMessage>) => {
  const m = ev.data;
  if (m.type === 'stop') current = 0;
  else start(m);
};
