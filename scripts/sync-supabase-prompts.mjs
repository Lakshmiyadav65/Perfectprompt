#!/usr/bin/env node
// Sync the three route system prompts from /prompts/*.md
// into supabase/functions/enhance/_prompts.json.
//
// Run before every `supabase functions deploy enhance` whenever you've
// edited a prompt file. The generated JSON is checked into source
// control so deploys are reproducible.

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const sources = {
  code:    resolve(ROOT, "prompts/code-enhancer.md"),
  writing: resolve(ROOT, "prompts/writing-enhancer.md"),
  generic: resolve(ROOT, "prompts/generic-enhancer.md"),
  polish:  resolve(ROOT, "prompts/polish-enhancer.md"),
};

const out = resolve(ROOT, "supabase/functions/enhance/_prompts.json");

const bundle = Object.fromEntries(
  Object.entries(sources).map(([route, path]) => {
    const text = readFileSync(path, "utf8");
    if (text.trim().length === 0) {
      throw new Error(`Prompt file is empty: ${path}`);
    }
    return [route, text];
  }),
);

mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, JSON.stringify(bundle, null, 2) + "\n", "utf8");

console.log(`Wrote ${out}`);
for (const [route, path] of Object.entries(sources)) {
  console.log(`  ${route.padEnd(8)} ← ${path.slice(ROOT.length + 1)}`);
}
