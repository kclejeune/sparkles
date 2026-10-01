# Bundled basemap

`ne-110m.json` is the basemap the UI's maps draw when the server has no
`--map-style-url`: land, coastlines and land boundaries of
[Natural Earth](https://www.naturalearthdata.com/) at 1:110m scale, release 5.1.2
(`ne_110m_land`, `ne_110m_coastline` and `ne_110m_admin_0_boundary_lines_land` from
<https://github.com/nvkelso/natural-earth-vector/tree/v5.1.2/geojson>).

**License:** Natural Earth is in the public domain
(<https://www.naturalearthdata.com/about/terms-of-use/>). No permission or credit is
needed; the map credits "Natural Earth" in its attribution all the same.

`scripts/basemap.mjs` writes the file from the three GeoJSON files: it keeps only the
geometries (each feature's `k` is `land`, `coast` or `border`), rounds coordinates to
0.01° and drops repeated points.

```sh
node scripts/basemap.mjs DIR   # DIR holds the three ne_110m_*.geojson files
```

`style.ts` is the MapLibre style that draws it.
