import { describe, expect, it } from 'vitest';
import { permissive } from './licenses.js';

describe('permissive', () => {
  it('accepts the allowed licenses', () => {
    for (const l of ['MIT', 'ISC', 'BSD-3-Clause', 'Apache-2.0', '0BSD', 'OFL-1.1', 'Apache-2.0+'])
      expect(permissive(l), l).toBe(true);
  });

  it('refuses copyleft, unknown and missing licenses', () => {
    for (const l of [
      'GPL-3.0-only',
      'LGPL-2.1-or-later',
      'MPL-2.0',
      'CC-BY-4.0',
      'SEE LICENSE',
      '',
    ])
      expect(permissive(l), l).toBe(false);
  });

  it('needs one side of OR and both sides of AND', () => {
    expect(permissive('MIT OR GPL-3.0-only')).toBe(true);
    expect(permissive('(GPL-2.0-only OR MPL-2.0) OR Apache-2.0')).toBe(true);
    expect(permissive('MIT AND ISC')).toBe(true);
    expect(permissive('MIT AND GPL-3.0-only')).toBe(false);
    expect(permissive('(MIT OR GPL-3.0-only) AND (BSD-2-Clause OR MPL-2.0)')).toBe(true);
    expect(permissive('Apache-2.0 WITH LLVM-exception')).toBe(true);
    expect(permissive('GPL-2.0-only WITH Classpath-exception-2.0')).toBe(false);
  });
});
