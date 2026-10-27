# U02: Editing forms generated from SHACL shapes

This design was written from the W3C SHACL Recommendation, in particular its
non-validating property shape characteristics (`sh:name`, `sh:description`, `sh:order`,
`sh:group`, `sh:defaultValue`) and `sh:PropertyGroup`, the SHACL 1.2 Core drafts for list
constraints (which [G03](../G03-shaclc.md) implements), the DASH Data Shapes vocabulary
published by TopQuadrant (its `dash:editor`, `dash:viewer`, `dash:singleLine` and
`dash:readOnly` properties and its editor and viewer instances), the public documentation
of TopBraid EDG's shape-driven forms, and the Schímatos paper on generating web forms
from SHACL shapes (ISWC 2020). It builds on write-time validation
([C10](../C10-write-time-validation.md)), write previews ([C15](../C15-write-previews.md)),
branches and merges ([F09](../F09-branches-and-merges.md)), schema discovery and drafted
shapes ([C02](../C02-schema-discovery.md)), access control
([C09](../C09-dataset-access-control.md), [C12](../C12-graph-access-control.md),
[C12b](../C12b-triple-access-control.md)), layered settings
([C19](../C19-layered-settings.md)) and the Sparkles code. No code of TopBraid products was
read or used.

## 1. Summary, goals, non-goals

The UI can change data only through SPARQL Update in the query editor, uploads, and the
review page of proposal branches (C18 §7.10), whose **Edit value** replaces one fact's
object. A person who maintains a curated dataset with SHACL shapes has no form that lists
an entity's fields with their types, required values and allowed values, and that keeps
the person from saving data the shapes reject.

This spec generates forms from the shapes a dataset already has: the shapes of its
write-time guard, its shapes graphs, or shapes drafted from the data. A form shows the
fields the shapes describe for a resource, checks values as they are typed, previews the
commit with C15's dry run, and saves to a branch or to `main`. Review and merge use the
existing merge page.

**Goals**

1. A form for any resource that a node shape targets, laid out by `sh:group` and
   `sh:order` and labelled by `sh:name`, with no configuration beyond the shapes.
2. The widget of each field follows from its constraints, and DASH hints override the
   choice.
3. Checks that need no data run in the browser on each change. Checks that need the data
   run on the server against the state the save would make, before the save.
4. A save is one commit with a message. It goes to a branch or to `main`, as the
   dataset's setting and branch protection decide, and never loses a concurrent change
   to the same field silently.
5. Grants decide: fields the caller cannot see are absent, and fields the caller cannot
   write are read-only.

**Non-goals**

- A general form designer. The shapes are the form's definition, and DASH hints are the
  only layout vocabulary.
- ShEx-driven forms. G02's schemas validate, but this spec reads SHACL only (§10).
- SHACL-SPARQL constraints or SHACL rules in the browser. The server checks them.
- Editing ontologies or the shapes themselves. The shapes editor of the Validate panel
  does that.
- Bulk editing of many resources in a table. That is a later spec.

## 2. User-visible behaviour

### 2.1 Where forms appear

- **Explore.** The properties panel of a focused resource gets **Edit** when a shape
  targets the resource and the caller can write. The panel turns into the form.
- **Schema tab.** A class with a shape gets **New instance**, which opens an empty form.
- **Direct link.** `/datasets/{name}/edit?iri=…&branch=…` opens the form alone, for
  links from other tools.

### 2.2 The form

```
┌─ Edit ex:alice · Person ─────────────── on branch edits.ana ▾ ─────────────┐
│ Shapes: write-time guard (3 shapes)                         [View shapes]  │
├────────────────────────────────────────────────────────────────────────────┤
│ Identity                                                                    │
│   Name *            [Alice Lima          ] en ▾   + add                     │
│   Born              [1990-04-02 📅]                                         │
│   Email             [alice@example     ]  ⚠ must match ^[^@]+@[^@]+\.[a-z]+$ │
│ Work                                                                        │
│   Works for *       [ex:acme  Acme Corp ✕]  search…                         │
│   Role              ( ) Engineer (•) Manager ( ) Other                      │
│ Other properties (not in the shapes)                                        │
│   ex:nickname       "Al"                                         read-only  │
├────────────────────────────────────────────────────────────────────────────┤
│ 1 problem · 2 changes          [Preview]   Message [Update Alice's email]   │
│                                                         [Cancel]  [Save]    │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Groups.** Property shapes with an `sh:group` are shown under the group's `rdfs:label`,
  ordered by the group's `sh:order`. Shapes without a group come first. Within a group,
  fields are ordered by `sh:order`, then by name.
- **Labels and help.** A field's label is `sh:name` in the UI's language, then English,
  then untagged, then the predicate's label from the schema report, then its prefixed
  name. `sh:description` is help text under the field.
- **Required.** `sh:minCount 1` or more marks the field "required" in text after the
  label as well as with `*`.
- **Many values.** A field whose `sh:maxCount` is absent or above 1 lists its values with
  **add** and a remove button on each. **add** is disabled at `sh:maxCount`.
- **Values outside the shapes.** Triples of the resource that no property shape covers
  are listed under **Other properties**, read-only, so a form never hides data. A shape
  with `sh:closed true` marks them as problems.
- **Read-only fields.** `dash:readOnly true`, a predicate the caller's grants protect from
  writes, and a value in a graph the caller cannot write are shown read-only with the
  reason in a tooltip.

### 2.3 Widgets

The first matching row chooses the widget, unless `dash:editor` names one (Phase 2).

| Constraint | Widget |
|---|---|
| `sh:in` | A radio group up to six values, a select above that. |
| `sh:datatype xsd:boolean` | A checkbox, or a tri-state select when the field is optional. |
| `sh:datatype xsd:date`, `xsd:dateTime` | A date or date-time input. |
| `sh:datatype` of a numeric XSD type | A number input with `step` from the type and bounds from `sh:minInclusive` and the like. |
| `sh:datatype rdf:langString`, or `sh:languageIn` | A text input with a language select limited to `sh:languageIn`. |
| `sh:datatype xsd:string` with `dash:singleLine false`, or `sh:maxLength` above 200 | A text area. |
| `sh:datatype` other | A text input checked against the datatype's lexical form. |
| `sh:class C` or `sh:nodeKind sh:IRI` | An entity picker that searches instances of `C` by label, and accepts a pasted IRI or prefixed name. |
| `sh:node S` with a blank node value | A nested form for `S` (Phase 2). |
| none | A term input that takes an IRI, a prefixed name or a literal in SPARQL syntax. |

DASH editors map to these widgets in Phase 2: `dash:TextFieldEditor`,
`dash:TextAreaEditor`, `dash:TextFieldWithLangEditor`, `dash:DatePickerEditor`,
`dash:DateTimePickerEditor`, `dash:BooleanSelectEditor`, `dash:EnumSelectEditor`,
`dash:InstancesSelectEditor`, `dash:AutoCompleteEditor`, `dash:URIEditor` and
`dash:DetailsEditor`. Viewers used in view mode are `dash:LabelViewer`,
`dash:LiteralViewer`, `dash:URIViewer` and `dash:DetailsViewer`. An editor or viewer the
UI does not know falls back to the table above and is listed under **View shapes**.

### 2.3.1 Nested read models in the Rust library

`Forms::nested_model` returns the ordinary root model and a pool of declared blank-node
children. Each child keeps its RDF identity, the visible parent field and graph anchors,
and a layout built from every visible `sh:node` declaration that reaches it. Shared
children use one identity. Cycles refer to an admitted child rather than expanding a
second copy. Field values retain their graph provenance even when the same child and
predicate occur in several graphs. Each child resolves the requested target-graph
policy against its own focus. An unchanged self-cycle does not generate another
layout; an actual addition to the declared shape union does.

The request admits at most 64 nested identities, 512 generated field views, 512 parent
anchors and a first-discovery depth of 16. Every expansion shares up to 65,536
preparation units. One unit admits a structural step or 128 lexical bytes, and a
smaller `QueryOptions::max_memory_bytes` lowers that allowance by dividing its value by
128. Field views generated again after another declaration joins a shared child use
the same cumulative allowance. Cancellation or exhaustion refuses the whole model.
Complex nested paths and matching composite range comparisons refuse before their
uncontrolled producers run. Visible blank-named graph contexts also refuse until the
form graph representation supports them. No truncated nested model is returned as complete.

This admission covers layout preparation and adapter-owned traversal and retention.
Raw scan work consumes units even when the caller's view filters out a row. Physical
column and dictionary decoding and permission mask preparation need controlled-reader
adoption through item 395. Source-shape parsing and prefix metadata admission also
remain under item 21. The preparation allowance is not a total model heap bound.

The nested model is a Rust library read primitive. The existing HTTP model, check/save
bodies and browser editors retain their flat contract. Atomic nested changes, fresh
child allocation, graph-specific conflict expectations and browser editing remain
under item 21. A read model or a request-local child id grants no write authority.

### 2.4 Checks

- **In the browser, on each change.** Lexical forms of XSD datatypes, `sh:pattern` with
  `sh:flags`, `sh:minLength`, `sh:maxLength`, numeric and date ranges, `sh:in`,
  `sh:minCount`, `sh:maxCount`, `sh:languageIn` and `sh:uniqueLang`. A problem shows
  under its field with `aria-describedby`, and the summary bar counts them.
- **On the server, on Preview and before Save.** Every constraint of the shapes, including
  `sh:class` of a value, `sh:node`, property paths, `sh:qualifiedValueShape`,
  `sh:closed` and SHACL-SPARQL constraints, over the state the save would make. Results
  are mapped to fields by `sh:resultPath` and the focus node. Results on other focus nodes,
  such as another resource whose `sh:maxCount` the change breaks through an inverse path,
  are listed in the summary with links to those resources.
- A problem never blocks typing. **Save** is enabled with problems only when the
  dataset's guard is in `warn` mode or absent, and then asks for confirmation. With a
  `reject` guard, **Save** stays disabled until the server's check passes, because the
  write would fail anyway.

### 2.5 Preview and save

**Preview** shows the result of C15's dry run: the commit that would be made, the quads
removed and added as signed lines (the `DiffLines` component), the validation outcome
and the quota. **Save** sends the same change with the message. The receipt's commit
number is shown, and the form reloads from the new state.

### 2.6 Branches

The dataset's `forms` setting (§3.3) chooses where saves go.

- **`main`.** Saves commit to `main` when the caller may write it and it is not
  protected.
- **`branch`** (the default when `main` is protected). The first save of a session
  creates a branch `edits.{user}.{yyyymmdd}` from `main`'s head, or reuses it, and later
  saves commit there. A banner says "Editing on branch edits.ana: 3 commits" with
  **Review and merge**, which opens the existing merge page
  (`/datasets/{name}/merge?source=edits.ana.20261012`). The branch selector in the form's
  header lets the person pick another branch they can write.

Merging is the merge page's job: it shows the changes, the conflicts and the target's
validation. The form adds nothing there.

### 2.7 Concurrent changes

The form remembers the values it loaded for each field. A save sends them as the
expected values of the fields it changes. When another commit changed one of those
fields meanwhile, the save fails with `409 form-conflict` and the fields. The form then
shows, per field, the value it loaded, the value now stored and the person's value, and
the person picks one. Changes to fields the person did not touch never conflict.

### 2.8 Empty and error states

| Case | What the person sees |
|---|---|
| No shape targets the resource | **Edit** is absent. The panel says "No shape describes this resource" and, for an admin, offers **Draft shapes** (C02's dialog). |
| No shapes at all, `forms.shapes: drafted` | Forms built from drafted shapes carry a banner "Shapes drafted from the data, not enforced". |
| Several shapes target the resource | Their property shapes are merged into one form. Two shapes that constrain one path are both applied, and the field lists both sources under **View shapes**. |
| The caller can only read | Forms open in view mode with the viewers, and **Edit** is absent. |
| The server check times out | "The shapes could not be checked within 10 s", and Save follows the guard's mode. |

### 2.9 Accessibility

Every input has a `<label>`. Groups are `<fieldset>`s with `<legend>`s. Problems are
linked with `aria-describedby` and also listed in an error summary at the top of the
form, each a link that moves focus to its field, after a failed save. The entity picker
is a combobox in the WAI-ARIA pattern. Nothing is conveyed by colour alone.

## 3. Server and API changes

### 3.1 `GET /$/forms/{ds}`

Returns the form model for a resource: `focus` (an IRI), or `class` for a new instance,
plus `branch`, `at` (view mode only) and `shapes` (`guard`, `graphs`, `drafted`, or the
setting's default).

```ts
type FormModel = {
  formFormat: 1;
  focus: string | null; types: string[]; commit: number; branch: string;
  shapes: { source: "guard" | "graphs" | "drafted"; nodeShapes: string[] };
  groups: { iri: string | null; label: string; order: number | null }[];
  fields: {
    id: string;                 // stable for this model: shape and path
    path: string;               // a predicate IRI, or SPARQL path text for complex paths
    editable: boolean; reason?: "readOnly" | "protected" | "graph" | "complex-path";
    label: string; description?: string; group: string | null; order: number | null;
    widget: string; editor?: string; viewer?: string;
    constraints: Record<string, unknown>;   // the constraints the browser checks
    values: { term: Term; graph: string | null; label?: string }[];
  }[];
  other: { predicate: string; values: { term: Term; graph: string | null }[] }[];
  graph: string | null;         // where new values go
};
```

The server builds it because it already parses shapes in every syntax (Turtle, SHACLC
and the rest) and resolves targets, subclasses and paths in `sparkles-shacl`. The values
are read under the caller's view, so hidden graphs and protected triples never appear.
It needs `read`. A focus no shape targets answers `200` with no fields and
`shapes.nodeShapes: []`.

### 3.2 `POST /$/forms/{ds}/check` and `POST /$/forms/{ds}/save`

Both take `{focus, branch, changes: [{field, remove: Term[], add: Term[]}], expect:
{field: Term[]}, message?}`. The server turns the changes into one update of
`DELETE DATA` and `INSERT DATA` in the right graphs, so the client never writes SPARQL
text for a save.

- `check` runs it as a C15 dry run (`dryRun=true`) and validates the focus nodes the
  change touches against the form's shapes, whether or not the dataset has a guard. It
  answers C15's preview plus `results` mapped to field ids. It needs the write
  permission the save needs.
- `save` checks `expect` against the current values of the changed fields under the
  writer lock, answers `409 form-conflict` with `{fields: [{id, now: Term[]}]}` when one
  differs, and otherwise commits the update with the message. The answer is the commit
  receipt. The write-time guard runs as for any write.

### 3.3 The `forms` settings kind

A C19 settings kind per dataset: `{shapes: "guard" | "graphs" | "drafted", graphs?:
[iri], saveTo: "main" | "branch", branchTemplate: "edits.{user}.{date}", targetGraph:
"type" | "default" | iri, newIri: "urn:uuid" | template}`. `targetGraph: "type"` writes new
values into the graph that holds the focus node's `rdf:type` triple. `newIri` mints IRIs
for new instances, for example `http://example.org/person/{uuid}`.

All routes are added to `docs/API.md`, the route table, the OpenAPI description and the
MCP tool list is unchanged.

## 4. Design

### 4.1 Choosing shapes for a resource

A node shape applies to the focus when it targets it by `sh:targetNode`,
`sh:targetClass` (including subclasses, read from the data and the inferred graph when
reasoning is on), an implicit class target (a shape that is also an `rdfs:Class`), or
`sh:targetSubjectsOf` and `sh:targetObjectsOf`. SHACL-SPARQL targets are evaluated on the
server. Property shapes reached through `sh:node` from an applying shape contribute
their fields too, under a group named after the referenced shape.

### 4.2 Paths

A predicate path or an inverse path (`sh:inversePath p`) is editable. Sequence,
alternative and transitive paths are shown read-only with `reason: "complex-path"`,
because a change to them has no single triple to write.

### 4.3 The checks in the browser

`lib/forms.ts` implements the constraint checks of §2.4 as pure functions over
`FormModel.constraints`, with XSD lexical checks in a new
`lib/xsd.ts`. The stored-query parameter form checks nothing beyond required values
today, and can use the same module later. `sh:pattern` compiles with the `u` flag plus `sh:flags`. A pattern
that JavaScript's regular expressions cannot compile is left to the server and noted on
the field.

### 4.4 Saves and history

Each save is one commit of kind `update` with the person as author, so the History panel,
F06's history queries and U01's entity History strip show who changed which field.

## 5. Architecture

| Part | Where |
|---|---|
| Form model, check, save | `sparkles-server` `http` forms handlers, `sparkles-shacl` target and path resolution, the `sparkles` crate's `Dataset` handle (P06) for library callers |
| Settings kind | C19's settings registry in `sparkles-server/src/settings` |
| Browser checks, change set | `ui/src/lib/forms.ts` (new) |
| Form and widgets | `ui/src/lib/components/ShapeForm.svelte`, `FormField.svelte`, `EntityPicker.svelte` (new) |
| Explore wiring | `routes/explore/+page.svelte` properties panel |
| Edit route | `routes/datasets/[name]/edit/+page.svelte` (new) |
| Diff and validation output | the existing `DiffLines.svelte` and `GuardReportView.svelte` |
| Review | the existing merge page `routes/datasets/[name]/merge` |
| Mock server | `ui/mock/forms.mjs` (new) |

## 6. Phases

1. **Phase 1.** Form model, check and save routes; the `forms` settings kind; forms for
   predicate and inverse paths with literal and IRI values; browser checks; preview;
   branch saves; conflicts.
2. **Phase 2.** DASH editors and viewers, nested forms with `dash:DetailsEditor` for
   blank nodes, **New instance** with minted IRIs, the entity picker backed by full-text
   search where F03 is enabled.
3. **Phase 3.** SHACL 1.2 list constraints (`sh:memberShape` and the list lengths) with
   an ordered list editor, and `sh:or` and `sh:xone` as a choice of sub-forms.

## 7. Acceptance examples

Shapes for A1–A7 (installed as a `reject` guard on dataset `people`):

```turtle
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:name "Name" ; sh:minCount 1 ; sh:datatype xsd:string ; sh:order 1 ] ;
  sh:property [ sh:path ex:email ; sh:name "Email" ; sh:maxCount 1 ;
                sh:pattern "^[^@]+@[^@]+\\.[a-z]+$" ; sh:order 2 ] ;
  sh:property [ sh:path ex:worksFor ; sh:name "Works for" ; sh:class ex:Org ; sh:minCount 1 ] ;
  sh:property [ sh:path ex:role ; sh:in ( "Engineer" "Manager" "Other" ) ] .
```

- **A1. Form layout.** Explore on `ex:alice` (a Person with a name and an employer) shows
  **Edit**. The form lists Name, Email, Works for and Role in that order, Name and Works
  for marked "required", Role as three radio buttons.
- **A2. Browser check.** Typing `alice@example` in Email shows the pattern problem under
  the field at once, and the summary reads "1 problem". No request is sent for it.
- **A3. Server check.** Picking `ex:bob`, who is not an `ex:Org`, in Works for and
  pressing **Preview** shows the `sh:class` result under Works for, and the preview says
  the write would be rejected.
- **A4. Save to a branch.** With `main` protected, fixing the email and saving with
  message "fix email" creates branch `edits.{user}.{date}` with one commit whose author is
  the user, and `main` is unchanged. **Review and merge** opens the merge page with the
  branch as source.
- **A5. Conflict.** Two pages open the form. One saves a new email. The other then saves
  another email and gets the conflict view with both values. Saving a change to Role from
  the second page alone succeeds.
- **A6. Read-only by grant.** A caller whose grants protect `ex:email` (C12b) sees no
  Email field and can still save Name.
- **A7. Other properties.** A triple `ex:alice ex:nickname "Al"` appears under **Other
  properties**, read-only.
- **A8. DASH (Phase 2).** Adding `dash:editor dash:TextAreaEditor` to the Name property
  shape makes Name a text area.
- **A9. Accessibility.** With the keyboard alone, a person can open the form, fix A2's
  problem from the error summary's link and save. Every input has an accessible name
  and every problem is reachable from the summary.

## 8. Testing

- **Vitest.** `lib/forms.ts`: each browser check against the SHACL test suite's core
  cases that need no data, widget choice from constraints, change sets with multiple
  values and language tags.
- **Rust.** The form model for targets by class with subclasses, inverse and complex
  paths, merged shapes and C12b-hidden predicates; `check` against `/{ds}/shacl` on the
  same state; `save` conflicts under concurrent writers.
- **Playwright.** A1–A7 and A9 against a real server built by the test (the existing
  `write-validation.spec.ts` shows the pattern), and the mock server for the form's
  layout and error states.

## 9. Rejected alternatives

- **Generating SPARQL Update text in the browser.** It would duplicate graph placement,
  term escaping and conflict checks in TypeScript. The server builds the update from
  terms.
- **Parsing shapes in the browser.** The UI ships no RDF parser for Turtle and SHACLC, and
  `sparkles-shacl` already resolves targets and paths. The browser gets a resolved model.
- **Conditional saves on the dataset's entity tag (`If-Match`).** Any commit to the
  dataset would fail a save, even one to another resource. Field expectations conflict
  only on real overlaps.
- **A separate review page for form edits.** The merge page already shows a branch's
  changes and conflicts, and two review flows would diverge.
- **Using a form library that interprets shapes.** The forms must follow Sparkles' grants,
  branches and previews, which such libraries do not know, and the widgets are few.

## 10. Open questions

1. Whether ShEx schemas should drive forms too, through a translation of the common
   subset (cardinalities, datatypes, value sets) to the same `FormModel`.
2. Whether a session branch should expire when merged, like C18's scratch branches.
3. How to show a field whose values sit in several graphs when the person edits one, and
   whether to offer moving a value between graphs.
4. Whether `dash:readOnly` should also be enforced on the server for form saves, or stay
   a UI hint.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`U02-shape-driven-forms.md`](SPEC.md), written on 2026-10-12 independently from:
  - the W3C SHACL Recommendation (non-validating property shape characteristics,
    `sh:PropertyGroup`) and the SHACL 1.2 Core drafts for list constraints;
  - the DASH Data Shapes vocabulary published by TopQuadrant (editors, viewers,
    `dash:singleLine`, `dash:readOnly`) and the public documentation of TopBraid EDG's
    forms, without any TopBraid code;
  - Schímatos, a SHACL-based web-form generator (ISWC 2020), as a published approach;
  - the WAI-ARIA Authoring Practices' combobox pattern;
  - the Sparkles code and specs C02, C09, C10, C12, C12b, C15, C18, C19, F09 and G03.
- **Implementation:** not built. No new runtime dependency is planned. Shapes are
  resolved by the existing `sparkles-shacl` crate, and widgets are written in Svelte.
- **Rejected** (spec §9):
  - SPARQL Update text generated in the browser;
  - parsing shapes in the browser;
  - conditional saves on the dataset's entity tag;
  - a separate review page for form edits;
  - a third-party library that interprets shapes.

## Outcome

Forms support literal and IRI editing, previews, branch saves, conflict handling, DASH
hints, new instances and entity search. Checks use shape dependencies beyond a fixed
graph radius and refuse incomplete discovery. The [delivery record](OUTCOME.md)
describes the affected-target contract and the initial implementation history.

The feature remains partially complete. The Rust library also exposes pooled nested
read models with visible graph anchors, shared identities, cyclic references and cumulative preparation admission. Atomic
nested edits, HTTP and browser consumers, list and alternative editors, and early
admission for the remaining target-node expression producers remain
under [roadmap item 21](../../ROADMAP.md). Strict physical reader admission uses the
bounded-read preparation work of item 395; the discovery allowance is not a total
SHACL heap limit.
