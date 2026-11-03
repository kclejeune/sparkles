// A stand-in model provider for asking questions of a real server without a paid model
// (`SPARKLES_SEED_ASK=1 node scripts/seed-design.mjs`). It speaks the OpenAI chat
// completions protocol on 127.0.0.1 and answers with fixed JSON: a draft lists the demo
// dataset's people with their labels, and a summary cites the first two rows. Nothing
// leaves the machine and no key is used.

import { createServer } from 'node:http';

const DRAFT = {
  query:
    'PREFIX ex: <http://example.org/ns#>\nPREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\nSELECT ?person ?name WHERE { ?person a ex:Person ; rdfs:label ?name } ORDER BY ?name LIMIT 20',
  explanation: 'Lists the people of the network with their names.',
  assumptions: ['"people" are the instances of ex:Person'],
  clarify: { question: '', choices: [] },
  graph: { subject: '', predicate: '', object: '' },
};

const SUMMARY = {
  text: 'The network lists its people by name, such as the first [1] and the second [2].',
  citations: [1, 2],
};

const reply = (text) =>
  JSON.stringify({
    id: 'stub',
    object: 'chat.completion',
    choices: [{ index: 0, message: { role: 'assistant', content: text }, finish_reason: 'stop' }],
    usage: { prompt_tokens: 10, completion_tokens: 5 },
  });

/** Starts the stub on a free port; resolves with its base URL and a close function. */
export function startAskStub() {
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      if (req.method === 'GET' && req.url?.endsWith('/models')) {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ object: 'list', data: [{ id: 'stub', object: 'model' }] }));
        return;
      }
      const text = body.includes('rows of a query result')
        ? JSON.stringify(SUMMARY)
        : JSON.stringify(DRAFT);
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(reply(text));
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      resolve({ url: `http://127.0.0.1:${port}/v1`, close: () => server.close() });
    });
  });
}

/** The model configuration that points the draft and summarize roles at the stub. */
export function stubModelConfig(url) {
  const pair = { provider: 'stub', model: 'stub' };
  return {
    providers: {
      stub: {
        kind: 'openai',
        endpoint: url,
        requestTimeoutSecs: 10,
        allowedModels: ['stub'],
        structuredOutput: 'json-schema',
      },
    },
    roles: { draft: [pair], repair: [pair], summarize: [pair] },
  };
}
