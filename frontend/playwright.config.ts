import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  timeout: 90_000,
  use: {
    baseURL: "http://127.0.0.1:8123",
    permissions: ["clipboard-read", "clipboard-write"],
    viewport: { width: 1440, height: 1000 },
    channel: "chrome",
  },
  webServer: {
    command:
      "rm -rf /tmp/lug-e2e && mkdir -p /tmp/lug-e2e && cp ../structured_ai_api.py ../platform_auth.py ../tests/e2e_server.py /tmp/lug-e2e/ && cp -R ../web /tmp/lug-e2e/ && cd /tmp/lug-e2e && python3 e2e_server.py",
    url: "http://127.0.0.1:8123/",
    reuseExistingServer: false,
    env: {
      HOST: "127.0.0.1",
      PORT: "8123",
      LUG_PUBLIC_URL: "http://127.0.0.1:8123",
      GITHUB_CLIENT_ID: "e2e-client",
      GITHUB_CLIENT_SECRET: "e2e-placeholder-not-a-secret",
      LUG_LEGACY_OWNER_GITHUB_ID: "",
    },
    stdout: "ignore",
  },
});
