# P01: Python bindings

> **Status:** designed, not built
>
> **Phases:** Phase 1 is the `sparkles-py` crate and the `sparkles` Python package. It
> covers datasets, loading, SPARQL, terms, quad access, transactions, dumps, compaction,
> reasoning and SHACL and ShEx validation, with type stubs, a pytest suite, `mise run
> py:test` and `py:build`, and a flake check. Phase 2 is listed in §9.
>
> **User docs:** [Usage: Python](../USAGE.md#python) ·
> [Features](../FEATURES.md#known-gaps) · [Comparison](../COMPARISON.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This is a clean-room spec. Its sources are the Sparkles code, the public documentation of
PyO3 and maturin, the Python packaging PEPs, and the documented Python APIs of pyoxigraph
(MIT OR Apache-2.0) and rdflib (BSD-3-Clause). pyoxigraph and rdflib were used as prior art
for the shape of the API and for the term model Python users expect. No code was copied
from either project. Fluree was not consulted.

The bindings depend on these parts of Sparkles:

* the embedded API in `crates/sparkles/src/dataset.rs` (`Dataset`, `Transaction`,
  `Solutions`);
* `sparkles::sparql::{query, QueryOptions, QueryResult}` and
  `sparkles::sparql::update`;
* `sparkles::io` (`Source`, `RdfFormat`, `format_for_path`, `parse_to_vec`) and
  `sparkles::codec` for compressed inputs and dumps;
* the public entry points of `sparkles-reasoner`, `sparkles-shacl` and `sparkles-shex`.

It closes the known gap "Embedding: Sparkles is Rust only" in
[FEATURES.md](../FEATURES.md#known-gaps) for Python, and the Embedding row of the
Oxigraph comparison in [COMPARISON.md](../COMPARISON.md).

## 1. Summary, goals, non-goals

Sparkles can be embedded in Rust programs only. Python is where most RDF tooling,
notebooks and data pipelines live, and pyoxigraph shows that a Rust store with a small
Python API is useful there. This spec adds a Python package, `sparkles`, built from a new
crate `crates/sparkles-py` with PyO3 and maturin. The package opens persistent or
in-memory datasets, loads files and strings, runs SPARQL queries and updates, reads and
writes quads as Python term objects, and exposes transactions, dumps, compaction,
reasoning and validation.

**Goals**

* **One process, no server.** The package links the engine. A Python program needs
  neither a running `sparkles serve` nor HTTP.
* **A familiar shape.** The term classes, `Dataset.query`, `load`, `dump` and
  `quads_for_pattern` follow pyoxigraph's names and signatures where Sparkles has the
  same concept, so code moves between the two with small changes. Terms convert to and
  from rdflib terms.
* **The GIL is released for the work.** Queries, updates, loads, dumps, compaction,
  reasoning and validation run without the GIL, so other Python threads keep running and
  several threads can query one dataset in parallel.
* **Results stream to Python.** Iterating a SELECT result or a quad pattern decodes terms
  and creates Python objects one item at a time. Quad patterns read the index in batches
  and never collect the whole match.
* **Pythonic errors.** Every engine error becomes an exception in one hierarchy rooted at
  `sparkles.SparklesError`. The classes also derive from the matching built-in exception,
  such as `SyntaxError`, `ValueError` or `TimeoutError`.
* **Typed.** The package ships `.pyi` stubs and a `py.typed` marker (PEP 561), and the
  stubs are checked against the compiled module.
* **One wheel per platform.** Wheels use the stable ABI (abi3) for CPython 3.10 and
  later, so one wheel serves every supported CPython on a platform.

**Non-goals**

* **The HTTP server and CLI from Python.** `sparkles serve` stays a separate binary. A
  Python client for a remote server is a SPARQL client concern and is left to existing
  libraries such as SPARQLWrapper.
* **An rdflib `Store` plugin.** rdflib's plugin protocol is large and its triple-at-a-time
  model is slow over FFI. This is Phase 2 at the earliest (open question 2).
* **Free-threaded CPython wheels.** abi3 does not cover the free-threaded build (3.14t).
  Phase 2 can add version-specific free-threaded wheels.
* **Windows wheels in Phase 1.** Sparkles builds and tests on Linux and macOS only (the
  flake's systems). Windows waits until the engine is tested there.
* **PyPy.** PyO3 supports PyPy 3.11, but Phase 1 builds and tests CPython only.
* **Publishing to PyPI.** Phase 1 builds wheels locally and in the flake. Publishing,
  signing and a release workflow are Phase 2.

## 2. Crate, package and build

### 2.1 Layout

```
crates/sparkles-py/
  Cargo.toml        the cdylib crate, its own workspace and Cargo.lock
  pyproject.toml    maturin build backend, project metadata
  src/              the Rust module `sparkles._sparkles`
  python/sparkles/  __init__.py, _errors.py, the .pyi stubs and py.typed
  tests/            the pytest suite
```

The compiled module is `sparkles._sparkles`. `sparkles/__init__.py` re-exports its
classes and functions, and defines the exception hierarchy in pure Python (§5), which is
the simplest way to give an exception two built-in bases. The distribution name is
`sparkles-rdf`, because a short name like `sparkles` is likely taken on PyPI. The import
name is `sparkles`.

### 2.2 Keeping the crate out of the default workspace

The crate is listed in the root workspace's `exclude` and declares its own `[workspace]`.
It has its own `Cargo.lock`, which starts as a copy of the root lock so that both resolve
the same versions, and it repeats the root's `[patch.crates-io]` entry for the vendored
spargebra and the root's profiles.

The alternative was a workspace member left out of `default-members`. It was rejected for
three reasons:

* `mise run lint` and `mise run test` use `--workspace`, so every developer and every CI
  run would compile PyO3 and the bindings, whether or not they touch Python.
* A test or doctest target of a PyO3 crate links against libpython. `cargo test
  --workspace` would then need a Python installation with its shared library, which the
  Rust build does not need today.
* maturin and PyO3 choose link arguments from the `PYO3_BUILD_EXTENSION_MODULE`
  environment variable, which `cargo` alone does not set. A member crate would build
  differently under `cargo build` and under maturin.

The cost is a second lock file. The `py:lock` task (§8) refreshes it from the root lock,
and the flake check fails when it no longer matches the crate's dependencies. The crate
uses path dependencies on `crates/sparkles` and the other library crates, which keep
inheriting their dependency versions from the root workspace.

### 2.3 Cargo features

The crate's features have the server's names, so a build can be configured the same way:

| Feature | Default | Effect |
|---|---|---|
| `reasoning` | on | `Dataset.reason`, `Dataset.clear_inferences` (crate `sparkles-reasoner`) |
| `shacl` | on | `Dataset.validate_shacl` (crate `sparkles-shacl`) |
| `shex` | on | `Dataset.validate_shex` (crate `sparkles-shex`) |
| `text` | on | `sparkles/text`, so `text:query` works on datasets with a text index |
| `geo` | on | `sparkles/geo`, the GeoSPARQL functions |

`sparkles/zstd` and `sparkles/brotli` are always on, as in the server, so compressed
inputs and dumps work with every codec. In a build without a feature, its methods still
exist, and they raise `UnsupportedError` that names the missing feature. The stubs then
stay valid for every build, and `sparkles.FEATURES` lists what the build has.

### 2.4 Wheels

* **ABI.** PyO3's `abi3-py310` feature. CPython 3.9 reached end of life in October 2025,
  which makes 3.10 the oldest supported release in Phase 1. `requires-python` is
  `>=3.10`.
* **Platforms.** manylinux 2.28 on x86_64 and aarch64, and macOS 11 or later on x86_64
  and arm64. maturin checks the manylinux policy with auditwheel rules. The flake check
  builds the wheel for the host platform only.
* **Build backend.** maturin 1.x, declared in `pyproject.toml` (PEP 517). `pip install
  crates/sparkles-py` builds from source with a Rust toolchain.
* **Licenses.** The wheel carries the Apache-2.0 license of Sparkles and a generated
  `THIRD_PARTY_LICENSES.md` for the crates it links, declared with `license-files`
  (PEP 639).
* **Allocator.** The extension uses the system allocator. The server's mimalloc is a
  process-wide choice that an extension module must not make for its host interpreter.

## 3. Python API

The names below are the Python API. The stubs in `python/sparkles/_sparkles.pyi` are its
reference. Parameters after `*` are keyword-only.

### 3.1 Terms

| Class | Constructor | Attributes |
|---|---|---|
| `NamedNode` | `NamedNode(value: str)` | `value` |
| `BlankNode` | `BlankNode(value: str \| None = None)` | `value` |
| `Literal` | `Literal(value, *, datatype=None, language=None, direction=None)` | `value`, `datatype`, `language`, `direction` |
| `Triple` | `Triple(subject, predicate, object)` | `subject`, `predicate`, `object` |
| `Quad` | `Quad(subject, predicate, object, graph_name=None)` | `subject`, `predicate`, `object`, `graph_name`, `triple` |
| `DefaultGraph` | `DefaultGraph()` | none |
| `Variable` | `Variable(value: str)` | `value` |

All term classes are immutable, hashable and compare by RDF term equality. `str(term)`
gives the N-Triples form, such as `<http://ex.org/a>` or `"42"^^<…#integer>`, and
`repr(term)` gives a constructor call. `Triple` and `Quad` unpack like tuples, so
`s, p, o = triple` works.

* `NamedNode` validates its IRI and raises `ValueError` on an invalid one.
* `BlankNode()` without a value gets a fresh random label. §3.5 explains how labels map
  to stored blank nodes.
* `Literal(value)` with a `str` value is a plain string literal, with `language` a
  language-tagged string, and with `datatype` a typed literal. `direction` (`"ltr"` or
  `"rtl"`) makes an RDF 1.2 directional language-tagged string and requires `language`.
  A non-string `value` is converted by its Python type (§4.2), and then `datatype` and
  `language` must be omitted.
* `Literal.to_python()` returns the native Python value for the XSD datatypes of §4.2
  and the lexical form otherwise.
* `Triple` is also an RDF 1.2 triple term and may be the object of another triple.
* `Quad`'s `graph_name` defaults to `DefaultGraph()`.

### 3.2 Datasets

```python
class Dataset:
    def __init__(self, path: str | os.PathLike | None = None, *,
                 union_default_graph: bool = False) -> None: ...
    @staticmethod
    def memory() -> Dataset: ...
    @staticmethod
    def open(path: str | os.PathLike, *, union_default_graph: bool = False) -> Dataset: ...
    def close(self) -> None: ...
    def __enter__(self) -> Dataset: ...
    def __exit__(self, *exc) -> None: ...
    path: str | None                  # None for an in-memory dataset
```

`Dataset()` is in memory and `Dataset(path)` opens or creates a persistent database
directory, which stays locked while the dataset is open. `close()` releases this handle.
The directory's lock is released once the iterators and transactions that still use the
dataset are gone. A closed dataset raises `InvalidInputError` on every method. The
dataset is a context manager that closes on exit.

### 3.3 Loading

```python
def load(self, input: str | bytes | IO[bytes] | IO[str] | None = None,
         format: RdfFormat | str | None = None, *, path: str | os.PathLike | None = None,
         base_iri: str | None = None, to_graph: NamedNode | str | None = None,
         compression: str | None = None, lenient: bool = False) -> int: ...
def load_files(self, paths: Iterable[str | os.PathLike], *,
               to_graph: NamedNode | str | None = None) -> int: ...
```

`load` reads RDF from `input` or from the file at `path` and returns the number of new
quads. `format` is required for `input`. For a path it defaults to the format of the file
name, so `data.ttl.gz` is gzipped Turtle. Formats are Turtle, N-Triples, N-Quads, TriG,
N3, RDF/XML and JSON-LD. Compressed data (gzip, zstd, brotli, LZ4) is recognized by its
magic bytes, then by the file extension, as in `sparkles load`. `compression` forces a
codec. `to_graph` puts the triples of a triple format into a named graph, `lenient`
skips the validation of IRIs and language tags, and `base_iri` resolves relative IRIs.
`load_files` loads many files in one commit through the parallel bulk path.

### 3.4 SPARQL

```python
def query(self, query: str, *, base_iri=None, prefixes: Mapping[str, str] | None = None,
          bindings: Mapping[str | Variable, Term] | None = None,
          default_graph: Iterable[NamedNode | str] | None = None,
          named_graphs: Iterable[NamedNode | str] | None = None,
          include_inferred: bool = False, timeout: float | None = None
          ) -> QuerySolutions | bool | QueryTriples: ...
def select(self, query: str, **options) -> QuerySolutions: ...
def ask(self, query: str, **options) -> bool: ...
def construct(self, query: str, **options) -> QueryTriples: ...
def update(self, update: str, *, base_iri=None, prefixes=None) -> UpdateStats: ...
```

`query` returns a `QuerySolutions` for SELECT, a `bool` for ASK, and a `QueryTriples` for
CONSTRUCT and DESCRIBE. `select`, `ask` and `construct` check the query form and raise
`InvalidInputError` on another one, which lets type checkers see the result type.

* `bindings` pre-binds variables, like `QueryOptions::initial_bindings`.
* `default_graph` and `named_graphs` replace the query's dataset, like the protocol's
  `default-graph-uri` and `named-graph-uri`.
* `include_inferred` merges the reasoner's `urn:x-sparkles:inferred` graph into the
  default graph, as the server does for reasoning datasets.
* `prefixes` declares prefixes for the query text, and `timeout` is in seconds.
* `update` runs a SPARQL Update request in one transaction and returns `UpdateStats`,
  whose `inserted` and `deleted` count quads.

`QuerySolutions` is an iterator of `QuerySolution` and has `variables`, a list of
`Variable`. A `QuerySolution` behaves like a tuple of terms and also maps names to terms:

* `solution[0]`, `solution["name"]` and `solution[Variable("name")]` return the term or
  `None` when the variable is unbound;
* `tuple(solution)` and iteration give the values in `variables` order;
* `solution.as_dict()` returns a `dict` of the bound variables;
* `len(solution)` is the number of variables.

`QueryTriples` is an iterator of `Triple`.

### 3.5 Quads

```python
def add(self, quad: Quad) -> bool: ...
def remove(self, quad: Quad) -> bool: ...
def extend(self, quads: Iterable[Quad | Triple]) -> int: ...
def quads_for_pattern(self, subject=None, predicate=None, object=None,
                      graph_name=None) -> QuadIterator: ...
def __iter__(self) -> QuadIterator: ...
def __contains__(self, quad: Quad) -> bool: ...
def __len__(self) -> int: ...
def named_graphs(self) -> list[NamedNode | BlankNode]: ...
def clear_graph(self, graph_name: NamedNode | DefaultGraph | str) -> int: ...
def clear(self) -> None: ...
```

A `None` pattern position matches anything. `graph_name=None` matches every graph,
including the default graph, and `DefaultGraph()` matches the default graph only, as in
pyoxigraph. `add`, `remove` and `extend` commit at once. `extend` writes all its quads in
one commit, and a `Triple` in it goes to the default graph.

Blank nodes follow the Rust API. A blank node read from the dataset has a label of the
form `b<hex>` that names the stored node, so it can be used in a later pattern, removal
or binding. Any other label names a new node, and the same label within one `extend` or
transaction names the same node. Two separate `add` calls with `BlankNode("x")` create two
nodes. pyoxigraph keeps labels across writes, and this difference is documented in the
usage guide.

### 3.6 Transactions

```python
with ds.transaction() as tx:
    tx.add(quad)
    tx.remove(other)
    tx.extend(quads)
    found = tx.quads_for_pattern(None, knows, None)   # sees the transaction's changes
```

`transaction()` takes the dataset's single writer lock and returns a `Transaction`. The
block commits when it ends normally and rolls back when it raises. `tx.commit()` and
`tx.rollback()` end it explicitly, and a transaction that is garbage-collected without
either rolls back. `quads_for_pattern` and `__contains__` on the transaction see its own
uncommitted changes. Queries on the dataset during the transaction read the last commit.

A write on the dataset from the thread that holds an open transaction would wait for its
own lock forever. The binding detects this and raises `ConflictError` instead. Writes
from other threads wait until the transaction ends, without holding the GIL.

### 3.7 Output and maintenance

```python
def dump(self, output: str | os.PathLike | IO[bytes] | None = None,
         format: RdfFormat | str | None = None, *,
         from_graph: NamedNode | DefaultGraph | str | None = None,
         compression: str | None = None) -> bytes | None: ...
def compact(self) -> None: ...
def backup(self, directory: str | os.PathLike) -> str: ...
prefixes: dict[str, str]
def set_prefix(self, prefix: str, iri: str) -> None: ...
```

`dump` serializes the dataset. It returns `bytes` when `output` is `None`, writes a file
when `output` is a path, and writes to a binary file object otherwise. A quad format
writes every graph, and a triple format writes the default graph, or `from_graph`. A path
takes its format and compression from its name, so `out.nq.zst` writes zstd-compressed
N-Quads. `compression` overrides it. `compact()` merges the pending changes into a new
index generation, like `sparkles compact`. `backup()` writes a compressed N-Quads backup
into a directory and returns its path.

The module also has `parse(input=None, format=None, *, path=None, base_iri=None,
lenient=False)`, which returns an iterator of `Quad` without a dataset, and
`serialize(input, output=None, format=None, *, prefixes=None)`, which writes triples or
quads. `RdfFormat` has the constants `TURTLE`, `N_TRIPLES`, `N_QUADS`, `TRIG`, `N3`,
`RDF_XML` and `JSON_LD`, the attributes `name`, `media_type`, `file_extension` and
`supports_datasets`, and the constructors `from_extension` and `from_media_type`. Every
`format` parameter also takes a name, extension or media type as a string.

### 3.8 Reasoning and validation

```python
def reason(self, profile: str = "rdfs", *, rules: str | None = None) -> ReasonReport: ...
def clear_inferences(self) -> int: ...
def validate_shacl(self, shapes: str | bytes | None = None, *, format="turtle",
                   shapes_graph: NamedNode | str | None = None,
                   data_graph: NamedNode | str | None = None,
                   include_inferred: bool = False) -> ShaclReport: ...
def validate_shex(self, schema: str, shape_map: str, *, format: str | None = None,
                  data_graph: NamedNode | str | None = None,
                  include_inferred: bool = False) -> ShexReport: ...
```

`reason` materializes the entailments of `rdfs`, `rdfs-simple` or `owl-rl`, or of Jena
rule text given as `rules`, into `urn:x-sparkles:inferred` (`sparkles.INFERRED_GRAPH`),
like `sparkles infer`. `ReasonReport` has `profile`, `rules`, `iterations`, `inferred`,
`millis` and `warnings`.

`validate_shacl` takes the shapes as text in an RDF format, or reads them from the named
graph `shapes_graph`. `ShaclReport` has `conforms` and `results`, and `to_turtle()`
returns the W3C validation report graph. Each `ShaclResult` has `focus_node`, `path`
(SPARQL property path syntax or `None`), `value`, `source_shape`,
`constraint_component`, `severity` and `message`.

`validate_shex` takes a ShExC or ShExJ schema (sniffed, or named by `format`) and a shape
map in compact or JSON syntax. Imports are resolved from files only. `ShexReport` has
`conforms`, `results` and `to_json()`. Each `ShexResult` has `node`, `shape` (an IRI
string or `"START"`), `conformant` and `reason`.

## 4. Type mapping

### 4.1 RDF terms

| RDF (oxrdf) | sparkles | pyoxigraph | rdflib |
|---|---|---|---|
| `NamedNode` | `NamedNode` | `NamedNode` | `URIRef` |
| `BlankNode` | `BlankNode` | `BlankNode` | `BNode` |
| `Literal` | `Literal` | `Literal` | `Literal` |
| `Triple` (RDF 1.2 triple term) | `Triple` | `Triple` | no equivalent |
| `GraphName::DefaultGraph` | `DefaultGraph` | `DefaultGraph` | `DATASET_DEFAULT_GRAPH_ID` |
| `Quad` | `Quad` | `Quad` | a 4-tuple |
| query variable | `Variable` | `Variable` | `Variable` |

Every parameter that takes a term also accepts the rdflib classes in the table by duck
typing, without importing rdflib. The binding recognizes `rdflib.term.URIRef`, `BNode`
and `Literal` by their class names and reads their `str` value, `datatype` and `language`.
A `str` is accepted where only an IRI makes sense, such as a graph name or `to_graph`.
`sparkles.rdflib` has `to_rdflib(term)` and `from_rdflib(term)` for explicit conversion,
and it imports rdflib only when called.

### 4.2 Literal values

| Python value | Literal datatype | `to_python()` returns |
|---|---|---|
| `str` | `xsd:string` | `str` |
| `bool` | `xsd:boolean` | `bool` |
| `int` | `xsd:integer` | `int` (also for the derived integer types) |
| `float` | `xsd:double` | `float` (also for `xsd:float`) |
| `decimal.Decimal` | `xsd:decimal` | `Decimal` |
| `datetime.datetime` | `xsd:dateTime` | `datetime` (aware when the literal has a zone) |
| `datetime.date` | `xsd:date` | `date` |
| `datetime.time` | `xsd:time` | `time` |
| `bytes` | `xsd:base64Binary` | `bytes` (also for `xsd:hexBinary`) |

Any other datatype, and an ill-typed lexical form, gives the lexical form as `str`. The
mapping uses the lexical forms of XSD 1.1, so `Literal(1.0)` is `"1.0E0"^^xsd:double`.

## 5. Errors

The exception classes are defined in Python so that each can have two bases. The binding
maps `sparkles::Error` variants and the validators' errors onto them.

| Exception | Bases | Raised for |
|---|---|---|
| `SparklesError` | `Exception` | the root, and errors without a narrower class |
| `ParseError` | `SparklesError`, `SyntaxError` | the root of the syntax errors |
| `SparqlSyntaxError` | `ParseError` | `Error::SparqlSyntax` |
| `RdfSyntaxError` | `ParseError` | `Error::RdfParse`, invalid rules, shapes and ShEx schemas |
| `InvalidInputError` | `SparklesError`, `ValueError` | `Error::Invalid`, a closed dataset, a wrong query form |
| `UnsupportedError` | `SparklesError`, `NotImplementedError` | `Error::Unsupported`, a feature left out of the build |
| `QueryTimeoutError` | `SparklesError`, `TimeoutError` | `Error::Timeout` |
| `CancelledError` | `SparklesError` | `Error::Cancelled` |
| `BudgetExceededError` | `SparklesError` | `Error::BudgetExceeded`, with `kind`, `limit` and `requested` |
| `StorageError` | `SparklesError`, `OSError` | `Error::Corrupt`, `Poisoned`, `StorageFull` |
| `DatasetLockedError` | `StorageError` | a database directory open in another process |
| `ConflictError` | `SparklesError` | `Error::Conflict`, `PreconditionFailed`, `WriterBusy`, a write that would deadlock on the thread's own transaction |
| `NotFoundError` | `SparklesError`, `LookupError` | `Error::NotFound` |
| `PermissionDeniedError` | `SparklesError`, `PermissionError` | `Error::NotPermitted` |
| `ServiceError` | `SparklesError` | `Error::Service` |
| `WriteRejectedError` | `SparklesError` | `Error::Rejected`, `GuardMissing` |

`Error::Io` becomes the built-in `OSError` subclass for its kind, such as
`FileNotFoundError`, as Python's own file functions raise. An invalid IRI or language tag
passed to a term constructor raises `ValueError`, and a wrong argument type raises
`TypeError`. The message of every exception is the engine's message.

## 6. Threads and the GIL

* Every call that reads or writes the store converts its arguments to Rust values with
  the GIL held, runs with the GIL released (PyO3's `Python::detach`), and converts the
  result after it takes the GIL back. That covers queries, updates, loads, dumps,
  `quads_for_pattern` batches, compaction, backups, reasoning and validation.
* `Dataset` is safe to share between threads. Reads work on snapshots and run in
  parallel. Writes go through the store's single writer lock and wait for it without the
  GIL, so a waiting writer never blocks other Python threads.
* The store's write transaction holds a lock guard that must stay on one OS thread. The
  binding runs each Python transaction on its own Rust thread, which owns the guard and
  executes the operations the Python thread sends it over a channel. The Python object
  is then free to move between threads, and the GIL is released while it waits for each
  answer.
* A dump to a Python file object writes through a buffer and takes the GIL only to call
  `write` with each full buffer.
* Ctrl-C does not interrupt a running query, because the query runs without the GIL and
  Python checks signals only in its own code. `timeout` bounds a query. Cancellation from
  Python is Phase 2 (open question 3).
* The extension module does not declare free-threading support, so a free-threaded
  interpreter loads it with the GIL enabled.

## 7. Iteration and streaming

* **SELECT.** The engine produces the whole solution table as ids, which is how it
  evaluates queries. `QuerySolutions` keeps the table and turns one row at a time into
  terms and Python objects. A large result costs 8 bytes per cell until it is consumed,
  instead of one Python object per value.
* **CONSTRUCT and DESCRIBE.** The engine returns the triples as a vector of terms.
  `QueryTriples` creates the Python objects one at a time.
* **Quad patterns.** `quads_for_pattern` picks the index permutation with the longest
  bound prefix, as `Dataset::find` does, and reads at most 4096 keys per batch. It decodes
  them as Python consumes them and starts the next batch after the last key. The
  iterator reads one snapshot, so it does not see later commits. This needs an additive
  API in `crates/sparkles`, `Dataset::quads` returning `QuadIter`, which Rust users get
  too.
* **Dumps** stream from the store to the output without building the document in memory.
* **Loads** from a Python file object read it whole first. Loads from a path stream as in
  `sparkles load`.
* **`parse`** parses the whole input before returning its iterator. A streaming parser
  is Phase 2.

## 8. Tests, tasks and CI

* **pytest suite** in `crates/sparkles-py/tests`, written from the acceptance examples
  in §10. Optional parts skip when rdflib or mypy is missing.
* **Stub check.** A test runs mypy's `stubtest` against the compiled module when mypy is
  installed, so the stubs cannot drift from the module.
* **`mise run py:build`** builds a release wheel with maturin into `target/wheels`.
* **`mise run py:test`** builds the extension with cargo in the root `target` directory,
  so it shares compiled dependencies with the workspace, assembles the package in
  `target/py`, and runs pytest on it. It uses the `python3` on `PATH` when it has pytest,
  as the dev shell's does. Otherwise it creates a virtual environment in `target/py-venv`
  with uv and installs pytest there.
* **`mise run py:lock`** refreshes `crates/sparkles-py/Cargo.lock` from the root lock.
* **Flake.** `packages.sparkles-py` builds the wheel with nixpkgs' maturin hook and
  installs it into a Python environment. `checks.python-bindings` builds it and runs the
  pytest suite on the installed package. The dev shell gets maturin, and its Python gets
  pytest, mypy and rdflib.
* **CI.** `py:test` joins `mise run ci` when it takes under a minute on a warm cache and
  needs nothing beyond the dev shell. Otherwise it stays a separate task and the flake
  check covers it (the Outcome records the choice).
* **Licenses.** `scripts/third-party-licenses.py` also writes
  `crates/sparkles-py/THIRD_PARTY_LICENSES.md` for the crates the wheel links, and
  `licenses:check` checks it.

## 9. Phasing

**Phase 1** is everything in §2 to §8.

**Phase 2** lists what is left out of Phase 1, in no particular order:

* free-threaded (3.14t) wheels, and Windows and PyPy wheels;
* publishing to PyPI, with a release workflow that builds the manylinux and macOS wheels
  in CI;
* cancelling a running query from Python, including on `KeyboardInterrupt`;
* an rdflib `Store` plugin (open question 2);
* SPARQL results serialization from Python (`QuerySolutions.serialize` to JSON, XML, CSV
  and TSV);
* streaming `parse` and streaming loads from Python file objects;
* `update` inside a transaction;
* history and commits (`head_commit`, `commits`, point-in-time queries with `at=`),
  named snapshots and `clone_to`;
* the full-text and vector index administration (`enable_text`, vector indexes);
* write-time validation (installing the SHACL or ShEx guard from Python);
* the query builder, and `QueryOptions` budgets (`max_rows`, `max_memory_bytes`);
* pickling of terms;
* async wrappers (`asyncio.to_thread` already works, because calls release the GIL).

## 10. Acceptance examples

* **A1.** `Dataset()` is empty. After `ds.load(b"<a:s> <a:p> \"x\" .", "nt")`,
  `len(ds) == 1` and `Quad(NamedNode("a:s"), NamedNode("a:p"), Literal("x")) in ds`.
* **A2.** `ds.load(path="data.ttl.gz")` loads the same quads as the uncompressed file,
  and `ds.load(gzip.compress(text), "turtle")` does too.
* **A3.** A persistent dataset written, closed and reopened has the same quads. Opening
  the same directory twice in the same process raises `DatasetLockedError`, and closing
  the first opens the way for the second.
* **A4.** `ds.query("SELECT ?s ?o WHERE { ?s ?p ?o } ORDER BY ?o")` iterates
  `QuerySolution`s where `sol["o"]`, `sol[1]` and `sol[Variable("o")]` are the same
  `Literal`, `tuple(sol)` has two terms, and an unbound OPTIONAL variable is `None`.
* **A5.** `ds.query("ASK { ?s ?p ?o }") is True`. A CONSTRUCT query returns `Triple`s,
  and `ds.select("ASK {}")` raises `InvalidInputError`.
* **A6.** `ds.query("SELECT * WHERE {")` raises `SparqlSyntaxError`, which is also a
  `SyntaxError` and a `SparklesError`. Loading malformed Turtle raises `RdfSyntaxError`,
  and loading a missing path raises `FileNotFoundError`.
* **A7.** `ds.update("INSERT DATA { <a:s> <a:q> 1 }").inserted == 1`, and
  `bindings={"s": NamedNode("a:s")}` restricts a query to that subject.
* **A8.** In `with ds.transaction() as tx:`, a quad added is visible to
  `tx.quads_for_pattern` and not to `ds.query` until the block ends. An exception in the
  block discards the changes. `ds.add` inside the block raises `ConflictError`.
* **A9.** A literal round-trips for every Python type of §4.2, and `Literal("chat",
  language="fr")`, a directional literal and a triple term round-trip through a dataset.
* **A10.** `ds.dump(format="nq")` returns bytes that load into a new dataset with the
  same quads, and `ds.dump(path / "out.nq.zst")` writes zstd that loads back.
* **A11.** With the default features, `ds.reason("rdfs")` makes `?x a :Animal` match a
  `:Dog` instance when `include_inferred=True`. `validate_shacl` reports a `sh:minCount`
  violation with its focus node and path, and `validate_shex` reports a nonconformant
  node.
* **A12.** While one thread runs a long query, another Python thread keeps running, and
  queries from four threads on one dataset all succeed.
* **A13.** `quads_for_pattern` over 10,000 matching quads yields them all in index order,
  and an iterator taken before a commit does not see it.
* **A14.** mypy's `stubtest sparkles` passes, and an rdflib `URIRef` passed as a subject
  matches like the equal `NamedNode`.

## 11. Open questions

1. **Distribution name.** `sparkles-rdf` is a placeholder until a PyPI name is chosen.
   Default: `sparkles-rdf`, import name `sparkles`.
2. **rdflib integration depth.** A `Store` plugin would let `rdflib.Graph(store="sparkles")`
   run SPARQL in Sparkles. Default: term conversion only in Phase 1, and the plugin after
   a measurement of its per-triple overhead.
3. **Interrupting queries.** Running the query on a worker thread while the calling
   thread polls `PyErr_CheckSignals` would make Ctrl-C work, at the cost of a thread
   hop per query. Default: Phase 2, with the hop only for queries that pass
   `interruptible=True`.
4. **Blank node labels across writes.** pyoxigraph keeps a label's identity across
   writes. Sparkles could keep a per-dataset label table in the binding, but it would grow
   without bound. Default: follow the Rust API and document it.

## 12. Sources

* PyO3 0.29 user guide and API docs: classes, `Python::detach`, `abi3` features, building
  and distribution, and `PYO3_BUILD_EXTENSION_MODULE`.
* maturin 1.x user guide: mixed Rust and Python projects, `python-source`,
  `module-name`, `pyproject.toml` metadata and manylinux compliance.
* PEP 384 (stable ABI), PEP 517 (build backends), PEP 561 (typed packages), PEP 599 and
  PEP 600 (manylinux), PEP 639 (license files), and the Python release schedule for end
  of life dates.
* pyoxigraph's documentation (MIT OR Apache-2.0): the `Store`, term, `parse`,
  `serialize` and `RdfFormat` API, read for names and signatures. Its source was not read.
* rdflib's documentation (BSD-3-Clause): `rdflib.term` and `Literal.toPython`, read for
  the term model and the native value mapping.
* mypy's `stubtest` documentation.
* Sparkles code: `dataset.rs`, `sparql/mod.rs`, `io.rs`, `codec.rs`, `error.rs`,
  `store.rs` (`WriteTxn`, scans), and the entry points of the reasoner, SHACL and ShEx
  crates.
