import { describe, expect, it } from 'vitest';
import { datasetSwitchHref } from './shell';

describe('datasetSwitchHref', () => {
  const at = (path: string) => new URL(`http://localhost${path}`);

  it('reopens a page that names its dataset in ?ds= without the old dataset parameters', () => {
    expect(datasetSwitchHref(at('/ui/explore?ds=uni&iri=x&branch=b'), 'foaf')).toBe(
      '/ui/explore?ds=foaf',
    );
  });

  it('opens the new dataset on the same tab of a dataset page', () => {
    expect(datasetSwitchHref(at('/ui/datasets/world?tab=communities&branch=b'), 'org')).toBe(
      '/ui/datasets/org?tab=communities',
    );
    expect(datasetSwitchHref(at('/ui/datasets/world'), 'a b')).toBe('/ui/datasets/a%20b');
  });

  it('leaves any other page where it is', () => {
    expect(datasetSwitchHref(at('/ui/datasets'), 'org')).toBeNull();
    expect(datasetSwitchHref(at('/ui/query'), 'org')).toBeNull();
  });
});
