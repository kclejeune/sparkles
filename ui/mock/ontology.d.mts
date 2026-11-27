// The types of mock/ontology.mjs for TypeScript tests that import it, so that the type
// checker reads these declarations instead of checking the mock's JavaScript.
export const UNI_PREFIXES: Record<string, string>;
export const UNI_TURTLE: string;
export const CELLS_PREFIXES: Record<string, string>;
export const CELLS_TURTLE: string;
export const DEEP_PREFIXES: Record<string, string>;
export const DEEP_CHAIN: number;
export const DEEP_WIDE: number;
export function deepTurtle(): string;
export function handleOntology(
  req: unknown,
  res: unknown,
  url: URL,
  seg: string[],
  ctx: unknown,
): Promise<boolean>;
