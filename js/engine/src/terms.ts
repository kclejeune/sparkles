import type * as RDF from '@rdfjs/types';
import { InvalidInputError } from '@sparkles-rdf/common';

// The binary form of quads that JavaScript sends to the addon, which reads it in
// crates/sparkles-node/src/input.rs. It has the layout of a result batch: one string of
// term text and one Uint32Array with four header words (flags, term entries including
// the unused entry 0, rows and cells per row), four words per term entry, then the cells.
// An entry's first word is its kind plus a handle number shifted left by 8. A handle asks
// the addon to keep the term under that number, and later requests send the number with
// bit 31 set instead of the term. Offsets count UTF-16 units. The array may be longer
// than the request, whose length the header gives.

const NAMED = 1;
const BLANK = 2;
const TYPED = 3;
const LANG = 4;
const LANG_LTR = 5;
const LANG_RTL = 6;
const DEFAULT_GRAPH = 7;
const TRIPLE = 8;
const RESET = 1;
const XSD_STRING = 'http://www.w3.org/2001/XMLSchema#string';
const HANDLE_BIT = 0x8000_0000;
/** Handles at most, the addon's limit too. Each costs about 100 bytes on either side. */
const MAX_HANDLES = 1 << 16;

/**
 * A dataset's numbering of the IRIs and literals it has sent to the addon. A repeated
 * predicate, class or subject then crosses as a number, and the addon keeps its engine id,
 * which saves the lookup in the store's vocabulary. Blank nodes are never numbered, since
 * a transaction scopes their labels.
 */
export class TermCache {
  private iris = new Map<string, number>();
  private literals = new Map<string, number>();
  private defaultGraph = 0;
  private next = 1;
  private reset = false;
  /** the request handed to the addon: the header and the entries, then the cells */
  private head = new Uint32Array(256);
  private entries = 0;
  /** the cells of the request being encoded */
  private cells = new Uint32Array(64);
  private cellCount = 0;
  private text = '';
  private units = 0;

  /** Encode one quad, as `encode([quad])` does. */
  encodeQuad(q: RDF.Quad): { text: string; data: Uint32Array } {
    try {
      this.begin(1);
      if (!q || q.termType !== 'Quad') throw new InvalidInputError('Expected an RDF quad');
      this.row(
        this.cell(q.subject),
        this.cell(q.predicate),
        this.cell(q.object),
        this.cell(q.graph),
      );
      return this.end(1);
    } catch (e) {
      this.forget();
      throw e;
    }
  }

  /**
   * Encode quads. The request is valid until the next one is encoded, and it is a prefix
   * of `data`, whose header gives its length.
   */
  encode(quads: readonly RDF.Quad[]): { text: string; data: Uint32Array } {
    try {
      this.begin(quads.length);
      for (const q of quads) {
        if (!q || q.termType !== 'Quad') throw new InvalidInputError('Expected an RDF quad');
        this.row(
          this.cell(q.subject),
          this.cell(q.predicate),
          this.cell(q.object),
          this.cell(q.graph),
        );
      }
      return this.end(quads.length);
    } catch (e) {
      // numbers given out for terms the addon never received must not be used
      this.forget();
      throw e;
    }
  }

  /** Encode a quad pattern, whose missing terms match any. */
  pattern(
    s?: RDF.Term | null,
    p?: RDF.Term | null,
    o?: RDF.Term | null,
    g?: RDF.Term | null,
  ): { text: string; data: Uint32Array } {
    try {
      this.begin(1);
      this.row(
        s == null ? 0 : this.cell(s),
        p == null ? 0 : this.cell(p),
        o == null ? 0 : this.cell(o),
        g == null ? 0 : this.cell(g),
      );
      return this.end(1);
    } catch (e) {
      this.forget();
      throw e;
    }
  }

  private begin(rows: number) {
    if (this.next >= MAX_HANDLES) this.forget();
    this.entries = 1;
    this.cellCount = 0;
    this.text = '';
    this.units = 0;
    if (this.cells.length < 4 * rows) this.cells = new Uint32Array(4 * rows);
  }

  private row(s: number, p: number, o: number, g: number) {
    const c = this.cells;
    const n = this.cellCount;
    c[n] = s;
    c[n + 1] = p;
    c[n + 2] = o;
    c[n + 3] = g;
    this.cellCount = n + 4;
  }

  private end(rows: number) {
    const at = 4 + 4 * this.entries;
    const n = this.cellCount;
    // The addon reads a request before the call returns, so one buffer serves them all
    // and no request allocates memory outside the JavaScript heap. The entries are
    // already in it, and the addon reads the request from its start, so the buffer is
    // handed over whole and no call makes a view of it.
    if (this.head.length < at + n) {
      const grown = new Uint32Array(Math.max(2 * this.head.length, at + n));
      grown.set(this.head.subarray(0, at));
      this.head = grown;
    }
    const data = this.head;
    const c = this.cells;
    if (n <= 64) for (let i = 0; i < n; i++) data[at + i] = c[i];
    else data.set(c.subarray(0, n), at);
    data[0] = this.reset ? RESET : 0;
    data[1] = this.entries;
    data[2] = rows;
    data[3] = 4;
    data[4] = data[5] = data[6] = data[7] = 0;
    this.reset = false;
    const text = this.text;
    this.text = '';
    return { text, data };
  }

  /** Start over, after the addon rejected a request and dropped its own table. */
  forget() {
    this.iris.clear();
    this.literals.clear();
    this.defaultGraph = 0;
    this.next = 1;
    this.reset = true;
  }

  private entry(kind: number, handle: number, a: number, b: number, c: number) {
    const m = 4 + 4 * this.entries;
    if (m + 4 > this.head.length) {
      const grown = new Uint32Array(this.head.length * 2);
      grown.set(this.head);
      this.head = grown;
    }
    const d = this.head;
    d[m] = kind | (handle << 8);
    d[m + 1] = a;
    d[m + 2] = b;
    d[m + 3] = c;
    return this.entries++;
  }

  private add(value: string) {
    const start = this.units;
    this.text += value;
    this.units += value.length;
    return start;
  }

  private handle() {
    return this.next < MAX_HANDLES ? this.next++ : 0;
  }

  private iri(value: string): number {
    const known = this.iris.get(value);
    if (known !== undefined) return (known | HANDLE_BIT) >>> 0;
    const h = this.handle();
    const a = this.add(value);
    const e = this.entry(NAMED, h, a, this.units, 0);
    if (h) this.iris.set(value, h);
    return e;
  }

  private cell(term: RDF.Term | null | undefined): number {
    if (!term || typeof term.value !== 'string') throw new TypeError('Expected an RDF/JS term');
    switch (term.termType) {
      case 'NamedNode':
        return this.iri(term.value);
      case 'Literal': {
        const language = term.language;
        const direction = (term as { direction?: string }).direction ?? '';
        const key = language
          ? term.value + '\u0000@' + language + '\u0000' + direction
          : term.value + '\u0000' + (term.datatype?.value ?? XSD_STRING);
        const known = this.literals.get(key);
        if (known !== undefined) return (known | HANDLE_BIT) >>> 0;
        let kind = TYPED;
        let c: number;
        let a: number;
        let b: number;
        if (language) {
          if (direction === '') kind = LANG;
          else if (direction === 'ltr') kind = LANG_LTR;
          else if (direction === 'rtl') kind = LANG_RTL;
          else throw new InvalidInputError('invalid literal direction');
          a = this.add(term.value);
          b = this.units;
          this.add(language);
          c = this.units;
        } else {
          if (direction) throw new InvalidInputError('direction requires a language');
          // the datatype by its IRI alone, as `encodeTerm` reads it
          const datatype = term.datatype?.value ?? XSD_STRING;
          if (typeof datatype !== 'string') throw new TypeError('Expected an RDF/JS term');
          c = this.iri(datatype);
          a = this.add(term.value);
          b = this.units;
        }
        const h = this.handle();
        const e = this.entry(kind, h, a, b, c);
        if (h) this.literals.set(key, h);
        return e;
      }
      case 'BlankNode': {
        const a = this.add(term.value);
        return this.entry(BLANK, 0, a, this.units, 0);
      }
      case 'DefaultGraph': {
        if (this.defaultGraph) return (this.defaultGraph | HANDLE_BIT) >>> 0;
        const h = this.handle();
        const e = this.entry(DEFAULT_GRAPH, h, 0, 0, 0);
        this.defaultGraph = h;
        return e;
      }
      case 'Quad': {
        const q = term as RDF.Quad;
        if (q.graph.termType !== 'DefaultGraph')
          throw new InvalidInputError('a triple term must have the default graph');
        const s = this.cell(q.subject);
        const p = this.cell(q.predicate);
        const o = this.cell(q.object);
        return this.entry(TRIPLE, 0, s, p, o);
      }
      default:
        throw new InvalidInputError(`a ${term.termType} cannot be stored`);
    }
  }
}
