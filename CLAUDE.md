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
cargo run --release --bin baugeschichte -- [--lang de|el|both]
```

The `*_recent_listings` binaries require headless Chrome to print PDFs. They look at
`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome` by default;
override with `CHROME=/path/to/chrome`.

`baugeschichte` is the exception: it builds its PDF in **pure Rust via `genpdf`**
(no Chrome), so it has no Chrome dependency — see its section below.

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

### `src/baugeschichte.rs` — Erica's house photo-documentation PDF

Unrelated to listing scraping: turns the WhatsApp messages Erica Baumann sent
about buying and rebuilding her house in Ermioni (synced into
`erica-house/messages.json` by the pegelstand WhatsApp toolchain) into a titled
photo book — German (`baugeschichte.pdf`) and Greek (`baugeschichte_gr.pdf`).

Pipeline:

1. Parse `erica-house/messages.json` (`matches[0].messages`) with `serde_json`,
   keep Erica's own messages (`!fromMe`) sorted by `ts`. First text note → intro
   blockquote; remaining text notes → closing note; messages with an image
   `file` → photo plates in order.
2. Build the document with `genpdf` (writes PDF via `printpdf` — **pure Rust, no
   Chrome**). DejaVu Sans family (Regular/Bold/Oblique/BoldOblique) is embedded
   from `$FONT_DIR` (default `/usr/share/fonts/dejavu`) because it covers Latin
   + Greek + umlauts in one face.
3. One photo per page (`PageBreak` before each plate, and before the closing
   note). genpdf can neither split an image nor keep a group together, so a tall
   portrait photo starting mid-page would overflow the bottom edge — giving each
   plate its own page is the fix.
4. Greek output: a static `TR` map (German-trimmed → Greek) translates the
   cover, intro, every caption and the closing note. Unmapped text falls through
   unchanged so nothing is silently dropped. The `Lang` enum carries all the
   static UI strings.
5. Clickable cover links (`add_cover_links`): genpdf 0.2 can't emit hyperlinks,
   so after `render_to_file` the PDF is reopened with `lopdf` and `/Link` URI
   annotations are overlaid on the Maps + goutos URL lines. The two URL lines
   are the only text on page 1 drawn at `COVER_LINK_FONT_SIZE` (10 pt), so they
   are located by scanning the content stream for `Td`/`Tm` origins at that font
   size — the glyph text is CID-encoded and unreadable, but the position
   operators are plain numbers. Lines are centred, so the link rect spans
   `x … (A4_WIDTH_PT − x)`. Keep the layout font size and the const in sync.

Non-obvious gotchas:

- **Image size.** `printpdf 0.3` (pinned by genpdf 0.2) embeds photos as a raw
  raster (Flate-compressed, no JPEG passthrough), so PDF size tracks pixel
  count, not source JPEG bytes. Photos are downscaled to a 1100 px long edge
  (Lanczos3) before embedding — that's ~150 dpi at full display size and keeps
  each PDF ~23 MB instead of ~46 MB. The result is still much larger than the
  old Chrome/weasyprint version (2.8 MB, real embedded JPEG); that's the price
  of dropping the Chrome dependency.
- **image-crate version skew.** This project uses `image` 0.25; genpdf 0.2 uses
  `image` 0.23 internally, so a `DynamicImage` can't be passed across. We
  resize with 0.25, re-encode to in-memory JPEG bytes, and hand those to
  `genpdf::elements::Image::from_reader` (genpdf decodes them itself).
- **DPI = fit, not quality.** `dpi_for` picks the DPI that makes a photo fit the
  printable box (cap on the more constraining dimension), so genpdf scales it
  down to the page rather than rendering at native size.

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
- For **catalog** PDFs (the `*_recent_listings` binaries) HTML→PDF is always
  Chrome `--headless=new --print-to-pdf` against a `file://` URL. Don't reach
  for `wkhtmltopdf` / `weasyprint` there — Chrome handles modern CSS, web
  fonts, and remote images for free, and the catalogs already standardise on
  it. The one exception is `baugeschichte`, which is deliberately pure-Rust
  (`genpdf`, no Chrome) at the cost of a simpler layout and larger files;
  reach for genpdf only when a no-Chrome dependency is the explicit goal.
