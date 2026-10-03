import { describe, expect, it, vi } from 'vitest';
import type { GeoConverted, Term } from './api';
import {
  bounds,
  crsAxes,
  crsLabel,
  CRS84,
  GEOJSON_LITERAL,
  geoCells,
  geoColumns,
  geometryQuery,
  GML_LITERAL,
  inverseMercator,
  isGeoLiteral,
  KML_LITERAL,
  localGeometry,
  nearbyQuery,
  normalizeCrs,
  parseGeoJson,
  parseWkt,
  resolveGeometries,
  splitWkt,
  WKT_LITERAL,
  type GeoLiteral,
} from './geo';

const wkt = (value: string): GeoLiteral => ({ value, datatype: WKT_LITERAL });
const json = (value: string): GeoLiteral => ({ value, datatype: GEOJSON_LITERAL });
const lit = (value: string, datatype?: string): Term => ({ type: 'literal', value, datatype });
const GML_POINT =
  '<gml:Point xmlns:gml="http://www.opengis.net/gml/3.2" srsName="http://www.opengis.net/def/crs/EPSG/0/4326"><gml:pos>48.853 2.3499</gml:pos></gml:Point>';
const KML_LINE =
  '<LineString xmlns="http://www.opengis.net/kml/2.2"><coordinates>2.29,48.86 2.35,48.85</coordinates></LineString>';

describe('geometry literals', () => {
  it('detects WKT, GeoJSON, GML and KML literals only', () => {
    expect(isGeoLiteral(lit('POINT(1 2)', WKT_LITERAL))).toBe(true);
    expect(isGeoLiteral(lit('{}', GEOJSON_LITERAL))).toBe(true);
    expect(isGeoLiteral(lit(GML_POINT, GML_LITERAL))).toBe(true);
    expect(isGeoLiteral(lit(KML_LINE, KML_LITERAL))).toBe(true);
    expect(isGeoLiteral(lit('POINT(1 2)'))).toBe(false);
    expect(isGeoLiteral({ type: 'uri', value: WKT_LITERAL })).toBe(false);
    expect(isGeoLiteral(null)).toBe(false);
  });

  it('finds the geometry columns and their cells', () => {
    const vars = ['f', 'w', 'n'];
    const rows: (Term | null)[][] = [
      [{ type: 'uri', value: 'urn:a' }, lit('POINT(1 2)', WKT_LITERAL), lit('a')],
      [{ type: 'uri', value: 'urn:b' }, null, lit('b')],
      [
        { type: 'uri', value: 'urn:c' },
        lit('{"type":"Point","coordinates":[3,4]}', GEOJSON_LITERAL),
        null,
      ],
    ];
    expect(geoColumns(vars, rows)).toEqual(['w']);
    expect(geoCells(vars, rows, ['w']).map((c) => [c.row, c.column])).toEqual([
      [0, 'w'],
      [2, 'w'],
    ]);
  });
});

describe('CRSs', () => {
  it('normalizes aliases', () => {
    expect(normalizeCrs('https://www.opengis.net/def/crs/EPSG/0/4326')).toBe(
      'http://www.opengis.net/def/crs/EPSG/0/4326',
    );
    expect(normalizeCrs('urn:ogc:def:crs:EPSG::4326')).toBe(
      'http://www.opengis.net/def/crs/EPSG/0/4326',
    );
    expect(normalizeCrs('urn:ogc:def:crs:OGC:1.3:CRS84')).toBe(CRS84);
    expect(normalizeCrs('CRS:84')).toBe(CRS84);
    expect(normalizeCrs('EPSG:3857')).toBe('http://www.opengis.net/def/crs/EPSG/0/3857');
  });

  it('knows the axis order of the built-in CRSs', () => {
    expect(crsAxes(CRS84)).toBe('lonlat');
    expect(crsAxes('http://www.opengis.net/def/crs/OGC/0/CRS84h')).toBe('lonlat');
    expect(crsAxes('http://www.opengis.net/def/crs/EPSG/4326')).toBe('lonlat');
    expect(crsAxes('http://www.opengis.net/def/crs/EPSG/0/4326')).toBe('latlon');
    expect(crsAxes('http://www.opengis.net/def/crs/EPSG/0/4979')).toBe('latlon');
    expect(crsAxes('urn:ogc:def:crs:EPSG::3857')).toBe('mercator');
    expect(crsAxes('http://www.opengis.net/def/crs/EPSG/0/32631')).toBeNull();
  });

  it('names CRSs in short', () => {
    expect(crsLabel(CRS84)).toBe('CRS84');
    expect(crsLabel('http://www.opengis.net/def/crs/OGC/0/CRS84h')).toBe('CRS84h');
    expect(crsLabel('http://www.opengis.net/def/crs/EPSG/0/32631')).toBe('EPSG:32631');
    expect(crsLabel('http://www.opengis.net/def/crs/EPSG/4326')).toBe('EPSG:4326 (lon, lat)');
    expect(crsLabel('urn:x:crs')).toBe('urn:x:crs');
  });

  it('inverts Web Mercator', () => {
    const [lon, lat] = inverseMercator(261848.15, 6250566.72);
    expect(lon).toBeCloseTo(2.3522, 3);
    expect(lat).toBeCloseTo(48.8566, 3);
    expect(inverseMercator(0, 0)).toEqual([0, 0]);
  });
});

describe('WKT', () => {
  it('splits the CRS IRI off', () => {
    expect(splitWkt('<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(1 2)')).toEqual({
      crs: 'http://www.opengis.net/def/crs/EPSG/0/4326',
      body: 'POINT(1 2)',
    });
    expect(splitWkt('POINT(1 2)')).toEqual({ crs: CRS84, body: 'POINT(1 2)' });
  });

  it('reads every geometry type, dropping Z and M', () => {
    expect(parseWkt('POINT(1 2)')).toEqual({ type: 'Point', coordinates: [1, 2] });
    expect(parseWkt('point z (1 2 3)')).toEqual({ type: 'Point', coordinates: [1, 2] });
    expect(parseWkt('POINTZM(1 2 3 4)')).toEqual({ type: 'Point', coordinates: [1, 2] });
    expect(parseWkt('LINESTRING(0 0, 1 1.5, -2e1 .5)')).toEqual({
      type: 'LineString',
      coordinates: [
        [0, 0],
        [1, 1.5],
        [-20, 0.5],
      ],
    });
    expect(parseWkt('POLYGON((0 0, 1 0, 1 1, 0 0), (0.2 0.2, 0.3 0.2, 0.3 0.3, 0.2 0.2))')).toEqual(
      {
        type: 'Polygon',
        coordinates: [
          [
            [0, 0],
            [1, 0],
            [1, 1],
            [0, 0],
          ],
          [
            [0.2, 0.2],
            [0.3, 0.2],
            [0.3, 0.3],
            [0.2, 0.2],
          ],
        ],
      },
    );
    expect(parseWkt('MULTIPOINT((1 2), (3 4))')).toEqual(parseWkt('MULTIPOINT(1 2, 3 4)'));
    expect(parseWkt('MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)), EMPTY)')).toEqual({
      type: 'MultiPolygon',
      coordinates: [
        [
          [
            [0, 0],
            [1, 0],
            [1, 1],
            [0, 0],
          ],
        ],
      ],
    });
    expect(parseWkt('GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))')).toEqual({
      type: 'GeometryCollection',
      geometries: [
        { type: 'Point', coordinates: [1, 2] },
        {
          type: 'LineString',
          coordinates: [
            [0, 0],
            [1, 1],
          ],
        },
      ],
    });
    expect(parseWkt('TRIANGLE((0 0, 1 0, 0 1, 0 0))')?.type).toBe('Polygon');
  });

  it('reads empty geometries as null and refuses bad text', () => {
    expect(parseWkt('POINT EMPTY')).toBeNull();
    expect(parseWkt('  ')).toBeNull();
    expect(() => parseWkt('POINT(1)')).toThrow(/a number/);
    expect(() => parseWkt('CIRCLE(1 2)')).toThrow(/geometry type/);
    expect(() => parseWkt('POINT(1 2) x')).toThrow(/end of the literal/);
  });
});

describe('GeoJSON', () => {
  it('reads geometries and features, cut to 2D', () => {
    expect(parseGeoJson('{"type":"Point","coordinates":[1,2,3]}')).toEqual({
      type: 'Point',
      coordinates: [1, 2],
    });
    expect(
      parseGeoJson(
        '{"type":"Feature","geometry":{"type":"LineString","coordinates":[[0,0],[1,1]]}}',
      ),
    ).toEqual({
      type: 'LineString',
      coordinates: [
        [0, 0],
        [1, 1],
      ],
    });
    expect(parseGeoJson('{"type":"Point","coordinates":[]}')).toBeNull();
  });

  it('refuses what is not a geometry', () => {
    expect(() => parseGeoJson('nope')).toThrow(/not JSON/);
    expect(() => parseGeoJson('{"type":"Circle"}')).toThrow(/geometry type/);
    expect(() => parseGeoJson('{"type":"Point","coordinates":["a",1]}')).toThrow(/coordinates/);
  });
});

describe('literals for the map', () => {
  it('draws CRS84 as written and swaps EPSG:4326', () => {
    expect(localGeometry(wkt('POINT(2.35 48.85)'))).toEqual({
      geometry: { type: 'Point', coordinates: [2.35, 48.85] },
    });
    expect(
      localGeometry(wkt('<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.85 2.35)')),
    ).toEqual({ geometry: { type: 'Point', coordinates: [2.35, 48.85] } });
  });

  it('projects Web Mercator back to longitude and latitude', () => {
    const r = localGeometry(
      wkt('<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(261848.15 6250566.72)'),
    );
    expect('geometry' in r && (r.geometry.coordinates as number[])[0]).toBeCloseTo(2.3522, 3);
  });

  it('sends other CRSs and WKT it cannot read to the server', () => {
    expect(
      localGeometry(wkt('<http://www.opengis.net/def/crs/EPSG/0/32631> POINT(448251 5411932)')),
    ).toMatchObject({ convert: true });
    expect(localGeometry(wkt('CIRCULARSTRING(0 0, 1 1, 2 0)'))).toMatchObject({ convert: true });
  });

  it('sends GML and KML to the server', async () => {
    const gml = { value: GML_POINT, datatype: GML_LITERAL };
    const kml = { value: KML_LINE, datatype: KML_LITERAL };
    expect(localGeometry(gml)).toEqual({ convert: true, why: 'GML' });
    expect(localGeometry(kml)).toEqual({ convert: true, why: 'KML' });
    const convert = vi.fn(async (batch: GeoLiteral[]): Promise<GeoConverted[]> =>
      batch.map((l) =>
        l.datatype === GML_LITERAL
          ? { geometry: { type: 'Point', coordinates: [2.3499, 48.853] } }
          : {
              geometry: {
                type: 'LineString',
                coordinates: [
                  [2.29, 48.86],
                  [2.35, 48.85],
                ],
              },
            },
      ),
    );
    const res = await resolveGeometries([wkt('POINT(1 2)'), gml, kml], convert);
    expect(convert).toHaveBeenCalledTimes(1);
    expect(convert.mock.calls[0][0]).toEqual([gml, kml]);
    expect(res[1]).toEqual({ geometry: { type: 'Point', coordinates: [2.3499, 48.853] } });
    expect(res[2]).toMatchObject({ geometry: { type: 'LineString' } });
  });

  it('lists empty and malformed GeoJSON as undrawable', () => {
    expect(localGeometry(wkt('POINT EMPTY'))).toEqual({ error: 'empty geometry' });
    expect(localGeometry(json('{'))).toEqual({ error: 'malformed GeoJSON: not JSON' });
  });

  it('converts the rest through the server, once per distinct literal', async () => {
    const utm = wkt('<http://www.opengis.net/def/crs/EPSG/0/32631> POINT(448251 5411932)');
    const convert = vi.fn(async (batch: GeoLiteral[]): Promise<GeoConverted[]> =>
      batch.map((l) =>
        l.value.includes('32631')
          ? { geometry: { type: 'Point', coordinates: [2.29, 48.86] } }
          : { error: 'malformed wktLiteral at offset 0' },
      ),
    );
    const res = await resolveGeometries([wkt('POINT(1 2)'), utm, utm, wkt('BAD(1)')], convert);
    expect(convert).toHaveBeenCalledTimes(1);
    expect(convert.mock.calls[0][0]).toHaveLength(2);
    expect(res).toEqual([
      { geometry: { type: 'Point', coordinates: [1, 2] } },
      { geometry: { type: 'Point', coordinates: [2.29, 48.86] } },
      { geometry: { type: 'Point', coordinates: [2.29, 48.86] } },
      { error: 'malformed wktLiteral at offset 0' },
    ]);
  });

  it('keeps going when the conversion request fails', async () => {
    const res = await resolveGeometries(
      [wkt('POINT(1 2)'), wkt('<http://www.opengis.net/def/crs/EPSG/0/32631> POINT(1 2)')],
      async () => {
        throw new Error('built without GeoSPARQL');
      },
    );
    expect(res[0]).toEqual({ geometry: { type: 'Point', coordinates: [1, 2] } });
    expect(res[1]).toEqual({ error: 'cannot convert: built without GeoSPARQL' });
  });

  it('asks nothing of the server when everything is local', async () => {
    const convert = vi.fn();
    await resolveGeometries([wkt('POINT(1 2)')], convert);
    expect(convert).not.toHaveBeenCalled();
  });

  it('computes the box of geometries', () => {
    expect(
      bounds([
        { type: 'Point', coordinates: [1, 2] },
        {
          type: 'LineString',
          coordinates: [
            [-3, 5],
            [4, -1],
          ],
        },
      ]),
    ).toEqual([-3, -1, 4, 5]);
    expect(bounds([])).toBeNull();
  });
});

describe('explorer queries', () => {
  it('reads a resource’s own, linked and Basic Geo geometries', () => {
    const q = geometryQuery('http://ex.org/paris');
    expect(q).toContain(
      '<http://ex.org/paris> <http://www.opengis.net/ont/geosparql#hasDefaultGeometry>',
    );
    expect(q).toContain('wgs84_pos#lat');
    expect(q).toContain(`<${GML_LITERAL}>, <${KML_LITERAL}>`);
    expect(q).toMatch(/LIMIT 20$/);
  });

  it('finds nearby features with the literal as a constant', () => {
    const q = nearbyQuery('http://ex.org/paris', wkt('POINT(2.35 48.85)'), 5, 25);
    expect(q).toContain(
      'spatial:nearbyGeom ("POINT(2.35 48.85)"^^<http://www.opengis.net/ont/geosparql#wktLiteral> 5 uom:kilometre 25)',
    );
    expect(q).toContain('FILTER(?f != <http://ex.org/paris>)');
  });
});
