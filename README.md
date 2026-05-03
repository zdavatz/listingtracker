# listingtracker

Track real estate listings — extract date and metadata signals from listing
pages so you can tell when a property was first published and when it was last
refreshed.

## Build

```
cargo build --release
```

## Tools

### `inspect_listing`

Probe a property URL for any date metadata. Checks response headers, JSON-LD
blocks, `<meta>` tags, inline `<script>` content, ISO/dd-mm-yyyy date strings
in the HTML, and any backend/CDN endpoints linked from the page (with a few
educated guesses at JSON endpoints).

```
cargo run --release --bin inspect_listing
cargo run --release --bin inspect_listing -- https://www.goutos.gr/en-US/property/500193
```

### `recent_listings`

Walk every listing in a goutos.gr area, rank them by the **latest CDN photo
upload date** (which the goutos.gr listing HTML doesn't expose directly — see
below) or by **price ascending**, and write a printable catalog to
`html/<area>-<sort>.html` and `pdf/<area>-<sort>.pdf`.

```
cargo run --release --bin recent_listings                          # Ermioni, latest-photo first
cargo run --release --bin recent_listings -- --sort price-asc      # Ermioni, cheapest first
cargo run --release --bin recent_listings -- --area 3237           # Portocheli
cargo run --release --bin recent_listings -- --top 20              # only render top 20
```

Sort options:

- `--sort latest` (default): newest photo upload first.
- `--sort price-asc`: cheapest first; "Price upon request" entries sort
  last. Note that some agents post `1 €` as a placeholder for "ask for
  price" on rentals — those will appear at the very top.

Defaults to all pages of the area's listings (currently 11 pages / 190
listings for Ermioni). PDF rendering uses headless Chrome via `--print-to-pdf`;
override the binary path with `CHROME=/path/to/chrome` if it isn't at
`/Applications/Google Chrome.app/...`.

Listings with zero photos in their search-result card automatically fall
back to scraping their detail page so they still get a real photo set
to rank by.

Latest committed Ermioni catalogs:

- `html/ermioni-recent.html`, `pdf/ermioni-recent.pdf` — sorted by recency
- `html/ermioni-price-asc.html`, `pdf/ermioni-price-asc.pdf` — sorted by price ascending

### `geoland_recent_listings`

Same idea as `recent_listings`, but targets [geoland.properties](https://www.geoland.properties)
(another Greek real estate agency, also covering Ermioni). Their search-result
cards expose richer per-listing data than goutos (title, location, beds, baths,
parking, sqm, price all inline) and each card carries a single full-size photo
which we resize to ≤600 px (Lanczos3 via the `image` crate) before base64-inlining
into the catalog.

```
cargo run --release --bin geoland_recent_listings                    # Ermioni, both sale + rent, price-asc
cargo run --release --bin geoland_recent_listings -- --sort latest   # newest photo first
cargo run --release --bin geoland_recent_listings -- --sale-only     # exclude rentals
```

Defaults to `--sort price-asc` (geoland mixes per-month rentals and one-off
agency `1 €` placeholders into the same list, so the very top of the
ascending sort is mostly junk — use `--sale-only` for a cleaner view, or just
read past the placeholders).

Latest committed Ermioni catalogs:

- `html/geoland-ermioni-recent.html`, `pdf/geoland-ermioni-recent.pdf`
- `html/geoland-ermioni-price-asc.html`, `pdf/geoland-ermioni-price-asc.pdf`

For Ermioni: 301 listings (278 sale + 23 rent) — significantly more than
goutos's 190.

## What we've learned about goutos.gr (e-agents CMS)

The HTML for a listing carries **no** date metadata: no `Last-Modified`, no
`ETag`, no JSON-LD, no `<meta>` date tags, no inline state with date keys, no
date-looking strings anywhere in the body. There is also no `sitemap.xml` and
no `robots.txt`, and the Wayback Machine has no captures.

The signal that does work is the **photo CDN**
(`ilist-cdn.e-agents.cloud`, Cloudflare-fronted), which returns a
`Last-Modified` header per JPEG. Each photo's upload date is a tight lower
bound on when the listing existed in its current form, and photos typically
cluster into a few discrete upload batches:

- earliest batch ≈ when the listing was first published
- subsequent batches ≈ photo refreshes / reshoots

For property `500193` this gives:

| Date (UTC)              | Photos | Likely meaning              |
|-------------------------|--------|-----------------------------|
| 2024-09-19, 12:24       | 5      | Original listing creation   |
| 2025-05-07, 13:25–14:37 | 5      | First photo refresh         |
| 2025-06-30, 13:36–13:45 | 15     | Major reshoot (current set) |
