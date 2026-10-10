// Terms in the compact syntax of the MCP tools and of `/check`, `/recall` and
// `/sparql/diagnose` results: `ex:ana`, `<urn:uuid:…>`, `"Ana Lima"@en`,
// `"5"^^xsd:integer`, `_:b0` and `<<( s p o )>>`. Each result carries the prefixes its
// prefixed names use.

import type { Term } from './api';

export type Prefixes = Record<string, string>;

const XSD = 'http://www.w3.org/2001/XMLSchema#';

/** Read one term at `i` of `s`; answers the term and the index after it. */
function readTerm(s: string, i: number, prefixes: Prefixes): [Term, number] | null {
  while (i < s.length && /\s/.test(s[i])) i++;
  if (i >= s.length) return null;
  if (s.startsWith('<<(', i)) {
    const parts: Term[] = [];
    let j = i + 3;
    for (let k = 0; k < 3; k++) {
      const r = readTerm(s, j, prefixes);
      if (!r) return null;
      parts.push(r[0]);
      j = r[1];
    }
    while (j < s.length && /\s/.test(s[j])) j++;
    if (!s.startsWith(')>>', j)) return null;
    return [
      { type: 'triple', value: { subject: parts[0], predicate: parts[1], object: parts[2] } },
      j + 3,
    ];
  }
  if (s[i] === '<') {
    const end = s.indexOf('>', i + 1);
    if (end < 0) return null;
    return [{ type: 'uri', value: s.slice(i + 1, end) }, end + 1];
  }
  if (s.startsWith('_:', i)) {
    const m = /^_:([^\s)]+)/.exec(s.slice(i));
    if (!m) return null;
    return [{ type: 'bnode', value: m[1] }, i + m[0].length];
  }
  if (s[i] === '"') {
    let j = i + 1;
    let value = '';
    while (j < s.length && s[j] !== '"') {
      if (s[j] === '\\' && j + 1 < s.length) {
        const c = s[j + 1];
        if (c === 'u' || c === 'U') {
          const n = c === 'u' ? 4 : 8;
          value += String.fromCodePoint(parseInt(s.slice(j + 2, j + 2 + n), 16));
          j += 2 + n;
          continue;
        }
        value += c === 'n' ? '\n' : c === 't' ? '\t' : c === 'r' ? '\r' : c;
        j += 2;
      } else {
        value += s[j];
        j++;
      }
    }
    if (j >= s.length) return null;
    j++;
    if (s[j] === '@') {
      const m = /^@([A-Za-z]+(?:-[A-Za-z0-9]+)*(?:--[a-z]+)?)/.exec(s.slice(j));
      if (m) return [{ type: 'literal', value, 'xml:lang': m[1] }, j + m[0].length];
    }
    if (s.startsWith('^^', j)) {
      const r = readTerm(s, j + 2, prefixes);
      if (r && r[0].type === 'uri') {
        const dt = r[0].value;
        return [
          dt === `${XSD}string`
            ? { type: 'literal', value }
            : { type: 'literal', value, datatype: dt },
          r[1],
        ];
      }
      return null;
    }
    return [{ type: 'literal', value }, j];
  }
  // a number or a boolean, as SPARQL abbreviates them
  const num = /^[+-]?(\d+\.?\d*([eE][+-]?\d+)?|\.\d+([eE][+-]?\d+)?)(?=[\s)]|$)/.exec(s.slice(i));
  if (num) {
    const t = num[0];
    const dt = /[eE]/.test(t) ? 'double' : t.includes('.') ? 'decimal' : 'integer';
    return [{ type: 'literal', value: t, datatype: XSD + dt }, i + t.length];
  }
  const bool = /^(true|false)(?=[\s)]|$)/.exec(s.slice(i));
  if (bool)
    return [{ type: 'literal', value: bool[1], datatype: `${XSD}boolean` }, i + bool[1].length];
  const pn = /^([A-Za-z][\w.-]*)?:([^\s)]*)/.exec(s.slice(i));
  if (pn) {
    const ns = prefixes[pn[1] ?? ''];
    const local = pn[2].replace(/\\([^\w])/g, '$1');
    // an unknown prefix stays visible as written rather than becoming a wrong IRI
    return [{ type: 'uri', value: ns != null ? ns + local : pn[0] }, i + pn[0].length];
  }
  return null;
}

/** The term a compact string names, or a plain literal of the text when it does not parse. */
export function parseCompact(s: string, prefixes: Prefixes = {}): Term {
  const r = readTerm(s, 0, prefixes);
  if (r && s.slice(r[1]).trim() === '') return r[0];
  return { type: 'literal', value: s };
}

/** The full IRI of a compact IRI (`ex:ana` or `<…>`), or null for anything else. */
export function compactIri(s: string, prefixes: Prefixes = {}): string | null {
  const t = parseCompact(s, prefixes);
  return t.type === 'uri' ? t.value : null;
}

/** The local name of an IRI: what follows its last `#`, `/` or `:`. */
export function localName(iri: string): string {
  const m = /[^#/:]*$/.exec(iri);
  return m ? m[0] : iri;
}
