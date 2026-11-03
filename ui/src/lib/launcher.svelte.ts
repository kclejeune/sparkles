// The top bar's search and launcher (docs/visualization/README.md, "Web UI"). The bar's
// field is the launcher's input: Ctrl+K, Cmd+K and `/` focus it from anywhere, and the
// providers registered here answer what is typed into it, each as a group of items.
//
// This is the extension point of the command palette (ROADMAP item 631): a page or a
// feature registers a provider for as long as it is mounted, and the bar shows its items
// beside the others, in `order`. The shell registers one provider of its own, the entity
// search of `GET /$/ontology/{ds}/search` (lib/entity-search.ts).

/** One row of the launcher's list. */
export interface LauncherItem {
  /** Unique within its provider. */
  id: string;
  title: string;
  /** Ranges of `title` to highlight, in UTF-16 code units, end exclusive. */
  highlight?: [number, number][];
  subtitle?: string;
  /** A short tag at the end of the row, such as the kind of an entity. */
  tag?: string;
  /** Where Enter or a click goes. */
  href?: string;
  /** What Enter or a click does when there is no `href`. */
  run?: () => void;
}

export interface LauncherContext {
  /** The dataset the UI has open, or null. */
  ds: string | null;
  signal: AbortSignal;
}

export interface LauncherProvider {
  id: string;
  /** The heading of the provider's group. */
  label: string | ((ctx: { ds: string | null }) => string);
  /** Groups show in ascending order. */
  order: number;
  /** Whether the provider answers in this context, such as only with a dataset open. */
  active?: (ctx: { ds: string | null }) => boolean;
  /** Items for `query`; an empty query may answer recent or suggested items. */
  search(query: string, ctx: LauncherContext): Promise<LauncherItem[]>;
}

export interface LauncherGroup {
  provider: string;
  label: string;
  items: LauncherItem[];
  error?: string;
}

class LauncherState {
  /** Whether the results are showing (and, on a phone, the field is expanded). */
  open = $state(false);
  query = $state('');
  #providers = $state<LauncherProvider[]>([]);
  #focus: (() => void) | null = null;

  get providers(): readonly LauncherProvider[] {
    return this.#providers;
  }

  /** Adds a provider; the returned function removes it again. */
  register(p: LauncherProvider): () => void {
    this.#providers = [...this.#providers.filter((x) => x.id !== p.id), p].sort(
      (a, b) => a.order - b.order,
    );
    return () => {
      this.#providers = this.#providers.filter((x) => x !== p);
    };
  }

  /** The top bar binds the function that focuses its field. */
  bindFocus(f: (() => void) | null) {
    this.#focus = f;
  }

  /** Opens the launcher and focuses its field. */
  show() {
    this.open = true;
    this.#focus?.();
  }

  close() {
    this.open = false;
  }
}

export const launcher = new LauncherState();

/** Whether `e` happened in a place where `/` types a character. */
export function isTypingTarget(target: EventTarget | null): boolean {
  const t = target as Partial<HTMLInputElement> | null;
  if (!t || typeof t.tagName !== 'string') return false;
  if (t.isContentEditable || t.closest?.('.cm-editor')) return true;
  const tag = t.tagName.toUpperCase();
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (tag !== 'INPUT') return false;
  const type = t.type ?? 'text';
  return !['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file'].includes(
    type,
  );
}

type KeyLike = Pick<
  KeyboardEvent,
  'key' | 'ctrlKey' | 'metaKey' | 'altKey' | 'shiftKey' | 'defaultPrevented' | 'target'
>;

/** Whether `e` is the launcher's key: Ctrl+K or Cmd+K, or `/` outside a text field. */
export function isLauncherKey(e: KeyLike): boolean {
  if (e.defaultPrevented && e.key === '/') return false;
  if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'k')
    return true;
  return (
    e.key === '/' && !e.ctrlKey && !e.metaKey && !e.altKey && !isTypingTarget(e.target ?? null)
  );
}

/** The ranges of `text` as alternating plain and highlighted parts. */
export function highlightParts(
  text: string,
  ranges: [number, number][] = [],
): { text: string; hit: boolean }[] {
  const out: { text: string; hit: boolean }[] = [];
  let at = 0;
  for (const [a, b] of [...ranges].sort((x, y) => x[0] - y[0])) {
    const s = Math.max(a, at);
    const e = Math.min(b, text.length);
    if (e <= s) continue;
    if (s > at) out.push({ text: text.slice(at, s), hit: false });
    out.push({ text: text.slice(s, e), hit: true });
    at = e;
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}
