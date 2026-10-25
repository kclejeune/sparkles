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
