import { describe, expect, it } from 'vitest';
import {
  aggregateLinks,
  angularDistance,
  capPlaces,
  arcLabel,
  arcPath,
  arcWidth,
  clusterPlaces,
  clustersInBox,
  hotArcs,
  hotClusters,
  layoutArcs,
  markerRadius,
  MAX_ARCS,
  MAX_POINTS,
  mercatorPixel,
  PALETTE,
  placesBox,
  predicateColours,
  splitAntimeridian,
  walkBack,
  type Link,
  type LonLat,
  type Place,
  wrapBox,
} from './arcs';

const LYON: LonLat = [4.8357, 45.764];
const PARIS: LonLat = [2.3522, 48.8566];
const TOKYO: LonLat = [139.6917, 35.6895];
const SF: LonLat = [-122.4194, 37.7749];

const place = (id: string, lon: number, lat: number, label = id): Place => ({
  id,
  label,
  lon,
  lat,
});

describe('great circles', () => {
  it('runs from the first position to the second along the shortest way', () => {
    const path = arcPath(LYON, PARIS);
    expect(path[0]).toEqual(LYON);
    expect(path.at(-1)).toEqual(PARIS);
    // every point is on the great circle: the angles add up to the whole
    let sum = 0;
    for (let i = 1; i < path.length; i++) sum += angularDistance(path[i - 1], path[i]);
    expect(sum).toBeCloseTo(angularDistance(LYON, PARIS), 6);
  });

  it('uses more segments for longer arcs', () => {
    expect(arcPath(TOKYO, SF).length).toBeGreaterThan(arcPath(LYON, PARIS).length);
    expect(arcPath(LYON, PARIS).length).toBeGreaterThanOrEqual(7);
    expect(arcPath(TOKYO, SF).length).toBeLessThanOrEqual(49);
  });

  it('bows to the left of the travel direction, so the way back bows the other side', () => {
    const mid = (p: LonLat[]) => p[Math.floor(p.length / 2)];
    const straight = mid(arcPath(LYON, PARIS, 0));
    const there = mid(arcPath(LYON, PARIS, 0.2));
    const back = mid(arcPath(PARIS, LYON, 0.2));
    // the two bows lie on opposite sides of the circle
    const dThere = angularDistance(there, straight);
    const dBack = angularDistance(back, straight);
    expect(dThere).toBeGreaterThan(0.005);
    expect(dBack).toBeGreaterThan(0.005);
    expect(angularDistance(there, back)).toBeGreaterThan(1.8 * Math.min(dThere, dBack));
  });

  it('keeps the ends exactly where they are however much it bows', () => {
    const p = arcPath(LYON, PARIS, 0.5);
    expect(p[0]).toEqual(LYON);
    expect(p.at(-1)).toEqual(PARIS);
  });

  it('handles the same place, nearby places and opposite sides of the earth', () => {
    expect(arcPath(LYON, LYON)).toHaveLength(2);
    const near = arcPath([4.8357, 45.764], [4.8358, 45.7641], 0.1);
    expect(near.every(([x, y]) => isFinite(x) && isFinite(y))).toBe(true);
    const far = arcPath([0, 0], [180, 0], 0.1);
    expect(far.every(([x, y]) => isFinite(x) && isFinite(y))).toBe(true);
    expect(Math.abs(far.at(-1)![0])).toBe(180);
  });

  it('runs on past 180 degrees instead of jumping, then splits there', () => {
    const path = arcPath(TOKYO, SF);
    // east from Tokyo: longitudes increase past 180
    expect(Math.max(...path.map((p) => p[0]))).toBeGreaterThan(180);
    for (let i = 1; i < path.length; i++)
      expect(Math.abs(path[i][0] - path[i - 1][0])).toBeLessThan(40);
    const parts = splitAntimeridian(path);
    expect(parts).toHaveLength(2);
    expect(parts[0].at(-1)![0]).toBe(180);
    expect(parts[1][0][0]).toBe(-180);
    // the crossing is at one latitude
    expect(parts[0].at(-1)![1]).toBeCloseTo(parts[1][0][1], 9);
    for (const part of parts)
      for (const [lon] of part) expect(Math.abs(lon)).toBeLessThanOrEqual(180);
    // the ends are still the ends
    expect(parts[0][0]).toEqual(TOKYO);
    expect(parts[1].at(-1)).toEqual(SF);
  });

  it('splits a westbound arc the other way', () => {
    const parts = splitAntimeridian(arcPath(SF, TOKYO));
    expect(parts).toHaveLength(2);
    expect(parts[0].at(-1)![0]).toBe(-180);
    expect(parts[1][0][0]).toBe(180);
  });

  it('does not split an arc that stays on one side', () => {
    expect(splitAntimeridian(arcPath(LYON, PARIS))).toHaveLength(1);
    expect(splitAntimeridian(arcPath([-170, 10], [170, -10]))).toHaveLength(2);
    expect(splitAntimeridian(arcPath([10, 10], [20, 10]))).toHaveLength(1);
  });

  it('keeps an arc that ends exactly on the antimeridian whole', () => {
    const parts = splitAntimeridian(arcPath([170, 0], [180, 0]));
    expect(parts.flat().every(([lon]) => Math.abs(lon) <= 180)).toBe(true);
    expect(parts.length).toBeGreaterThanOrEqual(1);
  });

  it('returns nothing for a path with fewer than two points', () => {
    expect(splitAntimeridian([])).toEqual([]);
    expect(splitAntimeridian([[1, 1]])).toEqual([]);
  });
});

describe('walking back from the target', () => {
  it('finds the point a given number of pixels before the end, with its bearing', () => {
    const path = arcPath([0, 0], [10, 0]);
    const r = walkBack(path, 4, 30)!;
    // heading east along the equator
    expect(r.bearing).toBeCloseTo(90, 3);
    const end = mercatorPixel([10, 0], 4);
    const at = mercatorPixel(r.at, 4);
    expect(Math.hypot(end[0] - at[0], end[1] - at[1])).toBeCloseTo(30, 3);
  });

  it('points north for a northbound arc and south for a southbound one', () => {
    expect(walkBack(arcPath([0, 0], [0, 10]), 4, 20)!.bearing).toBeCloseTo(0, 3);
    expect(Math.abs(walkBack(arcPath([0, 10], [0, 0]), 4, 20)!.bearing)).toBeCloseTo(180, 3);
  });

  it('gives up when the arc is shorter than the distance', () => {
    expect(walkBack(arcPath(LYON, PARIS), 0, 500)).toBeNull();
  });

  it('wraps a point past the antimeridian back into range', () => {
    const r = walkBack(arcPath(TOKYO, SF), 2, 40)!;
    expect(Math.abs(r.at[0])).toBeLessThanOrEqual(180);
  });
});

describe('clustering places closer than 20 px', () => {
  it('merges places that fall within 20 px at the zoom and keeps the others apart', () => {
    const places = [
      place('a', 4.8357, 45.764),
      place('b', 4.8367, 45.7645),
      place('c', 2.35, 48.85),
    ];
    const low = clusterPlaces(places, 2);
    expect(low.clusters.map((c) => c.members)).toEqual([['a', 'b'], ['c']]);
    expect(low.of.get('a')).toBe(0);
    expect(low.of.get('b')).toBe(0);
    expect(low.of.get('c')).toBe(1);
    // zoomed in far enough, a and b separate
    const high = clusterPlaces(places, 18);
    expect(high.clusters).toHaveLength(3);
  });

  it('puts places at one position in one cluster at any zoom', () => {
    const places = [place('a', 1, 1), place('b', 1, 1), place('c', 1, 1)];
    expect(clusterPlaces(places, 22).clusters).toHaveLength(1);
  });

  it('is deterministic and puts every place in exactly one cluster', () => {
    const places = Array.from({ length: 500 }, (_, i) =>
      place(`p${i}`, (i * 7.3) % 90, (i * 3.1) % 60),
    );
    const a = clusterPlaces(places, 3);
    const b = clusterPlaces(places, 3);
    expect(a.clusters.map((c) => c.id)).toEqual(b.clusters.map((c) => c.id));
    expect(a.clusters.flatMap((c) => c.members).sort()).toEqual(places.map((p) => p.id).sort());
    expect(a.of.size).toBe(500);
  });

  it('puts the cluster at the mean of its members and names it after the first', () => {
    const c = clusterPlaces([place('a', 10, 10, 'Alpha'), place('b', 10.001, 10.001)], 1)
      .clusters[0];
    expect(c.lon).toBeCloseTo(10.0005, 6);
    expect(c.lat).toBeCloseTo(10.0005, 6);
    expect(c.label).toBe('Alpha +1');
    expect(c.id).toBe('a+1');
  });

  it('averages across the antimeridian without going to the far side of the earth', () => {
    const c = clusterPlaces([place('a', 179.9999, 0), place('b', -179.9999, 0)], 1);
    // 0.0002 degrees apart across the seam, so the pixels are a few apart and not a world
    expect(c.clusters).toHaveLength(1);
    expect(Math.abs(c.clusters[0].lon)).toBeGreaterThan(179.99);
    expect(c.clusters[0].members).toEqual(['a', 'b']);
  });

  it('merges places on both sides of the seam at the world zoom, and not when far apart', () => {
    const near = clusterPlaces([place('a', 179, 0), place('b', -179, 0)], 0);
    expect(near.clusters).toHaveLength(1);
    expect(near.of.get('a')).toBe(near.of.get('b'));
    const far = clusterPlaces([place('a', 150, 0), place('b', -150, 0)], 0);
    expect(far.clusters).toHaveLength(2);
    // 170 and -179 are 15.6 px apart at zoom 0 with the seam between them
    expect(clusterPlaces([place('a', 170, 0), place('b', -179, 0)], 0).clusters).toHaveLength(1);
    // a place at exactly 180 and one at -180 are the same spot
    expect(clusterPlaces([place('a', 180, 10), place('b', -180, 10)], 3).clusters).toHaveLength(1);
  });

  it('grows the marker with the cluster', () => {
    expect(markerRadius(1)).toBe(5);
    expect(markerRadius(2)).toBeGreaterThan(5);
    expect(markerRadius(100)).toBeGreaterThan(markerRadius(10));
    expect(markerRadius(1e9)).toBe(18);
  });
});

describe('aggregating links', () => {
  it('draws one arc with the count for many links between the same two places (A1)', () => {
    // ten people in Lyon and four in Paris all work for an organization in Paris
    const places = [
      ...Array.from({ length: 10 }, (_, i) => place(`lyon${i}`, 4.8357 + i * 0.0002, 45.764)),
      ...Array.from({ length: 4 }, (_, i) => place(`paris${i}`, 2.3 + i * 0.0002, 48.85)),
      place('acme', 2.3522, 48.8566),
    ];
    const links: Link[] = [
      ...Array.from({ length: 10 }, (_, i) => ({
        from: `lyon${i}`,
        to: 'acme',
        predicate: 'worksIn',
      })),
      ...Array.from({ length: 4 }, (_, i) => ({
        from: `paris${i}`,
        to: 'acme',
        predicate: 'worksIn',
      })),
    ];
    const { clusters, of } = clusterPlaces(places, 3);
    const agg = aggregateLinks(links, of, clusters.length);
    // the people of Lyon are one cluster, the people of Paris are one with the organization
    const lyon = of.get('lyon0')!;
    const paris = of.get('acme')!;
    expect(clusters[lyon].members).toHaveLength(10);
    expect(of.get('paris0')).toBeDefined();
    const arcs = agg.arcs.filter((a) => a.from === lyon && a.to === paris);
    expect(arcs).toHaveLength(1);
    expect(arcs[0].count).toBe(10);
    expect(arcLabel(arcs[0])).toBe('10');
    // the Paris people are a local cluster: their links stay inside it
    expect(agg.local[paris] + agg.local[of.get('paris0')!]).toBeGreaterThanOrEqual(4);
    expect(agg.unlocated).toBe(0);
  });

  it('keeps one arc per predicate and per direction, with its own rank', () => {
    const { clusters, of } = clusterPlaces([place('a', 0, 0), place('b', 40, 0)], 1);
    const links: Link[] = [
      { from: 'a', to: 'b', predicate: 'p' },
      { from: 'a', to: 'b', predicate: 'q' },
      { from: 'b', to: 'a', predicate: 'p' },
      { from: 'a', to: 'b', predicate: 'p' },
    ];
    const agg = aggregateLinks(links, of, clusters.length);
    expect(agg.arcs).toHaveLength(3);
    const ab = agg.arcs.filter((a) => a.from === 0);
    expect(ab.map((a) => a.rank).sort()).toEqual([0, 1]);
    expect(ab.find((a) => a.predicate === 'p')!.count).toBe(2);
    expect(agg.arcs.find((a) => a.from === 1)!.rank).toBe(0);
  });

  it('keeps added and removed links apart from unchanged ones', () => {
    const { clusters, of } = clusterPlaces([place('a', 0, 0), place('b', 40, 0)], 1);
    const links: Link[] = [
      { from: 'a', to: 'b', predicate: 'p' },
      { from: 'a', to: 'b', predicate: 'p', change: 'added' },
      { from: 'a', to: 'b', predicate: 'p', change: 'removed' },
    ];
    const agg = aggregateLinks(links, of, clusters.length);
    expect(agg.arcs.map((a) => a.change ?? '').sort()).toEqual(['', 'added', 'removed']);
    expect(arcLabel(agg.arcs.find((a) => a.change === 'added')!)).toBe('+');
    expect(arcLabel(agg.arcs.find((a) => a.change === 'removed')!)).toBe('−');
  });

  it('counts links whose end has no position and draws nothing for them', () => {
    const { clusters, of } = clusterPlaces([place('a', 0, 0), place('b', 40, 0)], 1);
    const agg = aggregateLinks(
      [
        { from: 'a', to: 'ghost', predicate: 'p' },
        { from: 'ghost', to: 'b', predicate: 'p' },
        { from: 'a', to: 'b', predicate: 'p' },
      ],
      of,
      clusters.length,
    );
    expect(agg.unlocated).toBe(2);
    expect(agg.arcs).toHaveLength(1);
  });

  it('labels a single link with its own caption, and many with their count', () => {
    const { clusters, of } = clusterPlaces([place('a', 0, 0), place('b', 40, 0)], 1);
    const one = aggregateLinks(
      [{ from: 'a', to: 'b', predicate: 'p', label: 'Flight 12', row: 3 }],
      of,
      clusters.length,
    );
    expect(arcLabel(one.arcs[0])).toBe('Flight 12');
    expect(one.arcs[0].rows).toEqual([3]);
    const two = aggregateLinks(
      [
        { from: 'a', to: 'b', predicate: 'p', label: 'x' },
        { from: 'a', to: 'b', predicate: 'p', label: 'y' },
      ],
      of,
      clusters.length,
    );
    expect(arcLabel(two.arcs[0])).toBe('2');
  });

  it('reads at most MAX_ARCS links and says how many it left out', () => {
    const { clusters, of } = clusterPlaces([place('a', 0, 0), place('b', 40, 0)], 1);
    const links: Link[] = Array.from({ length: MAX_ARCS + 5 }, () => ({
      from: 'a',
      to: 'b',
      predicate: 'p',
    }));
    const agg = aggregateLinks(links, of, clusters.length);
    expect(agg.dropped).toBe(5);
    expect(agg.arcs[0].count).toBe(MAX_ARCS);
  });

  it('widens arcs with the count, up to a limit, or draws them all alike', () => {
    expect(arcWidth(1)).toBeLessThan(arcWidth(4));
    expect(arcWidth(4)).toBeLessThan(arcWidth(64));
    expect(arcWidth(1e9)).toBe(14);
    expect(arcWidth(1, 'uniform')).toBe(arcWidth(500, 'uniform'));
  });
});

describe('colours', () => {
  it('gives the most used predicate the first colour, stably', () => {
    const links: Link[] = [
      { from: 'a', to: 'b', predicate: 'q' },
      { from: 'a', to: 'b', predicate: 'p' },
      { from: 'a', to: 'b', predicate: 'p' },
    ];
    const c = predicateColours(links);
    expect(c.get('p')).toBe(PALETTE[0]);
    expect(c.get('q')).toBe(PALETTE[1]);
    expect(predicateColours([...links].reverse()).get('p')).toBe(PALETTE[0]);
  });

  it('reuses colours past the palette', () => {
    const links = Array.from({ length: PALETTE.length + 2 }, (_, i) => ({
      from: 'a',
      to: 'b',
      predicate: `p${i}`,
    }));
    expect(predicateColours(links).get(`p${PALETTE.length}`)).toBeDefined();
  });
});

describe('the layout handed to the map', () => {
  const places = [
    place('a', 4.8357, 45.764),
    place('b', 2.3522, 48.8566),
    place('c', 139.69, 35.69),
    place('d', -122.42, 37.77),
  ];
  const links: Link[] = [
    { from: 'a', to: 'b', predicate: 'p' },
    { from: 'b', to: 'a', predicate: 'p' },
    { from: 'c', to: 'd', predicate: 'q' },
  ];

  it('has a line and an arrowhead per arc and a marker per cluster', () => {
    const { clusters, of } = clusterPlaces(places, 3);
    const agg = aggregateLinks(links, of, clusters.length);
    const lay = layoutArcs(clusters, agg, 3, predicateColours(links));
    expect(lay.lines.features).toHaveLength(3);
    expect(lay.heads.features).toHaveLength(3);
    expect(lay.markers.features).toHaveLength(clusters.length);
    // the arc across the antimeridian is two parts
    const parts = lay.lines.features.map((f) => f.geometry.coordinates.length).sort();
    expect(parts).toEqual([1, 1, 2]);
    expect(lay.lines.features.every((f) => f.geometry.type === 'MultiLineString')).toBe(true);
  });

  it('draws the arcs of the two directions apart', () => {
    const { clusters, of } = clusterPlaces(places, 3);
    const agg = aggregateLinks(links, of, clusters.length);
    const lay = layoutArcs(clusters, agg, 3, predicateColours(links));
    const [ab, ba] = lay.lines.features.filter((f) => f.properties.id.includes('p|'));
    const mid = (f: typeof ab) => {
      const pts = f.geometry.coordinates[0];
      return pts[Math.floor(pts.length / 2)];
    };
    expect(angularDistance(mid(ab) as LonLat, mid(ba) as LonLat)).toBeGreaterThan(0.01);
  });

  it('colours changed arcs with the change colour and keeps the predicate colour otherwise', () => {
    const { clusters, of } = clusterPlaces(places, 3);
    const l: Link[] = [
      { from: 'a', to: 'b', predicate: 'p' },
      { from: 'a', to: 'b', predicate: 'p', change: 'added' },
      { from: 'a', to: 'b', predicate: 'p', change: 'removed' },
    ];
    const agg = aggregateLinks(l, of, clusters.length);
    const lay = layoutArcs(clusters, agg, 3, predicateColours(l), 'count', {
      added: '#00ff00',
      removed: '#ff0000',
    });
    const color = (change: string) =>
      lay.lines.features.find((f) => f.properties.change === change)!.properties.color;
    expect(color('')).toBe(PALETTE[0]);
    expect(color('added')).toBe('#00ff00');
    expect(color('removed')).toBe('#ff0000');
  });

  it('leaves out the arrowhead of an arc too short to hold it', () => {
    const near = [place('a', 0, 0), place('b', 0.01, 0.01)];
    const { clusters, of } = clusterPlaces(near, 0.5, 0.0001);
    const agg = aggregateLinks([{ from: 'a', to: 'b', predicate: 'p' }], of, clusters.length);
    const lay = layoutArcs(clusters, agg, 0.5, new Map());
    expect(lay.lines.features).toHaveLength(1);
    expect(lay.heads.features).toHaveLength(0);
  });

  it('picks the hot clusters and arcs of a selection', () => {
    const { clusters, of } = clusterPlaces(places, 3);
    const agg = aggregateLinks(links, of, clusters.length);
    const hot = hotClusters(clusters, (id) => id === 'a');
    expect([...hot]).toEqual([of.get('a')]);
    const arcs = hotArcs(agg, hot);
    expect(arcs.size).toBe(2);
  });
});

describe('what is in view', () => {
  it('lists the clusters in a box, largest first, and all of them without a box', () => {
    const { clusters } = clusterPlaces(
      [
        place('a', 0, 0, 'A'),
        place('b', 0.0001, 0, 'B'),
        place('c', 10, 10, 'C'),
        place('d', 50, 50, 'D'),
      ],
      2,
    );
    expect(clustersInBox(clusters, null).map((i) => clusters[i].label)).toEqual(['A +1', 'C', 'D']);
    expect(clustersInBox(clusters, [-1, -1, 12, 12]).map((i) => clusters[i].label)).toEqual([
      'A +1',
      'C',
    ]);
    // a box across the antimeridian
    const wrap = clusterPlaces(
      [place('x', 170, 0), place('y', -170, 0), place('z', 0, 0)],
      2,
    ).clusters;
    expect(clustersInBox(wrap, [160, -5, -160, 5])).toHaveLength(2);
  });

  it('puts a box as drawn into range, across the seam or around the world', () => {
    expect(wrapBox([170, -10, 190, 10])).toEqual([170, -10, -170, 10]);
    expect(wrapBox([-190, -10, -170, 10])).toEqual([170, -10, -170, 10]);
    expect(wrapBox([-20, -10, 30, 10])).toEqual([-20, -10, 30, 10]);
    expect(wrapBox([-400, -10, 400, 10])).toEqual([-180, -10, 180, 10]);
    // another copy of the world, with the box ending on the seam
    expect(wrapBox([100, -10, 180, 10])).toEqual([100, -10, 180, 10]);
    // a marker at -175 is in view when the map shows 170 to 190
    const c = clusterPlaces([place('p', -175, 0)], 4).clusters;
    expect(clustersInBox(c, wrapBox([170, -5, 190, 5]))).toHaveLength(1);
  });

  it('boxes places', () => {
    expect(placesBox([])).toBeNull();
    expect(placesBox([place('a', 1, 2), place('b', 3, -4)])).toEqual([1, -4, 3, 2]);
  });
});

describe('the caps', () => {
  it('draws the first 20,000 places and leaves the links that end in the others unlocated', () => {
    const places = Array.from({ length: MAX_POINTS + 10 }, (_, i) =>
      place(`p${i}`, (i % 300) - 150, (i % 100) - 50),
    );
    const capped = capPlaces(places);
    expect(capped).toHaveLength(MAX_POINTS);
    expect(capped[0].id).toBe('p0');
    expect(capPlaces(places.slice(0, 5))).toHaveLength(5);
    const { clusters, of } = clusterPlaces(capped, 4);
    const agg = aggregateLinks(
      [
        { from: 'p0', to: 'p1', predicate: 'q' },
        { from: 'p0', to: `p${MAX_POINTS + 5}`, predicate: 'q' },
      ],
      of,
      clusters.length,
    );
    expect(agg.unlocated).toBe(1);
  });
});

describe('the frame budget', () => {
  it('lays out 20,000 distinct arcs in a fraction of a second', () => {
    const n = 20_000;
    // pseudo-random but fixed
    let s = 12345;
    const rnd = () => (s = (s * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
    const places = Array.from({ length: 2 * n }, (_, i) =>
      place(`p${i}`, rnd() * 340 - 170, rnd() * 140 - 70),
    );
    const links: Link[] = Array.from({ length: n }, (_, i) => ({
      from: `p${2 * i}`,
      to: `p${2 * i + 1}`,
      predicate: `r${i % 4}`,
    }));
    const t0 = performance.now();
    const { clusters, of } = clusterPlaces(places, 8);
    const agg = aggregateLinks(links, of, clusters.length);
    const lay = layoutArcs(clusters, agg, 8, predicateColours(links));
    const ms = performance.now() - t0;
    // eslint-disable-next-line no-console
    console.log(
      `layout of ${lay.lines.features.length} arcs and ${clusters.length} markers: ${ms.toFixed(0)} ms`,
    );
    expect(lay.lines.features.length).toBeGreaterThan(19_000);
    expect(ms).toBeLessThan(5000);
  });
});
