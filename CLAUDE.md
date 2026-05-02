# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust workspace for extracting date / freshness signals from real estate
listing pages. First target site: `goutos.gr` (Greek real estate, ASP.NET +
"e-agents workspace" CMS, photos hosted on `ilist-cdn.e-agents.cloud` behind
Cloudflare).

## Build / run

```
cargo build --release
cargo run --release --bin inspect_listing -- <url>
```

Default URL when none given: `https://www.goutos.gr/en-US/property/500193`.

There are no tests yet. There is no separate lint config beyond the toolchain
defaults.

## Architecture

Single binary today: `src/inspect_listing.rs` (declared as
`[[bin]] inspect_listing` in `Cargo.toml`). It performs a single blocking
`reqwest::blocking::Client` GET, then runs each detector in order:

1. **Response headers / cookies** (`Date`, `Last-Modified`, `ETag`, etc.).
2. **JSON-LD** — `<script type="application/ld+json">`, parsed with
   `serde_json` and scanned for date-like keys.
3. **`<meta>` tags** with `time` / `date` in any attribute.
4. **Inline `<script>` blocks** — regex scan for date-like keys with
   surrounding context, and a separate scan of the raw HTML for
   ISO-8601 / dd-mm-yyyy date *values*.
5. **Backend / CDN URLs** referenced by the page (filtered to `ilist`,
   `e-agents`, `/api/`, `.json`).
6. **JSON endpoint probes** — a handful of educated guesses at
   per-property JSON URLs on the `ilist-cdn` and `goutos.gr` backends.

The two regexes (`date_keys_re`, `date_value_re`) are the configuration: if
you add a new target site, extending those is the first thing to do.

`safe_slice` exists because the snippet windows around regex matches must be
clamped to UTF-8 char boundaries — the HTML is full of multi-byte Greek text
and naive `&s[a..b]` panics.

## Domain knowledge — non-obvious

This is the part that took experimentation to discover and that future
sessions should not have to re-derive.

- **goutos.gr listing HTML carries zero date metadata.** No `Last-Modified`
  on the GET. (`HEAD` redirects to `/el-GR/error/not-found` — not a useful
  bypass.) No JSON-LD. No date-bearing meta tags. No inline state. No
  ISO/dd-mm-yyyy strings anywhere in the body. No `sitemap.xml`. No
  `robots.txt`. No Wayback Machine captures (CDX is empty).

- **The signal that works is photo `Last-Modified` from the CDN.**
  `ilist-cdn.e-agents.cloud` is Cloudflare-fronted and returns proper
  `Last-Modified` per JPEG. The earliest photo's upload time is a tight
  lower bound on when the listing existed in its current form. Photos
  typically cluster into a few discrete batches (initial publication,
  subsequent reshoots).

- **CMS is "e-agents workspace".** Confirmed via the `DC.publisher` meta
  tag on the site's 404 page and the `ilist-cdn.e-agents.cloud` host.
  Any other site running on this CMS will likely have the same blind spot
  (HTML date-free) and the same workaround (photo CDN `Last-Modified`).

- **Per-property JSON endpoint guesses all fail on goutos.gr.** Both
  `ilist-cdn` paths return 404; `goutos.gr/api/property/<id>` and
  `…/property/<id>.json` return 200 *with the regular HTML page*, not
  JSON. Don't waste time on those again unless probing a different site
  on the same CMS.

- **Property IDs (`/property/<n>`) appear sequential.** If a future task
  needs to rank listings by recency without doing per-photo HEADs, the ID
  itself is a coarse signal — but only relative to other IDs on the same
  site.

## Conventions

- Default to a single binary per concern; add new ones via `[[bin]]`
  entries in `Cargo.toml` rather than expanding `inspect_listing` into a
  multi-mode tool.
- Print sections with the existing `=== Title ===` style so output stays
  greppable.
- Keep the regexes as the extension point. New target sites should mostly
  mean adding key/value patterns, not new detector functions.
