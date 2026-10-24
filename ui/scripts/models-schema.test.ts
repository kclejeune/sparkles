import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { OPENAPI, OUT, extract, render } from './models-schema.mjs';

describe('models-schema.json', () => {
  it('is the current extract of docs/openapi.json (run `pnpm models-schema`)', () => {
    const doc = JSON.parse(readFileSync(OPENAPI, 'utf8'));
    expect(readFileSync(OUT, 'utf8')).toBe(render(doc));
  });

  it('follows references and leaves out the API links', () => {
    const doc = {
      components: {
        schemas: {
          ModelsSettings: {
            externalDocs: { url: 'x' },
            properties: { a: { $ref: '#/components/schemas/B' } },
          },
          B: { type: 'string' },
          C: { type: 'integer' },
        },
      },
    };
    expect(extract(doc)).toEqual({
      B: { type: 'string' },
      ModelsSettings: { properties: { a: { $ref: '#/components/schemas/B' } } },
    });
  });
});
