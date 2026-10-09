// One process of the binding comparison (scripts/bench-bindings/bench.py) for Node.js.
//
//   node runner.mjs CONFIG.json
//
// Engines:
//
// * `sparkles`: the @sparkles-rdf/engine package (`Dataset.memory()`), from `engine_module`;
// * `oxigraph`: Oxigraph's JavaScript package (`oxigraph.Store`, WebAssembly, in memory);
// * `n3`: N3.js's `Store`, with Comunica (`@comunica/query-sparql-rdfjs`) over it for SPARQL.
//
// The configuration and the JSON lines written to standard output are those of the JVM
// runner (jvm/sparkles-jena/src/test/kotlin/io/github/kclejeune/sparkles/jena/bench/BindingsBench.kt).
// Every row and term of a result is consumed. The incumbent libraries come from
// `deps_dir`, where the driver installs them with `npm ci` from this directory's lockfile.

import { readFileSync, createWriteStream, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';

const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const FOAF_NAME = 'http://xmlns.com/foaf/0.1/name';
const EX = 'http://example.org/';
const XSD_BOOLEAN = 'http://www.w3.org/2001/XMLSchema#boolean';

const cfg = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const engine = cfg.engine;
const emit = (o) => process.stdout.write(JSON.stringify(o) + '\n');

class Timeout extends Error {}

function encode(t) {
  if (!t) return null;
  switch (t.termType) {
    case 'NamedNode':
      return { type: 'uri', value: t.value };
    case 'BlankNode':
      return { type: 'bnode', value: t.value };
    case 'Literal':
      return t.language
        ? { type: 'literal', value: t.value, 'xml:lang': t.language }
        : { type: 'literal', value: t.value, datatype: t.datatype.value };
    default:
      return { type: 'other', value: String(t.value) };
  }
}

function touch(t) {
  if (!t) return 0;
  if (t.termType === 'Literal') return t.value.length + t.datatype.value.length + t.language.length;
  return t.value.length;
}

class Sink {
  constructor(out) {
    this.out = out;
    this.rows = 0;
    this.acc = 0;
  }
  row(terms) {
    this.rows++;
    if (!this.out) {
      for (const t of terms) this.acc += touch(t);
    } else this.out.write(JSON.stringify(terms.map(encode)) + '\n');
  }
  flag(b) {
    this.rows++;
    if (this.out)
      this.out.write(JSON.stringify([{ type: 'literal', value: String(b), datatype: XSD_BOOLEAN }]) + '\n');
  }
}

// ------------------------------------------------------------------------------ engines

const deps = createRequire(join(cfg.deps_dir, 'package.json'));
const versions = { node: process.version, v8: process.versions.v8 };
const depVersion = (name) =>
  JSON.parse(readFileSync(deps.resolve(`${name}/package.json`), 'utf8')).version;

async function makeEngine() {
  if (engine === 'sparkles') {
    const m = await import(pathToFileURL(cfg.engine_module).href);
    const { Dataset, factory } = m;
    versions.sparkles = 'workspace';
    let ds;
    return {
      factory,
      async load() {
        ds = Dataset.memory();
        await ds.loadFiles([cfg.data]);
      },
      async size() {
        return Number(await ds.count());
      },
      async query(text, vars, sink, deadline) {
        const opts = { noCache: true, timeout: Math.max(1, Math.floor(deadline - performance.now())) };
        const r = await ds.query(text, opts);
        if (r.type === 'boolean') return sink.flag(r.value);
        let i = 0;
        for await (const b of r) {
          sink.row(vars.map((v) => b.get(v)));
          if ((++i & 1023) === 0 && performance.now() > deadline) {
            await r.close();
            throw new Timeout();
          }
        }
      },
      async match(sink, s, p, o, deadline) {
        let i = 0;
        for await (const q of ds.match(s, p, o, null)) {
          sink.row([q.subject, q.predicate, q.object]);
          if ((++i & 65535) === 0 && performance.now() > deadline) throw new Timeout();
        }
      },
      async contains(q) {
        return ds.has(q);
      },
      async value(s, p) {
        for await (const q of ds.match(s, p, null, null)) return q.object;
        return null;
      },
      async adds(quads) {
        await ds.transaction(async (tx) => {
          for (const q of quads) await tx.add(q);
        });
      },
      async addsBulk(quads) {
        await ds.addAll(quads);
      },
    };
  }
  if (engine === 'oxigraph') {
    const ox = deps('oxigraph');
    versions.oxigraph = depVersion('oxigraph');
    let store;
    return {
      factory: ox,
      async load() {
        store = new ox.Store();
        store.load(readFileSync(cfg.data, 'utf8'), { format: 'application/n-triples' });
      },
      async size() {
        return store.size;
      },
      async query(text, vars, sink) {
        const r = store.query(text);
        if (typeof r === 'boolean') return sink.flag(r);
        for (const m of r) sink.row(vars.map((v) => m.get(v)));
      },
      async match(sink, s, p, o) {
        for (const q of store.match(s, p, o, null)) sink.row([q.subject, q.predicate, q.object]);
      },
      async contains(q) {
        return store.has(q);
      },
      async value(s, p) {
        const r = store.match(s, p, null, null);
        return r.length ? r[0].object : null;
      },
      // Oxigraph's JavaScript Store has no transaction over several adds: each add is
      // its own
      async adds(quads) {
        for (const q of quads) store.add(q);
      },
      addsBulk: null,
    };
  }
  if (engine === 'n3') {
    const N3 = deps('n3');
    const { QueryEngine } = deps('@comunica/query-sparql-rdfjs');
    versions.n3 = depVersion('n3');
    versions.comunica = depVersion('@comunica/query-sparql-rdfjs');
    const comunica = new QueryEngine();
    let store;
    return {
      factory: N3.DataFactory,
      async load() {
        store = new N3.Store();
        const parser = new N3.Parser({ format: 'N-Triples' });
        store.addQuads(parser.parse(readFileSync(cfg.data, 'utf8')));
      },
      async size() {
        return store.size;
      },
      async query(text, vars, sink, deadline) {
        if (/^\s*(PREFIX[^\n]*?)*\s*ASK\b/i.test(text) || /\bASK\s*\{/i.test(text)) {
          return sink.flag(await comunica.queryBoolean(text, { sources: [store] }));
        }
        const bs = await comunica.queryBindings(text, { sources: [store] });
        let i = 0;
        for await (const b of bs) {
          sink.row(vars.map((v) => b.get(v)));
          if ((++i & 1023) === 0 && performance.now() > deadline) {
            bs.destroy();
            throw new Timeout();
          }
        }
      },
      async match(sink, s, p, o) {
        for (const q of store.match(s, p, o, null)) sink.row([q.subject, q.predicate, q.object]);
      },
      async contains(q) {
        return store.has(q);
      },
      async value(s, p) {
        for (const q of store.match(s, p, null, null)) return q.object;
        return null;
      },
      // N3.js's Store has no transactions
      async adds(quads) {
        for (const q of quads) store.addQuad(q);
      },
      async addsBulk(quads) {
        store.addQuads(quads);
      },
    };
  }
  throw new Error(`unknown engine ${engine}`);
}

// --------------------------------------------------------------------------------- cases

const eng = await makeEngine();
emit({ event: 'versions', versions });
const f = eng.factory;
const iri = (s) => f.namedNode(s);
const subjects = cfg.subjects.map(iri);
const probes = cfg.probes.map(([s, p, o]) => f.quad(iri(s), iri(p), iri(o), f.defaultGraph()));
const name = iri(FOAF_NAME);
let addRound = 0;

async function run(c, sink, deadline) {
  switch (c.kind) {
    case 'query':
      await eng.query(c.query, c.vars, sink, deadline);
      return 1;
    case 'iter-all':
      await eng.match(sink, null, null, null, deadline);
      return 1;
    case 'pattern-s':
      for (const s of subjects) await eng.match(sink, s, null, null, deadline);
      return subjects.length;
    case 'pattern-po':
      await eng.match(sink, null, iri(RDF_TYPE), iri(EX + 'Researcher'), deadline);
      return 1;
    case 'contains':
      for (const q of probes) sink.flag(await eng.contains(q));
      return probes.length;
    case 'value':
      for (const s of subjects) sink.row([await eng.value(s, name)]);
      return subjects.length;
    case 'adds':
    case 'adds-bulk': {
      const fn = c.kind === 'adds' ? eng.adds : eng.addsBulk;
      const r = addRound++;
      const p = iri(EX + 'bench/p');
      const quads = [];
      for (let i = 0; i < cfg.add_count; i++)
        quads.push(
          f.quad(iri(`${EX}bench/add/${engine}/${c.kind}/${r}/${i}`), p, f.literal(`v${i}`), f.defaultGraph()),
        );
      await fn(quads);
      sink.rows += cfg.add_count;
      return cfg.add_count;
    }
    default:
      throw new Error(`unknown case kind ${c.kind}`);
  }
}

/** Runs `fn(deadline)` with the case timeout. Synchronous work that does not return is
 * left to the driver's watchdog, which resumes after the case. */
async function bounded(fn) {
  const ms = (cfg.timeout_s ?? 300) * 1000;
  const deadline = performance.now() + ms;
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Timeout()), ms + 1000);
  });
  try {
    return await Promise.race([fn(deadline), timeout]);
  } finally {
    clearTimeout(timer);
  }
}

const skip = new Set(cfg.skip ?? []);

async function answers() {
  mkdirSync(cfg.answers_dir, { recursive: true });
  for (const c of cfg.cases) {
    if (skip.has(c.name) || c.kind.startsWith('adds')) continue;
    emit({ event: 'start', case: c.name });
    const rec = { event: 'case', case: c.name };
    const out = createWriteStream(join(cfg.answers_dir, c.name.replace(':', '_') + '.jsonl'));
    try {
      out.write(JSON.stringify({ vars: c.vars ?? [] }) + '\n');
      const sink = new Sink(out);
      const t = performance.now();
      await bounded((d) => run(c, sink, d));
      Object.assign(rec, { status: 'ok', rows: sink.rows, ms: performance.now() - t });
    } catch (e) {
      Object.assign(rec, e instanceof Timeout ? { status: 'timeout' } : { status: 'error', error: String(e).slice(0, 300) });
    }
    await new Promise((resolve) => out.end(resolve));
    emit(rec);
  }
}

let consumed = 0;
async function timing() {
  const warmup = cfg.warmup ?? 2;
  const runs = cfg.runs ?? 10;
  const budget = (cfg.budget_s ?? 120) * 1000;
  for (const c of cfg.cases) {
    if (skip.has(c.name)) continue;
    emit({ event: 'start', case: c.name });
    const rec = { event: 'case', case: c.name };
    if (c.kind === 'adds-bulk' && !eng.addsBulk) {
      emit({ ...rec, status: 'unsupported', samples_ms: [] });
      continue;
    }
    const warm = [];
    const samples = [];
    let rows = -1;
    let ops = 1;
    const begin = performance.now();
    try {
      let i = 0;
      while (i < warmup + runs) {
        const sink = new Sink(null);
        if (globalThis.gc) globalThis.gc();
        const t = performance.now();
        ops = await bounded((d) => run(c, sink, d));
        const ms = performance.now() - t;
        consumed += sink.acc;
        rows = sink.rows;
        (i < warmup ? warm : samples).push(ms);
        emit({ event: 'tick', case: c.name });
        i++;
        // past its budget, a case skips the rest of its warm-up and stops after its
        // first measured sample
        if (performance.now() - begin > budget) {
          if (samples.length) break;
          i = Math.max(i, warmup);
        }
      }
      rec.status = 'ok';
    } catch (e) {
      Object.assign(rec, e instanceof Timeout ? { status: 'timeout' } : { status: 'error', error: String(e).slice(0, 300) });
    }
    Object.assign(rec, { rows, ops, warmup_ms: warm, samples_ms: samples });
    emit(rec);
  }
}

try {
  const t = performance.now();
  await eng.load();
  const ms = performance.now() - t;
  if (cfg.mode === 'load') {
    emit({ event: 'case', case: 'load', status: 'ok', rows: await eng.size(), samples_ms: [ms] });
  } else {
    emit({ event: 'ready', ms, quads: await eng.size() });
    await (cfg.mode === 'answers' ? answers() : timing());
  }
} catch (e) {
  emit({ event: 'fatal', error: String(e?.stack ?? e).slice(0, 500) });
  process.exit(2);
}
emit({ event: 'done', sink: consumed });
process.exit(0);
