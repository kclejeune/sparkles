// The routes of editing forms (docs/specs/U02-shape-driven-forms.md §3): the form model
// of a resource, the check of a form's changes and their save.

import { ApiError, json } from './api';
import { onBranch } from './branches';
import type { FieldChange, FieldResult, FormModel } from './forms';
import type { Term, Receipt } from './api';
import type { WritePreview } from './validation-overlay';

const enc = encodeURIComponent;

export type FormOptions = {
  /** The branch to open the form on (main when absent). */
  branch?: string | null;
  /** A past state (the form is then read-only). */
  at?: string;
  /** The language labels prefer. */
  lang?: string;
  /** Where the shapes come from, overriding the dataset's setting. */
  shapes?: 'guard' | 'graphs' | 'drafted';
  signal?: AbortSignal;
};

/** `GET /$/forms/{ds}?focus=`: the form model of a resource. */
export function getForm(ds: string, focus: string, opts: FormOptions = {}): Promise<FormModel> {
  const p = new URLSearchParams({ focus });
  if (opts.at) p.set('at', opts.at);
  if (opts.lang) p.set('lang', opts.lang);
  if (opts.shapes) p.set('shapes', opts.shapes);
  return json<FormModel>(`/$/forms/${enc(onBranch(ds, opts.branch))}?${p}`, {
    signal: opts.signal,
    cache: 'no-store',
  });
}

/** The body of a check or a save. */
export type FormChangeBody = {
  focus: string;
  branch?: string;
  shapes?: 'guard' | 'graphs' | 'drafted';
  changes: FieldChange[];
  expect: Record<string, Term[]>;
  message?: string;
};

export type FormCheckAnswer = {
  branch: string;
  preview: WritePreview;
  update: string;
  /** The changed fields another commit changed since the form loaded them. */
  conflicts?: FieldConflict[];
  validation: {
    checked: boolean;
    withheld: boolean;
    timedOut: boolean;
    conforms: boolean | null;
    results: FieldResult[] | null;
  };
};

export type FormSaveAnswer = Receipt & {
  branch: string;
  branchCreated: boolean;
};

/** The current values of the fields another commit changed since the form loaded them. */
export type FieldConflict = { id: string; now: Term[] };

const post = (body: FormChangeBody, signal?: AbortSignal): RequestInit => ({
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
  signal,
});

/** `POST /$/forms/{ds}/check`: a dry run of the change and its validation. */
export function checkForm(
  ds: string,
  body: FormChangeBody,
  signal?: AbortSignal,
): Promise<FormCheckAnswer> {
  return json<FormCheckAnswer>(`/$/forms/${enc(ds)}/check`, post(body, signal));
}

/**
 * `POST /$/forms/{ds}/save`: the commit of the change, or the fields another commit
 * changed meanwhile (`409 form-conflict`).
 */
export async function saveForm(
  ds: string,
  body: FormChangeBody,
  signal?: AbortSignal,
): Promise<{ saved: FormSaveAnswer } | { conflict: FieldConflict[]; branch: string }> {
  try {
    return { saved: await json<FormSaveAnswer>(`/$/forms/${enc(ds)}/save`, post(body, signal)) };
  } catch (e) {
    if (e instanceof ApiError && e.status === 409 && e.code === 'form-conflict') {
      const b = (e.body ?? {}) as { fields?: FieldConflict[]; branch?: string };
      return { conflict: b.fields ?? [], branch: b.branch ?? body.branch ?? 'main' };
    }
    throw e;
  }
}
