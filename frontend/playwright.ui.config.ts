import { defineConfig } from "@playwright/test";

// Frontend-only checks: platform responses are mocked in the tests, never
// production credentials. Run against the actual production build.
export default defineConfig({
  testDir: "./e2e",
  testMatch: ["branding.spec.ts", "lab530-style.spec.ts"],
  outputDir: "./test-results/ui",
  timeout: 60_000,
  workers: 2,
  use: {
    baseURL: "http://127.0.0.1:4174",
    channel: "chrome",
    permissions: ["clipboard-read", "clipboard-write"],
    viewport: { width: 1440, height: 1000 },
  },
  projects: [
    { name: "light", use: { colorScheme: "light" } },
    { name: "dark", use: { colorScheme: "dark" } },
  ],
  webServer: {
    command:
      "npm exec vite preview -- --host 127.0.0.1 --port 4174 --strictPort",
    url: "http://127.0.0.1:4174",
    reuseExistingServer: false,
  },
});
