import { defineConfig, devices } from '@playwright/test';

// End-to-end smoke tests of the UI against a real `sparkles serve` (`pnpm e2e`, or
// `mise run ui:e2e`, which builds the UI and the server first). tests/e2e/global-setup.ts
// starts the server on a free port with a temporary data directory and stops it afterwards.
export default defineConfig({
  testDir: 'tests/e2e',
  globalSetup: './tests/e2e/global-setup.ts',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        // the full browser in headless mode (not the headless shell): it has WebGL 2,
        // which the maps need
        channel: 'chromium',
      },
    },
  ],
});
