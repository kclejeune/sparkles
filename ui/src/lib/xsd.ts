// Lexical forms of the XSD datatypes that SHACL's `sh:datatype` names (XML Schema 1.1
// Part 2), for the checks an editing form runs in the browser (spec U02 §4.3). A
// datatype this module does not know passes, and the server checks it.

export const XSD = 'http://www.w3.org/2001/XMLSchema#';
export const RDF_LANG_STRING = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#langString';

const INTEGER = /^[+-]?\d+$/;
const DECIMAL = /^[+-]?(\d+(\.\d*)?|\.\d+)$/;
const FLOAT = /^([+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?|[+-]?INF|NaN)$/;
const TZ = '(Z|[+-]((0\\d|1[0-3]):[0-5]\\d|14:00))?';
const YEAR = '-?([1-9]\\d{3,}|0\\d{3})';
const MONTH = '(0[1-9]|1[0-2])';
const DAY = '(0[1-9]|[12]\\d|3[01])';
const TIME = '(([01]\\d|2[0-3]):[0-5]\\d:[0-5]\\d(\\.\\d+)?|24:00:00(\\.0+)?)';
const DATE = new RegExp(`^${YEAR}-${MONTH}-${DAY}${TZ}$`);
const DATE_TIME = new RegExp(`^${YEAR}-${MONTH}-${DAY}T${TIME}${TZ}$`);
const DATE_TIME_STAMP = new RegExp(
  `^${YEAR}-${MONTH}-${DAY}T${TIME}(Z|[+-]((0\\d|1[0-3]):[0-5]\\d|14:00))$`,
);
const TIME_RE = new RegExp(`^${TIME}${TZ}$`);
const G_YEAR = new RegExp(`^${YEAR}${TZ}$`);
const G_YEAR_MONTH = new RegExp(`^${YEAR}-${MONTH}${TZ}$`);
const G_MONTH_DAY = new RegExp(`^--${MONTH}-${DAY}${TZ}$`);
const G_DAY = new RegExp(`^---${DAY}${TZ}$`);
const G_MONTH = new RegExp(`^--${MONTH}${TZ}$`);
const DURATION = /^-?P(?=\d|T\d)(\d+Y)?(\d+M)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+(\.\d+)?S)?)?$/;
const LANGUAGE = /^[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*$/;

/** The bounds of the integer types (inclusive), as BigInt. */
const RANGES: Record<string, [bigint | null, bigint | null]> = {
  integer: [null, null],
  long: [-(2n ** 63n), 2n ** 63n - 1n],
  int: [-(2n ** 31n), 2n ** 31n - 1n],
  short: [-32768n, 32767n],
  byte: [-128n, 127n],
  nonNegativeInteger: [0n, null],
  positiveInteger: [1n, null],
  nonPositiveInteger: [null, 0n],
  negativeInteger: [null, -1n],
  unsignedLong: [0n, 2n ** 64n - 1n],
  unsignedInt: [0n, 2n ** 32n - 1n],
  unsignedShort: [0n, 65535n],
  unsignedByte: [0n, 255n],
};

/** The local name of an XSD datatype IRI, or null for another namespace. */
export function xsdLocal(dt: string | undefined | null): string | null {
  return dt && dt.startsWith(XSD) ? dt.slice(XSD.length) : null;
}

/** Whether a date's day exists in its month (February 29 in leap years only). */
function realDay(y: number, m: number, d: number): boolean {
  const days = [
    31,
    y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0) ? 29 : 28,
    31,
    30,
    31,
    30,
    31,
    31,
    30,
    31,
    30,
    31,
  ];
  return d <= days[m - 1];
}

function dateOk(v: string): boolean {
  const m = /^(-?\d{4,})-(\d\d)-(\d\d)/.exec(v);
  return !m || realDay(Number(m[1]), Number(m[2]), Number(m[3]));
}

/**
 * Whether `value` is a valid lexical form of `datatype`. Datatypes this module does not
 * know are accepted (the server checks them).
 */
export function validLexical(value: string, datatype: string): boolean {
  const local = xsdLocal(datatype);
  if (datatype === RDF_LANG_STRING) return true;
  if (local === null) return true;
  if (local in RANGES) {
    if (!INTEGER.test(value)) return false;
    const n = BigInt(value);
    const [lo, hi] = RANGES[local];
    return (lo === null || n >= lo) && (hi === null || n <= hi);
  }
  switch (local) {
    case 'string':
    case 'normalizedString':
    case 'token':
    case 'anyURI':
      return true;
    case 'boolean':
      return /^(true|false|1|0)$/.test(value);
    case 'decimal':
      return DECIMAL.test(value);
    case 'float':
    case 'double':
      return FLOAT.test(value);
    case 'date':
      return DATE.test(value) && dateOk(value);
    case 'dateTime':
      return DATE_TIME.test(value) && dateOk(value);
    case 'dateTimeStamp':
      return DATE_TIME_STAMP.test(value) && dateOk(value);
    case 'time':
      return TIME_RE.test(value);
    case 'gYear':
      return G_YEAR.test(value);
    case 'gYearMonth':
      return G_YEAR_MONTH.test(value);
    case 'gMonthDay':
      return G_MONTH_DAY.test(value);
    case 'gDay':
      return G_DAY.test(value);
    case 'gMonth':
      return G_MONTH.test(value);
    case 'duration':
      return DURATION.test(value);
    case 'language':
      return LANGUAGE.test(value);
    default:
      return true;
  }
}

/** Whether a datatype is numeric (a number input fits it). */
export function isNumeric(datatype: string | undefined | null): boolean {
  const l = xsdLocal(datatype);
  return l !== null && (l in RANGES || l === 'decimal' || l === 'float' || l === 'double');
}

/** Whether a datatype holds whole numbers. */
export function isInteger(datatype: string | undefined | null): boolean {
  const l = xsdLocal(datatype);
  return l !== null && l in RANGES;
}

/** The `step` of a number input for a datatype: 1 for integers, `any` otherwise. */
export function numberStep(datatype: string | undefined | null): string {
  return isInteger(datatype) ? '1' : 'any';
}

/** The `min` and `max` an integer type implies, for a number input. */
export function typeBounds(datatype: string | undefined | null): { min?: string; max?: string } {
  const l = xsdLocal(datatype);
  if (l === null || !(l in RANGES)) return {};
  const [lo, hi] = RANGES[l];
  return { min: lo === null ? undefined : String(lo), max: hi === null ? undefined : String(hi) };
}

/** A plain-words name of a datatype for problem messages. */
export function datatypeName(datatype: string): string {
  const l = xsdLocal(datatype);
  if (datatype === RDF_LANG_STRING) return 'text with a language';
  if (l === null) return datatype;
  switch (l) {
    case 'integer':
      return 'a whole number';
    case 'decimal':
      return 'a decimal number';
    case 'float':
    case 'double':
      return 'a number';
    case 'boolean':
      return 'true or false';
    case 'date':
      return 'a date (YYYY-MM-DD)';
    case 'dateTime':
      return 'a date and time (YYYY-MM-DDThh:mm:ss)';
    case 'gYear':
      return 'a year';
    default:
      return `xsd:${l}`;
  }
}

/**
 * A value comparable for ranges: a number for numeric types, a time in milliseconds for
 * dates and date-times with a time zone or none, or null when they do not compare.
 */
export function rangeValue(value: string, datatype: string | undefined | null): number | null {
  if (isNumeric(datatype)) {
    const n = Number(value);
    return Number.isNaN(n) ? null : n;
  }
  const l = xsdLocal(datatype);
  if (l === 'date' || l === 'dateTime' || l === 'dateTimeStamp' || l === 'gYear') {
    const v = l === 'gYear' ? `${value.slice(0, 4)}-01-01` : value;
    const t = Date.parse(l === 'date' && !/[Z+-]\d\d:?\d\d$|Z$/.test(v) ? `${v}T00:00:00Z` : v);
    return Number.isNaN(t) ? null : t;
  }
  return null;
}
