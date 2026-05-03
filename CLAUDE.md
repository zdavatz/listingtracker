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
cargo run --release --bin recent_listings -- [--area <id>] [--pages <n>] [--top <n>] [--sort latest|price-asc]
cargo run --release --bin geoland_recent_listings -- [--area <id>] [--top <n>] [--sort latest|price-asc] [--sale-only|--rent-only]
```

The `*_recent_listings` binaries require headless Chrome to print PDFs. They look at
`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` by default;
override with `CHROME=/path/to/chrome`.

There are no tests yet. There is no separate lint config beyond the toolchain
defaults.

## Architecture

Three binaries today, all registered explicitly in `Cargo.toml` (no `src/bin/`
auto-discovery). They share `reqwest::blocking::Client` + `scraper` + a small
set of regexes; nothing is factored into a library yet because the surface is
small. The two `*_recent_listings` binaries deliberately duplicate their
`Sort` enum / inlining / writer / Chrome invocation rather than share a module
— each site needs its own card parser + URL scheme, and keeping them
independent makes it easier to evolve one without breaking the other.

### `src/inspect_listing.rs` — single-listing detector

Performs a blocking GET, then runs each detector in order:

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

### `src/recent_listings.rs` — area-wide recency catalog

Walks every listing in a goutos.gr area and ranks them by latest CDN photo
upload date. Pipeline:

1. POST `/en-US/search-results` with `{"area": <id>, "page": <n>, "sorting": "newer"}`
   in a loop until a page returns zero cards. The endpoint is the same one the
   site's own JS calls (`render-partial.js`); response is rendered HTML, not JSON.
2. Parse each `article.geodir-category-listing` card: id, title, property type,
   price, details, and the carousel thumbnail URLs (`.carousel-inner img[src]`).
3. Concurrent HEAD on every photo URL via `std::thread::scope` (12 workers,
   no async runtime, no rayon dep). `Last-Modified` parsed with `httpdate`.
4. Sort listings by `latest` photo date desc; listings with no photos in the
   search-results card sink to the bottom (their `Option<SystemTime>` is `None`).
5. Backfill: any listing whose card carousel rendered zero `<img>` tags
   gets a follow-up GET on its `/en-US/property/<id>` page; full-size
   photo URLs are extracted by regex (`/fol<id>/<hash>.jpg`, excluding
   `-thumb.` / `Thumb_`). On the Ermioni dataset this catches ~2 of 190
   listings — small enough that the eight extra fetches don't matter,
   but large enough that ranking-by-photo-date would otherwise be wrong
   for those entries.
6. Sort according to `--sort latest|price-asc`. Latest = `latest` photo
   date desc (missing dates last). Price-asc = `parse_price` (digits-only
   parse) asc, missing prices ("Price upon request") last. The site uses
   `1 €` as a placeholder for "ask for price" on some rentals — treated
   as a real price for now since the user might want to spot them; revisit
   if it gets confusing.
7. Render an HTML catalog (one `<div class="card">` per listing, A4 print CSS),
   write it to `html/<area>-<sortslug>.html`, then spawn headless Chrome with
   `--print-to-pdf=pdf/<area>-<sortslug>.pdf`. Same Chrome invocation as
   `~/software/crawl2pump/src/bin/pumpfoil_report.rs`. Each sort writes its
   own pair of files so multiple views can coexist (`ermioni-recent.*`,
   `ermioni-price-asc.*`, etc.).

`fetch_area_name` resolves an `areaID` to its display name via
`/ajax/get-areas-by-code?area=<id>` so the catalog title reads "Ermioni"
instead of "area-3235".

### `src/geoland_recent_listings.rs` — geoland.properties area catalog

Parallel to `recent_listings` but targets [geoland.properties](https://www.geoland.properties),
a different agency that also covers Ermioni and exposes a much richer
search-result card (title / location / beds / baths / parking / sqm / price
all inline, no detail-page fallback needed).

Differences from the goutos version:

- One photo per listing (the `listing-img`), not a carousel — so each
  listing carries a single `last_modified` rather than `earliest`/`latest`.
- The "thumbnail" returned by geoland is the **full-size original** (~2 MB
  JPEG). Inlining 300 of those raw blew the HTML to 435 MB — fixed by
  decoding + Lanczos3-resizing to ≤600 px + re-encoding JPEG quality 70 in
  pure Rust via the `image` crate. Result: ~30-70 KB per thumbnail, ~20 MB
  HTML for 301 cards.
- Default `--sort` is `price-asc` (not `latest`) because that's what the
  user asked for. There's a `--sale-only` / `--rent-only` filter for when
  the cheap-rentals-at-the-top problem gets distracting.
- Walks both `for=1` (sale) and `for=2` (rent) by default and renders
  them in a single catalog with a SALE/RENT badge per card.

`fetch_area_name` resolves an `areaID` differently here: it calls
`/listingsearhPath/for/sale/areas/r<id>` (the slug-builder) and
title-cases the last URL segment. So `r3235 → "sale-akiniton/ermioni" →
"Ermioni"`.

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
  site. Empirically the site's own `sorting:"newer"` order does NOT
  match either ID order or photo-Last-Modified order, so don't trust it
  as a recency signal.

- **Useful geoland.properties endpoints (undocumented; reverse-engineered
  from `assets/js/listings.js` and `home.js`):**
  - `POST /getAllLoc/<term>` — area autocomplete; returns
    `[{id:"r3235", text:"Ermioni (Argolis)", areaID:..., ...}]`. The `r`
    prefix denotes a region; `l` denotes a sub-location.
  - `GET /listings_async/page/<n>/for/<1|2>/areas/r<id>` — paginated
    rendered HTML for a search. `for=1` is sale, `for=2` is rent. 12
    cards per page; iterate until empty.
  - `GET /listingsearhPath/for/sale/areas/r<id>` — converts URI params
    to a pretty slug (e.g. `sale-akiniton/ermioni`). Useful for
    extracting the area's English name.
  - `GET /property/<internal_id>` — single property page. Note: the
    `internal_id` (e.g. `7168`) and the public-facing `Code` field
    (e.g. `8208`) are **two different numbers** — both surfaced on the
    search-result card. The URL takes the internal id.
  - User-facing search-results URL is `/poliseis-akiniton/<area-name>`
    for sales or `/enoikiaseis-akiniton/<area-name>` for rentals — but
    those serve the SPA shell; data only loads via `listings_async`.
  - For Ermioni: 278 sale + 23 rent = 301 listings. Geoland's area IDs
    happen to match goutos.gr's (Ermioni=3235 on both, presumably both
    pulling from the same Greek public registry).

- **Useful goutos.gr endpoints (undocumented; reverse-engineered from
  the site's own JS):**
  - `POST /en-US/search-results` with JSON body
    `{"area":"<id>","page":<n>,"sorting":"newer"}` — paginated rendered
    HTML of the result cards. 18 cards per page; iterate `page` until empty.
  - `GET /ajax/get-areas?query=<text>` — area autocomplete; returns
    `{"areas":[{areaID, nameEN, nameGR, parentID, parentNameEN, ...}]}`.
  - `GET /ajax/get-areas-by-code?area=<id>` — areas by numeric ID
    (single ID or comma-list).
  - `POST /en-US/search-results-map` — same body as `/search-results`,
    returns map markers JSON.
  - Known top-level area IDs: 3235 = Ermioni, 3237 = Portocheli (both
    under parentID 151 = Argolis). Sub-areas under Ermioni include
    103235 Center, 119041 Kouverta, 119046 Kineta, 119047 Agioi Anargiroi,
    119053 Achladitsa.

## Conventions

- Default to a single binary per concern; add new ones via `[[bin]]`
  entries in `Cargo.toml` rather than expanding existing binaries into
  multi-mode tools.
- Print sections with the existing `=== Title ===` style so console
  output stays greppable.
- Keep the regexes as the extension point. New target sites should mostly
  mean adding key/value patterns, not new detector functions.
- Reports go to `html/<slug>.html` + `pdf/<slug>.pdf` at the repo root.
  Both directories are committed (mirroring `~/software/crawl2pump`'s
  `PDF/` convention) so the latest catalog is always visible on GitHub
  without rebuilding.
- HTML→PDF is always Chrome `--headless=new --print-to-pdf` against a
  `file://` URL. Don't reach for `wkhtmltopdf` / `weasyprint` / a Rust
  PDF crate — Chrome handles modern CSS, web fonts, and remote images
  for free, and the rest of the workspace already standardises on it.
