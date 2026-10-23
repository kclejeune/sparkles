// Mock of the model configuration of a server run without `--model-config`, for UI
// development: `GET /$/models`, `GET /$/server/secrets` and `GET /$/server/settings/models`,
// in the shapes of docs/openapi.json (`ModelProviders`, `SecretList`, `ServerSettingsKind`).
// A real server answers the three even when nothing configures models, with no provider and
// no role list. The mock keeps no configuration, so a write is refused.

/** `GET /$/server/settings/models` of a server with the built-in defaults only. */
function modelsKind() {
  return {
    scope: 'server',
    kind: 'models',
    effective: { providers: {}, roles: {} },
    declared: {},
    runtime: {},
    sources: {},
    locked: [],
    overridden: [],
    overrides: [],
    status: { valid: true },
    etag: '"mock-models"',
  };
}

/** Handles the routes above; false when the request is not one of them. */
export function handleModels(req, res, seg, ctx) {
  const { send, fail } = ctx;
  if (seg[0] !== '$') return false;
  const path = seg.slice(1).join('/');
  if (path !== 'models' && path !== 'server/secrets' && path !== 'server/settings/models')
    return false;
  if (req.method !== 'GET') return (fail(res, 405, 'the mock keeps no model configuration'), true);
  if (path === 'models') send(res, 200, { configured: false, providers: [], roles: {} });
  else if (path === 'server/secrets')
    send(res, 200, { storage: 'plaintext', keySource: null, keyMemory: null, secrets: [] });
  else send(res, 200, modelsKind());
  return true;
}
