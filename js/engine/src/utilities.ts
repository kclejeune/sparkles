import { nativeError, InvalidInputError } from '@sparkles-rdf/common';
import { native } from './native.js';

async function helper<T>(op: string, args: object): Promise<T> {
  try {
    return JSON.parse(await native.utility(op, JSON.stringify(args)));
  } catch (e) {
    throw nativeError(e);
  }
}
function timeout(value?: number) {
  if (value !== undefined && (!Number.isSafeInteger(value) || value < 0))
    throw new InvalidInputError('timeout must be a nonnegative integer');
  return value;
}
export interface IriCheck {
  errors: string[];
  warnings: string[];
}
export interface LangtagCheck {
  error: string | null;
  canonical: string | null;
  language: string | null;
  script: string | null;
  region: string | null;
  variant: string | null;
  extension: string | null;
  privateUse: string | null;
}
export interface DataIssue {
  message: string;
  line: bigint | null;
  column: bigint | null;
}
export function checkIri(text: string) {
  return helper<IriCheck>('checkIri', { text });
}
export function checkLangtag(text: string) {
  return helper<LangtagCheck>('checkLangtag', { text });
}
export async function checkData(
  text: string,
  format: string,
  options: { baseIri?: string } = {},
): Promise<DataIssue | null> {
  const result = await helper<{
    message: string;
    line: string | null;
    column: string | null;
  } | null>('checkData', { text, format, ...options });
  return (
    result && {
      ...result,
      line: result.line === null ? null : BigInt(result.line),
      column: result.column === null ? null : BigInt(result.column),
    }
  );
}
export interface ParseOptions {
  baseIri?: string;
  prefixes?: Record<string, string>;
}
/** Validate with the engine parser and return its normalized SPARQL document. */
export function parseQuery(text: string, options: ParseOptions = {}) {
  return helper<string>('parseQuery', { text, ...options });
}
export function parseUpdate(text: string, options: ParseOptions = {}) {
  return helper<string>('parseUpdate', { text, ...options });
}
export type FormatLanguage = 'sparql' | 'turtle' | 'trig' | 'ntriples' | 'nquads' | 'jsonld';
export interface FormatOptions {
  lineWidth?: number;
  indentWidth?: number;
  sort?: boolean;
  prunePrefixes?: boolean;
  canonicalize?: boolean;
  directiveStyle?: 'sparql' | 'turtle';
  prefixGroups?: string[][];
  typeShorthand?: boolean;
  compactIris?: boolean;
  quoteStyle?: 'double' | 'preserve';
  operatorPosition?: 'leading' | 'trailing';
  turtleLayout?: 'diff' | 'conventional';
  alignValues?: boolean;
  cursor?: number;
  timeout?: number;
}
export interface Formatted {
  text: string;
  changed: boolean;
  cursor: number | null;
  language: FormatLanguage;
  warnings: { code: string; message: string }[];
}
export function format(text: string, language: FormatLanguage, options: FormatOptions = {}) {
  const { timeout: ms, ...formatOptions } = options;
  return helper<Formatted>('format', {
    text,
    language,
    options: formatOptions,
    timeout: timeout(ms),
  });
}
export type LintSeverity = 'error' | 'warning' | 'info' | 'hint';
export interface LintDiagnostic {
  rule: string;
  severity: LintSeverity;
  message: string;
  start: number;
  end: number;
  line: number;
  column: number;
  endLine: number;
  endColumn: number;
  fix: { title: string; edits: { start: number; end: number; insert: string }[] } | null;
}
export function lint(
  text: string,
  language: FormatLanguage,
  options: { levels?: Record<string, LintSeverity | 'off'>; timeout?: number } = {},
) {
  return helper<{ language: FormatLanguage; diagnostics: LintDiagnostic[] }>('lint', {
    text,
    language,
    ...options,
    timeout: timeout(options.timeout),
  });
}
export interface GeometryLiteral {
  value: string;
  datatype: string;
}
export type ConvertedGeometry =
  | { geometry: { type: string; [key: string]: unknown } }
  | { error: string };
/** Geometry coordinates remain numbers; individual invalid literals produce errors. */
export function convertGeometries(items: GeometryLiteral[]) {
  return helper<ConvertedGeometry[]>('convertGeometries', { items });
}
export function previewSchedule(
  schedule: string,
  options: { timezone?: string; count?: number; after?: string | Date } = {},
) {
  if (
    options.count !== undefined &&
    (!Number.isSafeInteger(options.count) || options.count < 0 || options.count > 1000)
  )
    throw new InvalidInputError('count must be an integer from 0 to 1000');
  return helper<string[]>('previewSchedule', {
    schedule,
    ...options,
    after: options.after instanceof Date ? options.after.toISOString() : options.after,
  });
}
