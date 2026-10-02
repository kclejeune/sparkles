// MapLibre GL JS with its worker and stylesheet, and the map style to draw. Only
// MapView imports this module, with a dynamic import(), so MapLibre is loaded when a
// map is first shown and no other page pays for it.

import { LngLatBounds, Map, NavigationControl, Popup, setWorkerUrl } from 'maplibre-gl';
import 'maplibre-gl/dist/maplibre-gl.css';
// the worker as its own bundle, served by the UI (MapLibre looks for it next to its own
// module otherwise, which the bundler renames)
import workerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url';
import basemapUrl from './basemap/ne-110m.json?url';
import { basemapStyle, type StyleSpec } from './basemap/style';
import { cachedServerInfo } from './api';

setWorkerUrl(new URL(workerUrl, location.href).href);

export { LngLatBounds, Map, NavigationControl, Popup };

let configured: Promise<string | null> | null = null;

/** The server's `--map-style-url`, asked once (null: the bundled basemap). */
function styleUrl(): Promise<string | null> {
  configured ??= cachedServerInfo().then(
    (s) => s?.mapStyleUrl ?? null,
    () => null,
  );
  return configured;
}

/** The value of a CSS custom property of the page (its theme). */
function token(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

/** The style of a new map: the operator's style URL, or the bundled basemap. */
export async function mapStyle(): Promise<string | StyleSpec> {
  const url = await styleUrl();
  if (url) return url;
  return basemapStyle(new URL(basemapUrl, location.href).href, {
    water: token('--surface-2', '#f0f2f6'),
    land: token('--surface', '#ffffff'),
    coast: token('--border-strong', '#c5cad4'),
    border: token('--border', '#dcdfe6'),
  });
}
