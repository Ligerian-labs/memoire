import { defineConfig, devices } from '@playwright/test';

// Started by tests/e2e/run.mjs, which brings up a real gateway first.
const baseURL = process.env['MEMOIRE_E2E_BASE_URL'] ?? 'http://localhost:28981';

export default defineConfig({
  testDir: './tests/e2e',
  testMatch: '**/*.e2e.ts',
  outputDir: 'test-results/playwright',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 120_000,
  expect: { timeout: 15_000 },
  reporter: [['list']],
  use: {
    ...devices['Desktop Chrome'],
    baseURL,
    viewport: { width: 1440, height: 900 },
    // Traces would hold the session cookie and the minted key; keep failure screenshots only.
    trace: 'off',
    video: 'off',
    screenshot: 'only-on-failure',
  },
});
