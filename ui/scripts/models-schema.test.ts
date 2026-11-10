import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { OPENAPI, OUTPUTS, extract, render } from './models-schema.mjs';

describe('models-schema.json and notifications-schema.json', () => {
  it.each(Object.entries(OUTPUTS))(
    'the %s extract is current with docs/openapi.json (run `pnpm models-schema`)',
    (root, out) => {
      const doc = JSON.parse(readFileSync(OPENAPI, 'utf8'));
      expect(readFileSync(out, 'utf8')).toBe(render(doc, root));
    },
  );

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
