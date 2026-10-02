// SPARQL 1.1 support for CodeMirror 6: a StreamLanguage tokenizer, highlight
// style driven by CSS variables, autocompletion (keywords, functions, prefixes,
// prefixed names from the dataset vocabulary, variables in the document) and
// an error-line decoration used to point at parse errors.

import {
  autocompletion,
  startCompletion,
  type Completion,
  type CompletionContext,
  type CompletionResult,
} from '@codemirror/autocomplete';
import {
  HighlightStyle,
  StreamLanguage,
  syntaxHighlighting,
  type StringStream,
} from '@codemirror/language';
import { EditorState, StateEffect, StateField, type Extension } from '@codemirror/state';
import { Decoration, EditorView, type DecorationSet } from '@codemirror/view';
import { tags as t } from '@lezer/highlight';
import { declaredPrefixes, shorten, type PrefixMap } from './rdf';

export const KEYWORDS = [
  'BASE',
  'PREFIX',
  'SELECT',
  'DISTINCT',
  'REDUCED',
  'AS',
  'CONSTRUCT',
  'DESCRIBE',
  'ASK',
  'FROM',
  'NAMED',
  'WHERE',
  'ORDER',
  'BY',
  'ASC',
  'DESC',
  'LIMIT',
  'OFFSET',
  'VALUES',
  'OPTIONAL',
  'GRAPH',
  'SERVICE',
  'SILENT',
  'BIND',
  'UNDEF',
  'MINUS',
  'UNION',
  'FILTER',
  'GROUP',
  'HAVING',
  'EXISTS',
  'NOT',
  'IN',
  'LOAD',
  'CLEAR',
  'DROP',
  'CREATE',
  'ADD',
  'MOVE',
  'COPY',
  'INSERT',
  'DELETE',
  'DATA',
  'WITH',
  'USING',
  'DEFAULT',
  'ALL',
  'INTO',
  'TO',
  'AGG',
];

export const FUNCTIONS = [
  'STR',
  'LANG',
  'LANGMATCHES',
  'DATATYPE',
  'BOUND',
  'IRI',
  'URI',
  'BNODE',
  'RAND',
  'ABS',
  'CEIL',
  'FLOOR',
  'ROUND',
  'CONCAT',
  'STRLEN',
  'UCASE',
  'LCASE',
  'ENCODE_FOR_URI',
  'CONTAINS',
  'STRSTARTS',
  'STRENDS',
  'STRBEFORE',
  'STRAFTER',
  'YEAR',
  'MONTH',
  'DAY',
  'HOURS',
  'MINUTES',
  'SECONDS',
  'TIMEZONE',
  'TZ',
  'NOW',
  'UUID',
  'STRUUID',
  'MD5',
  'SHA1',
  'SHA256',
  'SHA384',
  'SHA512',
  'COALESCE',
  'IF',
  'STRLANG',
  'STRDT',
  'SAMETERM',
  'ISIRI',
  'ISURI',
  'ISBLANK',
  'ISLITERAL',
  'ISNUMERIC',
  'REGEX',
  'SUBSTR',
  'REPLACE',
  'COUNT',
  'SUM',
  'MIN',
  'MAX',
  'AVG',
  'SAMPLE',
  'GROUP_CONCAT',
  'SEPARATOR',
  // Jena ARQ's aggregates
  'MEDIAN',
  'MODE',
  'STDEV',
  'STDEV_SAMP',
  'STDEV_POP',
  'VARIANCE',
  'VAR_SAMP',
  'VAR_POP',
  'TRIPLE',
  'SUBJECT',
  'PREDICATE',
  'OBJECT',
  'ISTRIPLE',
];

const KW = new Set(KEYWORDS);
const FN = new Set(FUNCTIONS);

/**
 * GeoSPARQL and Jena spatial terms, offered after their prefix whatever the dataset
 * holds: namespace → [local name, whether it is a function (inserted with `(`)].
 */
const RELATIONS = ['Equals', 'Disjoint', 'Intersects', 'Touches', 'Within', 'Contains']
  .concat(['Overlaps', 'Crosses'])
  .map((r) => `sf${r}`)
  .concat(
    ['Equals', 'Disjoint', 'Meet', 'Overlap', 'Covers', 'CoveredBy', 'Inside', 'Contains'].map(
      (r) => `eh${r}`,
    ),
  )
  .concat(['eq', 'dc', 'ec', 'po', 'tppi', 'tpp', 'ntpp', 'ntppi'].map((r) => `rcc8${r}`));
export const SPATIAL_TERMS: Record<string, [string, boolean][]> = {
  'http://www.opengis.net/def/function/geosparql/': [
    ...RELATIONS,
    ...'relate distance metricDistance buffer metricBuffer convexHull concaveHull envelope boundary boundingCircle centroid intersection union difference symDifference area metricArea length metricLength perimeter metricPerimeter getSRID transform asWKT asGeoJSON dimension coordinateDimension spatialDimension numGeometries geometryN geometryType is3D isMeasured isEmpty isSimple minX minY minZ maxX maxY maxZ aggBoundingBox aggBoundingCircle aggCentroid aggConcaveHull aggConvexHull aggUnion'.split(
      ' ',
    ),
  ].map((f) => [f, true]),
  'http://jena.apache.org/function/spatial#':
    'convertLatLon convertLatLonBox equals nearby withinCircle distance greatCircle greatCircleGeom angle angleDeg azimuth azimuthDeg transform transformDatatype transformSRS'
      .split(' ')
      .map((f) => [f, true]),
  'http://jena.apache.org/spatial#':
    'nearby withinCircle nearbyGeom withinCircleGeom withinBox intersectBox withinBoxGeom intersectBoxGeom north south east west northGeom southGeom eastGeom westGeom equals'
      .split(' ')
      .map((f) => [f, false]),
  'http://www.opengis.net/ont/geosparql#': [
    ...'Feature Geometry SpatialObject hasGeometry hasDefaultGeometry asWKT asGeoJSON hasSerialization wktLiteral geoJSONLiteral'.split(
      ' ',
    ),
    ...RELATIONS,
  ].map((t) => [t, false]),
  'http://www.opengis.net/def/uom/OGC/1.0/': 'metre kilometre mile nauticalMile foot degree radian'
    .split(' ')
    .map((u) => [u, false]),
  'http://www.opengis.net/ont/sf#':
    'Point LineString Polygon MultiPoint MultiLineString MultiPolygon GeometryCollection'
      .split(' ')
      .map((t) => [t, false]),
};

type St = { inLongString: string | null };

const PN = /^(?:[A-Za-zÀ-￿][\w.\-À-￿]*)?:(?:[\wÀ-￿%\\-](?:[\w.\-À-￿%:\\]*[\w\-À-￿%:])?)?/;

function tokenBase(stream: StringStream, state: St): string | null {
  if (state.inLongString) {
    const q = state.inLongString;
    while (!stream.eol()) {
      if (stream.match(q)) {
        state.inLongString = null;
        return 'string';
      }
      if (stream.next() === '\\') stream.next();
    }
    return 'string';
  }
  if (stream.eatSpace()) return null;
  const ch = stream.peek()!;
  if (ch === '#') {
    stream.skipToEnd();
    return 'comment';
  }
  if (stream.match('"""') || stream.match("'''")) {
    state.inLongString = stream.current();
    return tokenBase(stream, state) ?? 'string';
  }
  if (ch === '"' || ch === "'") {
    stream.next();
    let esc = false;
    let c: string | void;
    while ((c = stream.next()) != null) {
      if (c === ch && !esc) break;
      esc = !esc && c === '\\';
    }
    return 'string';
  }
  if (stream.match(/^<[^<>"{}|^`\\\s]*>/)) return 'url';
  if (stream.match(/^[?$][\wÀ-￿]+/)) return 'variableName';
  if (stream.match(/^@[a-zA-Z]+(-[a-zA-Z0-9]+)*/)) return 'annotation';
  if (stream.match('^^')) return 'operator';
  if (stream.match(/^_:[\w.-]*/)) return 'labelName';
  if (stream.match(/^[+-]?(\d+\.?\d*([eE][+-]?\d+)?|\.\d+([eE][+-]?\d+)?)/)) return 'number';
  if (stream.match(PN)) return 'namespace';
  if (stream.match(/^[A-Za-z_][\w]*/)) {
    const w = stream.current().toUpperCase();
    if (KW.has(w)) return 'keyword';
    if (FN.has(w)) return 'function';
    if (w === 'A') return 'keyword';
    if (w === 'TRUE' || w === 'FALSE') return 'bool';
    return null;
  }
  if (stream.match(/^(\|\||&&|!=|<=|>=|[=<>!+*/|^])/)) return 'operator';
  if (stream.match(/^[{}()[\]]/)) return 'bracket';
  stream.next();
  return 'punctuation';
}

export const sparqlLanguage = StreamLanguage.define<St>({
  name: 'sparql',
  startState: () => ({ inLongString: null }),
  token: tokenBase,
  languageData: {
    commentTokens: { line: '#' },
    closeBrackets: { brackets: ['(', '[', '{', '"', "'", '<'] },
  },
  tokenTable: {
    function: t.function(t.variableName),
    namespace: t.namespace,
    labelName: t.labelName,
    annotation: t.annotation,
  },
});

export const sparqlHighlight = HighlightStyle.define([
  { tag: t.keyword, color: 'var(--kw)', fontWeight: '600' },
  { tag: t.function(t.variableName), color: 'var(--kw)' },
  { tag: t.variableName, color: 'var(--var)' },
  { tag: t.url, color: 'var(--iri)' },
  { tag: t.namespace, color: 'var(--iri)' },
  { tag: t.string, color: 'var(--literal)' },
  { tag: t.number, color: 'var(--num)' },
  { tag: t.bool, color: 'var(--num)' },
  { tag: t.annotation, color: 'var(--text-3)' },
  { tag: t.labelName, color: 'var(--bnode)' },
  { tag: t.comment, color: 'var(--comment)', fontStyle: 'italic' },
  { tag: t.operator, color: 'var(--text-2)' },
  { tag: t.bracket, color: 'var(--text-2)' },
  { tag: t.punctuation, color: 'var(--text-2)' },
]);

// --- completion -------------------------------------------------------------

export type CompletionData = {
  /** Known prefix → namespace map (dataset + well-known). */
  prefixes: () => PrefixMap;
  /** Full IRIs of predicates/classes present in the dataset. */
  vocab: () => string[];
};

/** Insert `PREFIX p: <ns>` at the top if the prefix is not declared yet. */
function ensurePrefix(view: EditorView, prefix: string, ns: string) {
  const doc = view.state.doc.toString();
  if (declaredPrefixes(doc).has(prefix)) return;
  // After the last existing PREFIX line, or at the very top.
  let pos = 0;
  const re = /^\s*(PREFIX|BASE)\b[^\n]*\n?/gim;
  let m: RegExpExecArray | null;
  while ((m = re.exec(doc)) && m.index === pos) pos = m.index + m[0].length;
  const line = `PREFIX ${prefix}: <${ns}>\n`;
  view.dispatch({ changes: { from: pos, insert: line } });
}

function sparqlCompletions(data: CompletionData) {
  return (ctx: CompletionContext): CompletionResult | null => {
    // Variables
    const v = ctx.matchBefore(/[?$][\wÀ-￿]*/);
    if (v) {
      const seen = new Set<string>();
      for (const m of ctx.state.doc.toString().matchAll(/[?$]([\wÀ-￿]+)/g)) seen.add(m[1]);
      const cur = v.text.slice(1);
      seen.delete(cur);
      return {
        from: v.from + 1,
        options: [...seen].map((name) => ({ label: name, type: 'variable', detail: 'variable' })),
        validFor: /^[\wÀ-￿]*$/,
      };
    }

    // Prefixed names: "foaf:na" → offer local names from the vocabulary.
    const pn = ctx.matchBefore(/(?:[A-Za-z][\w.-]*)?:[\wÀ-￿.-]*/);
    const prefixes = data.prefixes();
    if (pn && !/^\w+:\/\//.test(pn.text)) {
      const [pfx] = pn.text.split(':');
      const ns = prefixes[pfx];
      if (ns != null) {
        const locals = new Set<string>();
        for (const iri of data.vocab()) if (iri.startsWith(ns)) locals.add(iri.slice(ns.length));
        const options: Completion[] = [...locals].map((local) => ({
          label: `${pfx}:${local}`,
          type: 'property',
          detail: 'in dataset',
          apply: (view, _c, from, to) => {
            view.dispatch({ changes: { from, to, insert: `${pfx}:${local}` } });
            ensurePrefix(view, pfx, ns);
          },
        }));
        for (const [local, fn] of SPATIAL_TERMS[ns] ?? []) {
          if (locals.has(local)) continue;
          options.push({
            label: `${pfx}:${local}`,
            type: fn ? 'function' : 'property',
            detail: fn ? '()' : 'GeoSPARQL',
            apply: (view, _c, from, to) => {
              view.dispatch({ changes: { from, to, insert: `${pfx}:${local}${fn ? '(' : ''}` } });
              ensurePrefix(view, pfx, ns);
            },
          });
        }
        return { from: pn.from, options, validFor: /^(?:[A-Za-z][\w.-]*)?:[\wÀ-￿.-]*$/ };
      }
    }

    const word = ctx.matchBefore(/[A-Za-z_][\w-]*/);
    if (!word && !ctx.explicit) return null;
    const from = word ? word.from : ctx.pos;
    const line = ctx.state.doc.lineAt(ctx.pos);
    const before = line.text.slice(0, from - line.from);
    // After "PREFIX " suggest full declarations.
    if (/\bPREFIX\s+$/i.test(before)) {
      return {
        from,
        options: Object.entries(prefixes).map(([p, ns]) => ({
          label: `${p}: <${ns}>`,
          type: 'namespace',
        })),
      };
    }
    const options: Completion[] = [
      ...KEYWORDS.map((k) => ({ label: k, type: 'keyword', boost: 1 })),
      ...FUNCTIONS.map((f) => ({ label: f, type: 'function', apply: `${f}(`, detail: '()' })),
      ...Object.entries(prefixes).map(([p, ns]) => ({
        label: `${p}:`,
        type: 'namespace',
        detail: ns,
        boost: 2,
        apply: (view: EditorView, _c: Completion, f: number, t: number) => {
          view.dispatch({
            changes: { from: f, to: t, insert: `${p}:` },
            selection: { anchor: f + p.length + 1 },
          });
          ensurePrefix(view, p, ns);
          // Re-open completion for the local part.
          queueMicrotask(() => startCompletion(view));
        },
      })),
      // Shortened vocabulary terms can be typed directly without the prefix.
      ...data
        .vocab()
        .map((iri) => [iri, shorten(iri, prefixes)] as const)
        .filter((x): x is readonly [string, string] => !!x[1])
        .map(([iri, short]) => {
          const [p] = short.split(':');
          return {
            label: short,
            type: 'property',
            detail: 'in dataset',
            boost: -1,
            apply: (view: EditorView, _c: Completion, f: number, t: number) => {
              view.dispatch({ changes: { from: f, to: t, insert: short } });
              ensurePrefix(view, p, prefixes[p] ?? iri);
            },
          } satisfies Completion;
        }),
    ];
    return { from, options, validFor: /^[\w:-]*$/ };
  };
}

export function sparqlCompletion(data: CompletionData): Extension {
  return autocompletion({
    override: [sparqlCompletions(data)],
    activateOnTyping: true,
    maxRenderedOptions: 80,
  });
}

// --- error line -----------------------------------------------------------------

export const setErrorLocation = StateEffect.define<{ line: number; column?: number } | null>();

const errorLine = Decoration.line({ class: 'cm-error-line' });
const errorMark = Decoration.mark({ class: 'cm-error-mark' });

export const errorField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    // Any edit clears the error highlight.
    if (tr.docChanged) deco = Decoration.none;
    for (const e of tr.effects) {
      if (!e.is(setErrorLocation)) continue;
      if (!e.value) {
        deco = Decoration.none;
        continue;
      }
      const doc = tr.state.doc;
      const ln = Math.min(Math.max(1, e.value.line), doc.lines);
      const line = doc.line(ln);
      const ranges = [errorLine.range(line.from)];
      if (e.value.column != null && line.length > 0) {
        const from = line.from + Math.min(Math.max(0, e.value.column - 1), line.length - 1);
        ranges.push(errorMark.range(from, Math.min(line.to, from + 1)));
      }
      deco = Decoration.set(ranges, true);
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

export function highlightError(view: EditorView, line: number | undefined, column?: number) {
  if (!line) {
    view.dispatch({ effects: setErrorLocation.of(null) });
    return;
  }
  const ln = Math.min(Math.max(1, line), view.state.doc.lines);
  const pos = view.state.doc.line(ln).from;
  view.dispatch({
    effects: [
      setErrorLocation.of({ line, column }),
      EditorView.scrollIntoView(pos, { y: 'center' }),
    ],
  });
}

export function sparql(data: CompletionData): Extension {
  return [
    sparqlLanguage,
    syntaxHighlighting(sparqlHighlight),
    sparqlCompletion(data),
    errorField,
    EditorState.tabSize.of(2),
  ];
}
