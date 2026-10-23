// What the Pattern pane's pickers offer (U08 §2.2, §3.1, §4.6): the classes and
// predicates of the dataset's schema at the query page's branch and `at`, read under the
// caller's view, the profile of a class for ranking, and variable names in the manner of
// U03 §4.2. The pickers always accept a typed IRI or prefixed name, so a schema that does
// not load, or a dataset with more terms than one page, still builds any query.

import type { ClassProfile, Page, SchemaClass, SchemaPredicate, SchemaSummary } from './api';
import { RESERVED } from './stored-queries';

/** The reads the pickers make, so tests can stand in for the server. */
export type SchemaApi = {
  summary(
    ds: string,
    opts: { limit: number; at?: string; signal?: AbortSignal },
  ): Promise<SchemaSummary>;
  page<K extends 'classes' | 'predicates'>(
    ds: string,
    list: K,
    cursor: string,
    opts: { limit: number; at?: string; signal?: AbortSignal },
  ): Promise<Page<K extends 'classes' ? SchemaClass : SchemaPredicate>>;
  profile(
    ds: string,
    cls: string,
    opts: { at?: string; signal?: AbortSignal },
  ): Promise<ClassProfile | null>;
};

/** One choice of a picker: an IRI, its count, and why it is offered first. */
export type Choice = { iri: string; count: number | null; hint?: string };

/** The first page holds at most this many classes and predicates (§3.1). */
export const PAGE = 1000;
/** The pickers keep at most this many of each in memory (§4.6). */
export const HOLD = 10_000;
/** Profiles kept per commit (§4.6). */
export const PROFILES = 50;

/**
 * The schema of one dataset at one state, read once and paged on demand. `error` says why
 * the lists are empty when the read failed, and the pickers then take typed IRIs only.
 */
export class SchemaSource {
  classes: SchemaClass[] = [];
  predicates: SchemaPredicate[] = [];
  next: { classes: string | null; predicates: string | null } = { classes: null, predicates: null };
  error: string | null = null;
  commit: string | null = null;
  private loading: Promise<void> | null = null;
  private profiles = new Map<string, Promise<ClassProfile | null>>();

  constructor(
    private api: SchemaApi,
    readonly ds: string,
    readonly at?: string,
  ) {}

  /** The first page of both lists, read once. */
  load(signal?: AbortSignal): Promise<void> {
    this.loading ??= this.api
      .summary(this.ds, { limit: PAGE, at: this.at, signal })
      .then((s) => {
        this.classes = s.classes.items.slice(0, HOLD);
        this.predicates = s.predicates.items.slice(0, HOLD);
        this.next = { classes: s.classes.next, predicates: s.predicates.next };
        this.commit = String(s.snapshot.commit ?? s.snapshot.generation);
        this.error = null;
      })
      .catch((e: unknown) => {
        this.loading = null;
        this.error = e instanceof Error ? e.message : String(e);
      });
    return this.loading;
  }

  /** The next page of a list, when there is one and memory allows. */
  async more(list: 'classes' | 'predicates', signal?: AbortSignal): Promise<void> {
    const cursor = this.next[list];
    if (!cursor || this[list].length >= HOLD) return;
    try {
      const page = await this.api.page(this.ds, list, cursor, { limit: PAGE, at: this.at, signal });
      if (list === 'classes') this.classes.push(...(page.items as SchemaClass[]));
      else this.predicates.push(...(page.items as SchemaPredicate[]));
      this[list].length = Math.min(this[list].length, HOLD);
      this.next[list] = page.next;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    }
  }

  /** The profile of a class, fetched once per commit, at most `PROFILES` kept. */
  profile(cls: string, signal?: AbortSignal): Promise<ClassProfile | null> {
    const key = `${this.commit ?? ''}|${cls}`;
    let p = this.profiles.get(key);
    if (!p) {
      p = this.api.profile(this.ds, cls, { at: this.at, signal }).catch(() => null);
      this.profiles.set(key, p);
      while (this.profiles.size > PROFILES)
        this.profiles.delete(this.profiles.keys().next().value!);
    }
    return p;
  }

  /** The classes with the most instances, for the empty pane (§2.7). */
  topClasses(n = 5): Choice[] {
    return [...this.classes]
      .filter((c) => !c.builtin)
      .sort((a, b) => b.observed.instances - a.observed.instances)
      .slice(0, n)
      .map((c) => ({ iri: c.iri, count: c.observed.instances }));
  }
}

/**
 * The predicates for an edge, ranked (§2.2): those instances of the source's class use,
 * with their counts, then those whose values are instances of the target's class, then
 * the rest by their triples.
 */
export function rankPredicates(
  predicates: SchemaPredicate[],
  source: ClassProfile | null,
  target: ClassProfile | null,
): Choice[] {
  const out: Choice[] = [];
  const seen = new Set<string>();
  const push = (c: Choice) => {
    if (seen.has(c.iri)) return;
    seen.add(c.iri);
    out.push(c);
  };
  if (source)
    for (const p of [...source.properties].sort((a, b) => b.triples - a.triples))
      push({ iri: p.predicate, count: p.triples, hint: 'used by the class' });
  if (target)
    for (const p of [...target.incoming].sort((a, b) => b.triples - a.triples))
      push({ iri: p.predicate, count: p.triples, hint: 'reaches the class' });
  for (const p of [...predicates].sort((a, b) => b.observed.triples - a.observed.triples))
    push({ iri: p.iri, count: p.observed.triples });
  return out;
}

/** The classes for a node, by their instances. */
export function rankClasses(classes: SchemaClass[]): Choice[] {
  return [...classes]
    .sort((a, b) => b.observed.instances - a.observed.instances)
    .map((c) => ({ iri: c.iri, count: c.observed.instances }));
}

/** The choices whose IRI or short name holds `typed`, case aside, at most `n`. */
export function narrow(
  choices: Choice[],
  typed: string,
  short: (iri: string) => string,
  n = 50,
): Choice[] {
  const t = typed.trim().toLowerCase();
  if (!t) return choices.slice(0, n);
  return choices
    .filter((c) => c.iri.toLowerCase().includes(t) || short(c.iri).toLowerCase().includes(t))
    .slice(0, n);
}

/** The datatypes the profile of a subject's class saw for a predicate's literal values. */
export function datatypesOf(profile: ClassProfile | null, predicate: string): string[] | null {
  const p = profile?.properties.find((x) => x.predicate === predicate);
  const lits = p?.objects.literals;
  return lits?.length ? lits.map((l) => l.datatype) : null;
}

/** The local name of an IRI: after the last `#`, `/` or `:`. */
export function localName(iri: string): string {
  const m = /[^#/:]+$/.exec(iri.replace(/[#/]+$/, ''));
  return m ? m[0] : iri;
}

/**
 * A variable name in the manner of U03 §4.2: from a class's local name in lower camel
 * case (`?person`), else from a predicate (`?worksFor`), else `n1`. A clash with `taken`
 * gets a number (`?person2`), and the names stored queries reserve are skipped.
 */
export function varName(from: string | null, taken: Iterable<string>): string {
  const used = new Set(taken);
  const words = from
    ? localName(from)
        .replace(/[^\p{L}\p{N}]+/gu, ' ')
        .trim()
        .split(' ')
        .filter(Boolean)
    : [];
  let base = words
    .map((w, i) =>
      i === 0 ? w.charAt(0).toLowerCase() + w.slice(1) : w.charAt(0).toUpperCase() + w.slice(1),
    )
    .join('');
  if (!base || !/^[\p{L}_]/u.test(base)) base = base ? `v${base}` : 'n';
  const free = (n: string) => !used.has(n) && !RESERVED.has(n);
  if (base !== 'n' && free(base)) return base;
  for (let i = base === 'n' ? 1 : 2; ; i++) if (free(`${base}${i}`)) return `${base}${i}`;
}
