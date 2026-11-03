import { describe, expect, it } from 'vitest';
import { entityHref, entityItem, type EntityHit } from './entity-search';
import { highlightParts, isLauncherKey, launcher } from './launcher.svelte';

const key = (init: Partial<KeyboardEvent>, target?: object) => ({
  key: '',
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  defaultPrevented: false,
  target: (target ?? null) as EventTarget | null,
  ...init,
});
const el = (tagName: string, type?: string) => ({ tagName, type, closest: () => null });

describe('the launcher key', () => {
  it('is Ctrl+K and Cmd+K anywhere', () => {
    expect(isLauncherKey(key({ key: 'k', ctrlKey: true }))).toBe(true);
    expect(isLauncherKey(key({ key: 'K', metaKey: true }))).toBe(true);
    expect(isLauncherKey(key({ key: 'k', ctrlKey: true, shiftKey: true }))).toBe(false);
    expect(isLauncherKey(key({ key: 'k' }))).toBe(false);
  });

  it('is / only outside a text field', () => {
    expect(isLauncherKey(key({ key: '/' }))).toBe(true);
    expect(isLauncherKey(key({ key: '/' }, el('BUTTON')))).toBe(true);
    expect(isLauncherKey(key({ key: '/' }, el('INPUT', 'text')))).toBe(false);
    expect(isLauncherKey(key({ key: '/' }, el('TEXTAREA')))).toBe(false);
    expect(isLauncherKey(key({ key: '/' }, { tagName: 'DIV', closest: () => ({}) }))).toBe(false);
    expect(isLauncherKey(key({ key: '/' }, el('INPUT', 'checkbox')))).toBe(true);
  });
});

describe('highlightParts', () => {
  it('splits a text at its ranges', () => {
    expect(highlightParts('mitochondrion', [[0, 8]])).toEqual([
      { text: 'mitochon', hit: true },
      { text: 'drion', hit: false },
    ]);
    expect(highlightParts('a b c', [[2, 3]])).toEqual([
      { text: 'a ', hit: false },
      { text: 'b', hit: true },
      { text: ' c', hit: false },
    ]);
  });

  it('ignores ranges outside the text and overlaps', () => {
    expect(
      highlightParts('abc', [
        [1, 9],
        [0, 2],
      ]),
    ).toEqual([
      { text: 'ab', hit: true },
      { text: 'c', hit: true },
    ]);
    expect(highlightParts('abc', [[5, 6]])).toEqual([{ text: 'abc', hit: false }]);
  });
});

describe('the provider registry', () => {
  it('orders providers and removes them again', () => {
    const search = async () => [];
    const offB = launcher.register({ id: 'b', label: 'B', order: 20, search });
    const offA = launcher.register({ id: 'a', label: 'A', order: 5, search });
    expect(launcher.providers.map((p) => p.id)).toEqual(['a', 'b']);
    offA();
    offB();
    expect(launcher.providers.map((p) => p.id)).toEqual([]);
  });
});

const hit = (over: Partial<EntityHit>): EntityHit => ({
  type: 'entity',
  iri: 'http://purl.obolibrary.org/obo/GO_0005739',
  kinds: ['class'],
  label: 'mitochondrion',
  labelLang: null,
  shortForm: 'GO:0005739',
  deprecated: false,
  match: {
    field: 'label',
    property: null,
    text: 'mitochondrion',
    lang: null,
    kind: 'prefix',
    distance: 0,
    tier: 1,
    highlight: [[0, 8]],
  },
  ...over,
});

describe('entity hits', () => {
  it('open every kind on its page in the ontology browser', () => {
    expect(entityHref('/ui', 'go', hit({}))).toBe(
      '/ui/browse?ds=go&iri=http%3A%2F%2Fpurl.obolibrary.org%2Fobo%2FGO_0005739',
    );
    for (const k of ['concept', 'individual', 'objectProperty', 'conceptScheme'])
      expect(entityHref('/ui', 'th', hit({ kinds: [k] }))).toMatch(/^\/ui\/browse\?ds=th&iri=/);
  });

  it('highlight a label match and name any other field', () => {
    const a = entityItem('/ui', 'go', hit({}));
    expect(a.title).toBe('mitochondrion');
    expect(a.highlight).toEqual([[0, 8]]);
    expect(a.subtitle).toBe('GO:0005739');
    expect(a.tag).toBe('class');
    const b = entityItem(
      '/ui',
      'go',
      hit({
        label: 'organelle envelope',
        match: { ...hit({}).match, field: 'synonym', text: 'mitochondrial envelope' },
      }),
    );
    expect(b.highlight).toEqual([]);
    expect(b.subtitle).toBe('GO:0005739 · synonym "mitochondrial envelope"');
  });
});
