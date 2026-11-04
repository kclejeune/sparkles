// The hook through which a page asks about a resource in the Ask drawer (ROADMAP item 633).
// The drawer registers its opener here when it mounts. Until one is registered, the pages
// leave their "Ask about this" action out, so they do not depend on the drawer's module.

/** The resource a question is about, as the drawer shows it in its context chip. */
export type AskAbout = {
  iri: string;
  label: string;
  type?: 'class' | 'property' | 'individual' | 'concept' | 'resource';
};

class AskHook {
  /** Opens the drawer with the resource as the question's context. */
  open = $state<((about: AskAbout) => void) | null>(null);

  /** Registers the drawer's opener; returns the function that removes it. */
  register(fn: (about: AskAbout) => void): () => void {
    this.open = fn;
    return () => {
      if (this.open === fn) this.open = null;
    };
  }
}

export const askHook = new AskHook();
