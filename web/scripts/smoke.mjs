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
const tick = () => new Promise((r) => setTimeout(r, 30));
const text = () => window.document.body.textContent ?? "";
const all = (sel) => [...window.document.querySelectorAll(sel)];
const byText = (sel, needle) => all(sel).find((el) => (el.textContent ?? "").includes(needle));

window.eval(bundle);
await tick();

console.log("\nmount");
check("stylesheet is non-trivial", css.length > 8000);
check("rail renders the four views",
  ["Issues", "Discussion", "Messages", "Triage"].every((v) => text().includes(v)));
check("acting-as control shows the default actor", text().includes("alice"));
check("seeded beads render", text().includes("Surface reservation expiry warnings"));
check("a lease countdown is shown", /\d+m left/.test(text()));
check("priority stripes are painted", all(".pri").length > 0);

console.log("\nissues: open the detail pane");
byText(".row", "Surface reservation expiry warnings")?.click();
await tick();
check("detail pane opened", text().includes("History"));
check("rejected ops are shown with their reason", text().includes("clock mismatch"));
check("the from-discussion backlink is present", all(".src").length > 0);

console.log("\nissues: create a bead by hand");
byText("button", "New bead")?.click();
await tick();
const titleInput = window.document.querySelector(".modal input");
const setValue = (el, v) => {
  const proto = Object.getPrototypeOf(el);
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
  el.dispatchEvent(new window.Event("input", { bubbles: true }));
};
setValue(titleInput, "Wire the console to mote serve");
await tick();
byText(".modal-foot button", "Create bead")?.click();
await tick();
check("new bead appears in the list", text().includes("Wire the console to mote serve"));

console.log("\nissues: the conflict path");
window.location.hash = "/issues";
await tick(); await tick();
byText(".row", "Surface reservation expiry warnings")?.click();
await tick(); await tick();
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
await tick(); await tick();
check("conflict dialog is raised", text().includes("The reducer rejected this patch"));
check("the reducer's own reason is surfaced", text().includes("clock mismatch"));
check("it says the intent was recorded, not applied", text().includes("recorded as a rejected intent"));
check("three honest exits are offered",
  ["Discard", "Take theirs", "Retry on current"].every((b) => !!byText(".modal-foot button", b)));
byText(".modal-foot button", "Retry on current")?.click();
await tick(); await tick();
check("retry on current clears the conflict", !text().includes("The reducer rejected this patch"));
check("the rejected intent is kept in history", text().includes("patch rejected"));

console.log("\ndiscussion: threads, routing, composing");
window.location.hash = "/discussion";
await tick(); await tick();
check("topics list renders", text().includes("Planning"));
check("threaded reply is indented", all(".post").some((p) => parseInt(p.style.marginLeft || "0", 10) > 0));
check("a needs-bead post is flagged", text().includes("needs bead"));
check("a routed post shows its bead chip", all(".route.routed").length > 0);
const composer = window.document.querySelector(".composer textarea");
setValue(composer, "Console is wired up end to end.");
await tick();
byText(".composer-foot button", "Post")?.click();
await tick();
check("new post appears in the thread", text().includes("Console is wired up end to end."));

console.log("\ntriage: the seam");
window.location.hash = "/triage";
await tick(); await tick();
check("triage lists declared needs-bead discussion", text().includes("Reservation adoption after an orphan"));
check("promote is offered", !!byText("button", "Promote to bead"));
byText("button", "Promote to bead")?.click();
await tick();
check("promote modal prefills the post body", (window.document.querySelector(".modal textarea")?.value ?? "").includes("provenance"));
byText(".modal-foot button", "Create and route")?.click();
await tick();

console.log("\nmessages: two-sided conversation");
window.location.hash = "/messages";
await tick(); await tick();
check("actor roster renders", text().includes("codex-b"));
check("roster shows a last-message preview", text().includes("Cannot reserve src/watch.rs"));
byText(".peer", "codex-b")?.click();
await tick(); await tick();
check("an inbound request is shown", text().includes("Please take the parser work"));
check("a message this actor SENT is shown", text().includes("Taking it. Reserving src/parser.rs"));
check("request lifecycle strip is present", text().includes("awaiting response"));
check("ack and response are distinguished", text().includes("acked"));
check("bead chip on the request", all(".beadref").length > 0);

console.log(`\n${failures === 0 ? "PASS" : `FAIL (${failures})`}`);
process.exit(failures === 0 ? 0 : 1);
