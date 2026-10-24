// The types of mock/world.mjs for the TypeScript specs that import it, so that the type
// checker reads these declarations instead of checking the mock's JavaScript.
export const WORLD: string;
export const WORLD_PEOPLE: Map<string, string>;
export const WORLD_KNOWS: [string, string][];
export function worldTurtle(): string;
export function worldCommunities(): unknown;
export function worldFlights(): string;
