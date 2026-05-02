// Inspect a goutos.gr property listing for any date metadata.
//
// Checks, in order:
//   1. Response headers (Last-Modified, ETag, Date, Set-Cookie)
//   2. JSON-LD blocks (datePublished, dateCreated, dateModified, etc.)
//   3. <meta> tags (og:updated_time, article:published_time, etc.)
//   4. Inline <script> blocks for date-like fields
//   5. Backend API endpoints linked from the page (ilist / e-agents CDN)
//
// Usage:
//   cargo run --bin inspect_listing
//   cargo run --bin inspect_listing -- https://www.goutos.gr/en-US/property/500193

use std::collections::BTreeSet;
use std::env;
use std::time::Duration;

use anyhow::Result;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT_LANGUAGE, HeaderMap, USER_AGENT};
use scraper::{Html, Selector};
use serde_json::Value;

const DEFAULT_URL: &str = "https://www.goutos.gr/en-US/property/500193";

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) \
                  AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

fn date_keys_re() -> Regex {
    Regex::new(
        r"(?i)(datePublished|dateCreated|dateModified|publishDate|publishedAt|\
          createdAt|created_at|insertDate|insert_date|listingDate|listedOn|\
          updatedAt|updated_at|lastModified|last_modified|firstPublished)",
    )
    .unwrap()
}

fn date_value_re() -> Regex {
    Regex::new(
        r"\b(20\d{2}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2})?(?:Z|[+-]\d{2}:?\d{2})?)?\
          |\d{1,2}/\d{1,2}/20\d{2}\
          |\d{1,2}-\d{1,2}-20\d{2})\b",
    )
    .unwrap()
}

fn section(title: &str) {
    println!("\n=== {} ===", title);
}

// Slice a &str safely on char boundaries, used for snippet windows.
fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    let start = (0..=start).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    let end = (end.min(s.len())..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    &s[start..end]
}

fn show_headers(headers: &HeaderMap, cookies: &[(String, String)]) {
    section("Response headers of interest");
    for k in [
        "Date",
        "Last-Modified",
        "ETag",
        "Server",
        "Set-Cookie",
        "X-Powered-By",
    ] {
        for v in headers.get_all(k).iter() {
            if let Ok(s) = v.to_str() {
                println!("  {}: {}", k, s);
            }
        }
    }

    section("All cookies received");
    if cookies.is_empty() {
        println!("  (none)");
    } else {
        for (name, value) in cookies {
            println!("  {} = {}", name, value);
        }
    }
}

fn show_jsonld(html: &Html) {
    section("JSON-LD blocks");
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).unwrap();
    let blocks: Vec<_> = html.select(&sel).collect();
    if blocks.is_empty() {
        println!("  (none found)");
        return;
    }
    let dk = date_keys_re();
    for (i, b) in blocks.iter().enumerate() {
        let raw: String = b.text().collect::<String>().trim().to_string();
        let pretty = match serde_json::from_str::<Value>(&raw) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(raw.clone()),
            Err(_) => raw.clone(),
        };
        println!("  -- block #{} --", i + 1);
        let hits: BTreeSet<String> = dk
            .find_iter(&pretty)
            .map(|m| m.as_str().to_string())
            .collect();
        if !hits.is_empty() {
            let v: Vec<_> = hits.into_iter().collect();
            println!("  date-like keys present: {:?}", v);
        }
        println!("{}", safe_slice(&pretty, 0, 2000));
    }
}

fn show_meta_tags(html: &Html) {
    section("Meta tags with date hints");
    let sel = Selector::parse("meta").unwrap();
    let dk = date_keys_re();
    let mut found = false;
    for m in html.select(&sel) {
        let attrs_str = m
            .value()
            .attrs()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(" ");
        let lower = attrs_str.to_lowercase();
        if dk.is_match(&attrs_str) || lower.contains("time") || lower.contains("date") {
            println!("  <meta {}>", attrs_str);
            found = true;
        }
    }
    if !found {
        println!("  (none)");
    }
}

fn scan_inline_scripts(html: &Html, raw_html: &str) {
    section("Inline <script> hits for date-like keys");
    let sel = Selector::parse("script").unwrap();
    let dk = date_keys_re();
    let mut found = false;
    for s in html.select(&sel) {
        let text: String = s.text().collect();
        if text.is_empty() {
            continue;
        }
        for m in dk.find_iter(&text) {
            let start = m.start().saturating_sub(20);
            let end = m.end() + 80;
            let snippet = safe_slice(&text, start, end).replace('\n', " ");
            println!("  …{}…", snippet);
            found = true;
        }
    }
    if !found {
        println!("  (no date-like keys found in inline scripts)");
    }

    section("Any ISO/dd-mm-yyyy date-looking strings anywhere in the HTML");
    let dv = date_value_re();
    let dates: BTreeSet<String> = dv
        .find_iter(raw_html)
        .map(|m| m.as_str().to_string())
        .collect();
    if dates.is_empty() {
        println!("  (none)");
    } else {
        for d in &dates {
            println!("  {}", d);
        }
    }
}

fn find_backend_endpoints(raw_html: &str) -> Vec<String> {
    section("Backend / CDN URLs referenced by the page");
    let url_re = Regex::new(r#"https?://[^\s"'<>]+"#).unwrap();
    let urls: BTreeSet<String> = url_re
        .find_iter(raw_html)
        .map(|m| m.as_str().to_string())
        .collect();
    let interesting: Vec<String> = urls
        .into_iter()
        .filter(|u| {
            u.contains("ilist") || u.contains("e-agents") || u.contains("/api/") || u.ends_with(".json")
        })
        .collect();
    if interesting.is_empty() {
        println!("  (no obvious API/CDN endpoints found)");
    } else {
        for u in interesting.iter().take(30) {
            println!("  {}", u);
        }
    }
    interesting
}

fn try_backend_json(client: &Client, urls: &[String], property_code: &str) {
    section("Probing for a JSON endpoint with property data");
    let mut candidates: Vec<String> = urls.iter().filter(|u| u.ends_with(".json")).cloned().collect();
    candidates.extend([
        format!(
            "https://ilist-cdn.e-agents.cloud/appFol/appDetails/estate/fol{}.json",
            property_code
        ),
        format!(
            "https://ilist-cdn.e-agents.cloud/appFol/appDetails/fol{}.json",
            property_code
        ),
        format!("https://www.goutos.gr/api/property/{}", property_code),
        format!("https://www.goutos.gr/en-US/property/{}.json", property_code),
    ]);

    let dk = date_keys_re();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for u in candidates {
        if !seen.insert(u.clone()) {
            continue;
        }
        match client.get(&u).timeout(Duration::from_secs(10)).send() {
            Err(e) => {
                println!("  {} -> {}", u, e);
            }
            Ok(resp) => {
                let status = resp.status();
                let ct = resp
                    .headers()
                    .get("Content-Type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                println!("  {} -> {} {}", u, status.as_u16(), ct);
                if status.is_success() && ct.contains("json") {
                    let body = resp.text().unwrap_or_default();
                    match serde_json::from_str::<Value>(&body) {
                        Ok(v) => {
                            let pretty =
                                serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.clone());
                            let hits: BTreeSet<String> = dk
                                .find_iter(&pretty)
                                .map(|m| m.as_str().to_string())
                                .collect();
                            if !hits.is_empty() {
                                let v: Vec<_> = hits.into_iter().collect();
                                println!("     date-like keys: {:?}", v);
                            }
                            println!("{}", safe_slice(&pretty, 0, 2000));
                        }
                        Err(_) => println!("     (response was not valid JSON)"),
                    }
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let url = env::args().nth(1).unwrap_or_else(|| DEFAULT_URL.to_string());
    println!("Fetching {}", url);

    let client = Client::builder()
        .cookie_store(true)
        .timeout(Duration::from_secs(20))
        .build()?;

    let resp = client
        .get(&url)
        .header(USER_AGENT, UA)
        .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status);
    }
    let headers = resp.headers().clone();
    let cookies: Vec<(String, String)> = resp
        .cookies()
        .map(|c| (c.name().to_string(), c.value().to_string()))
        .collect();
    let body = resp.text()?;
    println!("  HTTP {}, {} bytes", status.as_u16(), body.len());

    show_headers(&headers, &cookies);

    let html = Html::parse_document(&body);
    show_jsonld(&html);
    show_meta_tags(&html);
    scan_inline_scripts(&html, &body);

    let code_re = Regex::new(r"/property/(\d+)").unwrap();
    let code = code_re
        .captures(&url)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "500193".to_string());
    let backend_urls = find_backend_endpoints(&body);
    try_backend_json(&client, &backend_urls, &code);

    Ok(())
}
