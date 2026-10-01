// The MapLibre style of the bundled basemap: Natural Earth 1:110m land, coastlines and
// land boundaries (ne-110m.json, see README.md) drawn in the page's theme colours. No
// tiles, glyphs or sprites: nothing is fetched but the one GeoJSON file of the UI.

import type { MapOptions } from 'maplibre-gl';

export type StyleSpec = Exclude<MapOptions['style'], string | undefined>;

export type BasemapColors = { water: string; land: string; coast: string; border: string };

/** The basemap style; `dataUrl` is the absolute URL of ne-110m.json. */
export function basemapStyle(dataUrl: string, c: BasemapColors): StyleSpec {
  const kind = (k: string) => ['==', ['get', 'k'], k] as ['==', ['get', string], string];
  return {
    version: 8,
    sources: {
      basemap: { type: 'geojson', data: dataUrl, attribution: 'Natural Earth' },
    },
    layers: [
      { id: 'water', type: 'background', paint: { 'background-color': c.water } },
      {
        id: 'land',
        type: 'fill',
        source: 'basemap',
        filter: kind('land'),
        paint: { 'fill-color': c.land },
      },
      {
        id: 'border',
        type: 'line',
        source: 'basemap',
        filter: kind('border'),
        paint: { 'line-color': c.border, 'line-width': 0.7, 'line-dasharray': [3, 2] },
      },
      {
        id: 'coast',
        type: 'line',
        source: 'basemap',
        filter: kind('coast'),
        paint: { 'line-color': c.coast, 'line-width': 0.8 },
      },
    ],
  };
}
