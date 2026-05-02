// List the most recent property listings for a goutos.gr area, ranked by
// the latest CDN photo Last-Modified across each listing's photo set.
// Renders an HTML catalog under html/ and a printable PDF under pdf/.
//
// Usage:
//   cargo run --release --bin recent_listings              # default: Ermioni, all pages
//   cargo run --release --bin recent_listings -- --area 3235 --pages 11
//   cargo run --release --bin recent_listings -- --pages 2 --top 10

use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_LANGUAGE, USER_AGENT};
use scraper::{ElementRef, Html, Selector};
use serde_json::json;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) \
                  AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

const CHROME_MAC: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

#[derive(Debug, Clone, Copy)]
enum Sort {
    Latest,
    PriceAsc,
}

impl Sort {
    fn slug(self) -> &'static str {
        match self {
            Sort::Latest => "recent",
            Sort::PriceAsc => "price-asc",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Sort::Latest => "latest photo upload",
            Sort::PriceAsc => "price ascending",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "latest" | "recent" => Ok(Sort::Latest),
            "price-asc" | "price" => Ok(Sort::PriceAsc),
            other => anyhow::bail!("unknown --sort value: {} (use 'latest' or 'price-asc')", other),
        }
    }
}

#[derive(Debug, Clone)]
struct Listing {
    id: String,
    title: String,
    property_type: String,
    price: String,
    detail: String,
    photo_urls: Vec<String>,
    earliest: Option<SystemTime>,
    latest: Option<SystemTime>,
}

fn build_client() -> Result<Client> {
    Ok(Client::builder().timeout(Duration::from_secs(20)).build()?)
}

fn fetch_search_page(client: &Client, area: &str, page: u32) -> Result<String> {
    let body = json!({
        "area": area,
        "page": page,
        "sorting": "newer",
    });
    let r = client
        .post("https://www.goutos.gr/en-US/search-results")
        .header(USER_AGENT, UA)
        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&body)?)
        .send()?;
    if !r.status().is_success() {
        anyhow::bail!("search-results HTTP {}", r.status());
    }
    Ok(r.text()?)
}

fn text_of(el: ElementRef) -> String {
    let s: String = el.text().collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_listings(html: &str) -> Vec<Listing> {
    let doc = Html::parse_document(html);
    let card_sel = Selector::parse("article.geodir-category-listing").unwrap();
    let img_sel = Selector::parse(".carousel-inner img").unwrap();
    let title_sel = Selector::parse("h3.title-sin_item").unwrap();
    let p_sel = Selector::parse("p").unwrap();
    let price_sel = Selector::parse(".geodir-category-content_price").unwrap();
    let detail_sel = Selector::parse(".geodir-category-content-details").unwrap();
    let id_re = Regex::new(r"/(?:en-US|el-GR)/property/(\d+)").unwrap();

    let mut out = Vec::new();
    for c in doc.select(&card_sel) {
        let html_str = c.html();
        let id = match id_re.captures(&html_str) {
            Some(m) => m[1].to_string(),
            None => continue,
        };
        let title = c.select(&title_sel).next().map(text_of).unwrap_or_default();
        // Property type is a <p> sibling/descendant just after the title.
        // Pick the first <p> with non-empty text inside the card.
        let property_type = c
            .select(&p_sel)
            .map(text_of)
            .find(|t| !t.is_empty())
            .unwrap_or_default();
        let price = c.select(&price_sel).next().map(text_of).unwrap_or_default();
        let detail = c.select(&detail_sel).next().map(text_of).unwrap_or_default();

        let photo_urls: Vec<String> = c
            .select(&img_sel)
            .filter_map(|i| i.value().attr("src"))
            .filter(|s| s.starts_with("https://ilist-cdn"))
            .map(|s| s.to_string())
            .collect();

        out.push(Listing {
            id,
            title,
            property_type,
            price,
            detail,
            photo_urls,
            earliest: None,
            latest: None,
        });
    }
    out
}

/// Fetch a property detail page and extract its full-size photo URLs from
/// the embedded `ilist-cdn` references. Used as a fallback for listings
/// whose search-results card carousel renders zero `<img>` tags.
fn fetch_detail_photos(client: &Client, listing_id: &str) -> Vec<String> {
    let url = format!("https://www.goutos.gr/en-US/property/{}", listing_id);
    let html = match client.get(&url).timeout(Duration::from_secs(15)).send() {
        Ok(r) if r.status().is_success() => r.text().unwrap_or_default(),
        _ => return Vec::new(),
    };
    let pat = format!(
        r#"https://ilist-cdn[^"'\s<>]+/fol{}/[A-Za-z0-9_-]+\.(?:jpg|jpeg|png)"#,
        regex::escape(listing_id)
    );
    let re = match Regex::new(&pat) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut seen = std::collections::BTreeSet::new();
    for m in re.find_iter(&html) {
        let s = m.as_str();
        // Prefer full-size; skip thumbs since they often share the same Last-Modified
        // but we want a stable URL set for the same listing.
        if s.contains("-thumb.") || s.contains("Thumb_") {
            continue;
        }
        seen.insert(s.to_string());
    }
    seen.into_iter().collect()
}

fn backfill_missing_photos(client: &Client, listings: &mut [Listing]) {
    let concurrency = 8usize;
    let missing: Vec<usize> = listings
        .iter()
        .enumerate()
        .filter(|(_, l)| l.photo_urls.is_empty())
        .map(|(i, _)| i)
        .collect();
    if missing.is_empty() {
        return;
    }
    eprintln!(
        "  backfilling photos from detail pages for {} listings...",
        missing.len()
    );
    let ids: Vec<(usize, String)> = missing
        .iter()
        .map(|&i| (i, listings[i].id.clone()))
        .collect();
    let ids = Arc::new(ids);
    let results: Mutex<Vec<(usize, Vec<String>)>> = Mutex::new(Vec::new());

    thread::scope(|s| {
        for w in 0..concurrency {
            let ids = Arc::clone(&ids);
            let results = &results;
            s.spawn(move || {
                let mut local = Vec::new();
                let mut idx = w;
                while idx < ids.len() {
                    let (li, id) = &ids[idx];
                    let urls = fetch_detail_photos(client, id);
                    local.push((*li, urls));
                    idx += concurrency;
                }
                results.lock().unwrap().extend(local);
            });
        }
    });

    for (i, urls) in results.into_inner().unwrap() {
        listings[i].photo_urls = urls;
    }
}

fn head_last_modified(client: &Client, url: &str) -> Option<SystemTime> {
    let r = client.head(url).timeout(Duration::from_secs(10)).send().ok()?;
    let v = r.headers().get("Last-Modified")?;
    let s = v.to_str().ok()?;
    httpdate::parse_http_date(s).ok()
}

fn enrich_with_dates(client: &Client, listings: &mut [Listing]) {
    let concurrency = 12usize;

    let mut tasks: Vec<(usize, String)> = Vec::new();
    for (i, l) in listings.iter().enumerate() {
        for u in &l.photo_urls {
            tasks.push((i, u.clone()));
        }
    }
    let tasks = Arc::new(tasks);
    let results: Mutex<Vec<(usize, Option<SystemTime>)>> =
        Mutex::new(Vec::with_capacity(tasks.len()));

    thread::scope(|s| {
        for w in 0..concurrency {
            let tasks = Arc::clone(&tasks);
            let results = &results;
            s.spawn(move || {
                let mut local = Vec::new();
                let mut idx = w;
                while idx < tasks.len() {
                    let (li, url) = &tasks[idx];
                    let lm = head_last_modified(client, url);
                    local.push((*li, lm));
                    idx += concurrency;
                }
                results.lock().unwrap().extend(local);
            });
        }
    });

    for (i, lm) in results.into_inner().unwrap() {
        let Some(t) = lm else { continue };
        let l = &mut listings[i];
        l.earliest = Some(l.earliest.map_or(t, |e| e.min(t)));
        l.latest = Some(l.latest.map_or(t, |e| e.max(t)));
    }
}

fn fmt_time(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => {
            let dt: DateTime<Utc> = t.into();
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        None => "—".to_string(),
    }
}

/// "1.250.000 €" -> Some(1_250_000); "650 €" -> Some(650); "Price upon request" -> None.
fn parse_price(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn fetch_area_name(client: &Client, area_id: &str) -> Option<String> {
    let url = format!("https://www.goutos.gr/ajax/get-areas-by-code?area={}", area_id);
    let resp = client.get(&url).timeout(Duration::from_secs(10)).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let areas = v.get("areas")?.as_array()?;
    let target_id: i64 = area_id.parse().ok()?;
    for a in areas.iter() {
        let obj = a.as_object()?;
        let aid = obj.get("areaID").and_then(|x| x.as_i64());
        if aid == Some(target_id) {
            if let Some(n) = obj.get("nameEN").and_then(|x| x.as_str()) {
                return Some(n.to_string());
            }
        }
    }
    None
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_html(
    listings: &[Listing],
    area_label: &str,
    area_id: &str,
    scan_at: DateTime<Utc>,
    sort_label: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Listings — {label} (area {id})</title>
<style>
  @page {{ size: A4; margin: 12mm; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Helvetica Neue", Arial, sans-serif; color: #222; margin: 0; }}
  header {{ border-bottom: 2px solid #333; padding: 0 0 8px; margin: 0 0 18px; }}
  header h1 {{ font-size: 20px; margin: 0 0 4px; }}
  header .meta {{ color: #666; font-size: 11px; }}
  .card {{ display: flex; gap: 14px; padding: 12px 0; border-bottom: 1px solid #ddd; page-break-inside: avoid; break-inside: avoid; }}
  .thumb {{ width: 200px; height: 150px; object-fit: cover; background: #f2f2f2; border-radius: 4px; flex-shrink: 0; }}
  .body {{ flex: 1; min-width: 0; }}
  .body h2 {{ font-size: 15px; margin: 0 0 4px; }}
  .body h2 a {{ color: #1a3d80; text-decoration: none; }}
  .body .type {{ color: #444; font-size: 12px; margin: 0 0 4px; }}
  .body .price {{ font-weight: 700; color: #1a3d80; font-size: 15px; margin: 0 0 4px; }}
  .body .detail {{ color: #555; font-size: 12px; margin: 0 0 6px; }}
  .body .dates {{ color: #888; font-size: 11px; }}
  .body .id {{ color: #aaa; font-size: 10px; margin: 4px 0 0; }}
  .empty {{ background: #f8f8f8; color: #aaa; display: flex; align-items: center; justify-content: center; }}
</style>
</head>
<body>
<header>
  <h1>Listings — {label} (area {id})</h1>
  <div class="meta">{n} listings · sorted by {sort} · scan {ts} UTC · source goutos.gr</div>
</header>
"#,
        label = html_escape(area_label),
        id = html_escape(area_id),
        n = listings.len(),
        sort = html_escape(sort_label),
        ts = scan_at.format("%Y-%m-%d %H:%M"),
    ));

    for l in listings {
        let thumb_html = match l.photo_urls.first() {
            Some(u) => format!(r#"<img class="thumb" src="{}" alt="">"#, html_escape(u)),
            None => r#"<div class="thumb empty">no photo</div>"#.to_string(),
        };
        s.push_str(&format!(
            r#"<div class="card">
  {thumb}
  <div class="body">
    <h2><a href="https://www.goutos.gr/en-US/property/{id}">{title}</a></h2>
    <p class="type">{ptype}</p>
    <p class="price">{price}</p>
    <p class="detail">{detail}</p>
    <p class="dates">photos: {nphotos} · earliest {earliest} · latest {latest}</p>
    <p class="id">ID {id}</p>
  </div>
</div>
"#,
            thumb = thumb_html,
            id = html_escape(&l.id),
            title = html_escape(if l.title.is_empty() { "(no title)" } else { &l.title }),
            ptype = html_escape(&l.property_type),
            price = html_escape(&l.price),
            detail = html_escape(&l.detail),
            nphotos = l.photo_urls.len(),
            earliest = fmt_time(l.earliest),
            latest = fmt_time(l.latest),
        ));
    }

    s.push_str("</body></html>\n");
    s
}

fn write_outputs(
    listings: &[Listing],
    area_label: &str,
    area_id: &str,
    scan_at: DateTime<Utc>,
    sort: Sort,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let html_dir = PathBuf::from("html");
    let pdf_dir = PathBuf::from("pdf");
    std::fs::create_dir_all(&html_dir)?;
    std::fs::create_dir_all(&pdf_dir)?;

    let area_slug = area_label
        .to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let slug = format!("{}-{}", area_slug, sort.slug());
    let html_path = html_dir.join(format!("{}.html", slug));
    let pdf_path = pdf_dir.join(format!("{}.pdf", slug));

    let html = render_html(listings, area_label, area_id, scan_at, sort.label());
    std::fs::write(&html_path, &html)?;
    eprintln!("wrote {}", html_path.display());

    let chrome = env::var("CHROME").unwrap_or_else(|_| CHROME_MAC.into());
    if !std::path::Path::new(&chrome).exists() {
        eprintln!(
            "skipping PDF: Chrome not found at {} (set $CHROME to override)",
            chrome
        );
        return Ok((html_path, None));
    }
    let abs = std::fs::canonicalize(&html_path)?;
    let status = Command::new(&chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-pdf-header-footer",
            &format!("--print-to-pdf={}", pdf_path.display()),
            &format!("file://{}", abs.display()),
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("Chrome --print-to-pdf exited {}", status);
    }
    eprintln!("wrote {}", pdf_path.display());
    Ok((html_path, Some(pdf_path)))
}

fn main() -> Result<()> {
    let mut area = "3235".to_string(); // Ermioni
    let mut pages: u32 = 0; // 0 = walk until empty
    let mut top: Option<usize> = None;
    let mut sort = Sort::Latest;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--area" => area = args.next().expect("--area needs a value"),
            "--pages" => pages = args.next().expect("--pages needs a value").parse()?,
            "--top" => top = Some(args.next().expect("--top needs a value").parse()?),
            "--sort" => sort = Sort::parse(&args.next().expect("--sort needs a value"))?,
            "-h" | "--help" => {
                println!(
                    "recent_listings [--area <id>] [--pages <n>] [--top <n>] [--sort latest|price-asc]"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown arg: {}", other),
        }
    }

    let client = build_client()?;
    let area_label = fetch_area_name(&client, &area).unwrap_or_else(|| format!("area-{}", area));
    eprintln!(
        "Fetching search-results for {} (area={}){}",
        area_label,
        area,
        if pages == 0 {
            " — walking all pages".to_string()
        } else {
            format!(" — pages 1..={}", pages)
        }
    );

    let max_pages = if pages == 0 { 200 } else { pages };
    let mut listings = Vec::new();
    for p in 1..=max_pages {
        let html = fetch_search_page(&client, &area, p)?;
        let mut got = parse_listings(&html);
        eprintln!("  page {}: {} listings", p, got.len());
        if got.is_empty() {
            break;
        }
        listings.append(&mut got);
    }

    backfill_missing_photos(&client, &mut listings);
    let total_photos: usize = listings.iter().map(|l| l.photo_urls.len()).sum();
    eprintln!(
        "Probing {} photos across {} listings for Last-Modified...",
        total_photos,
        listings.len()
    );
    enrich_with_dates(&client, &mut listings);

    match sort {
        Sort::Latest => {
            // Newest-first by latest photo upload; missing dates go last.
            listings.sort_by(|a, b| match (a.latest, b.latest) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }
        Sort::PriceAsc => {
            // Cheapest first; "Price upon request" / empty go last.
            listings.sort_by(|a, b| {
                match (parse_price(&a.price), parse_price(&b.price)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            });
        }
    }

    let scan_at: DateTime<Utc> = Utc::now();
    let limit = top.unwrap_or(listings.len());
    let display: Vec<Listing> = listings.iter().take(limit).cloned().collect();

    println!(
        "\n=== Listings for {} (area={}) — sorted by {} ===\n",
        area_label,
        area,
        sort.label()
    );
    for l in &display {
        println!(
            "ID {}  https://www.goutos.gr/en-US/property/{}",
            l.id, l.id
        );
        if !l.title.is_empty() {
            println!("  {}", l.title);
        }
        if !l.property_type.is_empty() {
            println!("  {}", l.property_type);
        }
        if !l.detail.is_empty() {
            println!("  {}", l.detail);
        }
        if !l.price.is_empty() {
            println!("  {}", l.price);
        }
        println!(
            "  photos: {}  earliest: {}  latest: {}",
            l.photo_urls.len(),
            fmt_time(l.earliest),
            fmt_time(l.latest)
        );
        println!();
    }

    write_outputs(&display, &area_label, &area, scan_at, sort)?;
    Ok(())
}
