// GeoSPARQL literals for the UI's maps: which terms are geometries, WKT and GeoJSON read
// into CRS84 GeoJSON (longitude, latitude) in the browser for the CRSs that need no
// projection library (CRS84, EPSG:4326 with its axes swapped, Web Mercator), and the
// rest, GML and KML among them, sent to `POST /$/geo/convert`. Also the queries behind
// the explorer's map card.

import type { GeoConverted, GeoJsonGeometry, Term } from './api';
import { sparqlIri, sparqlString } from './rdf';

export const GEO = 'http://www.opengis.net/ont/geosparql#';
export const GEOF = 'http://www.opengis.net/def/function/geosparql/';
export const WKT_LITERAL = `${GEO}wktLiteral`;
export const GEOJSON_LITERAL = `${GEO}geoJSONLiteral`;
export const GML_LITERAL = `${GEO}gmlLiteral`;
export const KML_LITERAL = `${GEO}kmlLiteral`;
/** The geometry literal datatypes the maps draw. */
export const GEO_LITERALS: readonly string[] = [
  WKT_LITERAL,
  GEOJSON_LITERAL,
  GML_LITERAL,
  KML_LITERAL,
];
export const WGS84_POS = 'http://www.w3.org/2003/01/geo/wgs84_pos#';
export const CRS84 = 'http://www.opengis.net/def/crs/OGC/1.3/CRS84';

/** A geometry literal: its lexical form and datatype. */
export type GeoLiteral = { value: string; datatype: string };

/** Whether a term is a `geo:wktLiteral`, `geo:geoJSONLiteral`, `geo:gmlLiteral` or
 *  `geo:kmlLiteral`. */
export function isGeoLiteral(t: Term | null | undefined): t is Term & GeoLiteral {
  return t?.type === 'literal' && !!t.datatype && GEO_LITERALS.includes(t.datatype);
}

// --- coordinate reference systems -------------------------------------------------

const OGC_CRS = 'http://www.opengis.net/def/crs/';

/** A CRS IRI in its `http://www.opengis.net/def/crs/…` form (URNs, `https`, short forms). */
export function normalizeCrs(iri: string): string {
  const s = iri.trim();
  let m = /^https?:\/\/www\.opengis\.net\/def\/crs\/(.*)$/i.exec(s);
  if (m) return OGC_CRS + m[1];
  m = /^urn:ogc:def:crs:(OGC|EPSG):([^:]*):(.+)$/i.exec(s);
  if (m) {
    const auth = m[1].toUpperCase();
    return auth === 'OGC' ? `${OGC_CRS}OGC/${m[2] || '1.3'}/${m[3]}` : `${OGC_CRS}EPSG/0/${m[3]}`;
  }
  if (/^CRS:?84$/i.test(s)) return CRS84;
  m = /^EPSG:(\d+)$/i.exec(s);
  if (m) return `${OGC_CRS}EPSG/0/${m[1]}`;
  return s;
}

/**
 * How a CRS's coordinates become longitude and latitude: as written, swapped
 * (latitude first), the inverse Web Mercator, or not here (null: the server converts).
 */
export function crsAxes(iri: string): 'lonlat' | 'latlon' | 'mercator' | null {
  const c = normalizeCrs(iri);
  if (c === CRS84 || c === `${OGC_CRS}OGC/0/CRS84` || /\/OGC\/[^/]+\/CRS84h?$/.test(c))
    return 'lonlat';
  // the legacy GeoSPARQL 1.0 form without a version is longitude first, as in Jena
  if (c === `${OGC_CRS}EPSG/4326`) return 'lonlat';
  if (/\/EPSG\/[^/]+\/(4326|4979)$/.test(c)) return 'latlon';
  if (/\/EPSG\/[^/]+\/(3857|900913)$/.test(c)) return 'mercator';
  return null;
}

/** A CRS IRI in short: `CRS84`, `CRS84h`, `EPSG:4326`, or the IRI itself. */
export function crsLabel(iri: string): string {
  const c = normalizeCrs(iri);
  const m = /\/def\/crs\/(?:OGC\/[^/]+\/(CRS84h?)|EPSG\/(?:0\/)?(\d+))$/.exec(c);
  if (!m) return iri;
  if (m[1]) return m[1];
  // the legacy form without a version is longitude first: not the same as EPSG:4326
  return c.endsWith(`EPSG/${m[2]}`) ? `EPSG:${m[2]} (lon, lat)` : `EPSG:${m[2]}`;
}

const R = 6378137;
const DEG = 180 / Math.PI;

/** Web Mercator metres to longitude and latitude. */
export function inverseMercator(x: number, y: number): [number, number] {
  return [(x / R) * DEG, (2 * Math.atan(Math.exp(y / R)) - Math.PI / 2) * DEG];
}

type Position = number[];

/** Apply `f` to every position of a geometry. */
function mapPositions(g: GeoJsonGeometry, f: (p: Position) => Position): GeoJsonGeometry {
  if (g.type === 'GeometryCollection')
    return { type: g.type, geometries: (g.geometries ?? []).map((x) => mapPositions(x, f)) };
  const walk = (c: unknown): unknown =>
    Array.isArray(c) && typeof c[0] === 'number' ? f(c as Position) : (c as unknown[]).map(walk);
  return { type: g.type, coordinates: walk(g.coordinates) };
}

// --- WKT ------------------------------------------------------------------------------

/** A WKT syntax error; `offset` is in the text after the CRS IRI. */
export class WktError extends Error {
  constructor(
    message: string,
    public offset: number,
  ) {
    super(message);
  }
}

/** Types read into GeoJSON: LINEARRING as a line, TRIANGLE as a polygon, TIN and
 * POLYHEDRALSURFACE as multipolygons (as the server reads them). */
const WKT_TYPES: Record<string, string> = {
  POINT: 'Point',
  LINESTRING: 'LineString',
  LINEARRING: 'LineString',
  POLYGON: 'Polygon',
  TRIANGLE: 'Polygon',
  MULTIPOINT: 'MultiPoint',
  MULTILINESTRING: 'MultiLineString',
  MULTIPOLYGON: 'MultiPolygon',
  POLYHEDRALSURFACE: 'MultiPolygon',
  TIN: 'MultiPolygon',
  GEOMETRYCOLLECTION: 'GeometryCollection',
};

class WktReader {
  i = 0;
  constructor(private s: string) {}

  fail(what: string): never {
    throw new WktError(`expected ${what}`, this.i);
  }
  ws() {
    while (this.i < this.s.length && /\s/.test(this.s[this.i])) this.i++;
  }
  peek(): string {
    this.ws();
    return this.s[this.i] ?? '';
  }
  eat(c: string): boolean {
    if (this.peek() !== c) return false;
    this.i++;
    return true;
  }
  expect(c: string) {
    if (!this.eat(c)) this.fail(`'${c}'`);
  }
  word(): string {
    this.ws();
    const m = /^[A-Za-z]+/.exec(this.s.slice(this.i));
    if (!m) return '';
    this.i += m[0].length;
    return m[0].toUpperCase();
  }
  /** `EMPTY` (consumed) or not. */
  empty(): boolean {
    const at = this.i;
    if (this.word() === 'EMPTY') return true;
    this.i = at;
    return false;
  }
  number(): number {
    this.ws();
    const m = /^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?/.exec(this.s.slice(this.i));
    if (!m) this.fail('a number');
    this.i += m[0].length;
    return Number(m[0]);
  }
  /** One position: 2 to 4 numbers, the first two kept. */
  position(): Position {
    const p = [this.number(), this.number()];
    for (let k = 0; k < 2 && /[-+.\d]/.test(this.peek()); k++) this.number();
    return p;
  }
  /** `(p, p, …)`. */
  positions(): Position[] {
    this.expect('(');
    const out = [this.position()];
    while (this.eat(',')) out.push(this.position());
    this.expect(')');
    return out;
  }
  /** `(x, x, …)` of `item`, EMPTY members dropped. */
  list<T>(item: () => T | null): T[] {
    this.expect('(');
    const out: T[] = [];
    do {
      const x = this.empty() ? null : item();
      if (x != null) out.push(x);
    } while (this.eat(','));
    this.expect(')');
    return out;
  }
  /** A tagged geometry; null when EMPTY. */
  geometry(): GeoJsonGeometry | null {
    const start = this.i;
    const w = this.word();
    // `POINTZ(…)`, `POINT Z (…)`, `POINT ZM EMPTY`
    const type = WKT_TYPES[w] ?? WKT_TYPES[w.replace(/(ZM|Z|M)$/, '')];
    if (!type) {
      this.i = start;
      this.fail('a geometry type');
    }
    const at = this.i;
    if (!['Z', 'M', 'ZM'].includes(this.word())) this.i = at;
    if (this.empty()) return null;
    const ring = () => this.positions();
    const polygon = () => this.list(ring);
    switch (type) {
      case 'Point': {
        this.expect('(');
        const p = this.position();
        this.expect(')');
        return { type, coordinates: p };
      }
      case 'LineString':
        return { type, coordinates: this.positions() };
      case 'Polygon':
        return { type, coordinates: polygon() };
      case 'MultiPoint':
        // both `MULTIPOINT((1 2), (3 4))` and `MULTIPOINT(1 2, 3 4)`
        return {
          type,
          coordinates: this.list(() => {
            if (!this.eat('(')) return this.position();
            const p = this.position();
            this.expect(')');
            return p;
          }),
        };
      case 'MultiLineString':
        return { type, coordinates: this.list(ring) };
      case 'MultiPolygon':
        return { type, coordinates: this.list(polygon) };
      case 'GeometryCollection':
        return { type, geometries: this.list(() => this.geometry()) };
    }
    return this.fail('a geometry type');
  }
}

/** A WKT body (no CRS IRI) as a GeoJSON geometry in its own axis order; null when empty. */
export function parseWkt(text: string): GeoJsonGeometry | null {
  const r = new WktReader(text);
  if (!text.trim()) return null;
  const g = r.geometry();
  if (r.peek() !== '') r.fail('the end of the literal');
  return g;
}

/** The CRS IRI (normalized; CRS84 when absent) and the WKT after it. */
export function splitWkt(lex: string): { crs: string; body: string } {
  const m = /^\s*<([^>]*)>\s*/.exec(lex);
  return m ? { crs: normalizeCrs(m[1]), body: lex.slice(m[0].length) } : { crs: CRS84, body: lex };
}

// --- GeoJSON -----------------------------------------------------------------------------

const GEOJSON_TYPES = new Set([
  'Point',
  'LineString',
  'Polygon',
  'MultiPoint',
  'MultiLineString',
  'MultiPolygon',
  'GeometryCollection',
]);

/** Depth of the position arrays of each GeoJSON type. */
const DEPTH: Record<string, number> = {
  Point: 0,
  LineString: 1,
  MultiPoint: 1,
  Polygon: 2,
  MultiLineString: 2,
  MultiPolygon: 3,
};

function checkCoords(c: unknown, depth: number): boolean {
  if (!Array.isArray(c)) return false;
  if (depth === 0) return c.length >= 2 && c.every((x) => typeof x === 'number' && isFinite(x));
  return c.every((x) => checkCoords(x, depth - 1));
}

function isEmpty(g: GeoJsonGeometry): boolean {
  if (g.type === 'GeometryCollection') return (g.geometries ?? []).every(isEmpty);
  return Array.isArray(g.coordinates) && g.coordinates.length === 0;
}

/** A GeoJSON geometry (a Feature's geometry too), positions cut to 2D; null when empty. */
export function parseGeoJson(text: string): GeoJsonGeometry | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new Error('not JSON');
  }
  const geom = (x: unknown): GeoJsonGeometry => {
    const o = x as Record<string, unknown> | null;
    if (!o || typeof o !== 'object') throw new Error('not a GeoJSON geometry');
    if (o.type === 'Feature') return geom(o.geometry);
    if (typeof o.type !== 'string' || !GEOJSON_TYPES.has(o.type))
      throw new Error(`not a GeoJSON geometry type: ${String(o.type)}`);
    if (o.type === 'GeometryCollection') {
      if (!Array.isArray(o.geometries)) throw new Error('GeometryCollection without geometries');
      return { type: o.type, geometries: o.geometries.map(geom) };
    }
    const empty = Array.isArray(o.coordinates) && o.coordinates.length === 0;
    if (!empty && !checkCoords(o.coordinates, DEPTH[o.type]))
      throw new Error(`bad coordinates for ${o.type}`);
    return { type: o.type, coordinates: o.coordinates };
  };
  const g = mapPositions(geom(v), (p) => [p[0], p[1]]);
  return isEmpty(g) ? null : g;
}

// --- literals to drawable geometries -------------------------------------------------

/** A literal read in the browser: a CRS84 geometry, one for the server, or why not. */
export type LocalGeometry =
  | { geometry: GeoJsonGeometry }
  | { convert: true; why: string }
  | { error: string };

/** Read a geometry literal for the map (see `LocalGeometry`). */
export function localGeometry(lit: GeoLiteral): LocalGeometry {
  if (lit.datatype === GEOJSON_LITERAL) {
    try {
      const g = parseGeoJson(lit.value);
      return g ? { geometry: g } : { error: 'empty geometry' };
    } catch (e) {
      return { error: `malformed GeoJSON: ${(e as Error).message}` };
    }
  }
  // the server reads GML and KML
  if (lit.datatype === GML_LITERAL) return { convert: true, why: 'GML' };
  if (lit.datatype === KML_LITERAL) return { convert: true, why: 'KML' };
  if (lit.datatype !== WKT_LITERAL) return { error: 'not a geometry literal' };
  const { crs, body } = splitWkt(lit.value);
  let g: GeoJsonGeometry | null;
  try {
    g = parseWkt(body);
  } catch (e) {
    // the server reads more of WKT than this reader (and says what is wrong)
    return { convert: true, why: (e as Error).message };
  }
  if (!g) return { error: 'empty geometry' };
  switch (crsAxes(crs)) {
    case 'lonlat':
      return { geometry: g };
    case 'latlon':
      return { geometry: mapPositions(g, ([lat, lon]) => [lon, lat]) };
    case 'mercator':
      return { geometry: mapPositions(g, ([x, y]) => inverseMercator(x, y)) };
    default:
      return { convert: true, why: `CRS ${crs}` };
  }
}

/** The geometry to draw for a literal, or why there is none. */
export type Resolved = { geometry: GeoJsonGeometry } | { error: string };

/** Most literals one `POST /$/geo/convert` takes. */
export const CONVERT_MAX = 10_000;

/**
 * Every literal as a CRS84 geometry: read in the browser where possible, the others
 * through `convert` (`POST /$/geo/convert`, in requests of at most `CONVERT_MAX`).
 * A failed conversion request leaves its literals undrawn, with the error.
 */
export async function resolveGeometries(
  lits: GeoLiteral[],
  convert: (batch: GeoLiteral[]) => Promise<GeoConverted[]>,
): Promise<Resolved[]> {
  const out: Resolved[] = new Array(lits.length);
  const remote: number[] = [];
  const seen = new Map<string, number>();
  const dups: [number, number][] = [];
  lits.forEach((lit, i) => {
    const key = `${lit.datatype}\n${lit.value}`;
    const first = seen.get(key);
    if (first != null) return dups.push([i, first]);
    seen.set(key, i);
    const r = localGeometry(lit);
    if ('convert' in r) remote.push(i);
    else out[i] = r;
  });
  for (let k = 0; k < remote.length; k += CONVERT_MAX) {
    const batch = remote.slice(k, k + CONVERT_MAX);
    try {
      const res = await convert(
        batch.map((i) => ({ value: lits[i].value, datatype: lits[i].datatype })),
      );
      batch.forEach((i, j) => {
        const r = res[j];
        out[i] =
          r && 'geometry' in r && r.geometry
            ? { geometry: r.geometry }
            : { error: (r && 'error' in r && r.error) || 'not converted' };
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      for (const i of batch) out[i] = { error: `cannot convert: ${msg}` };
    }
  }
  for (const [i, first] of dups) out[i] = out[first];
  return out;
}

// --- result rows ------------------------------------------------------------------------

/** The columns of a result that hold at least one geometry literal. */
export function geoColumns(vars: string[], rows: (Term | null)[][]): string[] {
  return vars.filter((_, c) => rows.some((r) => isGeoLiteral(r[c])));
}

/** One geometry literal of a result: its row and column. */
export type GeoCell = { row: number; column: string; literal: GeoLiteral };

/** The geometry literals of the given columns, row by row. */
export function geoCells(vars: string[], rows: (Term | null)[][], columns: string[]): GeoCell[] {
  const idx = columns.map((c) => [c, vars.indexOf(c)] as const).filter(([, i]) => i >= 0);
  const out: GeoCell[] = [];
  rows.forEach((r, row) => {
    for (const [column, i] of idx) {
      const t = r[i];
      if (isGeoLiteral(t))
        out.push({ row, column, literal: { value: t.value, datatype: t.datatype } });
    }
  });
  return out;
}

/** The longitude/latitude box of geometries, [west, south, east, north]; null if none. */
export function bounds(geoms: GeoJsonGeometry[]): [number, number, number, number] | null {
  let w = Infinity;
  let s = Infinity;
  let e = -Infinity;
  let n = -Infinity;
  for (const g of geoms)
    mapPositions(g, (p) => {
      w = Math.min(w, p[0]);
      e = Math.max(e, p[0]);
      s = Math.min(s, p[1]);
      n = Math.max(n, p[1]);
      return p;
    });
  return w <= e ? [w, s, e, n] : null;
}

/** A short text of a geometry literal (the start of its lexical form). */
export function abbreviate(value: string, max = 60): string {
  const s = value.replace(/\s+/g, ' ').trim();
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

// --- explorer queries ---------------------------------------------------------------------

/**
 * The geometry literals of a resource: its own (a geometry), those of its geometries
 * (a feature, through geo:hasDefaultGeometry or geo:hasGeometry), and a point from W3C
 * Basic Geo latitude and longitude. Variables: `?geom` (the geometry node), `?lit`.
 */
export function geometryQuery(iri: string, limit = 20): string {
  const s = sparqlIri(iri);
  const isGeo = `FILTER(isLiteral(?lit) && DATATYPE(?lit) IN (${GEO_LITERALS.map((d) => `<${d}>`).join(', ')}))`;
  return `SELECT ?geom ?lit WHERE {
  { BIND(${s} AS ?geom) ${s} ?p ?lit . ${isGeo} }
  UNION
  { ${s} <${GEO}hasDefaultGeometry>|<${GEO}hasGeometry> ?geom . ?geom ?p ?lit . ${isGeo} }
  UNION
  { BIND(${s} AS ?geom) ${s} <${WGS84_POS}lat> ?lat ; <${WGS84_POS}long> ?long .
    BIND(STRDT(CONCAT("POINT(", STR(?long), " ", STR(?lat), ")"), <${WKT_LITERAL}>) AS ?lit) }
}
LIMIT ${limit}`;
}

/** A geometry literal as a SPARQL constant. */
export function sparqlGeoLiteral(lit: GeoLiteral): string {
  return `${sparqlString(lit.value)}^^<${lit.datatype}>`;
}

/**
 * Features within `radiusKm` of a geometry (`spatial:nearbyGeom`), nearest first, the
 * resource itself left out. Variables: `?f`, `?label`, `?dist` (kilometres, when the
 * feature has a geometry literal), `?lit` (that literal).
 */
export function nearbyQuery(iri: string, lit: GeoLiteral, radiusKm: number, limit = 50): string {
  const g = sparqlGeoLiteral(lit);
  return `PREFIX spatial: <http://jena.apache.org/spatial#>
PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/>
SELECT ?f (SAMPLE(?l) AS ?label) (MIN(?d) AS ?dist) (SAMPLE(?w) AS ?lit) WHERE {
  ?f spatial:nearbyGeom (${g} ${radiusKm} uom:kilometre ${limit}) .
  FILTER(?f != ${sparqlIri(iri)})
  OPTIONAL { ?f <http://www.w3.org/2000/01/rdf-schema#label> ?l }
  OPTIONAL {
    ?f <${GEO}hasDefaultGeometry>|<${GEO}hasGeometry> ?gm .
    ?gm <${GEO}asWKT>|<${GEO}asGeoJSON>|<${GEO}asGML>|<${GEO}asKML> ?w .
    BIND(<${GEOF}distance>(?w, ${g}, uom:kilometre) AS ?d)
  }
}
GROUP BY ?f
ORDER BY DESC(BOUND(?dist)) ?dist ?f`;
}
