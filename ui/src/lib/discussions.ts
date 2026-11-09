// Discussions on entities (ROADMAP item 618, docs/specs/U12-ontology-workbench/SPEC.md and
// docs/specs/C31-checks-and-protected-merges/SPEC.md): comment threads on an entity or one of
// its axioms, stored as W3C Web Annotation RDF in an ordinary named graph beside the ontology
// graph on the branch, `urn:x-sparkles:discussions:<graph>`. A thread is an `oa:Annotation`
// motivated by `oa:commenting` whose target is the entity, or an `oa:SpecificResource` with the entity as source and the
// axiom's functional-style syntax as an `oa:TextQuoteSelector`. A reply is an annotation
// motivated by `oa:replying` whose target is the thread. Each body is an `oa:TextualBody`.
// Every comment, resolve and reopen is one ordinary commit through `POST /{ds}/update`.
import { sparqlIri, sparqlString } from './rdf';

export const OA = 'http://www.w3.org/ns/oa#';
export const DCT = 'http://purl.org/dc/terms/';
export const RDF = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#';
const XSD = 'http://www.w3.org/2001/XMLSchema#';
const SCHEMA = 'https://schema.org/';
/** Sparkles' own terms for what Web Annotation leaves out: whether a thread is resolved. */
export const DISC = 'urn:x-sparkles:discussion#';
/** The prefix of every discussion graph's IRI. */
export const DISCUSSIONS = 'urn:x-sparkles:discussions:';
/** The discussion graph of the default graph. */
export const DEFAULT_DISCUSSIONS = DISCUSSIONS + 'default';
/** The IRI prefix of a principal named as a creator or a mention. */
export const USER = 'urn:x-sparkles:user:';

/** The most threads and replies one read lists. */
export const READ_LIMIT = 1000;
/** The longest comment, in characters. */
export const MAX_TEXT = 10_000;

/**
 * The graph that holds the discussions of the ontology in `graph` (null: the default graph):
 * the ontology graph's whole IRI under `urn:x-sparkles:discussions:`, with `%`, `[` and `]`
 * percent-encoded, since an IPv6 host's brackets may not stand in a URN. The encoding can be
 * undone, so the name is distinct for every ontology graph, `onto`, `onto/` and `onto#`
 * included, and it cannot be an ontology graph's own IRI.
 */
export function discussionGraph(graph: string | null): string {
  if (!graph) return DEFAULT_DISCUSSIONS;
  return (
    DISCUSSIONS +
    graph.replace(
      /[%[\]]/g,
      (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0'),
    )
  );
}

/** Whether a graph IRI names a discussion graph, which the editor never reads as an ontology. */
export function isDiscussionGraph(iri: string | null | undefined): boolean {
  return !!iri && iri.startsWith(DISCUSSIONS);
}

export const userIri = (name: string) => USER + encodeURIComponent(name);
/** The principal a creator IRI names, or the IRI itself when it is not one of ours. */
export function userName(iri: string): string {
  if (!iri.startsWith(USER)) return iri;
  try {
    return decodeURIComponent(iri.slice(USER.length));
  } catch {
    return iri;
  }
}

/**
 * A mention: `@` and a principal's name of word characters, with `.`, `:`, `@` and `-`
 * inside, such as `@a` or `@alice@example.org`. It is not preceded by a word character or
 * `@`, so an e-mail address in the text is not one.
 */
const MENTION = /(^|[^\w@])(@\w(?:[\w.:@-]*\w)?)/g;

/** The principals a text mentions as `@name`, each once, in order. */
export function mentionsOf(text: string): string[] {
  const out: string[] = [];
  for (const p of textParts(text))
    if (p.mention && !out.includes(p.text.slice(1))) out.push(p.text.slice(1));
  return out;
}

/** A thread or reply as a write builds it. */
export type NewComment = {
  /** The annotation's IRI, a fresh `urn:uuid:`. */
  id: string;
  text: string;
  user: string;
  /** An RFC 3339 time. */
  now: string;
};

function commentTriples(c: NewComment, motivation: 'commenting' | 'replying'): string[] {
  const a = sparqlIri(c.id);
  const body = sparqlIri(`${c.id}#body`);
  return [
    `${a} a <${OA}Annotation> ; <${OA}motivatedBy> <${OA}${motivation}> ; <${OA}hasBody> ${body} ;`,
    `  <${DCT}creator> ${sparqlIri(userIri(c.user))} ; <${DCT}created> ${sparqlString(c.now)}^^<${XSD}dateTime>` +
      mentionsOf(c.text)
        .map((m) => ` ;\n  <${SCHEMA}mentions> ${sparqlIri(userIri(m))}`)
        .join('') +
      ' .',
    `${body} a <${OA}TextualBody> ; <${RDF}value> ${sparqlString(c.text)} ; <${DCT}format> "text/plain" .`,
  ];
}

function checkText(text: string) {
  if (!text.trim()) throw new Error('A comment needs some text.');
  if (text.length > MAX_TEXT) throw new Error(`A comment holds at most ${MAX_TEXT} characters.`);
}

/** The update that opens a thread on `entity`, or on one of its axioms. */
export function newThreadUpdate(
  graph: string | null,
  c: NewComment & { entity: string; axiom?: string | null },
): string {
  checkText(c.text);
  const a = sparqlIri(c.id);
  const target = c.axiom
    ? [
        `${a} <${OA}hasTarget> ${sparqlIri(`${c.id}#target`)} .`,
        `${sparqlIri(`${c.id}#target`)} a <${OA}SpecificResource> ; <${OA}hasSource> ${sparqlIri(c.entity)} ;`,
        `  <${OA}hasSelector> ${sparqlIri(`${c.id}#selector`)} .`,
        `${sparqlIri(`${c.id}#selector`)} a <${OA}TextQuoteSelector> ; <${OA}exact> ${sparqlString(c.axiom)} .`,
      ]
    : [`${a} <${OA}hasTarget> ${sparqlIri(c.entity)} .`];
  return insertData(graph, [
    ...commentTriples(c, 'commenting'),
    ...target,
    `${a} <${DISC}resolved> false .`,
  ]);
}

/** The update that replies to `thread`. */
export function replyUpdate(graph: string | null, c: NewComment & { thread: string }): string {
  checkText(c.text);
  return insertData(graph, [
    ...commentTriples(c, 'replying'),
    `${sparqlIri(c.id)} <${OA}hasTarget> ${sparqlIri(c.thread)} .`,
  ]);
}

/** The update that resolves or reopens `thread`, recording who did it and when. */
export function resolveUpdate(
  graph: string | null,
  thread: string,
  resolved: boolean,
  user: string,
  now: string,
): string {
  const g = sparqlIri(discussionGraph(graph));
  const t = sparqlIri(thread);
  const set = [
    `${t} <${DISC}resolved> ${resolved} .`,
    `${t} <${DCT}modified> ${sparqlString(now)}^^<${XSD}dateTime> .`,
    ...(resolved ? [`${t} <${DISC}resolvedBy> ${sparqlIri(userIri(user))} .`] : []),
  ];
  return (
    `DELETE { GRAPH ${g} { ${t} <${DISC}resolved> ?r . ${t} <${DCT}modified> ?m . ` +
    `${t} <${DISC}resolvedBy> ?b } }\n` +
    `INSERT { GRAPH ${g} {\n${set.join('\n')}\n} }\n` +
    `WHERE { GRAPH ${g} { ${t} a <${OA}Annotation> . ` +
    `OPTIONAL { ${t} <${DISC}resolved> ?r } OPTIONAL { ${t} <${DCT}modified> ?m } ` +
    `OPTIONAL { ${t} <${DISC}resolvedBy> ?b } } }`
  );
}

function insertData(graph: string | null, triples: string[]): string {
  return `INSERT DATA { GRAPH ${sparqlIri(discussionGraph(graph))} {\n${triples.join('\n')}\n} }`;
}

/**
 * The SELECT of the threads and replies in the discussion graph of `graph`, those about
 * `entity` when it is given (threads whose target or target's source is the entity, and
 * their replies), oldest first and at most `READ_LIMIT` rows.
 */
export function threadsQuery(graph: string | null, entity: string | null): string {
  const g = sparqlIri(discussionGraph(graph));
  const about = (v: string) =>
    entity
      ? `FILTER(${v} = ${sparqlIri(entity)} || EXISTS { ${v} <${OA}hasSource> ${sparqlIri(entity)} })`
      : '';
  const common = `?a a <${OA}Annotation> ; <${OA}hasTarget> ?target ; <${OA}hasBody> ?b ;
         <${DCT}creator> ?creator ; <${DCT}created> ?created .
      ?b <${RDF}value> ?text .`;
  return `SELECT ?a ?motivation ?target ?source ?exact ?text ?creator ?created ?resolved ?resolvedBy WHERE {
  GRAPH ${g} {
    {
      ?a <${OA}motivatedBy> <${OA}commenting> .
      ${common}
      BIND(<${OA}commenting> AS ?motivation)
      OPTIONAL { ?target <${OA}hasSource> ?source }
      OPTIONAL { ?target <${OA}hasSelector> ?sel . ?sel <${OA}exact> ?exact }
      OPTIONAL { ?a <${DISC}resolved> ?resolved }
      OPTIONAL { ?a <${DISC}resolvedBy> ?resolvedBy }
      ${about('?target')}
    } UNION {
      ?a <${OA}motivatedBy> <${OA}replying> .
      ${common}
      BIND(<${OA}replying> AS ?motivation)
      ?target <${OA}motivatedBy> <${OA}commenting> ; <${OA}hasTarget> ?about .
      ${about('?about')}
    }
  }
} ORDER BY ?created ?a LIMIT ${READ_LIMIT}`;
}

export type Reply = { id: string; text: string; creator: string; created: string };
export type Thread = Reply & {
  entity: string;
  /** The axiom the thread is about, in functional-style syntax. */
  axiom: string | null;
  resolved: boolean;
  resolvedBy: string | null;
  replies: Reply[];
};

type Cell = { value: string; type?: string } | undefined;
type Row = Record<string, Cell>;

/** The threads of `threadsQuery`'s rows, oldest first, each with its replies oldest first. */
export function parseThreads(rows: readonly Row[]): { threads: Thread[]; truncated: boolean } {
  const threads = new Map<string, Thread>();
  const replies: (Reply & { thread: string })[] = [];
  for (const r of rows) {
    const id = r.a?.value;
    if (!id || !r.text || !r.creator || !r.created) continue;
    const base = {
      id,
      text: r.text.value,
      creator: userName(r.creator.value),
      created: r.created.value,
    };
    if (r.motivation?.value === OA + 'replying') {
      if (r.target) replies.push({ ...base, thread: r.target.value });
      continue;
    }
    if (threads.has(id)) continue;
    threads.set(id, {
      ...base,
      entity: r.source?.value ?? r.target?.value ?? '',
      axiom: r.exact?.value ?? null,
      resolved: r.resolved?.value === 'true' || r.resolved?.value === '1',
      resolvedBy: r.resolvedBy ? userName(r.resolvedBy.value) : null,
      replies: [],
    });
  }
  for (const r of replies) {
    const t = threads.get(r.thread);
    if (t && !t.replies.some((x) => x.id === r.id))
      t.replies.push({ id: r.id, text: r.text, creator: r.creator, created: r.created });
  }
  return { threads: [...threads.values()], truncated: rows.length >= READ_LIMIT };
}

/** The text split into plain runs and `@name` mentions, for display. */
export function textParts(text: string): { text: string; mention: boolean }[] {
  const out: { text: string; mention: boolean }[] = [];
  let last = 0;
  for (const m of text.matchAll(MENTION)) {
    const at = (m.index ?? 0) + m[1].length;
    if (at > last) out.push({ text: text.slice(last, at), mention: false });
    out.push({ text: m[2], mention: true });
    last = at + m[2].length;
  }
  if (last < text.length) out.push({ text: text.slice(last), mention: false });
  return out;
}

/** A fresh annotation IRI. */
export const newId = () => `urn:uuid:${crypto.randomUUID()}`;
