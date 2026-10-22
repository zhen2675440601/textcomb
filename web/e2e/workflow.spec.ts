import { test, expect, type Page } from "@playwright/test";
import { readFile } from "node:fs/promises";

const user = { id: "00000000-0000-4000-8000-000000000001", username: "writer", role: "user" };
const job = {
  id: "job-1", document_id: "doc-1", model_profile_id: "model-1", analysis_profile: "general",
  status: "analyzing", stage: "analyzing", progress: 30, total_chunks: 2, completed_chunks: 1,
  error_code: null, error_message: null, report_id: null,
  created_at: new Date(Date.now() - 30_000).toISOString(), started_at: null, completed_at: null,
};
const article = "他今天的心情很繁重。通过这次学习，使我们懂得了校对的重要性。";
const report = {
  schema: "textcomb.report.v1", report_id: "report-1", job_id: job.id, complete: true,
  document: { original_name: "示例文章.txt", format: "txt", char_count: Array.from(article).length },
  analysis: { analysis_profile: "general", provider_kind: "openai_compatible", candidate_model: "fake", verifier_model: "fake", prompt_version: "test", analyzer_version: "test", reference_profile: false },
  summary: { total: 2, confirmed: 2, suspected: 0, typo: 0, punctuation: 0, grammar: 2, paragraph: 0 },
  generated_at: new Date().toISOString(),
  issues: ["心情很繁重", "通过这次学习，使"].map((quote, index) => ({
    id: `issue-${index + 1}`, category: "grammar", grammar_subtype: "collocation", level: "confirmed",
    location: { document_format: "txt", char_start: article.indexOf(quote), char_end: article.indexOf(quote) + quote.length, quote },
    original_text: quote, reason: "测试原因", suggestion: "测试建议", confidence: 96, evidence_refs: [],
    feedback: undefined as string | undefined,
  })), missed_issues: [],
};

async function signedIn(page: Page) {
  await page.route("**/api/v1/me", (route) => route.fulfill({ json: user }));
  await page.route("**/api/v1/model-profiles", (route) => route.fulfill({ json: [] }));
  await page.route("**/api/v1/reports/report-1", (route) => route.fulfill({ json: report }));
}

test("history searches and pages beyond 200 records while showing real totals", async ({ page }) => {
  await signedIn(page);
  await page.route("**/api/v1/analyses/history?*", (route) => {
    const params = new URL(route.request().url()).searchParams;
    const pageNumber = Number(params.get("page"));
    const q = params.get("q");
    return route.fulfill({ json: {
      items: [{ ...job, id: `job-${pageNumber}`, original_name: q || `文章 ${pageNumber}.txt`, document_format: "txt" }],
      total: q ? 1 : 205, page: pageNumber, page_size: 20,
      totals: { total: 205, active: 0, completed: 204, failed: 1 },
    } });
  });
  await page.goto("/analyses");
  await expect(page.locator(".stat-card").first()).toContainText("205");
  await expect(page.getByRole("heading", { name: "文章 1.txt" })).toBeVisible();
  await page.getByRole("button", { name: "下一页", exact: true }).click();
  await expect(page.getByRole("heading", { name: "文章 2.txt" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "文章历史分页" })).toContainText("第 2 / 11 页");
  await page.getByRole("searchbox", { name: "搜索文章名" }).fill("100%_文稿");
  await page.getByRole("button", { name: "搜索", exact: true }).click();
  await expect(page.getByRole("heading", { name: "100%_文稿" })).toBeVisible();
  await expect(page.locator(".stat-card").first()).toContainText("205");
  await expect(page.getByRole("navigation", { name: "文章历史分页" })).toContainText("第 1 / 1 页");
});

test("progress reconnects after stream and HTTP failures and stops when navigating away", async ({ page }) => {
  await signedIn(page);
  await page.addInitScript(() => {
    const streams: EventSource[] = [];
    class ControlledEvents extends EventTarget {
      onerror: (() => void) | null = null;
      onopen: (() => void) | null = null;
      closed = false;
      constructor() { super(); streams.push(this as unknown as EventSource); }
      close() { this.closed = true; }
    }
    Object.assign(window, { EventSource: ControlledEvents, testStreams: streams });
  });
  let calls = 0;
  await page.route("**/api/v1/analyses/job-1", (route) => {
    calls++;
    return route.fulfill(calls === 2 ? { status: 503, json: { status: 503, title: "offline", code: "HTTP_ERROR" } } : { json: job });
  });
  await page.goto("/analyses/job-1");
  await expect(page.getByText("已耗时", { exact: false })).toBeVisible();
  await page.evaluate(() => (window as unknown as { testStreams: EventSource[] }).testStreams[0]?.onerror?.(new Event("error")));
  await expect(page.getByRole("status")).toContainText("自动重连");
  await expect.poll(() => calls, { timeout: 10_000 }).toBeGreaterThanOrEqual(3);
  await page.getByRole("link", { name: "模型设置", exact: true }).click();
  const before = calls;
  await expect.poll(() => page.evaluate(() => (window as unknown as { testStreams: { closed: boolean }[] }).testStreams.every((stream) => stream.closed))).toBe(true);
  await page.waitForTimeout(1800);
  expect(calls).toBe(before);
});

test("report review advances, restores position and exports feedback", async ({ page }) => {
  await signedIn(page);
  const current = structuredClone(report);
  await page.route("**/api/v1/reports/report-1", (route) => route.fulfill({ json: current }));
  await page.route("**/api/v1/issues/*/feedback", async (route) => {
    const issue = current.issues.find((item) => route.request().url().includes(item.id));
    if (issue) issue.feedback = route.request().postDataJSON().verdict;
    await route.fulfill({ status: 204 });
  });
  await page.route("**/api/v1/reports/report-1/source?*", (route) => route.fulfill({ json: { original_name: "示例文章.txt", document_format: "txt", char_count: article.length, char_start: 0, char_end: article.length, text: article } }));
  await page.route("**/api/v1/reports/report-1/export/json", (route) => route.fulfill({
    contentType: "application/json", headers: { "content-disposition": 'attachment; filename="report.json"' }, body: JSON.stringify(current),
  }));
  await page.goto("/reports/report-1");
  await expect(page.locator("#issue-issue-1")).toHaveClass(/review-selected/);
  await page.locator("body").click({ position: { x: 1, y: 1 } });
  await page.keyboard.press("1");
  await expect(page.getByText("已审核 1 / 2 项", { exact: true })).toBeVisible();
  await expect(page.locator("#issue-issue-2")).toHaveClass(/review-selected/);
  await page.getByRole("button", { name: "查看原文", exact: true }).click();
  await expect(page.locator("#report-source-selection")).toHaveText("通过这次学习，使");
  await page.reload();
  await expect(page.locator("#issue-issue-2")).toHaveClass(/review-selected/);
  await expect(page.getByText("已恢复上次阅读位置", { exact: false })).toBeVisible();
  await page.evaluate(() => window.scrollTo({ top: 0, behavior: "instant" }));
  await page.screenshot({ path: test.info().outputPath("review-desktop.png"), fullPage: true, animations: "disabled" });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator(".sidebar")).not.toBeInViewport();
  await page.evaluate(() => window.scrollTo({ top: 0, behavior: "instant" }));
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: test.info().outputPath("review-mobile.png"), fullPage: true, animations: "disabled" });
  const stored = await page.evaluate(() => Object.entries(localStorage));
  expect(JSON.stringify(stored)).not.toContain(article);
  const download = page.waitForEvent("download");
  await page.getByRole("link", { name: "JSON", exact: true }).click();
  expect((await download).suggestedFilename()).toBe("report.json");
});

test("first-use model setup leads to an example that is only submitted by the user", async ({ page }) => {
  await signedIn(page);
  const profile = { id: "model-1", name: "示例模型", provider_kind: "openai_compatible", base_url: "http://fake-model:4010/v1", candidate_model: "fake", verifier_model: "fake", enabled: true, is_reference: false, max_concurrency: 1, shared: false, created_at: new Date().toISOString() };
  await page.route("**/api/v1/model-profiles", (route) => route.fulfill({ json: [profile] }));
  await page.route("**/api/v1/model-profiles/model-1/test", (route) => route.fulfill({ json: { ok: true } }));
  let submitted = false;
  await page.route("**/api/v1/documents/text", (route) => { submitted = true; return route.abort(); });
  await page.goto("/models");
  await page.getByRole("button", { name: "测试连接", exact: true }).click();
  await page.getByRole("link", { name: "分析示例文章 →" }).click();
  await expect(page.getByRole("textbox", { name: "文章正文" })).not.toBeEmpty();
  expect(submitted).toBe(false);
  await page.getByRole("button", { name: "开始分析", exact: false }).click();
  await expect.poll(() => submitted).toBe(true);
});

test("real fake-model workflow covers login, model setup, report feedback and three exports", async ({ page }) => {
  test.setTimeout(120_000);
  test.skip(process.env.TEXTCOMB_E2E_LIVE !== "1", "Requires the isolated fake-model stack; enabled in container CI.");
  await page.goto("/login");
  await page.getByRole("textbox", { name: "账号", exact: true }).fill(process.env.TEXTCOMB_E2E_USERNAME ?? "e2e-admin");
  await page.getByLabel("密码", { exact: true }).fill(process.env.TEXTCOMB_E2E_PASSWORD ?? "textcomb-e2e-test-only");
  await page.getByRole("button", { name: "进入工作台" }).click();
  await expect(page).toHaveURL(/\/analyses$/);
  await page.getByRole("link", { name: "模型设置", exact: true }).click();
  await page.getByRole("button", { name: "添加模型配置" }).click();
  await page.getByRole("textbox", { name: "配置名称" }).fill(`Browser test ${Date.now()}`);
  await page.getByRole("textbox", { name: "接口地址" }).fill(process.env.TEXTCOMB_E2E_FAKE_MODEL_URL ?? "http://fake-model:4010/v1");
  await page.getByLabel("API 密钥", { exact: true }).fill("fake-model-test-only");
  await page.getByRole("textbox", { name: "模型名称", exact: true }).fill("fake-textcomb");
  await page.getByRole("checkbox", { name: /我已了解/ }).check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await page.locator(".provider-card").filter({ hasText: "Browser test" }).last().getByRole("button", { name: "测试连接", exact: true }).click();
  await page.getByRole("link", { name: "分析示例文章 →" }).click();
  await page.getByRole("textbox", { name: "文章正文" }).fill(article);
  await page.getByRole("button", { name: "开始分析", exact: false }).click();
  await expect(page).toHaveURL(/\/reports\//, { timeout: 90_000 });
  await expect(page.locator(".issue-card").first()).toBeVisible();
  await page.locator(".issue-card").first().getByRole("button", { name: "✓ 正确", exact: true }).click();
  await expect(page.getByText(/已审核 1 \//)).toBeVisible();
  await page.getByRole("button", { name: "查看原文", exact: true }).click();
  await expect(page.locator(".source-text")).toContainText("心情很繁重");
  for (const format of ["JSON", "Markdown", "PDF"]) {
    const pending = page.waitForEvent("download");
    await page.getByRole("link", { name: format, exact: true }).click();
    const download = await pending;
    expect(await download.failure()).toBeNull();
    const file = await download.path();
    expect(file).not.toBeNull();
    const content = await readFile(file!);
    if (format === "JSON") expect(JSON.parse(content.toString()).issues.some((issue: { feedback?: string }) => issue.feedback === "correct")).toBe(true);
    if (format === "PDF") expect(content.subarray(0, 4).toString()).toBe("%PDF");
    if (format === "Markdown") expect(content.toString()).toContain("心情很繁重");
  }
});
