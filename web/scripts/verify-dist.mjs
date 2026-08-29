// Rebuild the embedded console and compare the bytes with the proposed Git
// index. Using the index makes this gate useful before the commit exists while
// still catching both stale tracked bundles and accidentally untracked dist.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const artifacts = ["index.html", "console.js", "console.css"];

execFileSync("npm", ["run", "build"], { stdio: "inherit" });

for (const artifact of artifacts) {
  const path = `dist/${artifact}`;
  let indexed;
  try {
    indexed = execFileSync("git", ["show", `:web/${path}`]);
  } catch {
    throw new Error(`${path} is absent from the Git index; stage all three dist artifacts explicitly`);
  }
  assert.deepEqual(
    readFileSync(new URL(`../${path}`, import.meta.url)),
    indexed,
    `${path} differs from the Git index; rebuild and stage the generated artifact`,
  );
}

console.log("PASS dist matches the Git index: index.html, console.js, console.css");
