import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: process.env.CI ? "github" : "list",
  timeout: 60_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: process.env.RVP_E2E_BASE_URL ?? "http://127.0.0.1:4173",
    trace: "on-first-retry",
    // WebTransport + getUserMedia need a real browser context with
    // permission grants (handled in the voice spec).
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        // Allow mic access so the voice pipeline can start capture.
        permissions: ["microphone"],
      },
    },
  ],
  webServer: process.env.RVP_E2E_BASE_URL
    ? undefined
    : {
        // CI already runs `npm run build` as its own step — rebuilding here
        // doubled the work and blew the 120s webServer timeout on cold
        // runners. `--host 127.0.0.1` pins IPv4 so the Playwright url check
        // (http://127.0.0.1:4173) can't hang on an IPv6-only localhost bind.
        command: process.env.CI
          ? "npm run preview -- --host 127.0.0.1 --port 4173 --strictPort"
          : "npm run build && npm run preview -- --host 127.0.0.1 --port 4173 --strictPort",
        url: "http://127.0.0.1:4173",
        reuseExistingServer: !process.env.CI,
        timeout: 120_000,
      },
});
