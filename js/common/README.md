# @sparkles-rdf/common

Shared RDF/JS terms, data factory, bindings, receipts, errors and the `SparqlDataset` interface for Sparkles embedded and remote clients. This package has no runtime dependency on a native addon or Node.js APIs.

```ts
import { factory, type SparqlDataset } from '@sparkles-rdf/common';

const number = factory.fromJs(9007199254740993n);
console.log(number.toJs()); // bigint
const directional = factory.literal('مرحبا', { language: 'ar', direction: 'rtl' });
```

Factories accept Unicode RDF blank-node labels and foreign RDF/JS terms. Literals preserve their lexical forms; `toJs()` converts supported, valid typed values without losing integer precision. Unsafe integer numbers are rejected; pass a `bigint` instead. Commit receipts likewise expose counts and commit numbers as `bigint`.

`SparqlDataset` provides `query`, `select`, `ask`, `construct` and `update`. Both `Dataset` from `@sparkles-rdf/engine` and `RemoteDataset` from `@sparkles-rdf/client` implement it and share the same error classes.
