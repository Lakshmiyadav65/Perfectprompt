You are reading a software project's digest. Your job is to write a
concise PROJECT.md that another AI assistant will use as authoritative
context every time it helps a developer work on this project.

The downstream AI cannot see the digest — only your PROJECT.md. Your
summary IS its source of truth. Write accurately. Do not invent
features that aren't in the digest. Do not omit features that are
clearly present.

Output EXACTLY the schema below, in this order, with these exact
headers. No preamble. No closing remarks. Markdown only.

# {Project Name extracted from package.json / Cargo.toml / pyproject.toml / similar}

## What this is
[2-4 sentence plain-English description. What it does, who it's for.
No marketing speak.]

## Stack
[1 paragraph naming language, framework, build tools, and key runtime
dependencies. Name versions when visible in the manifest. Name the
test framework explicitly (AVA / Jest / pytest / cargo test / etc.) —
the downstream LLM will route test-writing instructions to whatever
you name here.]

## Architecture
[1 paragraph plus a bulleted list of up to 8 directories with
one-line descriptions of each. Use real paths from the digest's
<directory_structure> section. Example:
- `source/core/` — main request lifecycle, Options class, errors
- `test/` — AVA integration tests grouped by feature]

## Existing capabilities
[Up to 12 bullets of CONCRETE features, APIs, options, methods, or
classes that ALREADY EXIST in this project. Name them by their
actual identifiers from the source. This is the most important
section — it's what stops the downstream LLM from treating existing
features as new. Inspect every options/config interface field, every
exported class, every documented hook. Examples:
- AbortSignal support via `signal` option on Options
- Retry with configurable backoff via `retry.calculateDelay`
- Request hooks: beforeRequest, afterResponse, beforeRetry, beforeError
- HTTPError / RequestError / TimeoutError exception hierarchy
- JSON body parsing via `json` option (existing); response JSON via
  `responseType: 'json'`]

## Conventions
[Up to 8 bullets of code, test, and error conventions visible in the
source. Examples:
- TypeScript strict mode, no `any`
- Tests live in `test/`, one file per feature, written for AVA
- Errors thrown as instances of `HTTPError` or its subclasses
- Hooks accept and return modified Options objects
- Public API re-exported from `source/index.ts`]

## Gotchas
[Up to 6 bullets of non-obvious things. Things that look like bugs but
are intentional. Things to NOT change without understanding. May be
empty if the digest reveals nothing notable. Examples:
- The `cache` option accepts both Map and cacheable-request adapters
  (do not "fix" by accepting only one)
- `responseType` defaults to 'text' for backward compat, not 'json']

Hard limits — the downstream LLM relies on these:
- Total output under 4000 characters. If you're running long, cut the
  Gotchas section first, then trim Conventions, then trim Existing
  capabilities. Never drop the first four sections.
- Every file path you mention MUST appear in the digest's
  <directory_structure>. If you can't find a path, omit the bullet —
  do not invent.
- Every API / option / symbol you name MUST appear in a <file> block
  in the digest. If you can't find a symbol, omit the bullet — do not
  invent.
- No filler. Do not write "This project provides..." or "It is
  designed to...". Concrete facts only.
- No preamble or closing remarks. Just the six sections starting with
  `# {Project Name}`.

The digest follows. Read it carefully — especially the
<api_surface>, <directory_structure>, and any package manifests
(package.json / Cargo.toml / pyproject.toml) — then emit PROJECT.md.
