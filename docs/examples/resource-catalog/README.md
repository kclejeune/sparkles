# Curated resource catalog records

These catalog documents register public RDF releases by URL, version and SHA-256.
Use a catalog administrator account and a running Sparkles server.

```sh
sparkles source add --server "$SPARKLES_SERVER" skos.json
sparkles source add --server "$SPARKLES_SERVER" prov.json
sparkles source add --server "$SPARKLES_SERVER" dcat.json
sparkles source add --server "$SPARKLES_SERVER" w3c-vocabularies.json
sparkles source recipe --server "$SPARKLES_SERVER" w3c-vocabularies 1 > w3c-recipe.json
sparkles source plan --server "$SPARKLES_SERVER" w3c-recipe.json
sparkles source load --server "$SPARKLES_SERVER" example-dataset w3c-recipe.json
```

The target dataset must exist. The last command replaces the three named graphs listed
in the recipe. To install a vocabulary's named graph and namespaces directly, register
its record and run `sparkles source bundle example-dataset skos 2009`. The files
[qudt.json](qudt.json) and [schema.json](schema.json) register QUDT and Schema.org releases.

The pinned distributions come from the vocabulary publishers. Some publishers update
their namespace documents at the same URL. If one changes, verify the new bytes
and register them under a new version before loading the updated distribution.

The DCAT document declares
CC BY 4.0, QUDT uses CC BY 4.0, and Schema.org uses CC BY-SA 3.0. The SKOS and PROV records
leave the licence unspecified. [DCAT 3](https://www.w3.org/ns/dcat3),
[SKOS vocabularies](https://www.w3.org/2004/02/skos/vocabs),
[PROV-O](https://www.w3.org/TR/prov-o/),
[QUDT licence](https://github.com/qudt/qudt-public-repo/blob/main/LICENSE.md),
[Schema.org terms](https://schema.org/docs/terms.html).
