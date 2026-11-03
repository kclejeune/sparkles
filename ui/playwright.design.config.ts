import { defineConfig, devices } from '@playwright/test';

// Layout review shots (`pnpm design:capture`) and the GO label check (`pnpm design:check`)
// against the server of `node scripts/seed-design.mjs` (`pnpm design:seed`). They need the
// downloaded GO and UNESCO data, so no other config and no CI task runs tests/design.
export default defineConfig({
  testDir: 'tests/design',
  workers: 1,
  fullyParallel: false,
  retries: 0,
  reporter: 'list',
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], channel: 'chromium' } }],
});
