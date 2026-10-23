// Where entities are, for the maps of relations: the geometry literals of a set of IRIs
// read through ordinary queries (batched with `VALUES`, so they run under the caller's
// view, branch and `at`, and an entity whose geometry the caller cannot read is simply not
// located), each reduced to one position.

import type { GeoConverted, Term } from './api';
import {
  geometriesQuery,
  isGeoLiteral,
  LOCATE_BATCH,
  representativePoint,
  resolveGeometries,
  type GeoLiteral,
} from './geo';

/** Lookups in flight at once. */
export const LOCATE_IN_FLIGHT = 4;

export type LocateDeps = {
  /** Run a SELECT; `limit` is the most rows the query can return. */
  run: (sparql: string, limit: number) => Promise<Record<string, Term | undefined>[]>;
  /** Convert literals the browser cannot read (`POST /$/geo/convert`). */
  convert: (batch: GeoLiteral[]) => Promise<GeoConverted[]>;
  signal?: AbortSignal;
  /** After every batch: what is found so far and how many entities were asked about. */
  onBatch?: (found: ReadonlyMap<string, [number, number]>, done: number, total: number) => void;
};

export type Located = {
  /** IRI to [longitude, latitude]. */
  found: Map<string, [number, number]>;
  /** Entities in batches whose query failed. */
  failed: number;
  firstError?: string;
};

/**
 * The position of every IRI that has a geometry, in batches of `LOCATE_BATCH` with
 * `LOCATE_IN_FLIGHT` batches at a time. The first geometry that can be drawn stands for
 * an entity with several. A batch that fails leaves its entities out and is counted.
 */
export async function locateEntities(iris: string[], deps: LocateDeps): Promise<Located> {
  const unique = [...new Set(iris)];
  const batches: string[][] = [];
  for (let i = 0; i < unique.length; i += LOCATE_BATCH)
    batches.push(unique.slice(i, i + LOCATE_BATCH));
  const out: Located = { found: new Map(), failed: 0 };
  let next = 0;
  let done = 0;
  const worker = async () => {
    for (;;) {
      if (deps.signal?.aborted) return;
      const mine = next++;
      if (mine >= batches.length) return;
      // a batch whose answer came back full is followed here by its retry, so that a
      // worker that is still busy never leaves a retry behind for workers that have gone
      let batch: string[] | undefined = batches[mine];
      while (batch) {
        let retry: string[] = [];
        try {
          const limit = batch.length * 6;
          const rows = await deps.run(geometriesQuery(batch), limit);
          if (deps.signal?.aborted) return;
          // The limit is shared by the batch, so an entity with many geometries can take
          // every row. A full answer sends the entities it has no row for round again.
          if (rows.length >= limit) {
            const answered = new Set<string>();
            for (const r of rows) if (r.s?.type === 'uri') answered.add(r.s.value);
            const again = batch.filter((iri) => !answered.has(iri));
            if (again.length && again.length < batch.length) retry = again;
          }
          const lits: { iri: string; lit: GeoLiteral }[] = [];
          for (const r of rows) {
            const s = r.s;
            const l = r.lit;
            if (s?.type === 'uri' && isGeoLiteral(l))
              lits.push({ iri: s.value, lit: { value: l.value, datatype: l.datatype } });
          }
          const geoms = await resolveGeometries(
            lits.map((l) => l.lit),
            deps.convert,
          );
          lits.forEach((l, i) => {
            if (out.found.has(l.iri)) return;
            const g = geoms[i];
            if (!g || !('geometry' in g)) return;
            const p = representativePoint(g.geometry);
            if (p) out.found.set(l.iri, p);
          });
        } catch (e) {
          if (deps.signal?.aborted) return;
          retry = [];
          out.failed += batch.length;
          out.firstError ??= e instanceof Error ? e.message : String(e);
        }
        done += batch.length - retry.length;
        deps.onBatch?.(out.found, done, unique.length);
        batch = retry.length ? retry : undefined;
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(LOCATE_IN_FLIGHT, batches.length) }, worker));
  return out;
}
