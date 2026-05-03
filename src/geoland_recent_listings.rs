// List property listings for a geoland.properties area, ranked by photo
// Last-Modified or by price ascending. Renders an HTML catalog under
// html/ and a printable PDF under pdf/. Mirror of `recent_listings`
// (which targets goutos.gr) but adapted to geoland's URL scheme and
// card markup. See CLAUDE.md for the reverse-engineered endpoints.

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

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) \
                  AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

const CHROME_MAC: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

const HOST: &str = "https://www.geoland.properties";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Sale,
    Rent,
}

impl Purpose {
    fn id(self) -> u8 {
        match self {
            Purpose::Sale => 1,
            Purpose::Rent => 2,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Purpose::Sale => "SALE",
            Purpose::Rent => "RENT",
        }
    }
}

#[derive(Debug, Clone)]
struct Listing {
    id: String,
    code: String,
    title: String,
    location: String,
    beds: Option<u32>,
    baths: Option<u32>,
    sqm: Option<f64>,
    parking: Option<u32>,
    price: String,
    photo_url: Option<String>,
    purpose: Purpose,
    last_modified: Option<SystemTime>,
    thumb_data_uri: Option<String>,
}

fn build_client() -> Result<Client> {
    Ok(Client::builder().timeout(Duration::from_secs(30)).build()?)
}

fn fetch_listings_page(
    client: &Client,
    area_id: &str,
    purpose: Purpose,
    page: u32,
) -> Result<String> {
    let url = format!(
        "{}/listings_async/page/{}/for/{}/areas/r{}",
        HOST,
        page,
        purpose.id(),
        area_id
    );
    let r = client
        .get(&url)
        .header(USER_AGENT, UA)
        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()?;
    if !r.status().is_success() {
        anyhow::bail!("listings_async HTTP {} for {}", r.status(), url);
    }
    Ok(r.text()?)
}

fn text_of(el: ElementRef) -> String {
    let s: String = el.text().collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_int(s: &str) -> Option<u32> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn parse_listings(html: &str, purpose: Purpose) -> Vec<Listing> {
    let doc = Html::parse_document(html);
    let card_sel = Selector::parse("div.property-item").unwrap();
    let fav_sel = Selector::parse("div.favorite-add[data-id]").unwrap();
    let img_sel = Selector::parse("img.listing-img").unwrap();
    let title_sel = Selector::parse("h2.fs-16, h2.fw-bold").unwrap();
    let loc_sel = Selector::parse("div.card-body > span").unwrap();
    let icon_sel = Selector::parse("li.icons-list span").unwrap();
    let price_sel = Selector::parse("div.price-p span.fw-bold").unwrap();
    let code_sel = Selector::parse("div.prop-id span").unwrap();

    let mut out = Vec::new();
    for c in doc.select(&card_sel) {
        let id = c
            .select(&fav_sel)
            .next()
            .and_then(|e| e.value().attr("data-id"))
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let title = c.select(&title_sel).next().map(text_of).unwrap_or_default();
        let location = c.select(&loc_sel).next().map(text_of).unwrap_or_default();
        let price = c.select(&price_sel).next().map(text_of).unwrap_or_default();
        let code_text = c.select(&code_sel).next().map(text_of).unwrap_or_default();
        let code = code_text.trim_start_matches("Code").trim().to_string();
        let photo_url = c
            .select(&img_sel)
            .next()
            .and_then(|e| e.value().attr("src"))
            .map(|s| s.to_string());

        let mut beds = None;
        let mut baths = None;
        let mut sqm = None;
        let mut parking = None;
        for icon in c.select(&icon_sel) {
            let t = text_of(icon).to_lowercase();
            if beds.is_none() && t.contains("bed") {
                beds = parse_int(&t);
            } else if baths.is_none() && (t.contains("bath") || t.contains("wc")) {
                baths = parse_int(&t);
            } else if parking.is_none() && (t.contains("spot") || t.contains("parking")) {
                parking = parse_int(&t);
            } else if sqm.is_none() && (t.contains("sq.m") || t.contains("sqm") || t.contains("m²")) {
                sqm = t
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect::<String>()
                    .parse()
                    .ok();
            }
        }

        out.push(Listing {
            id,
            code,
            title,
            location,
            beds,
            baths,
            sqm,
            parking,
            price,
            photo_url,
            purpose,
            last_modified: None,
            thumb_data_uri: None,
        });
    }
    out
}

fn head_last_modified(client: &Client, url: &str) -> Option<SystemTime> {
    let r = client.head(url).timeout(Duration::from_secs(10)).send().ok()?;
    let v = r.headers().get("Last-Modified")?;
    let s = v.to_str().ok()?;
    httpdate::parse_http_date(s).ok()
}

fn enrich_with_dates(client: &Client, listings: &mut [Listing]) {
    let concurrency = 12usize;
    let tasks: Vec<(usize, String)> = listings
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.photo_url.clone().map(|u| (i, u)))
        .collect();
    if tasks.is_empty() {
        return;
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
        listings[i].last_modified = lm;
    }
}

/// Geoland's `listing-img` is the FULL-SIZE original (~2 MB JPEG), not a
/// thumbnail. Inlining 300+ of those raw would balloon the HTML past
/// 400 MB. Download → decode → resize to ≤600 px (Lanczos3) → re-encode
/// JPEG at quality 70 → base64. Result is ~30-70 KB per thumbnail.
fn fetch_as_data_uri(client: &Client, url: &str) -> Option<String> {
    use base64::Engine;
    use image::ImageReader;
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    use std::io::Cursor;

    let resp = client.get(url).timeout(Duration::from_secs(20)).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().ok()?;

    let img = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let resized = img.resize(600, 600, FilterType::Lanczos3);

    let mut out = Vec::with_capacity(64 * 1024);
    {
        let mut encoder = JpegEncoder::new_with_quality(&mut out, 70);
        encoder
            .encode_image(&resized)
            .ok()?;
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&out);
    Some(format!("data:image/jpeg;base64,{}", b64))
}

fn inline_thumbnails(client: &Client, listings: &mut [Listing]) {
    let concurrency = 12usize;
    let tasks: Vec<(usize, String)> = listings
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.photo_url.clone().map(|u| (i, u)))
        .collect();
    if tasks.is_empty() {
        return;
    }
    eprintln!("Inlining {} thumbnails as data URIs...", tasks.len());

    let tasks = Arc::new(tasks);
    let results: Mutex<Vec<(usize, Option<String>)>> =
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
                    let uri = fetch_as_data_uri(client, url);
                    local.push((*li, uri));
                    idx += concurrency;
                }
                results.lock().unwrap().extend(local);
            });
        }
    });

    let mut ok = 0usize;
    for (i, uri) in results.into_inner().unwrap() {
        if uri.is_some() {
            ok += 1;
        }
        listings[i].thumb_data_uri = uri;
    }
    eprintln!("  inlined {} / {} thumbnails", ok, tasks.len());
}

/// "1.250.000 €" -> Some(1_250_000); "650 €" -> Some(650).
fn parse_price(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn fmt_amenities(l: &Listing) -> String {
    let mut parts = Vec::new();
    if let Some(b) = l.beds {
        parts.push(format!("{} bed", b));
    }
    if let Some(b) = l.baths {
        parts.push(format!("{} bath", b));
    }
    if let Some(p) = l.parking {
        parts.push(format!("{} park", p));
    }
    if let Some(sq) = l.sqm {
        parts.push(format!("{:.0} m²", sq));
    }
    parts.join(" · ")
}

fn render_html(
    listings: &[Listing],
    area_label: &str,
    area_id: &str,
    scan_at: DateTime<Utc>,
    sort_label: &str,
) -> String {
    let n_sale = listings.iter().filter(|l| l.purpose == Purpose::Sale).count();
    let n_rent = listings.iter().filter(|l| l.purpose == Purpose::Rent).count();
    let mut s = String::new();
    s.push_str(&format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Geoland — {label} (area {id})</title>
<style>
  @page {{ size: A4; margin: 12mm; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Helvetica Neue", Arial, sans-serif; color: #222; margin: 0; }}
  header {{ border-bottom: 2px solid #333; padding: 0 0 8px; margin: 0 0 18px; }}
  header h1 {{ font-size: 20px; margin: 0 0 4px; }}
  header .meta {{ color: #666; font-size: 11px; }}
  .card {{ display: flex; gap: 14px; padding: 12px 0; border-bottom: 1px solid #ddd; page-break-inside: avoid; break-inside: avoid; }}
  .thumb {{ width: 220px; height: 165px; object-fit: cover; background: #f2f2f2; border-radius: 4px; flex-shrink: 0; }}
  .body {{ flex: 1; min-width: 0; }}
  .body h2 {{ font-size: 15px; margin: 0 0 4px; }}
  .body h2 a {{ color: #1a3d80; text-decoration: none; }}
  .body .loc {{ color: #444; font-size: 12px; margin: 0 0 4px; }}
  .body .price {{ font-weight: 700; color: #1a3d80; font-size: 15px; margin: 0 0 4px; }}
  .body .amen {{ color: #555; font-size: 12px; margin: 0 0 6px; }}
  .body .dates {{ color: #888; font-size: 11px; }}
  .body .id {{ color: #aaa; font-size: 10px; margin: 4px 0 0; }}
  .badge {{ display: inline-block; font-size: 10px; padding: 2px 6px; border-radius: 3px; vertical-align: middle; margin-right: 4px; }}
  .badge.sale {{ background: #e8f0fb; color: #1a3d80; }}
  .badge.rent {{ background: #fdecec; color: #b22222; }}
  .empty {{ background: #f8f8f8; color: #aaa; display: flex; align-items: center; justify-content: center; }}
</style>
</head>
<body>
<header>
  <h1>Geoland — {label} (area {id})</h1>
  <div class="meta">{n} listings ({nsale} sale · {nrent} rent) · sorted by {sort} · scan {ts} UTC · source geoland.properties</div>
</header>
"#,
        label = html_escape(area_label),
        id = html_escape(area_id),
        n = listings.len(),
        nsale = n_sale,
        nrent = n_rent,
        sort = html_escape(sort_label),
        ts = scan_at.format("%Y-%m-%d %H:%M"),
    ));

    for l in listings {
        let thumb_html = match l
            .thumb_data_uri
            .as_deref()
            .or_else(|| l.photo_url.as_deref())
        {
            Some(u) => format!(r#"<img class="thumb" src="{}" alt="">"#, html_escape(u)),
            None => r#"<div class="thumb empty">no photo</div>"#.to_string(),
        };
        let badge_class = if l.purpose == Purpose::Sale {
            "sale"
        } else {
            "rent"
        };
        s.push_str(&format!(
            r#"<div class="card">
  {thumb}
  <div class="body">
    <h2><span class="badge {bclass}">{purpose}</span><a href="{host}/property/{id}">{title}</a></h2>
    <p class="loc">{loc}</p>
    <p class="price">{price}</p>
    <p class="amen">{amen}</p>
    <p class="dates">photo: 1 · last upload {when}</p>
    <p class="id">ID {id} · Code {code}</p>
  </div>
</div>
"#,
            thumb = thumb_html,
            host = HOST,
            bclass = badge_class,
            purpose = l.purpose.label(),
            id = html_escape(&l.id),
            code = html_escape(&l.code),
            title = html_escape(if l.title.is_empty() {
                "(no title)"
            } else {
                &l.title
            }),
            loc = html_escape(&l.location),
            price = html_escape(&l.price),
            amen = html_escape(&fmt_amenities(l)),
            when = fmt_time(l.last_modified),
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
    let slug = format!("geoland-{}-{}", area_slug, sort.slug());
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
            "--virtual-time-budget=60000",
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

fn fetch_purpose(
    client: &Client,
    area_id: &str,
    purpose: Purpose,
    max_pages: u32,
) -> Result<Vec<Listing>> {
    let mut out = Vec::new();
    for p in 1..=max_pages {
        let html = fetch_listings_page(client, area_id, purpose, p)?;
        let got = parse_listings(&html, purpose);
        eprintln!("  {} page {}: {} listings", purpose.label(), p, got.len());
        if got.is_empty() {
            break;
        }
        out.extend(got);
    }
    Ok(out)
}

/// Resolve the area's English display name by hitting the slug-builder
/// endpoint and reading the last URL segment (e.g. `sale-akiniton/ermioni`
/// → `ermioni` → `Ermioni`). Falls back to `area-<id>` if anything fails.
fn fetch_area_name(client: &Client, area_id: &str) -> Option<String> {
    let url = format!(
        "{}/listingsearhPath/for/sale/areas/r{}",
        HOST, area_id
    );
    let resp = client.get(&url).timeout(Duration::from_secs(10)).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().ok()?;
    let last = body.trim().rsplit('/').next()?.to_string();
    if last.is_empty() {
        return None;
    }
    let mut chars = last.chars();
    let first = chars.next()?.to_uppercase().next().unwrap_or(' ');
    Some(format!("{}{}", first, chars.collect::<String>()))
}

fn main() -> Result<()> {
    let mut area = "3235".to_string(); // Ermioni
    let mut top: Option<usize> = None;
    let mut sort = Sort::PriceAsc;
    let mut max_pages: u32 = 50;
    let mut purposes: Vec<Purpose> = vec![Purpose::Sale, Purpose::Rent];
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--area" => area = args.next().expect("--area needs a value"),
            "--top" => top = Some(args.next().expect("--top needs a value").parse()?),
            "--sort" => sort = Sort::parse(&args.next().expect("--sort needs a value"))?,
            "--max-pages" => max_pages = args.next().expect("--max-pages needs a value").parse()?,
            "--sale-only" => purposes = vec![Purpose::Sale],
            "--rent-only" => purposes = vec![Purpose::Rent],
            "-h" | "--help" => {
                println!(
                    "geoland_recent_listings [--area <id>] [--top <n>] [--sort latest|price-asc] [--sale-only|--rent-only]"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown arg: {}", other),
        }
    }

    let client = build_client()?;
    let area_label = fetch_area_name(&client, &area).unwrap_or_else(|| format!("area-{}", area));
    eprintln!(
        "Fetching geoland listings for {} (area=r{})",
        area_label, area
    );

    let mut listings = Vec::new();
    for p in &purposes {
        listings.extend(fetch_purpose(&client, &area, *p, max_pages)?);
    }
    eprintln!("  total: {} listings", listings.len());

    enrich_with_dates(&client, &mut listings);

    match sort {
        Sort::Latest => {
            listings.sort_by(|a, b| match (a.last_modified, b.last_modified) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }
        Sort::PriceAsc => {
            listings.sort_by(|a, b| match (parse_price(&a.price), parse_price(&b.price)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }
    }

    let scan_at: DateTime<Utc> = Utc::now();
    let limit = top.unwrap_or(listings.len());
    let mut display: Vec<Listing> = listings.iter().take(limit).cloned().collect();
    inline_thumbnails(&client, &mut display);

    println!(
        "\n=== Geoland listings for {} (area=r{}) — sorted by {} ===\n",
        area_label,
        area,
        sort.label()
    );
    for l in &display {
        println!(
            "[{}] ID {} (Code {})  {}/property/{}",
            l.purpose.label(),
            l.id,
            l.code,
            HOST,
            l.id
        );
        if !l.title.is_empty() {
            println!("  {}", l.title);
        }
        if !l.location.is_empty() {
            println!("  {}", l.location);
        }
        let amen = fmt_amenities(l);
        if !amen.is_empty() {
            println!("  {}", amen);
        }
        if !l.price.is_empty() {
            println!("  {}", l.price);
        }
        println!("  last photo upload: {}", fmt_time(l.last_modified));
        println!();
    }

    write_outputs(&display, &area_label, &area, scan_at, sort)?;
    let _ = Regex::new("dummy"); // ensure regex import used (kept for symmetry with other binary)
    Ok(())
}
