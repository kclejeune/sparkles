const nf = new Intl.NumberFormat('en-US');

export const fmtInt = (n: number | null | undefined) => (n == null || Number.isNaN(n) ? '—' : nf.format(Math.round(n)));

export function fmtCompact(n: number): string {
  if (n < 0) return '—';
  if (n < 1000) return String(n);
  const units = ['k', 'M', 'B', 'T'];
  let v = n;
  let i = -1;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)}${units[i]}`;
}

export function fmtBytes(b: number): string {
  if (!b) return '0 B';
  const u = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const i = Math.min(u.length - 1, Math.floor(Math.log(b) / Math.log(1024)));
  const v = b / 1024 ** i;
  return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}

export function fmtMs(ms: number): string {
  if (ms == null || Number.isNaN(ms)) return '—';
  if (ms < 1) return `${ms.toFixed(2)} ms`;
  if (ms < 100) return `${ms.toFixed(1)} ms`;
  if (ms < 10_000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function fmtDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d) return `${d}d ${h}h ${m}m`;
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m ${sec}s`;
  return `${sec}s`;
}

export function fmtTime(iso: string | undefined): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'medium' });
}

export function fmtRelative(iso: string | undefined, now = Date.now()): string {
  if (!iso) return '—';
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return iso;
  const s = Math.round((now - t) / 1000);
  if (s < 5) return 'just now';
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

// --- SSE (SPARQL algebra) pretty-printing ---------------------------------------

type Sexp = string | Sexp[];

function parseSse(src: string): Sexp[] {
  /** End (exclusive) of an `<iri>` starting at `at`, or -1 when `<` is an operator. */
  const iriEnd = (at: number) => {
    const end = src.indexOf('>', at);
    return end > at && !/\s/.test(src.slice(at + 1, end)) ? end + 1 : -1;
  };
  const root: Sexp[] = [];
  const stack: Sexp[][] = [root];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) {
      i++;
    } else if (c === '(') {
      const list: Sexp[] = [];
      stack[stack.length - 1].push(list);
      stack.push(list);
      i++;
    } else if (c === ')') {
      if (stack.length > 1) stack.pop();
      i++;
    } else {
      // atom: a quoted string (with escapes and an optional @lang / ^^type), an <iri>, or a bare token
      let j = i;
      if (c === '"' || c === "'") {
        j++;
        while (j < src.length && src[j] !== c) j += src[j] === '\\' ? 2 : 1;
        j++;
      }
      while (j < src.length && !/[\s()]/.test(src[j])) j = src[j] === '<' && iriEnd(j) > 0 ? iriEnd(j) : j + 1;
      stack[stack.length - 1].push(src.slice(i, j));
      i = j;
    }
  }
  return root;
}

const inline = (e: Sexp): string => (typeof e === 'string' ? e : `(${e.map(inline).join(' ')})`);

function printSse(e: Sexp, indent: string, width: number): string {
  const flat = inline(e);
  if (typeof e === 'string' || flat.length + indent.length <= width) return flat;
  // keep leading atoms (the operator and its scalar arguments) on the first line
  let k = 0;
  while (k < e.length && typeof e[k] === 'string') k++;
  const head = e.slice(0, Math.max(k, 1)).map(inline).join(' ');
  const inner = indent + '  ';
  const rest = e.slice(Math.max(k, 1)).map((x) => '\n' + inner + printSse(x, inner, width));
  return `(${head}${rest.join('')})`;
}

/** Indent a single-line SSE algebra expression; returns the input unchanged if it looks multi-line already. */
export function formatSse(src: string, width = 80): string {
  if (!src || src.includes('\n')) return src;
  try {
    return parseSse(src)
      .map((e) => printSse(e, '', width))
      .join('\n');
  } catch {
    return src;
  }
}
