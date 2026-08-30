// Real-browser integration against the binary-embedded console and a fresh
// on-disk mote store. Run `cargo build && npm --prefix web run live:e2e`.
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "node:child_process";
import { chromium } from "playwright";

const here = dirname(fileURLToPath(import.meta.url));
const binary = resolve(here, "../../target/debug/mote");
assert.ok(existsSync(binary), `missing ${binary}; run cargo build first`);

const store = mkdtempSync(join(tmpdir(), "mote-console-live-e2e-"));
let server;
let browser;
let releaseRefresh;
let serverStderr = "";

const sleep = (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
const isRunning = (child) => child.exitCode === null && child.signalCode === null;

function run(args, options = {}) {
  const result = spawnSync(binary, args, {
    cwd: store,
    encoding: "utf8",
    ...options,
  });
  assert.equal(
    result.status,
    0,
    `mote ${args.join(" ")} failed\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
  );
  return result.stdout.trim();
}

async function waitForLaunch() {
  const path = join(store, ".mote/local/serve-token");
  for (let attempt = 0; attempt < 120; attempt++) {
    if (server.exitCode !== null) {
      throw new Error(`mote serve exited during startup with ${server.exitCode}`);
    }
    const launch = serverStderr.match(/mote console listening on http:\/\/127\.0\.0\.1:(\d+)\/\?t=([0-9a-f]+)/);
    if (launch && existsSync(path)) {
      const token = readFileSync(path, "utf8").trim();
      if (token) {
        assert.equal(token, launch[2], "stderr launch token must match the private token file");
        return { token, baseUrl: `http://127.0.0.1:${launch[1]}` };
      }
    }
    await sleep(25);
  }
  throw new Error(`mote serve did not publish its launch URL and token: ${serverStderr}`);
}

try {
  run(["init"]);
  const expiredStart = run(["session", "start", "--as", "expired-peer", "--ttl", "5m"]);
  const expiredSession = expiredStart.match(/MOTE_SESSION='([^']+)'/)?.[1];
  assert.ok(expiredSession, `could not parse expired session id: ${expiredStart}`);
  run(["--actor", "expired-peer", "session", "end", expiredSession]);
  run(["session", "start", "--as", "live-peer", "--ttl", "30m"]);
  server = spawn(binary, ["serve", "--port", "0"], {
    cwd: store,
    stdio: ["ignore", "ignore", "pipe"],
  });
  server.stderr.setEncoding("utf8");
  server.stderr.on("data", (chunk) => { serverStderr += chunk; });
  const { token, baseUrl } = await waitForLaunch();

  async function api(method, path, body, actor = "admin") {
    const response = await fetch(`${baseUrl}/api${path}`, {
      method,
      headers: {
        "Content-Type": "application/json",
        "X-Mote-Actor": actor,
        "X-Mote-Token": token,
      },
      body: method === "GET" ? undefined : JSON.stringify(body ?? {}),
    });
    const text = await response.text();
    assert.ok(response.ok, `${method} ${path} -> ${response.status}: ${text}`);
    return text ? JSON.parse(text) : null;
  }

  const seed = await api("POST", "/beads", {
    title: "Live console seed issue",
    body: "Held open while another process changes its status.",
    priority: 1,
    tags: ["console", "e2e"],
  });
  await api("POST", "/topics", {
    topic: "planning",
    title: "Planning",
    body: "Live discussion needs a tracked outcome.",
  }, "bob");
  const posts = await api("GET", "/topics/planning/posts");
  assert.equal(posts.length, 1);
  await api("POST", `/posts/${posts[0].post_id}/needs-bead`, {});
  await api("POST", "/messages", {
    to: "admin",
    body: "Please review the live console boundary.",
    kind: "request",
    idempotency_key: "live-e2e-request",
  }, "bob");

  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext();
  const page = await context.newPage();
  const pageErrors = [];
  const badResponses = [];
  const bootstrapRequests = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  page.on("request", (request) => bootstrapRequests.push(request.url()));
  page.on("response", (response) => {
    if (response.status() >= 400 && response.status() !== 409) {
      badResponses.push(`${response.status()} ${response.url()}`);
    }
  });

  await page.goto(`${baseUrl}/?t=${token}#/issues`, {
    waitUntil: "domcontentloaded",
  });
  await page.getByText("Live console seed issue", { exact: true }).waitFor();
  await page.getByText("connected · live", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Switch actor. Acting as admin", exact: true }).waitFor();
  await page.waitForFunction(() => !new URL(location.href).searchParams.has("t"));
  assert.equal(new URL(page.url()).searchParams.has("t"), false, "bootstrap must strip the token from browser history");
  const cookie = (await context.cookies()).find((candidate) => candidate.name === "mote_console_token");
  assert.ok(cookie, "bootstrap must set the console session cookie");
  assert.equal(cookie.value, token);
  assert.equal(cookie.httpOnly, true);
  assert.equal(cookie.sameSite, "Strict");
  assert.equal(cookie.path, "/");
  for (const requested of bootstrapRequests.slice(1)) {
    assert.equal(new URL(requested).searchParams.has("t"), false, `token leaked into subrequest: ${requested}`);
  }

  // A CLI process publishes after the browser's EventSource is connected.
  // Only the issue-category cache slices should refetch.
  const observedApi = [];
  const onRequest = (request) => {
    const url = new URL(request.url());
    if (url.pathname.startsWith("/api/")) observedApi.push(url.pathname);
  };
  page.on("request", onRequest);
  run(["--actor", "cli-peer", "new", "Arrived over SSE"]);
  await page.getByText("Arrived over SSE", { exact: true }).waitFor();
  await sleep(250);
  page.off("request", onRequest);
  assert.ok(observedApi.includes("/api/beads"), `missing beads refresh: ${observedApi}`);
  assert.ok(observedApi.includes("/api/board"), `missing board refresh: ${observedApi}`);
  assert.ok(observedApi.includes("/api/unrouted"), `missing unrouted refresh: ${observedApi}`);
  for (const unrelated of ["/api/topics", "/api/actors", "/api/inflight"]) {
    assert.ok(!observedApi.includes(unrelated), `issue event refetched ${unrelated}: ${observedApi}`);
  }

  // Hold the detail refresh triggered by a genuine second-process patch. The
  // visible select therefore submits the clock it actually rendered, and the
  // reducer must return the durable 409 conflict path.
  await page.getByText("Live console seed issue", { exact: true }).click();
  await page.locator(".aside select[aria-label='Status']").waitFor();
  let sawRefreshResolve;
  const sawRefresh = new Promise((resolveSaw) => { sawRefreshResolve = resolveSaw; });
  const refreshGate = new Promise((resolveGate) => { releaseRefresh = resolveGate; });
  const detailPath = `/api/beads/${seed.id}`;
  await page.route(`**${detailPath}`, async (route) => {
    if (route.request().method() === "GET") {
      sawRefreshResolve();
      await refreshGate;
    }
    await route.continue();
  });
  run(["--actor", "rival-process", "set", seed.id, "status=doing"]);
  await Promise.race([
    sawRefresh,
    sleep(5000).then(() => { throw new Error("SSE did not invalidate the open bead detail"); }),
  ]);
  const conflictResponse = page.waitForResponse((response) =>
    response.status() === 409
      && response.request().method() === "PATCH"
      && new URL(response.url()).pathname === detailPath,
  );
  await page.locator(".aside select[aria-label='Status']").selectOption("blocked");
  const conflict = await (await conflictResponse).json();
  assert.equal(typeof conflict.op_id, "string");
  assert.equal(typeof conflict.reason, "string");
  assert.equal(conflict.current.status, "doing");
  await page.getByText("The reducer rejected this patch", { exact: false }).waitFor();
  await page.getByText(conflict.reason, { exact: false }).waitFor();
  releaseRefresh();
  releaseRefresh = undefined;
  await page.getByRole("button", { name: "Discard", exact: true }).click();
  await page.unroute(`**${detailPath}`);

  await page.locator(".rail-item").filter({ hasText: "Discussion" }).click();
  await page.getByText("Planning", { exact: true }).first().waitFor();
  await page.getByText("Live discussion needs a tracked outcome.", { exact: true }).waitFor();

  await page.locator(".rail-item").filter({ hasText: "Messages" }).click();
  await page.getByText("bob", { exact: true }).first().waitFor();
  await page.getByText("Please review the live console boundary.", { exact: true }).waitFor();

  await page.locator(".peer").filter({ hasText: "expired-peer" }).click();
  await page.getByLabel("Message body").fill("Use the public fallback if this session is gone.");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const recovery = page.getByRole("dialog", { name: "Message queued; recipient is not live" });
  await recovery.waitFor();
  await recovery.getByText("source=session_history, reason=ended", { exact: false }).waitFor();
  await recovery.getByRole("button", { name: "Send to live actor", exact: true }).waitFor();
  await recovery.getByRole("button", { name: "Post publicly", exact: true }).click();
  await recovery.waitFor({ state: "detached" });
  const publicPosts = await api("GET", "/topics/planning/posts");
  const publicFallback = publicPosts.at(-1);
  assert.match(publicFallback.body, /Use the public fallback if this session is gone/);
  assert.match(publicFallback.body, /delivery=queued/);
  assert.deepEqual(publicFallback.notification_recipients, ["expired-peer"]);

  await page.getByLabel("Message body").fill("Reroute this explicitly.");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await recovery.waitFor();
  await recovery.getByRole("button", { name: "Send to live actor", exact: true }).click();
  await recovery.waitFor({ state: "detached" });
  await page.getByText("live-peer", { exact: true }).first().waitFor();
  await page.locator(".bub-txt").filter({ hasText: "Rerouted from queued DM" }).waitFor();

  await page.locator(".rail-item").filter({ hasText: "Triage" }).click();
  await page.getByText("Live discussion needs a tracked outcome.", { exact: true }).waitFor();
  await page.getByText("Promote to bead", { exact: true }).waitFor();

  // The browser vocabulary mirrors the TUI and unread navigation remains
  // read-only: it changes focus and route, never the durable read cursor.
  await page.keyboard.press("g");
  await page.keyboard.press("i");
  await page.getByText("Live console seed issue", { exact: true }).waitFor();
  await page.keyboard.press("/");
  assert.equal(await page.locator("[data-console-search]").evaluate((node) => node === document.activeElement), true);
  await page.locator("[data-console-search]").evaluate((node) => node.blur());
  await page.keyboard.press("j");
  assert.equal(await page.locator("[data-nav-item]:focus").count(), 1);
  const unreadBefore = (await api("GET", "/board")).discussion_unread;
  await page.keyboard.press("u");
  await page.locator(".post:focus").waitFor();
  assert.match(page.url(), /#\/discussion\/planning\/post-/);
  const unreadAfter = (await api("GET", "/board")).discussion_unread;
  assert.equal(unreadAfter, unreadBefore, "u must not advance the discussion read cursor");

  assert.deepEqual(pageErrors, []);
  assert.deepEqual(badResponses, []);
  assert.match(serverStderr, /mote console listening on http:\/\/127\.0\.0\.1:\d+\/\?t=/);
  server.kill("SIGKILL");
  await new Promise((resolveExit) => server.once("exit", resolveExit));
  await page.getByText("stream offline", { exact: true }).waitFor();
  console.log("PASS live browser: embedded assets, four views, SSE slices, concurrent 409, offline rail");
} finally {
  if (releaseRefresh) releaseRefresh();
  if (browser) await browser.close();
  if (server && isRunning(server)) {
    server.kill("SIGKILL");
    await new Promise((resolveExit) => server.once("exit", resolveExit));
  }
  rmSync(store, { recursive: true, force: true });
}
