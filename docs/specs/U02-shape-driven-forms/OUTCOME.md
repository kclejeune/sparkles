# U02 delivery record

The [design's Outcome overview](SPEC.md#outcome) summarizes current scope. This record
preserves the initial delivery and the later affected-target validation contract.

## Nested library read models

`Forms::nested_model` supplies the ordinary root model plus declared blank-node
children. It pools shared RDF identities, preserves every visible graph-bearing
parent anchor and merges the declared child layouts. Cycles use references to an
already admitted identity and regenerate only when the declared shape union grows.
Each child resolves the target-graph policy against its own type facts, preserving
readable child fields when the root's type graph differs. Historical models remain read-only and require a retained
history state, as other historical reads do.

The request shares its preparation allowance across layouts, retained values, frontier
entries and repeated declarations. Node, field, anchor and depth limits refuse the
whole result. Complex nested paths and matching composite bound comparisons refuse
before uncontrolled producer work. The [design](SPEC.md#231-nested-read-models-in-the-rust-library)
records the finite limits, remaining model-source preparation and the separate item 395
reader boundary.

Native store fixtures cover shared and indistinguishable children, cycles, graph
provenance, graph and predicate grants in both caller orders, retained historical
state and resource refusal. The prepared-adapter allocation regression checks group
labels before their owned model copy, with parsing and source metadata outside that
measurement. Existing flat form behavior remains covered separately.

This delivery does not add nested changes to check/save, allocate fresh children or
wire the HTTP and browser editors. Those consumers, lists and alternative editors
remain under item 21. Stored blank labels and request-local model ids confer no write
authority; a future save must validate graph-bearing root anchors and changed-field
expectations inside the same writer.

## Bounded affected-target checks

Checks select targets from shape dependencies in both the writer's base and candidate
states. Sequence, inverse, alternative and repeated paths can reach beyond two hops.
Directly touched resources seed the dependency prefixes, so a nested shape's existing
failure on another property of that resource remains visible. Unrelated nearby targets
are excluded when no shape dependency reaches the touched resource.

All current violations on admitted targets are reported. The check does not reduce its
answer to newly introduced violations. A globally dependent shape validates its own
actual targets within the same allowance rather than expanding every unrelated shape.
The same allowance admits at most 500 additional distinct targets beyond the directly
touched resources, shared across shapes and both states. Exactly 500 is accepted. The
next target refuses the check rather than returning a partial conformance report.

Dependency analysis admits owned path normalization, generated prefixes and memo copies before cloning them. Lexical copies charge each 128-byte chunk to the same structural work allowance. SPARQL dependency localization admits directly anchored triples and single path patterns with pure filters. Genuinely global queries still enumerate bounded actual targets. Property functions and potentially computed predicates need their actual read producers. Indirect joins and other unadmitted query-dependency producers refuse the check rather than broadening it to unrelated targets. Streaming predicate and path traversal charges work before retaining new frontier
entries. Cancellation and one absolute deadline cover discovery and validation. The
allowance bounds target discovery and its retained state, not the entire SHACL heap or
the existing cold index decoder. Controlled physical reader integration remains under
items 21 and 395.

Form applicability checks admit graph-free target-node expressions before evaluating
their output. Constants, lists, focus variables, basic list operators, conditionals and
simple aggregates are supported. Data-reading, shape-dependent, custom and SPARQL
function or query target-node expression producers are refused until early admission
is implemented. Target filters that inspect only their input node remain available. Graph-reading target filters are refused before ordinary conformance can materialize a property fanout. This boundary also applies when constructing a form model or saving
through it. It does not remove those expressions from ordinary SHACL validation.

A refused recheck invalidates the browser's previous approval of the same changes.
With a reject guard, Save requests a new check and sends no save after another refusal.
The library fixtures cover far targets, pre-existing violations, global constraints,
shared target allowances, exact boundary acceptance and rollback. A separate allocator
fixture measures delta-backed traversal so cold base-block decoding does not masquerade
as retained discovery state. The browser regression fails before approval invalidation
and passes afterward.

## Initial forms delivery

The following section records the original delivery, including its former radius
limit. The current capability boundary is described above and in the user guide.

Phase 1 shipped on 2026-10-24 under [ROADMAP](../../ROADMAP.md) item 21. Phase 2 shipped
on the same day without nested forms. The DASH editors and viewers of §2.3 choose the
widgets and the read-only display, **New instance** in the schema tab opens the form of a
class and the first save mints the IRI, and the entity picker searches the instances of a
field's `sh:class` by label, through the full-text index when the dataset has one. Nested
forms for blank node values and Phase 3 remain.

**Where it lives.** `sparkles-shacl` reads the non-validating characteristics of property
shapes (`sh:name`, `sh:description`, `sh:order`, `sh:group`, `sh:defaultValue`), the labels
and orders of `sh:PropertyGroup`s and the DASH properties `dash:editor`, `dash:viewer`,
`dash:singleLine` and `dash:readOnly`, chooses the node shapes of a focus node with
`shapes_for_node`, and merges their property shapes into fields in its `form` module. The
`sparkles` crate's `Dataset::forms()` handle builds the model, checks a change and saves
it, so library callers get the same forms as HTTP. The server adds the routes of §3, the
`forms` settings kind kept in `forms.json` at version 1, the session branch and the grants.
The UI has `lib/forms.ts`, `lib/xsd.ts`, `ShapeForm.svelte`, `FormField.svelte`, an **Edit**
button in Explore and the page `/datasets/{name}/edit`.

**Decisions.** These settle what the spec left open or change it.

* A field's `id` is its path in SPARQL syntax, such as `<http://ex/name>` or
  `^<http://ex/memberOf>`, rather than a shape and a path. Two node shapes that constrain
  the same path give one field with the stricter of each constraint and both shapes in
  `sources`, as §2.8 asks, and the id stays the same across loads.
* The server chooses the widget by §2.3 and sends it, and the UI replaces it only with the
  widget of a DASH editor it knows. A field whose only constraints are on strings
  (`sh:pattern`, `sh:minLength`, `sh:maxLength`) and that states no node kind gets a text
  input for a plain literal instead of the term input.
* A field with `sh:in` and no `sh:maxCount` is a radio group, as A1 asks, unless the
  resource already has several values. With `sh:maxCount` above 1 or several values it is
  a list of checkboxes.
* `saveTo` has a third value, `auto`, which is the default. It saves to `main` unless
  `main` is protected, which is the behaviour §2.6 describes for the default.
* `expect` must name every changed field, and may be empty for a field that had no value.
  A check reports the same conflicts a save would, so a stale form learns of them on
  **Preview**.
* With a `reject` guard, **Save** stays focusable instead of disabled. It runs the check
  first when no check of the same changes passed, and when the guard would reject the
  change it shows the error summary and writes nothing. Without a guard or in `warn` mode,
  a save with problems asks once with **Save with problems**.
* A check of a session branch that does not exist yet runs on `main`'s head and previews
  the branch's first commit. The store's write options gained `preview_as_new_branch`,
  which lets such a dry run past `main`'s protection and does nothing for a commit. A check
  never creates the branch. A save creates only the caller's own session branch, which
  needs the right to write branches and grants that do not limit the caller to some graphs
  or triples, since such a branch would copy `main`.
* The session branch's `{user}` is the caller's name, or `anonymous` without
  authentication, and `{date}` is the day in UTC.
* A caller whose grants limit the graphs or triples it reads gets checks without
  validation results (`withheld: true`) and redacted receipts, as the validation routes and
  previews treat such callers.
* Values in the inferred graph `urn:x-sparkles:inferred` are neither shown nor edited.
* Forms open only for an IRI focus. A form for a new instance takes `class=` and has no
  values, its node shapes are those whose class targets select the class or a
  superclass, and its save mints the IRI from `newIri`, refuses one that already
  describes a resource and adds the `rdf:type` to the target graph.
* The model reads through the caller's query endpoint grants and decides what is writable
  from its update endpoint grants on the branch saves go to. A value in a graph the caller
  cannot write is read-only inside an editable field. A protection scoped by classes or a
  pattern does not make a field read-only, because it depends on the data, and the save
  enforces it.
* Shapes from shapes graphs are parsed from the state masked by the caller's protections.
* A save plans its update on the head before it waits for the writer lock. When another
  commit moved a changed field's values to other graphs or changed the graph new values go
  to, without changing the values, the save plans again on the new head, up to three
  times. A new instance's IRI is checked again under the writer lock, so a save refuses an
  IRI that another commit began to describe while it waited. A check plans on the state its dry run starts from, and is a dry run from the
  moment it opens, so a failed check leaves no terms behind.
* The check validates the resource, the IRIs a change adds or removes, and up to 500 nodes
  one or two triples away from one of them in either direction, so shapes with inverse
  steps are covered. With a guard on, the preview's guard
  verdict covers every other node. Without one, a shape that reaches the resource through
  a longer path is not validated by the check.
* The model, the check and the save stop when the request is cancelled or its deadline
  passes, also while they read values, and a model never answers with fields left empty
  by such a stop.
* The check and save routes are admitted on any branch the caller can see, as the branch
  routes are, and the handler checks write on the branch the body or the query names.
* A new instance's type counts as a change, so its form can be saved with no values.
  The entity picker of a field with several `sh:class` constraints offers only instances
  of all of them.
* A date-time keeps its timezone when edited, and a new `xsd:dateTimeStamp` takes the
  browser's offset for that date. Patterns with the `x` or `q` flag are checked only by the
  server. The fields are disabled while a save and its reload run.
* The check's validation holds the writer lock while it runs and stops after 10 seconds,
  after which the answer says `timedOut` and Save follows the guard's mode.
* A field with more than 200 values is read-only with reason `too-many`, the other
  properties list at most 100 predicates, and a change may remove and add at most 1,000
  terms.
* The `forms` settings are read from `main`, so every branch of a dataset uses the same
  form settings.
* A change counts each replaced, added or removed value once in the form's summary.
* The Python binding is `Dataset.forms.model`, `check` and `save`, which take and return
  the HTTP route's JSON as dicts and take the `forms` setting as a `settings` dict. A save
  that meets a conflict raises `ConflictError` with the code `form-conflict` and the
  fields. A session branch is edited through `Dataset.branch(name).forms`. The Node.js and
  JVM bindings remain planned in `bindings.toml`.

**Open questions.** Question 4 is settled: `dash:readOnly` is enforced on the server, and a
save that changes such a field answers `403`. For question 3, a removed value is deleted
from every graph that holds it, an added value goes to the `targetGraph`, and moving a value
between graphs is not offered. Questions 1 and 2 stay open: ShEx schemas do not drive
forms, and a session branch is an ordinary branch that stays after a merge until someone
deletes it.

**Tests.** `sparkles-shacl/tests/forms.rs` covers the layout, the merge of shapes and the
widgets. `sparkles/tests/forms.rs` covers targets by class with subclasses, inverse and
complex paths, other properties, checks and conflicts through the library. The server's
`http/forms_tests.rs` covers A1 to A5 and A7 over HTTP, the check against `/{ds}/shacl` on
the same state, a check before the session branch exists, conflicts under six concurrent
writers and the settings kind, and `router_tests/auth/forms.rs` covers A6 with a C12b
protection, readers and a caller limited to one graph. Vitest covers the browser checks,
the XSD lexical forms, the widgets, term parsing, change sets and the save plan. Playwright
runs A1 to A5, A7 and A9 against a real server in `tests/e2e/forms.spec.ts` and the layout,
the checks, branch saves, conflicts, the error summary, the empty state and the phone
layout against the mock in `tests/mock/forms.spec.ts`.

The review fixes added library tests for a value that moves between graphs while a save
waits, a failed check that must leave no terms, the dependents a check validates and a
cancelled model request. Server tests cover shapes graphs under a protection, a writer of
some branches that names its branch in the body, grants split between the query and update
endpoints, and a class-scoped protection. The mock tests cover a select with seven
choices, a value in a graph the caller cannot write, the accessible description of a
value's problem and edits during a save.

**Limits.** Nested forms for blank node values with `dash:DetailsEditor` remain from
Phase 2, and list editors and `sh:or` or `sh:xone` alternatives are Phase 3, all under
item 21. The check's validation of other nodes stops at two triples from the resource.
The entity picker searches labels as Explore's search does and does not limit its hits to
the field's `sh:class`, which the server's check reports instead. Complex paths are shown
without values. See the [screenshot](../../images/forms.png) of a form in Explore and the
[screenshot](../../images/forms-branch.png) of a form saving to a session branch.
