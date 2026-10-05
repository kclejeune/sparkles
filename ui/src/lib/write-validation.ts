// Labels for the dataset page's write-time validation panel.
import { ApiError, type GuardReport, type ValidationBaseline, type ValidationCheck } from './api';
import { fmtInt } from './format';

/** The baseline badge: class and label. */
export function baselineBadge(b: ValidationBaseline | null): { cls: string; label: string } {
  if (!b || b.conforms === null) return { cls: '', label: 'State unknown' };
  if (b.conforms) return { cls: 'ok', label: 'Conforms' };
  const n = b.blocking;
  return { cls: 'danger', label: `${fmtInt(n)} blocking result${n === 1 ? '' : 's'}` };
}

/** Reasons a write was validated in full, in words. */
const FALLBACKS: Record<string, string> = {
  baseline: 'the state of the head was unknown',
  shapes: 'the shapes changed',
  subclass: 'rdfs:subClassOf changed',
  sparql: 'a shape uses SHACL-SPARQL',
  recursive: 'a shape is recursive',
  bulk: 'bulk write',
  budget: 'too many affected focus nodes',
};

export function fallbackText(reason: string | undefined): string | null {
  return reason ? (FALLBACKS[reason] ?? reason) : null;
}

/** One line about a validated write: strategy, focus nodes, results, time. */
export function checkLine(c: ValidationCheck): string {
  const parts: string[] = [c.strategy === 'incremental' ? 'Incremental' : 'Full'];
  if (c.focusNodes != null) {
    parts.push(`${fmtInt(c.focusNodes)} focus node${c.focusNodes === 1 ? '' : 's'}`);
  }
  if (c.introduced != null) parts.push(`${fmtInt(c.introduced)} new blocking`);
  parts.push(`${fmtInt(c.blocking)} blocking of ${fmtInt(c.total)}`);
  parts.push(`${fmtInt(c.millis)} ms`);
  const why = fallbackText(c.fallback);
  if (why)
    parts.push(c.strategy === 'full' ? `full because ${why}` : `some shapes in full: ${why}`);
  return parts.join(' · ');
}

type Term = { type?: string; value?: string };

/** The shape and focus node of a check's first result, as display strings. */
export function firstResult(c: ValidationCheck): { shape: string; node: string } | null {
  const r = c.first;
  if (!r) return null;
  const term = (t: unknown) => {
    const x = (t ?? {}) as Term;
    if (x.type === 'start') return 'START';
    return x.type === 'bnode' ? `_:${x.value ?? ''}` : (x.value ?? '');
  };
  // SHACL results have sourceShape/focusNode, ShEx result-map entries shape/node
  if ('sourceShape' in r) return { shape: term(r.sourceShape), node: term(r.focusNode) };
  return { shape: term(r.shape), node: term(r.node) };
}

/** Only structured validation rejections belong in the results table. */
export function rejectionReport(error: unknown): GuardReport | null {
  if (!(error instanceof ApiError) || error.status !== 422) return null;
  const report = error.body?.validation;
  if (!report || typeof report !== 'object' || Array.isArray(report)) return null;
  const r = report as GuardReport;
  if (r.language !== undefined && r.language !== 'shacl' && r.language !== 'shex') return null;
  for (const n of [r.blocking, r.total]) {
    if (n !== undefined && (typeof n !== 'number' || !Number.isSafeInteger(n) || n < 0))
      return null;
  }
  if (
    r.results !== undefined &&
    (!Array.isArray(r.results) ||
      r.results.some((row) => !row || typeof row !== 'object' || Array.isArray(row)))
  )
    return null;
  if (r.blocking === undefined && r.results === undefined) return null;
  return r;
}
