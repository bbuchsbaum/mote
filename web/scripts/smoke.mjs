// Smoke test against the *built* bundle: mounts the console in jsdom, drives a
// few real interactions, and asserts what the user would see. No browser.
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const bundle = readFileSync(new URL("../dist/console.js", import.meta.url), "utf8");
const css = readFileSync(new URL("../dist/console.css", import.meta.url), "utf8");

const dom = new JSDOM(`<!doctype html><html><body><div id="root"></div></body></html>`, {
  url: "http://127.0.0.1:7717/#/issues",
  pretendToBeVisual: true,
  runScripts: "outside-only",
});
const { window } = dom;
window.structuredClone ??= (v) => JSON.parse(JSON.stringify(v));
window.matchMedia ??= () => ({ matches: false, addEventListener() {}, removeEventListener() {} });

let failures = 0;
const check = (label, cond) => {
  if (cond) console.log(`  ok   ${label}`);
  else { console.log(`  FAIL ${label}`); failures++; }
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const waitFor = async (label, predicate, timeout = 2000) => {
  const deadline = Date.now() + timeout;
  let lastError;
  while (Date.now() < deadline) {
    try {
      if (await predicate()) return;
    } catch (error) {
      lastError = error;
    }
    await sleep(10);
  }
  const detail = lastError instanceof Error ? `: ${lastError.message}` : "";
  throw new Error(`timed out after ${timeout}ms waiting for ${label}${detail}`);
};
const eventually = async (label, predicate) => {
  try {
    await waitFor(label, predicate);
    check(label, true);
  } catch (error) {
    check(label, false);
    console.log(`       ${error.message}`);
  }
};
const text = () => window.document.body.textContent ?? "";
const all = (sel) => [...window.document.querySelectorAll(sel)];
const byText = (sel, needle) => all(sel).find((el) => (el.textContent ?? "").includes(needle));
const waitByText = async (sel, needle) => {
  let element;
  await waitFor(`${sel} containing ${JSON.stringify(needle)}`, () => {
    element = byText(sel, needle);
    return !!element;
  });
  return element;
};

window.eval(bundle);

console.log("\nmount");
check("stylesheet is non-trivial", css.length > 8000);
await eventually("rail renders the four views", () =>
  ["Issues", "Discussion", "Messages", "Triage"].every((v) => text().includes(v)));
await eventually("acting-as control shows the default actor", () => text().includes("alice"));
await eventually("seeded beads render", () => text().includes("Surface reservation expiry warnings"));
await eventually("a lease countdown is shown", () => /\d+m left/.test(text()));
await eventually("priority stripes are painted", () => all(".pri").length > 0);

console.log("\nkeyboard: TUI vocabulary");
const press = (key) => window.document.dispatchEvent(new window.KeyboardEvent("keydown", { key, bubbles: true }));
press("/");
await eventually("slash focuses issue search", () =>
  window.document.activeElement?.getAttribute("aria-label") === "Filter beads");
window.document.activeElement?.blur();
press("j");
await eventually("j focuses the first navigation row", () =>
  window.document.activeElement?.hasAttribute("data-nav-item"));
const firstNav = window.document.activeElement;
press("k");
await eventually("k moves to the previous navigation row", () => window.document.activeElement !== firstNav);
press("g"); press("d");
await eventually("g d switches to discussion", () => window.location.hash.startsWith("#/discussion"));
check("fixture exposes unread posts", (await window.__MOTE_FIXTURE__.unread()).length > 0);
press("u");
await eventually("u selects an actor-scoped unread post", () => window.location.hash.split("/").length >= 4);
await eventually("u focuses the unread post", () => window.document.activeElement?.classList.contains("post"));
press("g"); press("i");
await eventually("g i returns to issues", () => window.location.hash === "#/issues");

console.log("\nissues: open the detail pane");
const seededRow = await waitByText(".row", "Surface reservation expiry warnings");
seededRow.click();
await eventually("detail pane opened", () => text().includes("History"));
check("rejected ops are shown with their reason", text().includes("clock mismatch"));
check("the from-discussion backlink is present", all(".src").length > 0);

console.log("\nissues: create a bead by hand");
const newBead = await waitByText("button", "New bead");
newBead.click();
await waitFor("new bead modal", () => !!window.document.querySelector(".modal input"));
const titleInput = window.document.querySelector(".modal input");
const setValue = (el, v) => {
  const proto = Object.getPrototypeOf(el);
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
  el.dispatchEvent(new window.Event("input", { bubbles: true }));
};
setValue(titleInput, "Wire the console to mote serve");
await waitFor("enabled create bead button", () => !byText(".modal-foot button", "Create bead")?.disabled);
byText(".modal-foot button", "Create bead").click();
await eventually("new bead appears in the list", () => text().includes("Wire the console to mote serve"));

console.log("\nissues: the conflict path");
window.location.hash = "/issues";
await waitFor("issues route reset", () => window.location.hash === "#/issues"
  && !window.document.querySelector(".aside"));
const conflictRow = await waitByText(".row", "Surface reservation expiry warnings");
conflictRow.click();
await waitFor("bead detail clocks", () => window.location.hash.split("/").length >= 3
  && !!window.document.querySelector(".aside select"));
// The detail pane now holds this bead's field clocks.
const held = decodeURIComponent(window.location.hash.split("/")[2] ?? "");
check("detail pane holds a bead", held.startsWith("bd-"));
// Another agent patches the same field. No event fires, so our snapshot stays
// stale — exactly the window per-field clocks exist to catch.
window.__MOTE_FIXTURE__.simulateExternalPatch(held, { silent: true });
const setSelect = (el, v) => {
  Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value").set.call(el, v);
  el.dispatchEvent(new window.Event("change", { bubbles: true }));
};
setSelect(window.document.querySelector(".aside select"), "blocked");
await eventually("conflict dialog is raised", () => text().includes("The reducer rejected this patch"));
check("the reducer's own reason is surfaced", text().includes("clock mismatch"));
check("it says the intent was recorded, not applied", text().includes("recorded as a rejected intent"));
check("three honest exits are offered",
  ["Discard", "Take theirs", "Retry on current"].every((b) => !!byText(".modal-foot button", b)));
byText(".modal-foot button", "Retry on current").click();
await eventually("retry on current clears the conflict", () => !text().includes("The reducer rejected this patch"));
await eventually("the rejected intent is kept in history", () => text().includes("patch rejected"));

console.log("\ndiscussion: threads, routing, composing");
window.location.hash = "/discussion";
await waitFor("discussion route", () => window.location.hash === "#/discussion"
  && byText(".pane-title", "Topics"));
await eventually("topics list renders", () => text().includes("Planning"));
await eventually("threaded reply is indented", () =>
  all(".post").some((p) => parseInt(p.style.marginLeft || "0", 10) > 0));
await eventually("a needs-bead post is flagged", () => text().includes("needs bead"));
await eventually("a routed post shows its bead chip", () => all(".route.routed").length > 0);
const composer = window.document.querySelector(".composer textarea");
setValue(composer, "Console is wired up end to end.");
await waitFor("enabled discussion post button", () => !byText(".composer-foot button", "Post")?.disabled);
byText(".composer-foot button", "Post").click();
await eventually("new post appears in the thread", () => text().includes("Console is wired up end to end."));

console.log("\ntriage: the seam");
window.location.hash = "/triage";
await waitFor("triage route", () => window.location.hash === "#/triage"
  && byText(".pane-title", "Triage"));
await eventually("triage lists declared needs-bead discussion", () =>
  text().includes("Reservation adoption after an orphan"));
check("promote is offered", !!byText("button", "Promote to bead"));
byText("button", "Promote to bead").click();
await eventually("promote modal prefills the post body", () =>
  (window.document.querySelector(".modal textarea")?.value ?? "").includes("provenance"));
byText(".modal-foot button", "Create and route").click();
await waitFor("promote modal closes", () => !byText(".modal-foot button", "Create and route"));

console.log("\nmessages: two-sided conversation");
window.location.hash = "/messages";
await waitFor("messages route", () => window.location.hash === "#/messages"
  && byText(".pane-title", "Actors"));
await eventually("actor roster renders", () => text().includes("codex-b"));
check("roster shows a last-message preview", text().includes("Cannot reserve src/watch.rs"));
byText(".peer", "codex-b").click();
await eventually("an inbound request is shown", () => text().includes("Please take the parser work"));
check("a message this actor SENT is shown", text().includes("Taking it. Reserving src/parser.rs"));
check("request lifecycle strip is present", text().includes("awaiting response"));
check("ack and response are distinguished", text().includes("acked"));
check("bead chip on the request", all(".beadref").length > 0);

console.log(`\n${failures === 0 ? "PASS" : `FAIL (${failures})`}`);
process.exit(failures === 0 ? 0 : 1);
