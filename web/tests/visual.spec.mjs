import { expect, test } from "@playwright/test";

const VISUAL_STABILIZERS = `
  * { animation: none !important; transition: none !important; }
  .row-right .mono-id,
  .post-head .mono-id,
  .tl-item .t,
  .beadref,
  .src,
  .conflict code,
  .delivery-recovery code { color: transparent !important; }
`;

async function settle(page) {
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(75);
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.clear();
    sessionStorage.clear();
  });
  await page.goto("/#/issues");
  await page.addStyleTag({ content: VISUAL_STABILIZERS });
});

test("issues list retains its intended layout", async ({ page }) => {
  await page.getByText("Surface reservation expiry warnings", { exact: false }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("issues-list-light.png");
});

test("issue detail and durable conflict remain legible", async ({ page }) => {
  await page.getByText("Surface reservation expiry warnings", { exact: false }).click();
  await page.getByText("History", { exact: true }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("issues-detail-light.png");

  const held = decodeURIComponent(new URL(page.url()).hash.split("/")[2] ?? "");
  await page.evaluate((id) => {
    window.__MOTE_FIXTURE__.simulateExternalPatch(id, { silent: true });
  }, held);
  await page.locator(".aside select[aria-label='Status']").selectOption("blocked");
  await page.getByText("The reducer rejected this patch", { exact: false }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("conflict-dialog-light.png");
});

test("discussion retains its intended layout", async ({ page }) => {
  await page.locator(".rail-item").filter({ hasText: "Discussion" }).click();
  await page.getByText("Decision: split parser work from test work.", { exact: false }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("discussion-light.png");
});

test("messages retain their intended layout", async ({ page }) => {
  await page.locator(".rail-item").filter({ hasText: "Messages" }).click();
  await page.locator(".peer").filter({ hasText: "codex-b" }).click();
  await page.getByText("Please take the parser work", { exact: false }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("messages-light.png");
});

test("queued message recovery keeps its choices and provenance legible", async ({ page }) => {
  await page.locator(".rail-item").filter({ hasText: "Messages" }).click();
  await page.locator(".peer").filter({ hasText: "parser-session" }).click();
  await page.getByLabel("Message body").fill("READ THIS BEFORE YOU EDIT hsmm.scala.");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page.getByRole("dialog", { name: "Message queued; recipient is not live" }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("message-delivery-recovery-light.png");
});

test("triage retains its intended layout", async ({ page }) => {
  await page.locator(".rail-item").filter({ hasText: "Triage" }).click();
  await page.getByText("Reservation adoption after an orphan", { exact: false }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("triage-light.png");
});

test("issues list remains readable in dark mode", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark", reducedMotion: "reduce" });
  await page.getByText("Surface reservation expiry warnings", { exact: false }).waitFor();
  await settle(page);
  await expect(page).toHaveScreenshot("issues-list-dark.png");
});
