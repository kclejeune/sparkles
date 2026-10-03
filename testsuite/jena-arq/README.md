# Apache Jena's ARQ tests

The directories here are copied unchanged from Apache Jena's `jena-arq/testing`:

- `SPARQL-CDTs` holds the tests of the composite datatypes `cdt:List` and `cdt:Map`,
  of their functions, of the `FOLD` aggregate and of the `UNFOLD` operator.

`crates/sparkles/tests/w3c.rs` runs their manifests with the W3C harness. The tests whose
answers Sparkles gives otherwise on purpose are listed in `expected-failures.txt` with the
reason.

Apache Jena is distributed under the Apache License 2.0, which is in `LICENSE-APACHE`.
Jena's `NOTICE` file is included as the license asks.
