// Mock of the GeoSPARQL endpoints for UI development (docs/API.md, "GeoSPARQL"): the
// spatial index status and its tasks (`/$/geo/{ds}` GET, PUT, DELETE, `/rebuild`), the
// map box query (`GET /{ds}/geo`), the conversion of literals (`POST /$/geo/convert`)
// and `spatial:nearbyGeom` queries (geoQuery, called by the query endpoint).
//
// Seeded on the first request: a `places` dataset of a few Paris sights and cities with
// their geometries in CRS84, EPSG:4326 (latitude first), GeoJSON, a UTM zone (which only
// the server converts) and one malformed literal, with the index enabled. Geometries are
// read just enough for the mock: every coordinate pair of the literal, and its centroid.

import ox from 'oxigraph';

const GEO = 'http://www.opengis.net/ont/geosparql#';
const WKT = `${GEO}wktLiteral`;
const GEOJSON = `${GEO}geoJSONLiteral`;
const SERIALIZATIONS = [`${GEO}asWKT`, `${GEO}asGeoJSON`, `${GEO}hasSerialization`];
const LINKS = [`${GEO}hasDefaultGeometry`, `${GEO}hasGeometry`];
const CRS84 = 'http://www.opengis.net/def/crs/OGC/1.3/CRS84';
const EPSG4326 = 'http://www.opengis.net/def/crs/EPSG/0/4326';
const UTM31N = 'http://www.opengis.net/def/crs/EPSG/0/32631';
const RDFS_LABEL = 'http://www.w3.org/2000/01/rdf-schema#label';
const XSD_DOUBLE = 'http://www.w3.org/2001/XMLSchema#double';

/** The one UTM literal of the seed and its position (only the real server projects). */
const UTM_EIFFEL = `<${UTM31N}> POINT(448252 5411935)`;
const CONVERTED = new Map([[UTM_EIFFEL, [2.2945, 48.8584]]]);

export const PLACES = `@prefix ex: <http://example.org/places/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

ex:paris a ex:City ; rdfs:label "Paris" ; geo:hasDefaultGeometry ex:parisGeom .
ex:parisGeom geo:asWKT "POINT(2.3522 48.8566)"^^geo:wktLiteral .
ex:lyon a ex:City ; rdfs:label "Lyon" ; geo:hasDefaultGeometry ex:lyonGeom .
ex:lyonGeom geo:asWKT "POINT(4.8357 45.764)"^^geo:wktLiteral .
ex:louvre a ex:Museum ; rdfs:label "Louvre" ; geo:hasGeometry ex:louvreGeom .
ex:louvreGeom geo:asWKT "<${EPSG4326}> POINT(48.8606 2.3376)"^^geo:wktLiteral .
ex:eiffel a ex:Monument ; rdfs:label "Eiffel Tower" ; geo:hasGeometry ex:eiffelGeom .
ex:eiffelGeom geo:asWKT "${UTM_EIFFEL}"^^geo:wktLiteral .
ex:seine a ex:River ; rdfs:label "Seine" ; geo:hasGeometry ex:seineGeom .
ex:seineGeom geo:asGeoJSON "{\\"type\\":\\"LineString\\",\\"coordinates\\":[[2.25,48.84],[2.3,48.86],[2.36,48.85]]}"^^geo:geoJSONLiteral .
ex:bois a ex:Park ; rdfs:label "Bois de Boulogne" ; geo:hasGeometry ex:boisGeom .
ex:boisGeom geo:asWKT "POLYGON((2.23 48.85, 2.27 48.85, 2.27 48.88, 2.23 48.88, 2.23 48.85))"^^geo:wktLiteral .
ex:atlantis a ex:City ; rdfs:label "Atlantis" ; geo:hasGeometry ex:atlantisGeom .
ex:atlantisGeom geo:asWKT "POINT(1)"^^geo:wktLiteral .
`;

let seeded = false;

/** A literal's CRS, its coordinate pairs in longitude/latitude, or an error. */
function read(value, datatype) {
  if (datatype === GEOJSON) {
    try {
      const nums = JSON.stringify(JSON.parse(value).coordinates ?? []).match(/-?[\d.]+(e-?\d+)?/gi);
      return { crs: CRS84, points: pairs((nums ?? []).map(Number)) };
    } catch {
      return { crs: CRS84, error: 'malformed geoJSONLiteral' };
    }
  }
  const m = /^\s*<([^>]*)>\s*/.exec(value);
  const crs = m ? m[1] : CRS84;
  const body = value.slice(m ? m[0].length : 0);
  if (CONVERTED.has(value)) return { crs, points: [CONVERTED.get(value)] };
  const nums = (body.match(/-?\d+(\.\d+)?(e-?\d+)?/gi) ?? []).map(Number);
  if (!/^\s*[A-Za-z]+\s*\(/.test(body) || nums.length < 2 || nums.length % 2)
    return { crs, error: 'malformed wktLiteral at offset 0' };
  const ps = pairs(nums);
  if (crs === EPSG4326) return { crs, points: ps.map(([a, b]) => [b, a]) };
  if (crs !== CRS84) return { crs, error: `unknown CRS <${crs}>` };
  return { crs, points: ps };
}

function pairs(nums) {
  const out = [];
  for (let i = 0; i + 1 < nums.length; i += 2) out.push([nums[i], nums[i + 1]]);
  return out;
}

const centroid = (ps) => [
  ps.reduce((s, p) => s + p[0], 0) / ps.length,
  ps.reduce((s, p) => s + p[1], 0) / ps.length,
];

/** Great-circle distance in kilometres. */
function haversine([lon1, lat1], [lon2, lat2]) {
  const r = Math.PI / 180;
  const a =
    Math.sin(((lat2 - lat1) * r) / 2) ** 2 +
    Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(((lon2 - lon1) * r) / 2) ** 2;
  return 2 * 6371.0088 * Math.asin(Math.sqrt(a));
}

/** GeoJSON of a literal for the map (the mock only draws what it can read). */
function geometry(value, datatype) {
  if (datatype === GEOJSON && !CONVERTED.has(value)) {
    try {
      const g = JSON.parse(value);
      if (g && typeof g.type === 'string') return g;
    } catch {
      /* below */
    }
  }
  const r = read(value, datatype);
  if (r.error) return null;
  if (/^\s*(<[^>]*>\s*)?POLYGON/i.test(value)) return { type: 'Polygon', coordinates: [r.points] };
  if (r.points.length === 1) return { type: 'Point', coordinates: r.points[0] };
  return { type: 'LineString', coordinates: r.points };
}

/** Every indexed geometry: subject (the geometry node), feature, graph, predicate, literal. */
function rows(ds, cfg) {
  const preds = new Set(cfg?.predicates ?? SERIALIZATIONS);
  const links = cfg?.featureLinks ?? LINKS;
  const out = [];
  for (const q of ds.store.match()) {
    if (q.object.termType !== 'Literal' || !preds.has(q.predicate.value)) continue;
    const dt = q.object.datatype.value;
    if (dt !== WKT && dt !== GEOJSON) continue;
    const feature = ds.store
      .match(null, null, q.subject, null)
      .find((x) => links.includes(x.predicate.value));
    out.push({
      subject: q.subject.value,
      feature: feature?.subject.value,
      graph: q.graph.termType === 'DefaultGraph' ? null : q.graph.value,
      predicate: q.predicate.value,
      value: q.object.value,
      datatype: dt,
    });
  }
  return out;
}

function status(ds) {
  const g = ds.geo;
  const all = rows(ds, g.config);
  const crs = {};
  const skipped = { malformed: 0, unknownCrs: 0, tooLarge: 0, empty: 0 };
  let indexed = 0;
  for (const r of all) {
    const x = read(r.value, r.datatype);
    crs[x.crs] = (crs[x.crs] ?? 0) + 1;
    if (x.error?.startsWith('unknown CRS') && !CONVERTED.has(r.value)) skipped.unknownCrs++;
    else if (x.error) skipped.malformed++;
    else indexed++;
  }
  return {
    enabled: true,
    state: g.building ? 'building' : 'ready',
    ...(g.building ? { progress: g.building.progress ?? 0 } : {}),
    generation: ds.type === 'mem' ? 'mem' : 'gen-0001',
    commit: g.commit(),
    rows: { base: indexed, overlay: 0, tail: 0 },
    literals: indexed,
    skipped,
    crs,
    memory: {
      treeBytes: indexed * 40,
      geometryBytes: indexed * 96,
      overlayBytes: 0,
      budgetBytes: 256 << 20,
      ...(ds.type === 'mem' ? {} : { mappedBytes: indexed * 136 }),
    },
    config: {
      predicates: SERIALIZATIONS,
      featureLinks: LINKS,
      graphs: { include: 'all', exclude: [] },
      wgs84: false,
      queryRewrite: false,
      distance: 'geodesic',
      maxGeometryBytes: 16 << 20,
      maxVertices: 1_000_000,
      formatVersion: 1,
      ...g.config,
    },
    formatVersion: 1,
    ...(g.lastBuild ? { lastBuild: g.lastBuild } : {}),
    ...(ds.type === 'mem' ? {} : { files: { bytes: indexed * 136 + 4096, opened: !g.lastBuild } }),
  };
}

/** A `geo-index` task that finishes after a few seconds. */
function build(ds, ctx, message) {
  const task = {
    id: ctx.nextTaskId(),
    kind: 'geo-index',
    dataset: ds.name,
    state: 'running',
    startedAt: new Date().toISOString(),
    progress: 0,
    message,
  };
  ctx.tasks.unshift(task);
  const g = ds.geo;
  g.building = task;
  const t0 = Date.now();
  const timer = setInterval(() => {
    task.progress = Math.min(1, task.progress + 0.2);
    if (task.progress < 1) return;
    clearInterval(timer);
    if (ds.geo !== g) return;
    g.building = null;
    const n = status(ds).rows.base;
    g.lastBuild = { at: new Date().toISOString(), ms: Date.now() - t0, rows: n };
    task.state = 'done';
    task.message = `spatial index: ${n} rows`;
    task.finishedAt = new Date().toISOString();
    delete task.progress;
  }, 600);
  return task;
}

function enable(ds, ctx, config) {
  ds.geo = { config, building: null, lastBuild: null, commit: () => ctx.headCommit(ds).seq };
  return build(ds, ctx, 'building the spatial index');
}

function seed(ctx) {
  seeded = true;
  if (ctx.datasets.has('places')) return;
  const ds = ctx.makeDataset('places', 'persistent');
  ds.store.load(PLACES, { format: 'text/turtle' });
  ds.baseQuads = ds.store.size;
  ds.prefixes = { ...ds.prefixes, ex: 'http://example.org/places/', geo: GEO };
  ctx.addCommit(ds, 'upload', ds.store.size, 0, { bulk: true });
  ds.geo = {
    config: {},
    building: null,
    lastBuild: { at: new Date(Date.now() - 3600_000).toISOString(), ms: 3.2, rows: 6 },
    commit: () => ctx.headCommit(ds).seq,
  };
}

async function json(req, ctx) {
  const body = (await ctx.readBody(req)).toString('utf8').trim();
  return body ? JSON.parse(body) : {};
}

/**
 * Answer a GeoSPARQL request; false when it is not one. `ctx` holds the server's state
 * and helpers (datasets, tasks, nextTaskId, makeDataset, addCommit, headCommit, send,
 * readBody).
 */
export async function handleGeo(req, res, url, seg, ctx) {
  if (!seeded) seed(ctx);
  const fail = (status, error) => ctx.send(res, status, { error });
  // GET /{ds}/geo?bbox=…
  if (seg[0] !== '$' && seg[1] === 'geo' && seg.length === 2) {
    const ds = ctx.datasets.get(seg[0]);
    if (!ds) return false;
    if (req.method !== 'GET') return (fail(405, 'method not allowed'), true);
    const box = (url.searchParams.get('bbox') ?? '').split(',').map(Number);
    if (box.length !== 4 || box.some((x) => !isFinite(x)) || box[0] > box[2] || box[1] > box[3])
      return (fail(400, 'bbox: not a box of longitudes and latitudes (min before max)'), true);
    const limit = Number(url.searchParams.get('limit') ?? 5000);
    const features = [];
    for (const r of rows(ds, ds.geo?.config)) {
      const g = geometry(r.value, r.datatype);
      const x = read(r.value, r.datatype);
      if (!g || x.error) continue;
      const inside = x.points.some(
        ([lon, lat]) => lon >= box[0] && lon <= box[2] && lat >= box[1] && lat <= box[3],
      );
      if (!inside) continue;
      features.push({
        type: 'Feature',
        id: r.subject,
        geometry: g,
        properties: {
          subject: r.subject,
          ...(r.feature ? { feature: r.feature } : {}),
          graph: r.graph,
          predicate: r.predicate,
        },
      });
    }
    ctx.send(
      res,
      200,
      {
        type: 'FeatureCollection',
        features: features.slice(0, limit),
        truncated: features.length > limit,
      },
      'application/geo+json',
    );
    return true;
  }
  if (seg[0] !== '$' || seg[1] !== 'geo') return false;
  const [, , name, extra] = seg;
  // POST /$/geo/convert
  if (name === 'convert' && !extra) {
    if (req.method !== 'POST') return (fail(405, 'method not allowed'), true);
    let body;
    try {
      body = await json(req, ctx);
    } catch {
      return (fail(400, 'expected {"literals": [{"value", "datatype"}]}'), true);
    }
    if (!Array.isArray(body.literals))
      return (fail(400, 'expected {"literals": [{"value", "datatype"}]}'), true);
    if (body.literals.length > 10_000)
      return (fail(400, 'literals: at most 10000 per request'), true);
    const results = body.literals.map(({ value, datatype }) => {
      const g = geometry(String(value), String(datatype));
      if (g) return { geometry: g };
      return { error: read(String(value), String(datatype)).error ?? 'not a geometry' };
    });
    ctx.send(res, 200, { results });
    return true;
  }
  const ds = name ? ctx.datasets.get(name) : undefined;
  if (!ds) return (fail(404, `No such dataset: ${name ?? ''}`), true);
  if (extra === 'rebuild') {
    if (req.method !== 'POST') return (fail(405, 'method not allowed'), true);
    if (!ds.geo) return (fail(400, 'spatial index is not enabled'), true);
    if (ds.geo.building) return (fail(409, 'a spatial index build is running'), true);
    ctx.send(res, 202, build(ds, ctx, 'rebuilding the spatial index'));
    return true;
  }
  if (extra) return false;
  switch (req.method) {
    case 'GET':
      ctx.send(res, 200, ds.geo ? status(ds) : { enabled: false });
      return true;
    case 'PUT': {
      let config;
      try {
        config = await json(req, ctx);
      } catch (e) {
        return (fail(400, `invalid geo configuration: ${e.message}`), true);
      }
      if (config.distance && !['geodesic', 'haversine'].includes(config.distance))
        return (fail(400, 'invalid geo configuration: distance: geodesic or haversine'), true);
      ctx.send(res, 202, enable(ds, ctx, config));
      return true;
    }
    case 'DELETE':
      ds.geo = null;
      res.writeHead(204, { 'Access-Control-Allow-Origin': '*' });
      res.end();
      return true;
  }
  return (fail(405, 'method not allowed'), true);
}

const NEARBY =
  /\?(\w+)\s+(?:spatial:nearbyGeom|<http:\/\/jena\.apache\.org\/spatial#nearbyGeom>)\s*\(\s*("(?:[^"\\]|\\.)*")\^\^<([^>]+)>\s+([\d.]+)\s+\S+(?:\s+(\d+))?\s*\)/;

/**
 * The result of a query with `spatial:nearbyGeom` (the explorer's Nearby: the features
 * and the `?label ?dist ?lit` of the UI's query), or null for any other query.
 */
export function geoQuery(ds, query, maxRows, head) {
  const m = NEARBY.exec(query);
  if (!m) return null;
  const center = read(JSON.parse(m[2]), m[3]);
  const radius = Number(m[4]);
  const limit = m[5] ? Number(m[5]) : Infinity;
  const self = /FILTER\s*\(\s*\?\w+\s*!=\s*<([^>]+)>\s*\)/.exec(query)?.[1];
  const hits = [];
  if (!center.error) {
    const c = centroid(center.points);
    for (const r of rows(ds, ds.geo?.config)) {
      const x = read(r.value, r.datatype);
      const f = r.feature ?? r.subject;
      if (x.error || f === self) continue;
      const d = haversine(c, centroid(x.points));
      if (d <= radius) hits.push({ f, d, r });
    }
  }
  hits.sort((a, b) => a.d - b.d || a.f.localeCompare(b.f));
  const term = (v) => ox.namedNode(v);
  const label = (f) => ds.store.match(term(f), term(RDFS_LABEL), null, null)[0]?.object.value;
  const vars = ['f', 'label', 'dist', 'lit'];
  const out = hits.slice(0, limit).map(({ f, d, r }) => {
    const l = label(f);
    return [
      { type: 'uri', value: f },
      l == null ? null : { type: 'literal', value: l },
      { type: 'literal', value: String(+d.toFixed(4)), datatype: XSD_DOUBLE },
      { type: 'literal', value: r.value, datatype: r.datatype },
    ];
  });
  const cap = Math.min(out.length, maxRows);
  return {
    queryType: 'SELECT',
    vars,
    rows: out.slice(0, cap),
    meta: {
      totalRows: out.length,
      sentRows: cap,
      timing: { parseMs: 0.1, planMs: 0.1, execMs: 0.4, serializeMs: 0.02, totalMs: 0.62 },
      plan: {
        operator: 'SpatialPf',
        description: 'spatial:nearbyGeom',
        columns: ['?f'],
        sortedOn: [],
        estimatedRows: out.length,
        estimatedCost: out.length,
        actualRows: out.length,
        timeMs: 0.4,
        cached: false,
        children: [],
      },
      commit: head,
      datasetId: ds.id,
    },
  };
}
