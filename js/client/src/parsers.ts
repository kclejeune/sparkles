import type * as RDF from '@rdfjs/types';
import { Bindings, BudgetExceededError, RdfSyntaxError, factory } from '@sparkles-rdf/common';
import type { BodyReader } from './transport.js';

const LIMIT = 64 << 20;
const pnBase =
  'A-Za-z\\u00C0-\\u00D6\\u00D8-\\u00F6\\u00F8-\\u02FF\\u0370-\\u037D\\u037F-\\u1FFF\\u200C-\\u200D\\u2070-\\u218F\\u2C00-\\u2FEF\\u3001-\\uD7FF\\uF900-\\uFDCF\\uFDF0-\\uFFFD\\u{10000}-\\u{EFFFF}';
const blankStart = new RegExp(
  `^[${pnBase}_0-9][${pnBase}_0-9\\-\\u00b7\\u0300-\\u036f\\u203f\\u2040.]*`,
  'u',
);
const blankLabel = new RegExp(
  `^[${pnBase}_0-9](?:[${pnBase}_0-9\\-\\u00b7\\u0300-\\u036f\\u203f\\u2040.]*[${pnBase}_0-9\\-\\u00b7\\u0300-\\u036f\\u203f\\u2040])?$`,
  'u',
);
function malformed(message: string): never {
  throw new RdfSyntaxError(message);
}
function scalar(text: string) {
  for (const c of text) {
    const n = c.codePointAt(0)!;
    if (n >= 0xd800 && n <= 0xdfff) malformed('Invalid Unicode scalar');
  }
  return text;
}
function iri(value: unknown): string {
  if (
    typeof value !== 'string' ||
    !/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value) ||
    /[\u0000-\u0020<>"{}|\\^`]/u.test(value) ||
    /%(?![\da-f]{2})/i.test(value)
  )
    malformed('Invalid absolute IRI');
  return scalar(value);
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    malformed('Expected a SPARQL JSON object');
  return value as Record<string, unknown>;
}
function triple(s: RDF.Term, p: RDF.Term, o: RDF.Term, f: RDF.DataFactory): RDF.Quad {
  if (
    !['NamedNode', 'BlankNode'].includes(s.termType) ||
    p.termType !== 'NamedNode' ||
    !['NamedNode', 'BlankNode', 'Literal', 'Quad'].includes(o.termType)
  )
    malformed('Invalid triple-term positions');
  return f.quad(s as RDF.Quad_Subject, p as RDF.Quad_Predicate, o as RDF.Quad_Object);
}
function language(tag: string, direction?: string) {
  if (
    !/^[a-z]+(?:-[a-z\d]+)*$/i.test(tag) ||
    (direction !== undefined && direction !== 'ltr' && direction !== 'rtl')
  )
    malformed('Invalid language or direction');
}
export function queryTerm(value: unknown, f: RDF.DataFactory, depth = 0): RDF.Term {
  if (depth > 64) malformed('Triple-term nesting exceeds 64');
  const v = object(value);
  try {
    if (v.type === 'uri') return f.namedNode(iri(v.value));
    if (v.type === 'bnode' && typeof v.value === 'string') {
      if (!blankLabel.test(v.value)) malformed('Invalid blank node label');
      return f.blankNode(v.value);
    }
    if (v.type === 'triple') {
      const q = object(v.value);
      return triple(
        queryTerm(q.subject, f, depth + 1),
        queryTerm(q.predicate, f, depth + 1),
        queryTerm(q.object, f, depth + 1),
        f,
      );
    }
    if (v.type === 'literal' || v.type === 'typed-literal') {
      if (typeof v.value !== 'string') malformed('Literal value must be a string');
      scalar(v.value);
      if (v['xml:lang'] !== undefined) {
        if (typeof v['xml:lang'] !== 'string' || v.datatype !== undefined)
          malformed('Invalid language literal');
        const dir = v['its:dir'];
        if (dir !== undefined && typeof dir !== 'string') malformed('Invalid direction');
        language(v['xml:lang'], dir as string | undefined);
        return f.literal(
          v.value,
          dir ? { language: v['xml:lang'], direction: dir as 'ltr' | 'rtl' } : v['xml:lang'],
        );
      }
      if (v['its:dir'] !== undefined) malformed('Direction requires a language');
      return f.literal(
        v.value,
        f.namedNode(iri(v.datatype ?? 'http://www.w3.org/2001/XMLSchema#string')),
      );
    }
    return malformed('Unknown SPARQL result term');
  } catch (e) {
    if (e instanceof RdfSyntaxError) throw e;
    return malformed(`Invalid SPARQL result term: ${e instanceof Error ? e.message : String(e)}`);
  }
}

// Keep one token/row at a time; tokens may span UTF-8 chunks.
async function* tokens(reader: Pick<BodyReader, 'read'>): AsyncGenerator<string> {
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let buffer = '';
  let eof = false;
  async function more() {
    const r = await reader.read();
    eof = r.done;
    try {
      buffer += decoder.decode(r.value, { stream: !eof });
    } catch {
      malformed('Invalid UTF-8 in SPARQL JSON');
    }
  }
  while (true) {
    buffer = buffer.replace(/^[ \t\r\n]+/, '');
    if (!buffer.length) {
      if (eof) return;
      await more();
      continue;
    }
    if ('{}[]:,'.includes(buffer[0])) {
      yield buffer[0];
      buffer = buffer.slice(1);
      continue;
    }
    let end = -1;
    if (buffer[0] === '"') {
      let escaped = false;
      for (let i = 1; i < buffer.length; i++) {
        if (!escaped && buffer[i] === '"') {
          end = i + 1;
          break;
        }
        if (!escaped && buffer[i] === '\\') escaped = true;
        else escaped = false;
      }
    } else {
      const m = /^[^ \t\r\n{}\[\]:,]+(?=[ \t\r\n{}\[\]:,])/.exec(buffer);
      if (m) end = m[0].length;
      else if (eof) end = buffer.length;
    }
    if (end < 0) {
      if (eof) malformed('Incomplete SPARQL JSON');
      if (buffer.length > LIMIT) throw new BudgetExceededError('SPARQL JSON token exceeds 64 MiB');
      await more();
      continue;
    }
    if (end > LIMIT) throw new BudgetExceededError('SPARQL JSON token exceeds 64 MiB');
    const token = buffer.slice(0, end);
    buffer = buffer.slice(end);
    yield token;
  }
}
class JsonTokens {
  position = 0;
  constructor(private stream: AsyncIterator<string>) {}
  async take() {
    const r = await this.stream.next();
    if (r.done) return undefined;
    this.position += r.value.length;
    return r.value;
  }
  async next() {
    return (await this.take()) ?? malformed('Unexpected end of SPARQL JSON');
  }
  async expect(value: string) {
    if ((await this.next()) !== value) malformed(`Expected ${value} in SPARQL JSON`);
  }
  key(token: string): string {
    const key = this.primitive(token);
    if (typeof key !== 'string') malformed('JSON object keys must be strings');
    return key;
  }
  primitive(token: string): unknown {
    try {
      return JSON.parse(token);
    } catch {
      return malformed('Invalid SPARQL JSON token');
    }
  }
  async value(first?: string, depth = 0, start = this.position): Promise<unknown> {
    if (depth > 64) malformed('SPARQL JSON nesting exceeds 64');
    if (this.position - start > LIMIT)
      throw new BudgetExceededError('SPARQL JSON object exceeds 64 MiB');
    const token = first ?? (await this.next());
    if (token !== '{' && token !== '[') return this.primitive(token);
    const result: Record<string, unknown> | unknown[] = token === '{' ? Object.create(null) : [];
    const end = token === '{' ? '}' : ']';
    let t = await this.next();
    if (t === end) return result;
    while (true) {
      if (Array.isArray(result)) result.push(await this.value(t, depth + 1, start));
      else {
        const key = this.key(t);
        if (Object.hasOwn(result, key)) malformed('Duplicate JSON object member');
        await this.expect(':');
        result[key] = await this.value(undefined, depth + 1, start);
      }
      if (this.position - start > LIMIT)
        throw new BudgetExceededError('SPARQL JSON object exceeds 64 MiB');
      t = await this.next();
      if (t === end) return result;
      if (t !== ',') malformed('Invalid SPARQL JSON delimiter');
      t = await this.next();
    }
  }
}
export async function* jsonRows(
  reader: Pick<BodyReader, 'read'>,
  variables: RDF.Variable[],
  f: RDF.DataFactory,
): AsyncGenerator<Bindings, boolean | undefined> {
  const input = new JsonTokens(tokens(reader));
  await input.expect('{');
  let key = await input.next();
  let kind: 'boolean' | 'bindings' | undefined;
  let answer: boolean | undefined;
  let seenHead = false;
  const seen = new Set<string>();
  while (key !== '}') {
    const name = input.key(key);
    if (seen.has(name)) malformed('Duplicate SPARQL JSON member');
    seen.add(name);
    await input.expect(':');
    if (name === 'head') {
      const head = object(await input.value());
      seenHead = true;
      if (head.vars !== undefined) {
        if (
          !Array.isArray(head.vars) ||
          head.vars.some((v) => typeof v !== 'string' || !/^[\p{L}_\d][\p{L}\p{N}_]*$/u.test(v)) ||
          new Set(head.vars).size !== head.vars.length
        )
          malformed('Invalid SPARQL variable list');
        variables.splice(
          0,
          variables.length,
          ...head.vars.map((v) => (f.variable ?? factory.variable!).call(f, v as string)),
        );
      }
    } else if (name === 'results') {
      if (kind) malformed('Mixed ASK and SELECT results');
      kind = 'bindings';
      await input.expect('{');
      let resultKey = await input.next();
      let bindings = false;
      while (resultKey !== '}') {
        const member = input.key(resultKey);
        await input.expect(':');
        if (member === 'bindings') {
          if (bindings) malformed('Duplicate bindings array');
          bindings = true;
          await input.expect('[');
          let row = await input.next();
          while (row !== ']') {
            const value = object(await input.value(row));
            if (
              seenHead &&
              Object.keys(value).some((name) => !variables.some((v) => v.value === name))
            )
              malformed('Binding is absent from the variable list');
            if (!seenHead)
              for (const name of Object.keys(value))
                if (!variables.some((v) => v.value === name))
                  variables.push((f.variable ?? factory.variable!).call(f, name));
            yield new Bindings(Object.entries(value).map(([k, v]) => [k, queryTerm(v, f)]));
            row = await input.next();
            if (row === ']') break;
            if (row !== ',') malformed('Invalid bindings array');
            row = await input.next();
            if (row === ']') malformed('Trailing bindings comma');
          }
        } else await input.value();
        resultKey = await input.next();
        if (resultKey === '}') break;
        if (resultKey !== ',') malformed('Invalid results delimiter');
        resultKey = await input.next();
        if (resultKey === '}') malformed('Trailing results comma');
      }
      if (!bindings) malformed('Missing bindings array');
    } else if (name === 'boolean') {
      if (kind) malformed('Mixed ASK and SELECT results');
      kind = 'boolean';
      const value = await input.value();
      if (typeof value !== 'boolean') malformed('Invalid ASK boolean');
      answer = value;
    } else await input.value();
    key = await input.next();
    if (key === '}') break;
    if (key !== ',') malformed('Invalid SPARQL JSON delimiter');
    key = await input.next();
    if (key === '}') malformed('Trailing SPARQL JSON comma');
  }
  if (!kind) malformed('Missing SPARQL query results');
  if ((await input.take()) !== undefined) malformed('Trailing SPARQL JSON data');
  return answer;
}

/** One complete N-Quads line, including RDF 1.2 triple terms. */
export function parseNQuad(text: string, f: RDF.DataFactory): RDF.Quad | null {
  let at = 0;
  const spaces = () => {
    const start = at;
    while (text[at] === ' ' || text[at] === '\t' || text[at] === '\r') at++;
    return at !== start;
  };
  const required = () => {
    if (!spaces()) malformed('Missing N-Quads whitespace');
  };
  const escape = () => {
    const c = text[at++];
    if (c === 'u' || c === 'U') {
      const count = c === 'u' ? 4 : 8;
      const hex = text.slice(at, at + count);
      if (!new RegExp(`^[\\da-f]{${count}}$`, 'i').test(hex)) malformed('Invalid Unicode escape');
      at += count;
      const n = parseInt(hex, 16);
      if (n > 0x10ffff || (n >= 0xd800 && n <= 0xdfff)) malformed('Invalid Unicode escape');
      return String.fromCodePoint(n);
    }
    const escapes: Record<string, string> = {
      t: '\t',
      b: '\b',
      n: '\n',
      r: '\r',
      f: '\f',
      '"': '"',
      "'": "'",
      '\\': '\\',
    };
    return escapes[c] ?? malformed('Invalid RDF escape');
  };
  const term = (depth = 0): RDF.Term => {
    if (depth > 64) malformed('Triple-term nesting exceeds 64');
    if (text.startsWith('<<(', at)) {
      at += 3;
      spaces();
      const s = term(depth + 1);
      required();
      const p = term(depth + 1);
      required();
      const o = term(depth + 1);
      spaces();
      if (!text.startsWith(')>>', at)) malformed('Invalid triple term');
      at += 3;
      return triple(s, p, o, f);
    }
    if (text[at] === '<') {
      at++;
      let value = '';
      while (at < text.length && text[at] !== '>') {
        const c = text[at++];
        if (c === '\\') {
          if (text[at] !== 'u' && text[at] !== 'U') malformed('Invalid IRI escape');
          value += escape();
        } else value += c;
      }
      if (text[at++] !== '>') malformed('Unterminated IRI');
      return f.namedNode(iri(value));
    }
    if (text.startsWith('_:', at)) {
      at += 2;
      const m = blankStart.exec(text.slice(at));
      if (!m) malformed('Invalid blank node');
      const label = m[0].replace(/\.+$/, '');
      at += label.length;
      return f.blankNode(label);
    }
    if (text[at] === '"') {
      at++;
      let value = '';
      let closed = false;
      while (at < text.length) {
        const c = text[at++];
        if (c === '"') {
          closed = true;
          break;
        }
        if (c === '\n' || c === '\r') malformed('Unescaped literal newline');
        value += c === '\\' ? escape() : c;
      }
      if (!closed) malformed('Unterminated literal');
      scalar(value);
      if (text[at] === '@') {
        at++;
        const m = /^[a-z]+(?:-[a-z\d]+)*/i.exec(text.slice(at));
        if (!m) malformed('Invalid language tag');
        at += m[0].length;
        const tag = m[0];
        let dir: 'ltr' | 'rtl' | undefined;
        if (text.startsWith('--ltr', at) || text.startsWith('--rtl', at)) {
          dir = text.slice(at + 2, at + 5) as 'ltr' | 'rtl';
          at += 5;
        }
        language(tag, dir);
        return f.literal(value, dir ? { language: tag, direction: dir } : tag);
      }
      if (text.startsWith('^^', at)) {
        at += 2;
        const datatype = term(depth + 1);
        if (datatype.termType !== 'NamedNode') malformed('Invalid datatype');
        return f.literal(value, datatype);
      }
      return f.literal(value);
    }
    return malformed('Invalid N-Quads term');
  };
  try {
    spaces();
    if (at === text.length || text[at] === '#') return null;
    const s = term();
    required();
    const p = term();
    required();
    const o = term();
    const separated = spaces();
    let g: RDF.Term = f.defaultGraph();
    if (text[at] !== '.') {
      if (!separated) malformed('Missing graph whitespace');
      g = term();
      spaces();
    }
    if (
      text[at++] !== '.' ||
      (text.slice(at).trim() && !text.slice(at).trimStart().startsWith('#'))
    )
      malformed('Invalid N-Quads terminator');
    if (
      !['NamedNode', 'BlankNode'].includes(s.termType) ||
      p.termType !== 'NamedNode' ||
      !['NamedNode', 'BlankNode', 'Literal', 'Quad'].includes(o.termType) ||
      !['NamedNode', 'BlankNode', 'DefaultGraph'].includes(g.termType)
    )
      malformed('Invalid N-Quads positions');
    return f.quad(
      s as RDF.Quad_Subject,
      p as RDF.Quad_Predicate,
      o as RDF.Quad_Object,
      g as RDF.Quad_Graph,
    );
  } catch (e) {
    if (e instanceof RdfSyntaxError) throw e;
    return malformed(`Invalid N-Quads: ${e instanceof Error ? e.message : String(e)}`);
  }
}
