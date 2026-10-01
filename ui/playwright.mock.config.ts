import { defineConfig, devices } from '@playwright/test';

// UI tests against the mock backend (`pnpm e2e:mock`) for pages whose server side is not
// built yet (backup repositories). It starts mock/server.mjs and `vite dev` on free ports
// (MOCK_E2E_PORT and MOCK_E2E_PORT + 1, random unless set). The mock keeps state across
// tests, so they run in one worker, in order.
const mockPort = (process.env.MOCK_E2E_PORT ??= String(20000 + Math.floor(Math.random() * 20000)));
const uiPort = String(Number(mockPort) + 1);

export default defineConfig({
  testDir: 'tests/mock',
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: `http://127.0.0.1:${uiPort}`,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: [
    {
      command: 'node mock/server.mjs',
      url: `http://127.0.0.1:${mockPort}/$/ping`,
      env: { PORT: mockPort },
      reuseExistingServer: false,
    },
    {
      command: `pnpm exec vite dev --host 127.0.0.1 --port ${uiPort} --strictPort`,
      url: `http://127.0.0.1:${uiPort}/ui/`,
      // the mock's stand-in formatter, not the browser formatter of `mise run ui:wasm`
      env: { SPARKLES_API: `http://127.0.0.1:${mockPort}`, VITE_FMT_WASM: 'off' },
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
});
