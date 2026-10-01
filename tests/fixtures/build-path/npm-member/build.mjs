// The fixture's standard build: produces the dist/ the published tarball
// must carry. Zero third-party dependencies — the check runs offline.
import { mkdirSync, writeFileSync } from "node:fs";

mkdirSync("dist", { recursive: true });
writeFileSync(
  "dist/index.js",
  "export const memberValue = 42;\n",
);
console.log("built dist/index.js");
