// Relations between located entities as arcs on a map (docs/specs/U11-linked-views.md,
// Phase 1): great-circle curves raised a little off the straight line so that the arcs
// of both directions between two places stay apart, split where they cross the
// antimeridian, with entities closer than 20 px merged into one marker and the arcs
// joining the same two markers merged into one arc whose width and label give the count.
// The module is pure: the maps feed it places and links and draw what it returns.

export type LonLat = [number, number];

/** A located entity: its term key (`<iri>`), a caption, and its position in degrees. */
export type Place = { id: string; label: string; lon: number; lat: number };

/** A relation between two entities, by term key. */
export type Link = {
  from: string;
  to: string;
  /** The predicate IRI (or column name), which colours the arc and has a legend entry. */
  predicate: string;
  /** The predicate in short. */
  predicateLabel?: string;
  /** A caption of the relation itself (a labelling column of a result). */
  label?: string;
  /** In compare mode, whether only the later state has the link or only the earlier one. */
  change?: 'added' | 'removed';
  /** The result row the link came from, so that a row can highlight its arc. */
  row?: number;
};

/** Most places and most links a map draws, before aggregation (`MAX_DRAWN` of the result map). */
export const MAX_POINTS = 20_000;
export const MAX_ARCS = 20_000;
/** The places a map draws: the first `MAX_POINTS`. */
export const capPlaces = (places: Place[]): Place[] =>
  places.length > MAX_POINTS ? places.slice(0, MAX_POINTS) : places;
/** Markers closer than this many pixels merge into one. */
export const CLUSTER_PX = 20;
/** The side of the world in pixels at zoom 0, as MapLibre draws it. */
export const TILE = 512;

const RAD = Math.PI / 180;

// --- great circles ----------------------------------------------------------------------

type V3 = [number, number, number];
const dot = (a: V3, b: V3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a: V3, b: V3): V3 => [
  a[1] * b[2] - a[2] * b[1],
  a[2] * b[0] - a[0] * b[2],
  a[0] * b[1] - a[1] * b[0],
];
const norm = (a: V3) => Math.hypot(a[0], a[1], a[2]);
const scale = (a: V3, k: number): V3 => [a[0] * k, a[1] * k, a[2] * k];
const sub = (a: V3, b: V3): V3 => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const unit = (a: V3): V3 => scale(a, 1 / (norm(a) || 1));

function toVec([lon, lat]: LonLat): V3 {
  const l = lon * RAD;
  const p = lat * RAD;
  return [Math.cos(p) * Math.cos(l), Math.cos(p) * Math.sin(l), Math.sin(p)];
}

function toLonLat(v: V3): LonLat {
  return [Math.atan2(v[1], v[0]) / RAD, Math.asin(Math.max(-1, Math.min(1, v[2]))) / RAD];
}

/** The angle between two positions seen from the centre of the earth, in radians. */
export function angularDistance(a: LonLat, b: LonLat): number {
  const va = toVec(a);
  const vb = toVec(b);
  return Math.atan2(norm(cross(va, vb)), dot(va, vb));
}

/**
 * The great circle from `a` to `b` as one continuous path: longitudes keep running past
 * 180 or -180 instead of jumping (see `splitAntimeridian`). `bow` raises the middle of the
 * arc off the circle, to the left of the direction of travel, by that fraction of the
 * arc's length, so that the arc from `b` to `a` bows to the other side.
 */
export function arcPath(a: LonLat, b: LonLat, bow = 0, segments?: number): LonLat[] {
  const va = toVec(a);
  const vb = toVec(b);
  const omega = angularDistance(a, b);
  if (omega < 1e-9) return [a, [b[0], b[1]]];
  const n = segments ?? Math.max(6, Math.min(48, Math.ceil(omega / RAD / 4)));
  // the circle is P(t) = cos t * e1 + sin t * e2, with e2 the unit tangent at a toward b
  let t = sub(vb, scale(va, dot(va, vb)));
  if (norm(t) < 1e-9) {
    // opposite ends of the earth: any circle through both will do
    t = cross(va, Math.abs(va[2]) < 0.9 ? [0, 0, 1] : [1, 0, 0]);
  }
  const e2 = unit(t);
  // the pole of the circle, a constant tangent to the sphere, perpendicular to travel
  const pole = cross(va, e2);
  const out: LonLat[] = [];
  let prev = a[0];
  for (let i = 0; i <= n; i++) {
    let lon: number;
    let lat: number;
    if (i === 0) {
      [lon, lat] = a;
    } else if (i === n) {
      [lon, lat] = b;
    } else {
      const th = (omega * i) / n;
      const lift = bow * omega * Math.sin((Math.PI * i) / n);
      const p: V3 = [
        Math.cos(th) * va[0] + Math.sin(th) * e2[0] + lift * pole[0],
        Math.cos(th) * va[1] + Math.sin(th) * e2[1] + lift * pole[1],
        Math.cos(th) * va[2] + Math.sin(th) * e2[2] + lift * pole[2],
      ];
      [lon, lat] = toLonLat(unit(p));
    }
    while (lon - prev > 180) lon -= 360;
    while (lon - prev < -180) lon += 360;
    prev = lon;
    out.push([lon, lat]);
  }
  return out;
}

/**
 * Cut a continuous path (see `arcPath`) where it crosses the antimeridian: every part is
 * in [-180, 180], and parts that end at the antimeridian end on it exactly.
 */
export function splitAntimeridian(path: LonLat[]): LonLat[][] {
  if (path.length < 2) return [];
  const band = (lon: number) => Math.floor((lon + 180) / 360);
  const parts: LonLat[][] = [];
  let k = band(path[0][0]);
  let cur: LonLat[] = [[path[0][0] - 360 * k, path[0][1]]];
  for (let i = 1; i < path.length; i++) {
    const [lon0, lat0] = path[i - 1];
    const [lon1, lat1] = path[i];
    const k1 = band(lon1);
    // a step of less than a half turn crosses at most one boundary
    if (k1 !== k) {
      const edge = k1 > k ? 360 * k + 180 : 360 * k - 180;
      const f = lon1 === lon0 ? 0 : (edge - lon0) / (lon1 - lon0);
      const lat = lat0 + f * (lat1 - lat0);
      const side = k1 > k ? 180 : -180;
      cur.push([side, lat]);
      if (cur.length > 1) parts.push(cur);
      cur = [[-side, lat]];
      k = k1;
    }
    cur.push([lon1 - 360 * k, lat1]);
  }
  if (cur.length > 1) parts.push(cur);
  return parts;
}

// --- pixels ----------------------------------------------------------------------------

/** A position in Web Mercator pixels at a zoom; longitudes past 180 keep going. */
export function mercatorPixel([lon, lat]: LonLat, zoom: number): [number, number] {
  const world = TILE * 2 ** zoom;
  const s = Math.sin(Math.max(-85.05112878, Math.min(85.05112878, lat)) * RAD);
  return [((lon + 180) / 360) * world, (0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)) * world];
}

function mercatorLonLat([x, y]: [number, number], zoom: number): LonLat {
  const world = TILE * 2 ** zoom;
  const n = Math.PI - (2 * Math.PI * y) / world;
  return [(x / world) * 360 - 180, (Math.atan(Math.sinh(n)) * 180) / Math.PI];
}

/**
 * The point `back` pixels before the end of a path at a zoom, and the bearing there
 * (degrees clockwise from north); null when the path is too short to hold it.
 */
export function walkBack(
  path: LonLat[],
  zoom: number,
  back: number,
): { at: LonLat; bearing: number } | null {
  let left = back;
  let [x1, y1] = mercatorPixel(path[path.length - 1], zoom);
  for (let i = path.length - 1; i > 0; i--) {
    const [x0, y0] = mercatorPixel(path[i - 1], zoom);
    const d = Math.hypot(x1 - x0, y1 - y0);
    if (d > 0) {
      if (left <= d) {
        const f = left / d;
        const at = mercatorLonLat([x1 + (x0 - x1) * f, y1 + (y0 - y1) * f], zoom);
        at[0] = ((((at[0] + 180) % 360) + 360) % 360) - 180;
        return { at, bearing: (Math.atan2(x1 - x0, y0 - y1) * 180) / Math.PI };
      }
      left -= d;
    }
    x1 = x0;
    y1 = y0;
  }
  return null;
}

// --- clustering ------------------------------------------------------------------------

/** Places that lie within `CLUSTER_PX` of each other at the current zoom. */
export type Cluster = {
  /** The id of its first member, plus the number of others when there are some. */
  id: string;
  lon: number;
  lat: number;
  /** The ids of the places in it. */
  members: string[];
  /** The first member's caption, plus the number of others. */
  label: string;
};

/** The radius in pixels of the marker of a cluster of `size` places. */
export const markerRadius = (size: number) =>
  size <= 1 ? 5 : Math.min(18, 5 + 2.5 * Math.log2(size));

/**
 * Merge places closer than `px` pixels at `zoom`. A place joins the first cluster
 * whose seed is within reach, in the order given, so the same input gives the same
 * clusters. `of` maps every place id to its cluster's index.
 */
export function clusterPlaces(
  places: Place[],
  zoom: number,
  px = CLUSTER_PX,
): { clusters: Cluster[]; of: Map<string, number> } {
  const xy = places.map((p) => mercatorPixel([p.lon, p.lat], zoom));
  const world = TILE * 2 ** zoom;
  // the columns of the grid wrap, so that places either side of the antimeridian meet
  const columns = Math.max(1, Math.ceil(world / px));
  const column = (v: number) => ((Math.floor(v / px) % columns) + columns) % columns;
  const cell = (v: number) => Math.floor(v / px);
  // a grid of cells `px` wide: a place's neighbours within reach are in the 3 by 3 around it
  const grid = new Map<string, number[]>();
  const key = (cx: number, cy: number) => `${cx},${cy}`;
  xy.forEach(([x, y], i) => {
    const k = key(column(x), cell(y));
    const g = grid.get(k);
    if (g) g.push(i);
    else grid.set(k, [i]);
  });
  const owner = new Int32Array(places.length).fill(-1);
  const clusters: Cluster[] = [];
  const of = new Map<string, number>();
  for (let i = 0; i < places.length; i++) {
    if (owner[i] >= 0) continue;
    const ci = clusters.length;
    const members = [i];
    owner[i] = ci;
    const [x, y] = xy[i];
    const cx = column(x);
    const cy = cell(y);
    for (let dx = -1; dx <= 1; dx++) {
      for (let dy = -1; dy <= 1; dy++) {
        for (const j of grid.get(key((((cx + dx) % columns) + columns) % columns, cy + dy)) ?? []) {
          if (owner[j] >= 0) continue;
          const across = Math.abs(xy[j][0] - x);
          if (Math.hypot(Math.min(across, world - across), xy[j][1] - y) <= px) {
            owner[j] = ci;
            members.push(j);
          }
        }
      }
    }
    members.sort((a, b) => a - b);
    // the mean position, with longitudes taken relative to the seed's
    let lon = 0;
    let lat = 0;
    for (const m of members) {
      let d = places[m].lon - places[i].lon;
      while (d > 180) d -= 360;
      while (d < -180) d += 360;
      lon += d;
      lat += places[m].lat - places[i].lat;
    }
    const mlon = places[i].lon + lon / members.length;
    const first = places[members[0]];
    clusters.push({
      id: members.length === 1 ? first.id : `${first.id}+${members.length - 1}`,
      lon: ((((mlon + 180) % 360) + 360) % 360) - 180,
      lat: places[i].lat + lat / members.length,
      members: members.map((m) => places[m].id),
      label: members.length === 1 ? first.label : `${first.label} +${members.length - 1}`,
    });
    for (const m of members) of.set(places[m].id, ci);
  }
  return { clusters, of };
}

// --- aggregation -----------------------------------------------------------------------

/** The links of one predicate (and change) from one cluster to another. */
export type Arc = {
  id: string;
  from: number;
  to: number;
  predicate: string;
  predicateLabel: string;
  change?: 'added' | 'removed';
  count: number;
  /** The captions the links carried, for the label of a single link. */
  labels: string[];
  /** The result rows the links came from. */
  rows: number[];
  /** The place of this arc among those with the same ends and direction (0, 1, 2…). */
  rank: number;
};

export type Aggregated = {
  arcs: Arc[];
  /** The links that stay inside one cluster, per cluster. */
  local: number[];
  /** Links with an end that is not located. */
  unlocated: number;
  /** Links left out because there are more than `MAX_ARCS`. */
  dropped: number;
};

/**
 * Group the links by the clusters they join. Links between two places of one cluster
 * are counted on the cluster instead of drawn, and links with an end that has no
 * position are counted as unlocated. Only the first `MAX_ARCS` links are read.
 */
export function aggregateLinks(
  links: Link[],
  of: Map<string, number>,
  clusterCount: number,
): Aggregated {
  const local = new Array<number>(clusterCount).fill(0);
  const byKey = new Map<string, Arc>();
  const ranks = new Map<string, number>();
  let unlocated = 0;
  const read = links.length > MAX_ARCS ? links.slice(0, MAX_ARCS) : links;
  for (const l of read) {
    const a = of.get(l.from);
    const b = of.get(l.to);
    if (a === undefined || b === undefined) {
      unlocated++;
      continue;
    }
    if (a === b) {
      local[a]++;
      continue;
    }
    const k = `${a}>${b}|${l.predicate}|${l.change ?? ''}`;
    let arc = byKey.get(k);
    if (!arc) {
      const dir = `${a}>${b}`;
      const rank = ranks.get(dir) ?? 0;
      ranks.set(dir, rank + 1);
      arc = {
        id: k,
        from: a,
        to: b,
        predicate: l.predicate,
        predicateLabel: l.predicateLabel ?? l.predicate,
        change: l.change,
        count: 0,
        labels: [],
        rows: [],
        rank,
      };
      byKey.set(k, arc);
    }
    arc.count++;
    if (l.label && arc.labels.length < 3) arc.labels.push(l.label);
    if (l.row !== undefined) arc.rows.push(l.row);
  }
  return { arcs: [...byKey.values()], local, unlocated, dropped: links.length - read.length };
}

// --- colour ---------------------------------------------------------------------------

/** Distinguishable on light and dark backgrounds (after Okabe and Ito). */
export const PALETTE = [
  '#0072b2',
  '#d55e00',
  '#009e73',
  '#cc79a7',
  '#e69f00',
  '#56b4e9',
  '#8c8c00',
  '#7a7a7a',
];

/** A colour for every predicate, the most used first. */
export function predicateColours(links: Link[]): Map<string, string> {
  const n = new Map<string, number>();
  for (const l of links) n.set(l.predicate, (n.get(l.predicate) ?? 0) + 1);
  const order = [...n.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
  return new Map(order.map(([p], i) => [p, PALETTE[i % PALETTE.length]]));
}

// --- drawing ---------------------------------------------------------------------------

export type WidthBy = 'count' | 'uniform';

/** The width in pixels of an arc. */
export const arcWidth = (count: number, by: WidthBy = 'count') =>
  by === 'uniform' ? 2 : Math.min(14, 1.6 + 2.2 * Math.log2(count));

export type GeoFeature<G, P> = { type: 'Feature'; geometry: G; properties: P };
export type Collection<G, P> = { type: 'FeatureCollection'; features: GeoFeature<G, P>[] };

export type LineProps = {
  id: string;
  color: string;
  width: number;
  change: string;
  /** The text of the label: the count, or the caption of a single link; empty for none. */
  label: string;
  count: number;
};
export type HeadProps = { id: string; color: string; bearing: number; change: string };
export type MarkerProps = {
  id: string;
  size: number;
  radius: number;
  /** The members' count as text, empty for a single place. */
  label: string;
  local: number;
};

export type ArcLayout = {
  lines: Collection<{ type: 'MultiLineString'; coordinates: LonLat[][] }, LineProps>;
  heads: Collection<{ type: 'Point'; coordinates: LonLat }, HeadProps>;
  markers: Collection<{ type: 'Point'; coordinates: LonLat }, MarkerProps>;
};

/** How far above a straight line the first arc between two places bows, and each next one more. */
const BOW = 0.12;
const BOW_STEP = 0.07;

/** The text drawn on an arc: the count, or the caption of a single link. */
export function arcLabel(a: Arc): string {
  const sign = a.change === 'added' ? '+' : a.change === 'removed' ? '−' : '';
  if (a.count === 1) return a.labels[0] ? a.labels[0].slice(0, 24) : sign;
  return `${sign}${a.count}`;
}

/** The arc as the parts of a line, from the position of its two clusters. */
export function arcParts(a: Arc, clusters: Cluster[]): { path: LonLat[]; parts: LonLat[][] } {
  const f = clusters[a.from];
  const t = clusters[a.to];
  const path = arcPath([f.lon, f.lat], [t.lon, t.lat], BOW + BOW_STEP * a.rank);
  return { path, parts: splitAntimeridian(path) };
}

/**
 * The GeoJSON of clusters and arcs for a map: a line per arc, an arrowhead at its
 * target (set back from the marker so it shows) and a marker per cluster.
 */
export function layoutArcs(
  clusters: Cluster[],
  agg: Aggregated,
  zoom: number,
  colours: Map<string, string>,
  by: WidthBy = 'count',
  changeColours: { added: string; removed: string } = { added: '#1a7f37', removed: '#c0392b' },
): ArcLayout {
  const lines: ArcLayout['lines']['features'] = [];
  const heads: ArcLayout['heads']['features'] = [];
  for (const a of agg.arcs) {
    const { path, parts } = arcParts(a, clusters);
    if (!parts.length) continue;
    // compare mode: the colour of the change, as on map features, over the predicate's
    const color = a.change ? changeColours[a.change] : (colours.get(a.predicate) ?? PALETTE[0]);
    const change = a.change ?? '';
    lines.push({
      type: 'Feature',
      geometry: { type: 'MultiLineString', coordinates: parts },
      properties: {
        id: a.id,
        color,
        width: arcWidth(a.count, by),
        change,
        label: arcLabel(a),
        count: a.count,
      },
    });
    const r = markerRadius(clusters[a.to].members.length);
    const head = walkBack(path, zoom, r + 8);
    if (head)
      heads.push({
        type: 'Feature',
        geometry: { type: 'Point', coordinates: head.at },
        properties: { id: a.id, color, bearing: head.bearing, change },
      });
  }
  const markers = clusters.map((c, i) => ({
    type: 'Feature' as const,
    geometry: { type: 'Point' as const, coordinates: [c.lon, c.lat] as LonLat },
    properties: {
      id: c.id,
      size: c.members.length,
      radius: markerRadius(c.members.length),
      label: c.members.length > 1 ? String(c.members.length) : '',
      local: agg.local[i] ?? 0,
    },
  }));
  return {
    lines: { type: 'FeatureCollection', features: lines },
    heads: { type: 'FeatureCollection', features: heads },
    markers: { type: 'FeatureCollection', features: markers },
  };
}

/** The ids of the arcs with an end in one of the given clusters. */
export function hotArcs(agg: Aggregated, hotClusters: Set<number>): Set<string> {
  const out = new Set<string>();
  for (const a of agg.arcs) if (hotClusters.has(a.from) || hotClusters.has(a.to)) out.add(a.id);
  return out;
}

/** The clusters that hold at least one of the selected places. */
export function hotClusters(clusters: Cluster[], selected: (id: string) => boolean): Set<number> {
  const out = new Set<number>();
  clusters.forEach((c, i) => {
    if (c.members.some(selected)) out.add(i);
  });
  return out;
}

/**
 * A box whose longitudes are as drawn, such as 170 to 190 when the antimeridian is in view,
 * with its longitudes in range. A box that crosses the seam comes back with west greater
 * than east, and one at least a world wide covers every longitude.
 */
export function wrapBox(box: [number, number, number, number]): [number, number, number, number] {
  const [w, s, e, n] = box;
  if (e - w >= 360) return [-180, s, 180, n];
  const wrap = (x: number) => ((((x + 180) % 360) + 360) % 360) - 180;
  const west = wrap(w);
  let east = wrap(e);
  // an edge exactly on the seam belongs to the side the box extends over
  if (east === -180 && e > w) east = 180;
  return [west, s, east, n];
}

/** Clusters in a box, largest first, for the list alternative and for keyboard order. */
export function clustersInBox(
  clusters: Cluster[],
  box: [number, number, number, number] | null,
): number[] {
  const idx = clusters
    .map((_, i) => i)
    .filter((i) => {
      if (!box) return true;
      const c = clusters[i];
      const inLon =
        box[0] <= box[2] ? c.lon >= box[0] && c.lon <= box[2] : c.lon >= box[0] || c.lon <= box[2];
      return inLon && c.lat >= box[1] && c.lat <= box[3];
    });
  return idx.sort(
    (a, b) =>
      clusters[b].members.length - clusters[a].members.length ||
      (clusters[a].label < clusters[b].label ? -1 : clusters[a].label > clusters[b].label ? 1 : 0),
  );
}

/** The box of places, [west, south, east, north]; null when there are none. */
export function placesBox(places: Place[]): [number, number, number, number] | null {
  if (!places.length) return null;
  let w = Infinity;
  let s = Infinity;
  let e = -Infinity;
  let n = -Infinity;
  for (const p of places) {
    w = Math.min(w, p.lon);
    e = Math.max(e, p.lon);
    s = Math.min(s, p.lat);
    n = Math.max(n, p.lat);
  }
  return [w, s, e, n];
}
