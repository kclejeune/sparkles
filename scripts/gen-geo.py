#!/usr/bin/env python3
"""Generate a synthetic GeoSPARQL dataset (N-Triples) for tests and benchmarks.

usage: gen-geo.py N [--seed S] [--admin LEVELS] [--queries DIR] > data.nt

Seeded, so equal arguments give equal output:

* N point features clustered around 500 "cities" with log-normal populations, in CRS84
  (1% written in EPSG:4326, latitude first, to exercise axis swapping);
* N/10 line features (random walks of 2 to 200 vertices);
* N/20 polygon features (star polygons of 4 to 500 vertices, 10% with a hole, 5%
  multipolygons);
* an administrative hierarchy of polygons tiling the world: 40 countries, 1,600 states and
  64,000 counties (`--admin 1`/`2`/`3` levels, default 3, 0 for none). The cells of all
  levels lie on one lattice, so neighbours share their borders exactly (touches) and every
  cell is within its parent (within);
* every geometry is linked from a feature with `geo:hasDefaultGeometry` and typed with its
  Simple Features class; half of the features also have a label and a type, and 10% of the
  point features carry W3C Basic Geo `lat`/`long` as well.

`--queries DIR` also writes the benchmark queries over this data (Q1-Q4, Q8, Q9; Q10 is
Q1 without the index) with their constants taken from the generated geometries.
"""
import argparse
import math
import os
import random
import sys

EX = "http://example.org/"
GEO = "http://www.opengis.net/ont/geosparql#"
SF = "http://www.opengis.net/ont/sf#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
WGS = "http://www.w3.org/2003/01/geo/wgs84_pos#"
XSD = "http://www.w3.org/2001/XMLSchema#"
EPSG4326 = "http://www.opengis.net/def/crs/EPSG/0/4326"

ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
ap.add_argument("n", type=int, nargs="?", default=10000)
ap.add_argument("--seed", type=int, default=42)
ap.add_argument("--admin", type=int, default=3, choices=range(4))
ap.add_argument("--queries")
args = ap.parse_args()
n = args.n
rnd = random.Random(args.seed)
out = sys.stdout
w = out.write


def num(x):
    s = f"{x:.6f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def ring(coords):
    return "(" + ", ".join(f"{num(x)} {num(y)}" for x, y in coords) + ")"


def wkt_lit(text):
    return f'"{text}"^^<{GEO}wktLiteral>'


def clamp_lat(y):
    return max(-89.9, min(89.9, y))


def wrap_lon(x):
    return max(-179.9, min(179.9, x))


feature_no = 0


def feature(geom_wkt, sf_class, kind, label_prefix, latlon=None):
    """A feature, its geometry and the geometry's serialization."""
    global feature_no
    i = feature_no
    feature_no += 1
    f, g = f"<{EX}f/{i}>", f"<{EX}geom/{i}>"
    w(f"{f} <{GEO}hasDefaultGeometry> {g} .\n")
    w(f"{g} <{RDF}type> <{SF}{sf_class}> .\n")
    w(f"{g} <{GEO}asWKT> {wkt_lit(geom_wkt)} .\n")
    if rnd.random() < 0.5:
        w(f'{f} <{RDFS}label> "{label_prefix} {i}" .\n')
        w(f"{f} <{RDF}type> <{EX}{kind}> .\n")
    if latlon is not None:
        lat, lon = latlon
        w(f'{f} <{WGS}lat> "{num(lat)}"^^<{XSD}decimal> .\n')
        w(f'{f} <{WGS}long> "{num(lon)}"^^<{XSD}decimal> .\n')
    return i


# ------------------------------------------------------------------------- cities
cities = []
for c in range(500):
    lon, lat = rnd.uniform(-170, 170), rnd.uniform(-55, 70)
    cities.append((lon, lat, rnd.lognormvariate(0, 1.2)))
weights = [c[2] for c in cities]


def near_city(spread):
    lon, lat, _ = rnd.choices(cities, weights)[0]
    return wrap_lon(rnd.gauss(lon, spread)), clamp_lat(rnd.gauss(lat, spread * 0.7))


# ------------------------------------------------------------------------- points
for _ in range(n):
    x, y = near_city(0.4)
    if rnd.random() < 0.01:
        text = f"<{EPSG4326}> POINT({num(y)} {num(x)})"
    else:
        text = f"POINT({num(x)} {num(y)})"
    feature(text, "Point", "Place", "Place", (y, x) if rnd.random() < 0.1 else None)

# -------------------------------------------------------------------------- lines
for _ in range(max(1, n // 10)):
    x, y = near_city(0.6)
    pts = [(x, y)]
    heading = rnd.uniform(0, 2 * math.pi)
    for _ in range(rnd.randint(1, 199)):
        heading += rnd.gauss(0, 0.4)
        step = rnd.uniform(0.002, 0.02)
        x, y = wrap_lon(x + step * math.cos(heading)), clamp_lat(y + step * math.sin(heading))
        pts.append((x, y))
    feature("LINESTRING" + ring(pts), "LineString", "Road", "Road")


# ----------------------------------------------------------------------- polygons
def star(cx, cy, r, k, reverse=False):
    """A star-shaped ring of k vertices around (cx, cy), closed."""
    angles = sorted(rnd.uniform(0, 2 * math.pi) for _ in range(k))
    pts = [
        (wrap_lon(cx + r * rr * math.cos(a)), clamp_lat(cy + r * rr * math.sin(a)))
        for a, rr in ((a, rnd.uniform(0.5, 1.0)) for a in angles)
    ]
    if reverse:
        pts.reverse()
    return pts + [pts[0]]


def polygon_rings(cx, cy, r):
    rings = [ring(star(cx, cy, r, rnd.randint(4, 500)))]
    if rnd.random() < 0.1:
        # a hole well inside the star (whose radius is at least r / 2)
        rings.append(ring(star(cx, cy, r * 0.2, rnd.randint(3, 20), reverse=True)))
    return "(" + ", ".join(rings) + ")"


for _ in range(max(1, n // 20)):
    x, y = near_city(0.5)
    r = rnd.uniform(0.005, 0.05)
    if rnd.random() < 0.05:
        parts = [polygon_rings(x + i * 3 * r, y, r) for i in range(rnd.randint(2, 4))]
        feature("MULTIPOLYGON(" + ", ".join(parts) + ")", "MultiPolygon", "Area", "Area")
    else:
        feature("POLYGON" + polygon_rings(x, y, r), "Polygon", "Area", "Area")

# ------------------------------------------------- administrative hierarchy (lattice)
# countries are 8 x 5 cells of 45 x 36 degrees; states split each 8 x 5, counties again.
# Every cell edge is subdivided at the lattice of the finest level times SUB, so the
# borders of neighbours (and of a cell and its parent) share their vertices.
LEVELS = [("Country", 8, 5), ("State", 8, 5), ("County", 8, 5)][: args.admin]
SUB = 2  # vertices per finest-cell edge
cols, rows = 1, 1
for _, cx_, cy_ in LEVELS:
    cols, rows = cols * cx_, rows * cy_
FX, FY = 360.0 / max(cols, 1), 180.0 / max(rows, 1)


def cell_ring(c0, r0, c1, r1):
    """The lattice cells [c0, c1) x [r0, r1) as one counter-clockwise ring."""
    pts = []
    steps_x, steps_y = (c1 - c0) * SUB, (r1 - r0) * SUB
    lon = lambda c: -180.0 + c * FX
    lat = lambda r: -90.0 + r * FY
    for i in range(steps_x):
        pts.append((lon(c0 + i / SUB), lat(r0)))
    for i in range(steps_y):
        pts.append((lon(c1), lat(r0 + i / SUB)))
    for i in range(steps_x):
        pts.append((lon(c1 - i / SUB), lat(r1)))
    for i in range(steps_y):
        pts.append((lon(c0), lat(r1 - i / SUB)))
    return pts + [pts[0]]


admin = {}  # level name -> list of (feature number, (c0, r0, c1, r1))
span_c, span_r = cols, rows
cells = [(0, 0, cols, rows)]
for level, kx, ky in LEVELS:
    span_c, span_r = span_c // kx, span_r // ky
    nxt = []
    for c0, r0, c1, r1 in cells:
        for j in range(ky):
            for i in range(kx):
                nxt.append((c0 + i * span_c, r0 + j * span_r, c0 + (i + 1) * span_c, r0 + (j + 1) * span_r))
    cells = nxt
    admin[level] = []
    for box in cells:
        f = feature("POLYGON(" + ring(cell_ring(*box)) + ")", "Polygon", level, level)
        admin[level].append((f, box))

# ------------------------------------------------------------------------ queries
if args.queries:
    os.makedirs(args.queries, exist_ok=True)
    P = (
        f"PREFIX geo: <{GEO}> PREFIX geof: <http://www.opengis.net/def/function/geosparql/> "
        f"PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> PREFIX spatial: <http://jena.apache.org/spatial#> "
        f"PREFIX sf: <{SF}> PREFIX rdfs: <{RDFS}> PREFIX ex: <{EX}> "
    )
    lon0, lat0, _ = max(cities, key=lambda c: c[2])
    # the finest admin cell holding the largest city, else a 1-degree box around it
    finest = admin[LEVELS[-1][0]] if LEVELS else []
    box = None
    for _, (c0, r0, c1, r1) in finest:
        if -180 + c0 * FX <= lon0 < -180 + c1 * FX and -90 + r0 * FY <= lat0 < -90 + r1 * FY:
            box = (c0, r0, c1, r1)
    if box:
        region = "POLYGON(" + ring(cell_ring(*box)) + ")"
    else:
        region = "POLYGON(" + ring([(lon0 - 0.5, lat0 - 0.5), (lon0 + 0.5, lat0 - 0.5), (lon0 + 0.5, lat0 + 0.5), (lon0 - 0.5, lat0 + 0.5), (lon0 - 0.5, lat0 - 0.5)]) + ")"
    point = f"POINT({num(lon0)} {num(lat0)})"
    within = f"FILTER(geof:sfWithin(?w, {wkt_lit(region)}))"
    qs = {
        "geo-q1-within": f"SELECT ?g WHERE {{ ?g a sf:Point ; geo:asWKT ?w {within} }}",
        "geo-q2-distance": f"SELECT ?g WHERE {{ ?g a sf:Point ; geo:asWKT ?w FILTER(geof:distance(?w, {wkt_lit(point)}, uom:kilometre) < 5) }}",
        "geo-q3-nearby": f"SELECT ?f WHERE {{ ?f spatial:nearby ({num(lat0)} {num(lon0)} 50 uom:kilometre 10) }}",
        "geo-q4-withinbox": f"SELECT (COUNT(*) AS ?n) WHERE {{ ?f spatial:withinBox ({num(lat0 - 9)} {num(lon0 - 18)} {num(lat0 + 9)} {num(lon0 + 18)}) }}",
        "geo-q8-area": "SELECT (SUM(geof:metricArea(?w)) AS ?area) WHERE { ?g a sf:Polygon ; geo:asWKT ?w }",
        "geo-q9-star": f"SELECT ?f ?l ?t WHERE {{ ?f geo:hasDefaultGeometry ?g ; rdfs:label ?l ; a ?t . ?g a sf:Point ; geo:asWKT ?w {within} }}",
    }
    for name, q in qs.items():
        with open(os.path.join(args.queries, name + ".rq"), "w") as fh:
            fh.write(P + q + "\n")
