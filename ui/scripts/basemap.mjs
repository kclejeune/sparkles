// Builds the UI's bundled basemap (src/lib/basemap/ne-110m.json) from three Natural Earth
// 1:110m GeoJSON files (public domain; see src/lib/basemap/README.md):
//
//   node scripts/basemap.mjs DIR
//
// where DIR holds ne_110m_land.geojson, ne_110m_coastline.geojson and
// ne_110m_admin_0_boundary_lines_land.geojson. The output is one FeatureCollection whose
// features carry only `k` (land, coast or border), with coordinates rounded to 0.01°
// (about 1 km, far below what the 1:110m data resolves) and repeated points dropped.

import { readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const LAYERS = [
  ['land', 'ne_110m_land.geojson'],
  ['coast', 'ne_110m_coastline.geojson'],
  ['border', 'ne_110m_admin_0_boundary_lines_land.geojson'],
];

const round = (x) => Math.round(x * 100) / 100;

/** A line or ring with rounded points, repeats dropped. */
function line(points) {
  const out = [];
  for (const [x, y] of points) {
    const p = [round(x), round(y)];
    const last = out.at(-1);
    if (!last || last[0] !== p[0] || last[1] !== p[1]) out.push(p);
  }
  return out;
}

function geometry(g) {
  switch (g.type) {
    case 'LineString':
      return { type: g.type, coordinates: line(g.coordinates) };
    case 'MultiLineString':
    case 'Polygon':
      return { type: g.type, coordinates: g.coordinates.map(line) };
    case 'MultiPolygon':
      return { type: g.type, coordinates: g.coordinates.map((p) => p.map(line)) };
    default:
      throw new Error(`unexpected geometry ${g.type}`);
  }
}

const dir = process.argv[2];
if (!dir) {
  console.error('usage: node scripts/basemap.mjs DIR');
  process.exit(2);
}
const features = [];
for (const [k, file] of LAYERS) {
  const fc = JSON.parse(readFileSync(join(dir, file), 'utf8'));
  for (const f of fc.features)
    features.push({ type: 'Feature', properties: { k }, geometry: geometry(f.geometry) });
}
const out = resolve(import.meta.dirname, '../src/lib/basemap/ne-110m.json');
writeFileSync(out, JSON.stringify({ type: 'FeatureCollection', features }) + '\n');
console.log(`wrote ${features.length} features to ${out}`);
