// The dataset and credentials of the end-to-end tests (see global-setup.ts).

export const DATASET = 'e2e';
export const USER = 'alice';
export const PASSWORD = 'correct horse battery staple';

export const EX = 'http://example.org/e2e/';

/** The dataset the server without auth is started with (`--mem`), so it is declared. */
export const DECLARED_DATASET = 'declared-e2e';

/** A dataset the settings tests create; the settings file declares values for it. */
export const SETTINGS_DATASET = 'settings-e2e';

/** The settings file of the server without auth (`--settings`). */
export const SETTINGS = {
  datasets: {
    [SETTINGS_DATASET]: {
      assistant: { historyDays: 30, send: 'schema' },
      memory: {
        agentGraphs: ['urn:x-sparkles:e2e/agents/*'],
        consolidatedGraph: 'urn:x-sparkles:e2e/consolidated',
      },
      locked: ['assistant.send'],
    },
  },
};

const PREFIXES = `@prefix ex: <${EX}> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix spk: <urn:x-sparkles:> .
`;

/** Loaded in two requests, so the history shows two commits. */
export const DATA = [
  `${PREFIXES}
ex:Person a rdfs:Class ; rdfs:label "Person" .

ex:ada a ex:Person ;
  rdfs:label "Ada Lovelace" ;
  rdfs:comment "Wrote the first program for the analytical engine" ;
  ex:knows ex:grace ;
  ex:embedding "[1.0, 0.0, 0.0]"^^spk:vector .

ex:grace a ex:Person ;
  rdfs:label "Grace Hopper" ;
  rdfs:comment "Built the first compiler and found a moth in the relay" ;
  ex:embedding "[0.9, 0.1, 0.0]"^^spk:vector .
`,
  `${PREFIXES}
ex:alan a ex:Person ;
  rdfs:label "Alan Turing" ;
  rdfs:comment "Asked whether machines can think" ;
  ex:embedding "[0.0, 1.0, 0.0]"^^spk:vector .

ex:edsger a ex:Person ;
  rdfs:label "Edsger Dijkstra" ;
  rdfs:comment "Found the shortest path and considered goto harmful" ;
  ex:embedding "[0.0, 0.0, 1.0]"^^spk:vector .
`,
];
