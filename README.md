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
below), and write a printable catalog to `html/<area>-recent.html` and
`pdf/<area>-recent.pdf`.

```
cargo run --release --bin recent_listings                       # default: Ermioni (area 3235), all pages
cargo run --release --bin recent_listings -- --area 3237        # Portocheli
cargo run --release --bin recent_listings -- --top 20           # only render the 20 most recent
```

Defaults to all pages of the area's "newer" sort (currently 11 pages / ~190
listings for Ermioni). PDF rendering uses headless Chrome via `--print-to-pdf`;
override the binary path with `CHROME=/path/to/chrome` if it isn't at the
default `/Applications/Google Chrome.app/...`.

Output for Ermioni is committed under `html/ermioni-recent.html` and
`pdf/ermioni-recent.pdf` and refreshed on each run.

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
