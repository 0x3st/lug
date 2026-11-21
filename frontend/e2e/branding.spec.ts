import { test, expect } from "@playwright/test";

// Branding checks are frontend-only; do not depend on an OAuth provider or session.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("lug_lang", "zh"));
  await page.route("**/f/a/o/platform/me", (route) =>
    route.fulfill({ status: 401, json: {} }),
  );
  await page.route("**/f/a/o/platform/auth", (route) =>
    route.fulfill({ json: { github_enabled: false } }),
  );
});

for (const width of [1440, 768, 561, 560, 390, 320]) {
  test(`Lab 530 branding across pages and languages at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 1000 });
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));

    for (const path of ["/", "/docs", "/platform"]) {
      await page.goto(path);
      await expect(page.locator("h1")).toBeVisible();

      for (const language of ["zh", "en"]) {
        if (language === "en") {
          await page.getByRole("button", { name: "Switch to English" }).click();
        }
        await expect(page.locator("html")).toHaveAttribute(
          "lang",
          language === "zh" ? "zh-CN" : "en",
        );
        await page.evaluate(() => document.fonts.ready);
        await expect(page).toHaveTitle(/Lug/);
        await expect(page).not.toHaveTitle(/Lab 530/);
        await expect(page.locator('meta[name="description"]')).toHaveAttribute(
          "content",
          /^Lug — /,
        );
        const logo = page.locator("header .logo");
        await expect(logo).toHaveAccessibleName("Lug");
        await expect(logo).toHaveAttribute("href", "/");
        await expect(logo).toHaveText("Lug");
        await expect(logo.locator("svg, img, .brand-mark")).toHaveCount(0);
        await expect(logo.locator(".logo-name")).toHaveCSS(
          "font-family",
          /STIX Two Text/,
        );
        await expect(
          page.locator(".logo-affiliation, .logo-divider"),
        ).toHaveCount(0);
        expect(
          await logo.evaluate((element) => {
            const centers = Array.from(element.children)
              .map((child) => child.getBoundingClientRect())
              .filter((box) => box.width > 0)
              .map((box) => box.y + box.height / 2);
            return Math.max(...centers) - Math.min(...centers) <= 1;
          }),
          "Logo should be a single, vertically centered row",
        ).toBe(true);
        await expect(page.locator("body")).not.toContainText(
          /\/\s*Lab 530|A project by Lab 530|由 Lab 530 实验室打造|结构化计算 API|structured calculation API/i,
        );
        await expect(page.getByText(/Lab 530/)).toHaveCount(
          path === "/" ? 2 : 0,
        );
        expect(
          await page.locator(".language-toggle span").evaluate((label) => {
            return (
              label.getBoundingClientRect().height <=
              parseFloat(getComputedStyle(label).lineHeight) + 1
            );
          }),
          "Language label should stay on one line",
        ).toBe(true);
        expect(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= window.innerWidth,
          ),
          `${language} ${path} should not overflow at ${width}px`,
        ).toBe(true);
        expect(
          await page
            .locator(".header-inner, .app-header")
            .evaluate((header) => {
              const boxes = Array.from(header.children)
                .map((child) => child.getBoundingClientRect())
                .filter((box) => box.width > 0);
              return boxes.every(
                (box, index) =>
                  index === 0 || boxes[index - 1].right <= box.left + 1,
              );
            }),
          `${language} ${path} header items should not overlap`,
        ).toBe(true);

        if (path === "/") {
          await expect(page.locator(".hero img")).toHaveCount(0);
          const crest = page.locator(".footer-crest");
          await crest.scrollIntoViewIfNeeded();
          await expect(crest).toBeVisible();
          await expect
            .poll(() =>
              crest.evaluate((el) => {
                const image = el as HTMLImageElement;
                return image.complete && image.naturalWidth > 0;
              }),
            )
            .toBe(true);
          const crestBox = await crest.boundingBox();
          expect(crestBox!.width).toBeGreaterThanOrEqual(56);
          expect(crestBox!.width).toBeCloseTo(crestBox!.height, 0);
          const brand = page.locator(".footer-brand");
          expect(
            await brand.evaluate((el) => {
              const boxes = Array.from(el.children).map((child) =>
                child.getBoundingClientRect(),
              );
              return boxes.every(
                (box, i) => i === 0 || boxes[i - 1].right <= box.left,
              );
            }),
          ).toBe(true);
          await brand.screenshot({
            path: testInfo.outputPath(`footer-brand-${language}-${width}.png`),
          });
          await expect(page.locator(".release-label")).toHaveText(
            language === "zh" ? "结构化计算 AI" : "STRUCTURED CALCULATION AI",
          );
          await expect(page.locator(".hero-description")).toHaveText(
            language === "zh"
              ? "面向结构化计算的 AI，支持表达式求值、流式计算过程与用量统计。By Lab 530"
              : "AI for structured calculation, with expression evaluation, streamed steps and usage tracking. By Lab 530",
          );
          await expect(page.locator(".footer-bottom")).toContainText(
            language === "zh"
              ? "结构化计算 AI"
              : "AI for structured calculation",
          );
          await expect(
            page.locator('meta[name="description"]'),
          ).toHaveAttribute("content", /面向结构化计算的 AI/);
          await expect(page.locator(".integration-copy > p")).toContainText(
            language === "zh"
              ? "通过 API 将 Lug 接入你的应用"
              : "Use the API to integrate Lug into your application",
          );
          await expect(page.locator('pre[aria-label="cURL"]')).toContainText(
            "https://lug.by530.com/f/a/o/calculate",
          );
          await expect(page.locator(".hero-attribution")).toHaveCount(0);
          await expect(page.locator(".site-footer .logo")).toHaveText("Lug");
          await expect(
            page.locator(
              ".site-footer .logo svg, .site-footer .logo img, .brand-mark",
            ),
          ).toHaveCount(0);
          await expect(page.locator(".lab-logo")).toHaveAccessibleName(
            "Lab 530",
          );
          await expect(page.locator(".lab-logo")).toHaveAttribute(
            "href",
            "https://by530.com/",
          );
          await logo.screenshot({
            path: testInfo.outputPath(`logo-${language}-${width}.png`),
          });
          await page.screenshot({
            path: testInfo.outputPath(`home-${language}-${width}.png`),
            fullPage: true,
          });
        }
      }
    }
    expect(errors).toEqual([]);
  });
}
