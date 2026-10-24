import { describe, expect, it } from 'vitest';
import {
  datatypeName,
  isInteger,
  isNumeric,
  numberStep,
  rangeValue,
  typeBounds,
  validLexical,
  XSD,
} from './xsd';

const x = (l: string) => XSD + l;

describe('validLexical', () => {
  it('checks the integer types and their bounds', () => {
    expect(validLexical('42', x('integer'))).toBe(true);
    expect(validLexical('+42', x('integer'))).toBe(true);
    expect(validLexical('4.2', x('integer'))).toBe(false);
    expect(validLexical('', x('integer'))).toBe(false);
    expect(validLexical('127', x('byte'))).toBe(true);
    expect(validLexical('128', x('byte'))).toBe(false);
    expect(validLexical('0', x('positiveInteger'))).toBe(false);
    expect(validLexical('-1', x('nonNegativeInteger'))).toBe(false);
    expect(validLexical('18446744073709551615', x('unsignedLong'))).toBe(true);
    expect(validLexical('18446744073709551616', x('unsignedLong'))).toBe(false);
  });

  it('checks decimals, floats and booleans', () => {
    expect(validLexical('1.5', x('decimal'))).toBe(true);
    expect(validLexical('.5', x('decimal'))).toBe(true);
    expect(validLexical('1e3', x('decimal'))).toBe(false);
    expect(validLexical('1e3', x('double'))).toBe(true);
    expect(validLexical('INF', x('float'))).toBe(true);
    expect(validLexical('NaN', x('double'))).toBe(true);
    expect(validLexical('nan', x('double'))).toBe(false);
    expect(validLexical('1', x('boolean'))).toBe(true);
    expect(validLexical('yes', x('boolean'))).toBe(false);
  });

  it('checks dates, times and real days', () => {
    expect(validLexical('2024-02-29', x('date'))).toBe(true);
    expect(validLexical('2023-02-29', x('date'))).toBe(false);
    expect(validLexical('1900-02-29', x('date'))).toBe(false);
    expect(validLexical('2000-02-29', x('date'))).toBe(true);
    expect(validLexical('2024-04-31', x('date'))).toBe(false);
    expect(validLexical('2024-04-30Z', x('date'))).toBe(true);
    expect(validLexical('2024-04-30+14:00', x('date'))).toBe(true);
    expect(validLexical('2024-04-30+14:01', x('date'))).toBe(false);
    expect(validLexical('2024-04-30T10:00:00', x('dateTime'))).toBe(true);
    expect(validLexical('2024-04-30T10:00', x('dateTime'))).toBe(false);
    expect(validLexical('2024-04-30T24:00:00', x('dateTime'))).toBe(true);
    expect(validLexical('2024-04-30T10:00:00', x('dateTimeStamp'))).toBe(false);
    expect(validLexical('2024-04-30T10:00:00Z', x('dateTimeStamp'))).toBe(true);
    expect(validLexical('10:00:00.5', x('time'))).toBe(true);
    expect(validLexical('2024', x('gYear'))).toBe(true);
    expect(validLexical('--04-30', x('gMonthDay'))).toBe(true);
    expect(validLexical('P1Y2M', x('duration'))).toBe(true);
    expect(validLexical('P', x('duration'))).toBe(false);
    expect(validLexical('PT', x('duration'))).toBe(false);
  });

  it('accepts what it does not know and strings', () => {
    expect(validLexical('anything', x('string'))).toBe(true);
    expect(validLexical('anything', 'http://example.org/dt')).toBe(true);
    expect(validLexical('anything', x('hexBinary'))).toBe(true);
    expect(validLexical('en-GB', x('language'))).toBe(true);
    expect(validLexical('en_GB', x('language'))).toBe(false);
  });
});

describe('number inputs', () => {
  it('derives step and bounds from the type', () => {
    expect(isNumeric(x('int'))).toBe(true);
    expect(isNumeric(x('double'))).toBe(true);
    expect(isNumeric(x('string'))).toBe(false);
    expect(isInteger(x('decimal'))).toBe(false);
    expect(numberStep(x('integer'))).toBe('1');
    expect(numberStep(x('decimal'))).toBe('any');
    expect(typeBounds(x('unsignedByte'))).toEqual({ min: '0', max: '255' });
    expect(typeBounds(x('integer'))).toEqual({ min: undefined, max: undefined });
    expect(typeBounds(x('string'))).toEqual({});
  });
});

describe('rangeValue', () => {
  it('compares numbers and dates', () => {
    expect(rangeValue('17', x('integer'))).toBe(17);
    expect(rangeValue('x', x('integer'))).toBeNull();
    expect(rangeValue('2024-01-02', x('date'))! > rangeValue('2024-01-01', x('date'))!).toBe(true);
    expect(rangeValue('abc', x('string'))).toBeNull();
  });
});

describe('datatypeName', () => {
  it('names datatypes in words', () => {
    expect(datatypeName(x('integer'))).toBe('a whole number');
    expect(datatypeName(x('date'))).toBe('a date (YYYY-MM-DD)');
    expect(datatypeName(x('hexBinary'))).toBe('xsd:hexBinary');
  });
});
