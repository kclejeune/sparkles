// Browser-local viewer preferences. Keep the old full-IRI switch as a shortcut and
// migrate its saved value when there is no supported rendering preference record.
import { loadRaw, saveRaw } from './storage';
import { expandLanguages, readRendering, RENDERING_KEY, type EntityDisplay } from './rendering';
const initial = readRendering(loadRaw(RENDERING_KEY), loadRaw('sparkles.iris') === 'full');
class IriForm {
  mode = $state<EntityDisplay>(initial.mode);
  private languageTags = $state.raw(initial.languages);
  private effectiveLanguages = $derived(
    expandLanguages(
      this.languageTags.length
        ? this.languageTags
        : typeof navigator === 'undefined'
          ? ['en']
          : [...navigator.languages],
    ),
  );
  get full(): boolean {
    return this.mode === 'iri';
  }
  get languages(): readonly string[] {
    return this.effectiveLanguages;
  }
  get languageText(): string {
    return this.languageTags.join(', ');
  }
  set(full: boolean) {
    this.update(full ? 'iri' : 'prefixed');
  }
  update(mode: EntityDisplay, languages: readonly string[] = this.languageTags) {
    this.mode = mode;
    if (languages.join(',') !== this.languageTags.join(',')) this.languageTags = [...languages];
    saveRaw(RENDERING_KEY, JSON.stringify({ version: 1, mode, languages: [...languages] }));
    saveRaw('sparkles.iris', mode === 'iri' ? 'full' : null);
  }
}
export const iriForm = new IriForm();
