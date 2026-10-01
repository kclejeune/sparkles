import { defineConfig, devices } from '@playwright/test';

// The README's screenshots (`mise run docs:screenshots`), not a test suite: `pnpm e2e` and
// `pnpm e2e:mock` do not run them. tests/screenshots/global-setup.ts serves the demo
// dataset of docs/demo with a release build; the spec writes docs/images/*.png.
export default defineConfig({
  testDir: 'tests/screenshots',
  globalSetup: './tests/screenshots/global-setup.ts',
  // one page at a time: the server and the browser are not shared with anything else, and
  // the images come out the same on every run
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 60_000,
  reporter: 'list',
  use: {
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
    colorScheme: 'dark',
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
        // the full browser in headless mode (not the headless shell): it has WebGL 2,
        // which the maps need
        channel: 'chromium',
      },
    },
  ],
});
