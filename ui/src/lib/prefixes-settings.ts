// Declared prefixes (spec C20): the `prefixes` settings kind of a dataset as the
// Settings tab's Prefixes section shows it. The kind is a map from a prefix name to an
// IRI. Its built-in defaults are the well-known prefixes, the server's settings file
// declares more, and the runtime layer holds the dataset's own bindings and, as `null`,
// the declared or well-known prefixes it removes.

import {
  datasetKindUrl,
  fieldInfo,
  pathString,
  type FieldInfo,
  type JsonObject,
  type SettingsKind,
} from './settings';

/** A prefix that shadows a well-known one with another IRI. */
export type PrefixWarning = { prefix: string; iri: string; wellKnown: string; message: string };

/** The prefixes kind's answer, which adds `warnings`. */
export type PrefixesKind = SettingsKind & { warnings?: PrefixWarning[] };

/** The URL of a dataset's prefixes kind. */
export const prefixesUrl = (ds: string) => datasetKindUrl(ds, 'prefixes');

/** Where a prefix comes from, in the table's words. */
export type PrefixSource = 'well-known' | 'server config' | 'changed' | 'locked' | 'removed';

/** One row of the Prefixes table. */
export type PrefixRow = {
  name: string;
  /** The effective IRI, or null for a prefix the runtime layer removes. */
  iri: string | null;
  source: PrefixSource;
  /** The source, lock and reset state of the name, as the other settings rows use it. */
  info: FieldInfo;
  /** A runtime value or removal that a reset takes away, bringing back the IRI below. */
  resettable: boolean;
  /** The IRI the server's settings file declares for the name, if any. */
  declared?: string;
  warning?: PrefixWarning;
};

const isString = (v: unknown): v is string => typeof v === 'string';

/** The rows of the table: every effective prefix and every removed one, by name. */
export function prefixRows(k: PrefixesKind): PrefixRow[] {
  const runtime = (k.runtime ?? {}) as JsonObject;
  const declared = (k.declared ?? {}) as JsonObject;
  const names = new Set([...Object.keys(k.effective ?? {}), ...Object.keys(runtime)]);
  const warnings = new Map((k.warnings ?? []).map((w) => [w.prefix, w]));
  return [...names]
    .sort((a, b) => a.localeCompare(b))
    .map((name) => {
      const path = pathString([name]);
      const info = fieldInfo(k, path);
      // a lock of the whole kind is the empty path
      if (k.locked.includes('') || info.source === 'locked') info.locked = true;
      const eff = k.effective?.[name];
      const removed = !isString(eff) && runtime[name] === null && !info.locked;
      const d = declared[name];
      let source: PrefixSource;
      if (info.locked) source = 'locked';
      else if (removed) source = 'removed';
      else if (info.source === 'runtime') source = 'changed';
      else if (info.source === 'declared') source = 'server config';
      else source = 'well-known';
      return {
        name,
        iri: isString(eff) ? eff : null,
        source,
        info,
        resettable: !info.locked && name in runtime,
        declared: isString(d) ? d : undefined,
        warning: warnings.get(name),
      };
    });
}

/**
 * Why a prefix name is refused, or null: a SPARQL `PN_PREFIX` in the ASCII subset the
 * server accepts, or the empty name.
 */
export function prefixNameError(name: string): string | null {
  if (name === '') return null;
  if (new TextEncoder().encode(name).length > 256) return 'A prefix name has at most 256 bytes.';
  if (!/^[A-Za-z]([A-Za-z0-9_.-]*[A-Za-z0-9_-])?$/.test(name))
    return 'A prefix name starts with a letter and holds letters, digits, "_", "-" and "." (not at the end).';
  return null;
}

/** Why a prefix IRI is refused, or null: it must be absolute and at most 4096 bytes. */
export function prefixIriError(iri: string): string | null {
  if (!iri) return 'Give the namespace IRI.';
  if (new TextEncoder().encode(iri).length > 4096) return 'An IRI has at most 4096 bytes.';
  if (!/^[A-Za-z][A-Za-z0-9+.-]*:[^\s<>"{}|^`\\]*$/.test(iri))
    return 'Give an absolute IRI, such as https://example.org/ns#, without spaces or <>.';
  return null;
}

/** The merge patch that binds `name` to `iri`. */
export const bindPatch = (name: string, iri: string): JsonObject => ({ [name]: iri });

/**
 * The merge patch that deletes a prefix. A `null` removes a runtime binding, and for a
 * prefix that the settings file declares or a well-known one, the server stores the
 * removal so the prefix stays gone until it is reset.
 */
export const deletePatch = (name: string): JsonObject => ({ [name]: null });

/** The field a reset of a prefix clears, as `DELETE ?field=` takes it. */
export const prefixField = (name: string) => pathString([name]);

/** Words for the source of a row, as its label's tooltip says it. */
export function prefixSourceTitle(r: PrefixRow): string {
  switch (r.source) {
    case 'well-known':
      return 'A prefix Sparkles knows without configuration.';
    case 'server config':
      return "The prefix comes from the server's settings file.";
    case 'locked':
      return "The operator locked this prefix in the server's settings file, so it cannot be changed here.";
    case 'removed':
      return r.declared
        ? `Removed here. The server's settings file binds it to ${r.declared}.`
        : 'A well-known prefix removed here.';
    case 'changed':
      return r.info.overrides !== undefined
        ? `Changed here, in place of the server config's ${String(r.info.overrides)}.`
        : 'Added or changed here, or bound by loaded data.';
  }
}
