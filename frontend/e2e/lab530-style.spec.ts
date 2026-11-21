import { test, expect, type Page } from "@playwright/test";
import { readFile, readdir } from "node:fs/promises";

async function signedOut(page: Page) {
  await page.route("**/f/a/o/platform/me", (route) =>
    route.fulfill({ status: 401, json: {} }),
  );
  await page.route("**/f/a/o/platform/auth", (route) =>
    route.fulfill({ json: { github_enabled: true } }),
  );
}

async function noOverflow(page: Page) {
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
}

async function noDecorativeBorders(page: Page, selector: string) {
  const elements = page.locator(selector);
  expect(await elements.count()).toBeGreaterThan(0);
  expect(
    await elements.evaluateAll((nodes) =>
      nodes
        .filter((el) => {
          const style = getComputedStyle(el);
          return [
            style.borderTopWidth,
            style.borderRightWidth,
            style.borderBottomWidth,
            style.borderLeftWidth,
          ].some((width) => parseFloat(width) > 0);
        })
        .map((el) => el.className),
    ),
  ).toEqual([]);
}

// Text contrast against the closest opaque surface, including nested cards.
async function readable(page: Page, selector: string) {
  const ratio = await page
    .locator(selector)
    .first()
    .evaluate((el) => {
      const channels = (color: string) =>
        (color.match(/[\d.]+/g) ?? []).map(Number);
      const luminance = (rgb: number[]) =>
        rgb
          .slice(0, 3)
          .map((v) => {
            v /= 255;
            return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
          })
          .reduce((sum, v, i) => sum + v * [0.2126, 0.7152, 0.0722][i], 0);
      let current: Element | null = el;
      let background = [255, 255, 255];
      while (current) {
        const value = channels(getComputedStyle(current).backgroundColor);
        if (value.length === 3 || value[3] === 1) {
          background = value;
          break;
        }
        current = current.parentElement;
      }
      const fg = luminance(channels(getComputedStyle(el).color));
      const bg = luminance(background);
      return (Math.max(fg, bg) + 0.05) / (Math.min(fg, bg) + 0.05);
    });
  expect(ratio, `${selector} text contrast`).toBeGreaterThanOrEqual(4.5);
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("lug_lang", "en"));
  await signedOut(page);
});

test("STIX, semantic colors, responsive app pages and keyboard focus", async ({
  page,
}, info) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const dark = info.project.name === "dark";
  for (const viewport of [
    { width: 1440, height: 1000 },
    { width: 1024, height: 600 },
    { width: 390, height: 844 },
    { width: 320, height: 568 },
  ]) {
    await page.setViewportSize(viewport);
    for (const path of ["/", "/docs", "/platform"]) {
      await page.goto(path);
      await expect(page.locator("h1")).toBeVisible();
      expect(
        await page.evaluate(async () => {
          const loaded = await document.fonts.load(
            '400 48px "STIX Two Text"',
            "Lug",
          );
          return (
            loaded.length > 0 &&
            loaded.every((face) => face.status === "loaded")
          );
        }),
      ).toBe(true);
      await expect(page.locator("body")).toHaveCSS(
        "background-color",
        dark ? "rgb(17, 20, 24)" : "rgb(250, 249, 246)",
      );
      await expect(page.locator("h1")).toHaveCSS(
        "color",
        dark ? "rgb(242, 240, 233)" : "rgb(3, 25, 49)",
      );
      await expect(page.locator("h1")).toHaveCSS("font-weight", "400");
      const typography = await page.locator("h1").evaluate((el) => {
        const style = getComputedStyle(el);
        return {
          family: style.fontFamily,
          size: parseFloat(style.fontSize),
          tracking:
            parseFloat(style.letterSpacing) / parseFloat(style.fontSize),
        };
      });
      expect(typography.family).toContain("STIX Two Text");
      expect(typography.size).toBeGreaterThanOrEqual(36);
      expect(typography.size).toBeLessThanOrEqual(48);
      expect(typography.tracking).toBeCloseTo(0.055, 3);
      await readable(page, "h1");
      await readable(
        page,
        path === "/"
          ? ".hero-description"
          : path === "/docs"
            ? "#introduction > p"
            : ".login-story > p",
      );
      await noOverflow(page);
      await page.keyboard.press("Tab");
      expect(await page.locator(":focus-visible").count()).toBe(1);
      await expect(page.locator(":focus-visible")).not.toHaveCSS(
        "outline-style",
        "none",
      );
      if (path !== "/") {
        await page.screenshot({
          path: info.outputPath(`${path.slice(1)}-${viewport.width}.png`),
        });
      }
    }
  }
  expect(errors).toEqual([]);
});

test("grids align content without decorative rules; functional lines remain", async ({
  page,
}) => {
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/");
    await page.getByRole("button", { name: "Process", exact: true }).click();
    await noDecorativeBorders(
      page,
      ".demo-window, .demo-toolbar, .demo-footer, .expression-node > code, .trace-panel, .trace-steps, .trace-steps li, .feature-grid, .feature, .integration-code, .integration-code-header, .integration-response, .site-header, .site-footer, .footer-bottom",
    );
    await expect(page.locator(".feature").first()).toHaveCSS("display", "grid");
    await expect(page.locator(".demo-canvas")).toHaveCSS("display", "grid");
    await expect(
      page.getByRole("button", { name: "Process", exact: true }),
    ).toHaveCSS("border-bottom-width", "1px");
    await expect(page.locator(".footer-brand-divider")).toHaveCSS(
      "width",
      "1px",
    );
    await noOverflow(page);
    await page.goto("/docs");
    await noDecorativeBorders(
      page,
      ".app-header, .docs-sidebar, .docs-content section, .doc-code, .doc-code-header",
    );
    await expect(page.locator(".doc-table-wrap th").first()).toHaveCSS(
      "border-bottom-width",
      "1px",
    );
    await noOverflow(page);
  }
});

test("system theme switches live, including wordmark and favicon", async ({
  page,
}) => {
  for (const path of ["/", "/docs", "/platform"]) {
    await page.goto(path);
    for (const colorScheme of ["light", "dark", "light"] as const) {
      await page.emulateMedia({ colorScheme });
      await expect(page.locator("body")).toHaveCSS(
        "background-color",
        colorScheme === "dark" ? "rgb(17, 20, 24)" : "rgb(250, 249, 246)",
      );
      await expect(page.locator("header .logo-name")).toHaveCSS(
        "color",
        colorScheme === "dark" ? "rgb(242, 240, 233)" : "rgb(3, 25, 49)",
      );
      await expect(
        page.locator("header .logo svg, header .logo img, .brand-mark"),
      ).toHaveCount(0);
      if (path === "/") {
        await expect(page.locator(".footer-crest")).toHaveCSS(
          "filter",
          colorScheme === "dark" ? "brightness(0) invert(1)" : "none",
        );
        await expect(page.locator(".engine-wordmark")).toHaveText("Lug");
        await expect(page.locator(".engine-wordmark")).toHaveCSS(
          "font-family",
          /STIX Two Text/,
        );
        await expect(page.locator(".engine-wordmark")).toHaveCSS(
          "color",
          colorScheme === "dark" ? "rgb(242, 240, 233)" : "rgb(3, 25, 49)",
        );
      } else {
        await expect(page.locator(".footer-crest")).toHaveCount(0);
      }
    }
    await expect(
      page.locator(
        'meta[name="theme-color"][media="(prefers-color-scheme: light)"]',
      ),
    ).toHaveAttribute("content", "#faf9f6");
    await expect(
      page.locator(
        'meta[name="theme-color"][media="(prefers-color-scheme: dark)"]',
      ),
    ).toHaveAttribute("content", "#111418");
  }
  const favicon = await page.locator('link[rel="icon"]').getAttribute("href");
  expect(favicon).toMatch(/^\/assets\/favicon-.*\.svg$/);
  await page.goto(favicon!);
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await expect(page.locator(".wordmark")).toHaveText("Lug");
    await expect(page.locator("rect, .tile")).toHaveCount(0);
    await expect(page.locator(".wordmark")).toHaveCSS(
      "fill",
      colorScheme === "dark" ? "rgb(242, 240, 233)" : "rgb(3, 25, 49)",
    );
  }
});

test("production serves a real local font, license and no debug resources", async ({
  page,
  request,
}) => {
  await page.goto("/");
  const font = await page
    .locator('link[rel="preload"][as="font"]')
    .getAttribute("href");
  expect(font).toMatch(/^\/assets\/stix-two-text-.*\.woff2$/);
  const response = await request.get(font!);
  expect(response.ok()).toBe(true);
  expect((await response.body()).subarray(0, 4).toString()).toBe("wOF2");
  const assets = await readdir(new URL("../../web/assets/", import.meta.url));
  expect(assets.some((name) => /stix-two-text.*\.woff2/.test(name))).toBe(true);
  expect(
    assets.filter((name) =>
      /preview|garamond|baskerville|geist-latin/.test(name),
    ),
  ).toEqual([]);
  const license = await readFile(
    new URL("../../web/assets/stixtwotext-OFL.txt", import.meta.url),
    "utf8",
  );
  expect(license).toContain("SIL OPEN FONT LICENSE");
  for (const file of ["index.html", "docs.html", "platform.html"]) {
    const html = await readFile(
      new URL(`../../web/${file}`, import.meta.url),
      "utf8",
    );
    expect(html).not.toMatch(
      /noindex|preview-toolbar|data-theme|fonts\.googleapis/,
    );
  }
});

test("demo modes, examples, docs navigation and copy still work", async ({
  page,
}, info) => {
  await page.goto("/");
  for (const [index, result, steps] of [
    [1, "23", 4],
    [2, "25", 4],
    [3, "8", 2],
  ] as const) {
    await page
      .getByRole("button", { name: `Example ${index}`, exact: true })
      .click();
    await expect(page.locator(".demo-answer")).toHaveText(result);
    await page.getByRole("button", { name: "Process", exact: true }).click();
    await expect(page.locator(".trace-steps li")).toHaveCount(steps);
    await page.getByRole("button", { name: "Result", exact: true }).click();
    await expect(page.locator(".trace-panel")).toHaveCount(0);
  }
  await page.getByRole("button", { name: "Process", exact: true }).click();
  await page
    .locator(".demo-window")
    .screenshot({ path: info.outputPath("demo-process.png") });
  await page.getByRole("button", { name: "Copy", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toContain("$LUG_API_KEY");
  await page.getByRole("link", { name: "Read the docs" }).click();
  await page.locator('nav a[href="#calculate"]').click();
  await expect(page).toHaveURL(/#calculate$/);
  await page
    .locator("#calculate")
    .getByRole("button", { name: "Copy", exact: true })
    .first()
    .click();
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toContain("https://lug.by530.com/f/a/o/calculate");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(page.locator("html")).toHaveCSS("scroll-behavior", "auto");
});

for (const width of [1440, 390, 320]) {
  test(`console mock: tables, creation and revocation dialogs at ${width}px`, async ({
    page,
  }, info) => {
    await page.setViewportSize({ width, height: width === 1440 ? 1000 : 740 });
    const usage = {
      requests: 23,
      prompt_tokens: 72,
      completion_tokens: 23,
      total_tokens: 95,
    };
    const keys = [
      {
        id: "demo",
        name: "research-notebook",
        prefix: "sk-demo…",
        status: "active",
        created_at: 1700000000,
        last_used: null,
        usage,
      },
    ];
    await page.route("**/f/a/o/platform/me", (route) =>
      route.fulfill({
        json: {
          user: { id: "test", login: "researcher", provider: "github" },
          csrf_token: "ui-test-only",
        },
      }),
    );
    await page.route("**/f/a/o/platform/usage", (route) =>
      route.fulfill({ json: { total: usage } }),
    );
    await page.route("**/f/a/o/platform/keys", async (route) => {
      if (route.request().method() === "POST") {
        expect(route.request().headers()["x-csrf-token"]).toBe("ui-test-only");
        const name = route.request().postDataJSON().name;
        keys.push({ ...keys[0], id: "created", name });
        await route.fulfill({
          json: { name, key: "sk-ui-test-only-not-a-real-key" },
        });
      } else await route.fulfill({ json: { data: keys } });
    });
    await page.route("**/f/a/o/platform/keys/demo", async (route) => {
      expect(route.request().method()).toBe("DELETE");
      keys[0].status = "revoked";
      await route.fulfill({ json: {} });
    });
    await page.goto("/platform");
    await expect(page.locator("tbody tr")).toHaveCount(1);
    await noDecorativeBorders(
      page,
      ".metrics, .metric, .workspace-sidebar, .key-section, .key-section-head, .key-create-form, .key-section-foot, .quick-reference > div, .quick-reference > a",
    );
    await expect(page.locator(".metrics")).toHaveCSS("display", "grid");
    await expect(page.locator("#keyName")).toHaveCSS("border-top-width", "1px");
    await expect(page.locator("thead tr")).toHaveCSS(
      "border-bottom-width",
      "1px",
    );
    await noOverflow(page);
    await readable(page, ".metric-label");
    await page.screenshot({
      path: info.outputPath(`dashboard-${width}.png`),
      fullPage: true,
    });
    // Horizontal table overflow remains inside its scroll container.
    const revoke = page.getByRole("button", {
      name: "Revoke key research-notebook",
    });
    await revoke.click();
    await expect(page.locator("dialog[open]")).toBeVisible();
    await page
      .locator("dialog[open]")
      .screenshot({ path: info.outputPath(`revoke-${width}.png`) });
    await readable(page, "dialog[open] button.bg-destructive");
    await page.keyboard.press("Escape");
    await expect(page.locator("dialog[open]")).toHaveCount(0);
    await expect(revoke).toBeFocused();
    await revoke.click();
    await page
      .locator("dialog[open]")
      .getByRole("button", { name: "Revoke key", exact: true })
      .click();
    await expect(page.getByText("Revoked", { exact: true })).toBeVisible();
    await page.locator("#keyName").fill("new-notebook");
    await page.getByRole("button", { name: "Create key", exact: true }).click();
    await expect(page.locator("#created-warning")).toBeVisible();
    await readable(page, "#created-warning");
    await readable(page, "dialog[open] .text-muted-foreground");
    await page
      .locator("dialog[open]")
      .screenshot({ path: info.outputPath(`created-${width}.png`) });
    await noOverflow(page);
    await page.getByRole("button", { name: "I saved it" }).click();
    await expect(page.locator("dialog[open]")).toHaveCount(0);
    await expect(page.locator("tbody tr")).toHaveCount(2);
    await page.getByRole("button", { name: "切换到中文" }).click();
    await expect(page.locator("h1")).toHaveText("概览与密钥");
    await noOverflow(page);
    await page.screenshot({
      path: info.outputPath(`dashboard-zh-${width}.png`),
      fullPage: true,
    });
  });
}

test("console startup failure and retry remain usable", async ({ page }) => {
  await page.route("**/f/a/o/platform/auth", (route) =>
    route.fulfill({ status: 503, json: {} }),
  );
  await page.goto("/platform");
  await expect(
    page.getByRole("heading", { name: "Can't reach the console" }),
  ).toBeVisible();
  await page.route("**/f/a/o/platform/auth", (route) =>
    route.fulfill({ json: { github_enabled: false } }),
  );
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.locator("#login-title")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Continue with GitHub" }),
  ).toBeDisabled();
});
