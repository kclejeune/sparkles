import type { components } from '../client/src/openapi.js';

// Compile the generated explanation contract as a normal API consumer does.
export function explanationDetails(answer: components['schemas']['Explanation']) {
  const missing = answer.hypotheses?.flatMap((hypothesis) =>
    hypothesis.missing.map((triple) => triple.s.value),
  );
  const blockers = answer.rules?.flatMap((candidate) =>
    candidate.blockedBy?.map((blocker) => blocker.call),
  );
  const reads = answer.trees?.map((tree) => tree.read?.source);
  return { missing, blockers, reads };
}

// These values remain typed through generated tuples, supports and preview reports.
export function entailmentDetails(
  diff: components['schemas']['EntailmentDiff'],
  supports: components['schemas']['JustificationReport'],
  preview: components['schemas']['DryRunReport'],
  request: components['schemas']['InferenceExplainRequest'],
) {
  const subjects = diff.sample.added.map(([subject]) => subject.type);
  const roles = supports.results.flatMap((result) =>
    result.justifications.flatMap((support) => support.facts.map((fact) => fact.role)),
  );
  return { subjects, roles, changes: preview.entailments?.added, triple: request.triple };
}
