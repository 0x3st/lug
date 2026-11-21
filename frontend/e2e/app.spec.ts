import { test, expect, type Page } from "@playwright/test";

const BASE = "http://127.0.0.1:8123";
const PAGES = ["/", "/platform", "/docs"];

async function signInWithGitHub(page: Page, uid: string) {
  // Fetch our real start handler without following GitHub; preserve its state cookie.
  // Playwright cannot intercept a later hop of the same HTTP redirect chain.
  const provider = "**/f/a/o/platform/auth/github";
  await page.route(provider, async (route) => {
    const response = await route.fetch({ maxRedirects: 0 });
    expect(response.status()).toBe(302);
    const authorization = new URL(response.headers().location);
    expect(authorization.origin).toBe("https://github.com");
    expect(authorization.searchParams.has("scope")).toBe(false);
    const state = authorization.searchParams.get("state")!;
    await route.fulfill({
      response,
      headers: {
        ...response.headers(),
        location: `${BASE}/f/a/o/platform/auth/github/callback?state=${encodeURIComponent(state)}&code=${uid}`,
      },
    });
  });
  await page
    .getByRole("button", { name: /使用 GitHub 继续|Continue with GitHub/ })
    .click();
  await expect(page.locator("#overview")).toBeVisible();
  await page.unroute(provider);
}

async function expectNoOverflow(page: Page, label: string) {
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
    `${label} overflows`,
  ).toBe(true);
}

async function screenshot(page: Page, name: string) {
  await page.screenshot({ path: `/tmp/lug-ui-qa/${name}.png`, fullPage: true });
}

test.describe("Lug React frontend", () => {
  let page: Page;

  test.beforeEach(async ({ browser }, testInfo) => {
    page = await browser.newPage();
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    (page as unknown as { _errors: string[] })._errors = errors;
    if (!testInfo.title.includes("default language")) {
      await page.addInitScript(() => {
        if (!localStorage.getItem("lug_lang"))
          localStorage.setItem("lug_lang", "zh");
      });
    }
  });

  test.afterEach(async () => {
    const errors = (page as unknown as { _errors?: string[] })._errors ?? [];
    expect(errors, `browser errors: ${errors.join(" | ")}`).toEqual([]);
    await page.close();
  });

  test("pages render at desktop and mobile sizes without overflow", async () => {
    for (const width of [1440, 768, 390, 320]) {
      await page.setViewportSize({ width, height: 1000 });
      for (const path of PAGES) {
        await page.goto(BASE + path);
        await expect(page.locator("h1:visible")).toHaveCount(1);
        await expectNoOverflow(page, `${width} ${path}`);
      }
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.goto(BASE + "/");
    await screenshot(page, "react-home-1440");
    await page.goto(BASE + "/docs");
    await screenshot(page, "react-docs-1440");
    await page.setViewportSize({ width: 390, height: 1000 });
    await page.goto(BASE + "/");
    await screenshot(page, "react-home-390");
    await page.setViewportSize({ width: 1440, height: 1000 });
  });

  test("default language follows navigator.language; toggle switches and persists", async () => {
    await page.goto(BASE + "/");
    await expect(page.locator("h1")).toContainText("Introducing");
    await page.getByRole("button", { name: "切换到中文" }).click();
    await expect(page.locator("h1")).toContainText("正式登场");
    await page.reload();
    await expect(page.locator("h1")).toContainText("正式登场");
    await page.getByRole("button", { name: "Switch to English" }).click();
    await expect(page.locator("h1")).toContainText("Introducing");
  });

  test("home copy button copies curl command; docs copy copies code", async () => {
    await page.goto(BASE + "/");
    await page.getByRole("button", { name: "复制" }).click();
    await expect
      .poll(() => page.evaluate(() => navigator.clipboard.readText()))
      .toContain("https://lug.by530.com/f/a/o/calculate");
    await page.goto(BASE + "/docs");
    await expect(
      page.locator('nav[aria-label="文档目录"] a[href^="#"]'),
    ).toHaveCount(8);
    await page.locator("#calculate").scrollIntoViewIfNeeded();
    await page
      .locator("#calculate")
      .getByRole("button", { name: "复制", exact: true })
      .first()
      .click();
    await expect
      .poll(() => page.evaluate(() => navigator.clipboard.readText()))
      .toContain("$LUG_API_KEY");
  });

  test("platform: login, create, use, revoke, session expiry", async ({
    request,
  }) => {
    await page.goto(BASE + "/platform");
    await expect(page.locator('input[type="password"]')).toHaveCount(0);
    await signInWithGitHub(page, "101");
    await expect(page.locator("#overview")).toBeVisible();
    await expect(page.locator("text=还没有 API Key")).toBeVisible();
    // XSS-safe name
    const name = "<img src=x onerror=alert(1)> test";
    await page.locator("#keyName").fill(name);
    await page.getByRole("button", { name: "创建密钥" }).click();
    const dialog = page.locator("dialog[open]");
    await expect(dialog).toBeVisible();
    const key = (await dialog.locator(".select-all").textContent()) || "";
    expect(key.startsWith("sk-")).toBe(true);
    await expect(dialog).toContainText(name);
    expect(await page.locator("tbody img").count()).toBe(0);
    // copy + close
    await page.getByRole("button", { name: "复制", exact: true }).click();
    await expect
      .poll(() => page.evaluate(() => navigator.clipboard.readText()))
      .toBe(key);
    await page.getByRole("button", { name: "我已保存" }).click();
    await expect(page.locator("dialog[open]")).toHaveCount(0);
    // use the key: JSON + SSE
    let r = await request.post(`${BASE}/f/a/o/calculate`, {
      headers: { Authorization: `Bearer ${key}` },
      data: { expression: "3+5*2**(1+1)" },
    });
    expect((await r.json()).result).toBe(23);
    r = await request.post(`${BASE}/f/a/o/calculate`, {
      headers: { Authorization: `Bearer ${key}` },
      data: { expression: "(3+5)*2", process: true },
    });
    expect(await r.text()).toContain("data: [DONE]");
    // usage reflects the two calls
    await page.getByRole("button", { name: "刷新数据" }).click();
    await expect(page.locator("text=更新于")).toBeVisible();
    await expect(page.locator(".tabular-nums").first()).toContainText("2");
    // revoke: cancel keeps, failure shows error, success revokes
    await page.getByRole("button", { name: `撤销密钥 ${name}` }).click();
    await page.getByRole("button", { name: "取消" }).click();
    await expect(page.getByText("使用中")).toHaveCount(1);
    await page.getByRole("button", { name: `撤销密钥 ${name}` }).click();
    await page.route("**/f/a/o/platform/keys/*", (route) =>
      route.fulfill({
        status: 500,
        contentType: "application/json",
        body: JSON.stringify({ error: { message: "test failure" } }),
      }),
    );
    await page.getByRole("button", { name: "确认撤销" }).click();
    await expect(page.locator('div[role="alert"]').last()).toContainText(
      "test failure",
    );
    await page.unroute("**/f/a/o/platform/keys/*");
    await page.getByRole("button", { name: "确认撤销" }).click();
    await expect(page.getByText("已撤销")).toBeVisible();
    // revoked key is rejected
    r = await request.post(`${BASE}/f/a/o/calculate`, {
      headers: { Authorization: `Bearer ${key}` },
      data: { expression: "1+1" },
    });
    expect(r.status()).toBe(401);
    // session expiry returns to login
    await page.context().clearCookies();
    await page.getByRole("button", { name: "刷新数据" }).click();
    await expect(page.locator("#login-title")).toBeVisible();
    await expect(page.locator("#login-err")).toContainText("会话已过期");
    await expect
      .poll(() => page.evaluate(() => localStorage.getItem("lug_session")))
      .toBe(null);
  });

  test("headline typography and bilingual responsive layouts", async () => {
    for (const language of ["zh", "en"]) {
      for (const width of [1440, 768, 390, 320]) {
        await page.setViewportSize({ width, height: 1000 });
        for (const path of PAGES) {
          await page.goto(BASE + path);
          await expect(page.locator("h1")).toBeVisible();
          await page.evaluate((value) => {
            // Use the language switch so this verifies the rendered state too.
            if (document.documentElement.lang.startsWith(value)) return;
            document
              .querySelector<HTMLButtonElement>(".language-toggle")
              ?.click();
          }, language);
          await expect(page.locator("html")).toHaveAttribute(
            "lang",
            language === "zh" ? "zh-CN" : "en",
          );
          await page.evaluate(() => document.fonts.ready);
          if (path === "/") {
            await expect(page.locator("h1")).toHaveText(
              language === "zh" ? "Lug，正式登场。" : "Introducing Lug.",
            );
          }
          await expect(page.locator("body")).not.toContainText(
            new RegExp(
              [
                "少一点",
                "多一点创造",
                "表达式进，答案出",
                "清晰输入，确定结果",
                "计算交给 Lug",
                "下一次计算，从这里开始",
                "为开发者而生",
                "Structured input. Clear results.",
                "Less friction.",
                "More creation.",
                "LESS INTERFACE. MORE CLARITY.",
                "Leave the math to Lug.",
                "Your next computation starts here.",
                "YOUR NEXT BUILD STARTS HERE",
              ].join("|"),
            ),
          );
          await expectNoOverflow(page, `${language} ${width} ${path}`);
          const headingSize = await page
            .locator("h1")
            .evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
          expect(
            headingSize,
            `${language} ${width} ${path} headline must not fall back to body size`,
          ).toBeGreaterThanOrEqual(path === "/" ? 36 : 28);
        }
      }
    }
  });

  test("demo examples match the engine in result and process modes", async ({
    request,
  }) => {
    await page.goto(BASE + "/platform");
    await signInWithGitHub(page, "102");
    const client = page.context().request;
    const session = await (
      await client.get(`${BASE}/f/a/o/platform/me`)
    ).json();
    const creation = await client.post(`${BASE}/f/a/o/platform/keys`, {
      headers: { "X-CSRF-Token": session.csrf_token },
      data: { name: "demo-verification" },
    });
    expect(creation.ok()).toBe(true);
    const key = await creation.json();
    await page.goto(BASE + "/");
    try {
      const expressions = ["3+5*2**(1+1)", "128/(2+6)+3**2", "(18+6)/3"];
      for (const [index, expression] of expressions.entries()) {
        await page
          .getByRole("button", { name: `示例 ${index + 1}`, exact: true })
          .click();
        await page.getByRole("button", { name: "结果", exact: true }).click();
        const response = await request.post(`${BASE}/f/a/o/calculate`, {
          headers: { Authorization: `Bearer ${key.key}` },
          data: { expression },
        });
        const answer = await response.json();
        await expect(page.locator(".demo-answer")).toHaveText(
          String(answer.result),
        );
        await expect(page.locator(".token-count")).toContainText(
          `${answer.usage.total_tokens} tokens`,
        );
        await page.getByRole("button", { name: "过程", exact: true }).click();
        const stream = await request.post(`${BASE}/f/a/o/calculate`, {
          headers: { Authorization: `Bearer ${key.key}` },
          data: { expression, process: true },
        });
        const events = (await stream.text())
          .split("\n")
          .filter((line) => line.startsWith("data: {"))
          .map((line) => JSON.parse(line.slice(6)));
        const final = events.find((event) => event.type === "final");
        await expect(page.locator(".trace-steps li")).toHaveCount(
          final.steps_count,
        );
        await expect(page.locator(".token-count")).toContainText(
          `${final.usage.total_tokens} tokens`,
        );
        await expect(page.locator(".trace-prefix code")).toHaveText(
          events.find((event) => event.type === "prefix").prefix,
        );
      }
    } finally {
      await client.delete(`${BASE}/f/a/o/platform/keys/${key.id}`, {
        headers: { "X-CSRF-Token": session.csrf_token },
      });
    }
  });

  test("cookie session survives reload and sign-out revokes it on the server", async ({
    request,
  }) => {
    await page.goto(BASE + "/platform");
    await signInWithGitHub(page, "103");
    const cookie = (await page.context().cookies()).find(
      (item) => item.name === "lug_session",
    )!;
    expect(cookie.httpOnly).toBe(true);
    expect(cookie.sameSite).toBe("Lax");
    expect(await page.evaluate(() => localStorage.getItem("lug_session"))).toBe(
      null,
    );
    expect(await page.evaluate(() => document.cookie)).not.toContain(
      "lug_session",
    );
    await page.reload();
    await expect(page.locator("#overview")).toBeVisible();
    await page.getByRole("button", { name: "退出登录" }).click();
    await expect(page.locator("#login-title")).toBeVisible();
    const response = await request.get(`${BASE}/f/a/o/platform/me`, {
      headers: { "X-Session-Token": cookie.value },
    });
    expect(response.status()).toBe(401);
    await page.reload();
    await expect(page.locator("#login-title")).toBeVisible();
  });

  test("GitHub login availability, redirect and cancellation feedback", async () => {
    await page.route("**/f/a/o/platform/auth", (route) =>
      route.fulfill({ json: { github_enabled: false } }),
    );
    await page.goto(BASE + "/platform");
    await expect(
      page.getByRole("button", { name: "使用 GitHub 继续" }),
    ).toBeDisabled();
    await page.route("**/f/a/o/platform/auth", (route) =>
      route.fulfill({
        json: { github_enabled: true },
      }),
    );
    // Mock only the provider redirect; no GitHub credentials/network in UI tests.
    await page.route("**/f/a/o/platform/auth/github", (route) =>
      route.fulfill({
        status: 302,
        headers: { Location: "/platform?login_error=github_cancelled" },
      }),
    );
    await page.reload();
    await expect(page.locator("#pwd")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "使用 GitHub 继续" }),
    ).toBeEnabled();
    await page.getByRole("button", { name: "使用 GitHub 继续" }).click();
    await expect(page.locator("#login-err")).toContainText("GitHub 授权已取消");
    await expect(page).toHaveURL(BASE + "/platform");
    await expect(
      page.getByRole("button", { name: "使用 GitHub 继续" }),
    ).toBeEnabled();
    await page.getByRole("button", { name: "Switch to English" }).click();
    await expect(page.locator("#login-err")).toContainText(
      "GitHub authorization was cancelled",
    );
  });

  test("english console renders key management strings", async () => {
    await page.goto(BASE + "/platform");
    await page.getByRole("button", { name: "Switch to English" }).click();
    await expect(page.locator("#login-title")).toContainText(
      "Sign in to the console",
    );
    await signInWithGitHub(page, "104");
    await expect(page.locator("h1")).toContainText("Overview & keys");
    await expect(
      page.getByRole("button", { name: "Create key" }),
    ).toBeVisible();
  });
});
