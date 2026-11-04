// Whether the UI writes IRIs in full or as prefixed names (ROADMAP item 633). Prefixed names
// come from the dataset's prefix map together with the conventional prefixes of
// `WELL_KNOWN`. The choice is kept per viewer in the browser and every `displayIri` follows it.

const KEY = 'sparkles.iris';

function load(): boolean {
  try {
    return typeof localStorage !== 'undefined' && localStorage.getItem(KEY) === 'full';
  } catch {
    return false;
  }
}

class IriForm {
  /** Write IRIs in full instead of as prefixed names. */
  full = $state(load());

  set(full: boolean) {
    this.full = full;
    try {
      if (full) localStorage.setItem(KEY, 'full');
      else localStorage.removeItem(KEY);
    } catch {
      // the choice lasts for this page only
    }
  }
}

export const iriForm = new IriForm();
