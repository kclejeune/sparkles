// Materialized entailments are a recorded profile's facts, not classifier proofs.
import type { Selection } from './ontology';

export const INFERRED_GRAPH = 'urn:x-sparkles:inferred';
export type MaterializationSource = {
  kind: 'materialization';
  sourceId: string;
  profile: 'rdfs' | 'owl-rl';
  producerCommit: number;
  commit: number;
  boundStoreId: string;
  familyId: string;
  inputGraphs: string[];
  inputGraphCount: number;
  inputGraphsTruncated: boolean;
  warnings: string[];
  warningCount: number;
  warningsTruncated: boolean;
  complete: null;
  consistency: 'unknown';
  justifications: 'unavailable';
};
export type InferredView = {
  target: string;
  selection: Selection;
  commit: number;
  languages: string[];
};
export function captureView(view: InferredView): InferredView {
  if (!Number.isSafeInteger(view.commit) || view.commit < 0)
    throw new Error('The displayed revision cannot be read exactly.');
  return { ...view, selection: { ...view.selection }, languages: [...view.languages] };
}
export function sameView(a: InferredView, b: InferredView) {
  return (
    a.target === b.target &&
    a.commit === b.commit &&
    a.selection.graph === b.selection.graph &&
    a.selection.imports === b.selection.imports &&
    a.languages.join('\u0000') === b.languages.join('\u0000')
  );
}
export function validateMaterialization(
  source: MaterializationSource | undefined,
  view: InferredView,
  pin?: string,
): MaterializationSource {
  if (
    !source ||
    source.kind !== 'materialization' ||
    !/^sha256:[a-f0-9]{64}$/.test(source.sourceId) ||
    (pin != null && source.sourceId !== pin) ||
    source.commit !== view.commit ||
    !Number.isSafeInteger(source.producerCommit) ||
    source.producerCommit < 0 ||
    source.producerCommit > view.commit ||
    !['rdfs', 'owl-rl'].includes(source.profile) ||
    !source.boundStoreId ||
    !source.familyId ||
    source.complete !== null ||
    source.consistency !== 'unknown' ||
    source.justifications !== 'unavailable' ||
    !Array.isArray(source.inputGraphs) ||
    source.inputGraphs.length > 64 ||
    source.inputGraphs.some((g) => typeof g !== 'string') ||
    !Number.isSafeInteger(source.inputGraphCount) ||
    source.inputGraphCount < source.inputGraphs.length ||
    source.inputGraphsTruncated !== source.inputGraphCount > source.inputGraphs.length ||
    !Array.isArray(source.warnings) ||
    source.warnings.length > 100 ||
    source.warnings.some((w) => typeof w !== 'string') ||
    !Number.isSafeInteger(source.warningCount) ||
    source.warningCount < source.warnings.length ||
    source.warningsTruncated !== source.warningCount > source.warnings.length
  ) {
    throw new Error('The materialized source changed or could not be verified. Read it again.');
  }
  return source;
}

/** Shared by visible regions at one revision. A replacement requires a new view. */
export class InferredPins {
  constructor(readonly context: string) {}
  materializationId: string | undefined;
  classificationId: string | undefined;
  acceptMaterialization(source: MaterializationSource) {
    if (this.materializationId && this.materializationId !== source.sourceId) return false;
    this.materializationId = source.sourceId;
    return true;
  }
  acceptClassification(resultId: string) {
    if (this.classificationId && this.classificationId !== resultId) return false;
    this.classificationId = resultId;
    return true;
  }
}
