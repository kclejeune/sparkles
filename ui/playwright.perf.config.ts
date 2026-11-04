import { defineConfig, devices } from '@playwright/test';

// The Workbench and Explore scale measurements (ROADMAP item 725), not a test suite: no CI
// task runs them. They need a server that `node scripts/perf-seed.mjs` started, whose URL
// is PERF_URL. PERF_OUT is the directory for the JSON of each run, PERF_LABEL names the
// build under test, PERF_REPEAT the repetitions (default 3). See tests/perf/README.txt.
export default defineConfig({
  testDir: 'tests/perf',
  testMatch: '**/*.perf.ts',
  workers: 1,
  fullyParallel: false,
  retries: 0,
  timeout: 20 * 60_000,
  reporter: 'list',
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 900 },
        // the full browser in headless mode: it has WebGL 2, in software on a host without a GPU
        channel: 'chromium',
        launchOptions: { args: ['--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] },
      },
    },
  ],
});
