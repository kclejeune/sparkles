import { defineConfig, devices } from '@playwright/test';

// The README's screenshots of branches and of the agent pages (`mise run docs:screenshots`),
// not a test suite. The pages run against the mock backend, whose `org` dataset has agent
// memory and whose assistant (mock/assistant.mjs) scripts the ask pipeline without a model.
// It starts mock/server.mjs and `vite dev` on free ports, as playwright.mock.config.ts does,
// and the spec in tests/screenshots-mock writes docs/images/*.png in the light theme.
const mockPort = (process.env.MOCK_SCREENSHOTS_PORT ??= String(
  20000 + Math.floor(Math.random() * 20000),
));
const uiPort = String(Number(mockPort) + 1);

export default defineConfig({
  testDir: 'tests/screenshots-mock',
  // one page at a time, in order: the mock keeps its state across tests
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 90_000,
  reporter: 'list',
  use: {
    baseURL: `http://127.0.0.1:${uiPort}`,
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
    colorScheme: 'light',
    locale: 'en-GB',
    timezoneId: 'UTC',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 900 },
        deviceScaleFactor: 2,
        channel: 'chromium',
      },
    },
  ],
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
      env: { SPARKLES_API: `http://127.0.0.1:${mockPort}`, VITE_FMT_WASM: 'off' },
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
});
