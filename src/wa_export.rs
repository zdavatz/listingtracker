// wa_export — parse a WhatsApp "Export Chat" dump (iOS `_chat.txt` + media)
// and merge new messages from Erica's chat into erica-house/messages.json, the
// input the `baugeschichte` photo book reads.
//
// This is the pure-Rust replacement for the Node/Baileys live-sync path: a
// chat export needs no pairing, no QR, no live WhatsApp protocol — just a text
// file plus image files — so the whole parse can live in Rust.
//
// Export format (WhatsApp iOS, German locale):
//
//   ‎[28.06.26, 14:55:47] Erica Davatz-Baumann: Caption text ‎<Anhang: 00000270-PHOTO-...jpg>
//   [28.06.26, 14:59:36] Erica Davatz-Baumann: A text-only message that can
//   wrap onto continuation lines with no leading timestamp.
//
// Each message starts with an optional U+200E (LTR mark) then `[date, time]
// Sender: `. Lines without that prefix continue the previous message. Media is
// referenced inline as `<Anhang: FILENAME>` (English exports: `<attached: …>`).
//
// Merge rule: by default we append only messages strictly newer than the newest
// `ts` already in messages.json (so re-running is idempotent and never disturbs
// the curated existing book). `--since <epoch>` overrides the floor; `--all`
// rebuilds from scratch (rarely wanted — the export includes years of unrelated
// chat). Photos are copied into erica-house/ as `img_<ts>.jpg`; videos, PDFs and
// deleted/system messages are skipped.
//
// Usage:
//   cargo run --release --bin wa_export -- [--import <dir>] [--since <epoch>] [--all] [--dry-run]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{FixedOffset, NaiveDate, TimeZone};
use serde_json::{json, Value};

const DIR: &str = "erica-house";
const DEFAULT_IMPORT: &str = "erica-house/sync/import";
// Erica's LID as it appears on each per-message `jid` in the existing data.
const ERICA_LID: &str = "161881780133908@lid";
// Export local times are Europe/Zurich. Every message we actually merge is from
// late June (CEST = UTC+2), and the existing `ts` values were computed at +02:00
// (verified: 14:45:02 local ⇒ ts 1781786702 ⇒ 12:45:02Z). A fixed offset keeps
// us dependency-free; revisit only if we ever merge winter (CET, +01:00) dates.
const TZ_OFFSET_SECS: i32 = 2 * 3600;

#[derive(Debug)]
struct Msg {
    ts: i64,
    from_me: bool,
    sender: String,
    text: String,
    attachment: Option<String>,
}

fn parse_header(line: &str) -> Option<(i64, String, String)> {
    // strip a leading U+200E if present
    let l = line.strip_prefix('\u{200e}').unwrap_or(line);
    if !l.starts_with('[') {
        return None;
    }
    let close = l.find(']')?;
    let stamp = &l[1..close];
    let rest = l[close + 1..].trim_start();
    // stamp = "DD.MM.YY, HH:MM:SS"
    let (date, time) = stamp.split_once(", ")?;
    let d: Vec<&str> = date.split('.').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        return None;
    }
    let day: u32 = d[0].parse().ok()?;
    let month: u32 = d[1].parse().ok()?;
    let year: i32 = 2000 + d[2].parse::<i32>().ok()?;
    let hh: u32 = t[0].parse().ok()?;
    let mm: u32 = t[1].parse().ok()?;
    let ss: u32 = t[2].parse().ok()?;
    let offset = FixedOffset::east_opt(TZ_OFFSET_SECS)?;
    let dt = offset
        .from_local_datetime(&NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hh, mm, ss)?)
        .single()?;
    // sender = up to the first ": "
    let (sender, body) = rest.split_once(": ")?;
    Some((dt.timestamp(), sender.to_string(), body.to_string()))
}

fn extract_attachment(body: &str) -> (String, Option<String>) {
    // `<Anhang: FILE>` (de) or `<attached: FILE>` (en). Drop the U+200E marks.
    for tag in ["<Anhang:", "<attached:"] {
        if let Some(start) = body.find(tag) {
            if let Some(rel_end) = body[start..].find('>') {
                let inner = &body[start + tag.len()..start + rel_end];
                let file = inner.trim().to_string();
                let mut text = String::new();
                text.push_str(&body[..start]);
                text.push_str(&body[start + rel_end + 1..]);
                let text = text.replace('\u{200e}', "").trim().to_string();
                return (text, Some(file));
            }
        }
    }
    (body.replace('\u{200e}', "").trim().to_string(), None)
}

fn parse_chat(txt: &str) -> Vec<Msg> {
    let mut out: Vec<Msg> = Vec::new();
    for raw in txt.lines() {
        if let Some((ts, sender, body)) = parse_header(raw) {
            let (text, attachment) = extract_attachment(&body);
            let from_me = sender.starts_with("Zeno");
            out.push(Msg { ts, from_me, sender, text, attachment });
        } else if let Some(last) = out.last_mut() {
            // continuation line of the previous message
            let line = raw.replace('\u{200e}', "");
            if !last.text.is_empty() {
                last.text.push('\n');
            }
            last.text.push_str(&line);
            last.text = last.text.trim_end().to_string();
        }
    }
    out
}

fn is_image(name: &str) -> bool {
    let n = name.to_lowercase();
    n.ends_with(".jpg") || n.ends_with(".jpeg") || n.ends_with(".png")
}

fn is_deleted_or_system(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t.contains("wurde gelöscht")
        || t.contains("This message was deleted")
        || t.contains("Ende-zu-Ende-verschlüsselt")
        || t.contains("end-to-end encrypted")
}

fn iso_utc(ts: i64) -> String {
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut import = PathBuf::from(DEFAULT_IMPORT);
    let mut since: Option<i64> = None;
    let mut all = false;
    let mut dry = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--import" => { import = PathBuf::from(args.get(i + 1).ok_or_else(|| anyhow!("--import needs a path"))?); i += 1; }
            "--since" => { since = Some(args.get(i + 1).ok_or_else(|| anyhow!("--since needs an epoch"))?.parse()?); i += 1; }
            "--all" => all = true,
            "--dry-run" => dry = true,
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }

    let chat_path = import.join("_chat.txt");
    let txt = fs::read_to_string(&chat_path)
        .with_context(|| format!("read {}", chat_path.display()))?;
    let parsed = parse_chat(&txt);
    eprintln!("=== Parsed {} messages from {} ===", parsed.len(), chat_path.display());

    // Load existing messages.json
    let msg_json = Path::new(DIR).join("messages.json");
    let mut root: Value = serde_json::from_str(&fs::read_to_string(&msg_json)?)
        .with_context(|| format!("parse {}", msg_json.display()))?;
    let existing = root["matches"][0]["messages"]
        .as_array()
        .ok_or_else(|| anyhow!("messages.json: matches[0].messages missing"))?
        .clone();
    let existing_ts: BTreeSet<i64> = existing.iter().filter_map(|m| m["ts"].as_i64()).collect();
    let max_ts = existing_ts.iter().copied().max().unwrap_or(0);

    let floor = if all { i64::MIN } else { since.unwrap_or(max_ts) };
    eprintln!(
        "=== Merge floor ts={} ({}) — existing {} messages, newest {} ===",
        floor,
        if floor == i64::MIN { "ALL".into() } else { iso_utc(floor) },
        existing.len(),
        iso_utc(max_ts),
    );

    let mut added: Vec<Value> = Vec::new();
    let mut copied = 0usize;
    let mut skipped_video = 0usize;
    for m in &parsed {
        if m.from_me {
            continue; // the book uses only Erica's messages
        }
        if m.ts <= floor || existing_ts.contains(&m.ts) {
            continue;
        }
        // Determine image attachment
        let (typ, file, bytes, media): (&str, Option<String>, Option<u64>, Value) = match &m.attachment {
            Some(name) if is_image(name) => {
                let src = import.join(name);
                let dst_name = format!("img_{}.jpg", m.ts);
                let dst = Path::new(DIR).join(&dst_name);
                let len = match fs::metadata(&src) {
                    Ok(md) => md.len(),
                    Err(_) => { eprintln!("[warn] missing image file: {}", src.display()); continue; }
                };
                if !dry {
                    fs::copy(&src, &dst).with_context(|| format!("copy {}", src.display()))?;
                }
                copied += 1;
                ("image", Some(dst_name), Some(len), json!({"mimetype": "image/jpeg"}))
            }
            Some(_name) => { skipped_video += 1; continue; } // video / pdf / other attachment
            None => {
                if is_deleted_or_system(&m.text) { continue; }
                ("text", None, None, Value::Null)
            }
        };

        let mut rec = json!({
            "id": format!("{}-0", m.ts),
            "jid": ERICA_LID,
            "ts": m.ts,
            "iso": iso_utc(m.ts),
            "fromMe": false,
            "sender": m.sender,
            "type": typ,
            "text": m.text,
            "media": media,
        });
        if let (Some(f), Some(b)) = (file, bytes) {
            rec["file"] = json!(f);
            rec["bytes"] = json!(b);
        }
        eprintln!(
            "[+] {} {} {}{}",
            iso_utc(m.ts),
            typ,
            rec["file"].as_str().unwrap_or(""),
            if m.text.is_empty() { String::new() } else { format!("  “{}”", m.text.chars().take(50).collect::<String>()) },
        );
        added.push(rec);
    }

    eprintln!(
        "=== {} new message(s): {} photo(s) copied, {} video/other attachment(s) skipped ===",
        added.len(), copied, skipped_video
    );

    if added.is_empty() {
        eprintln!("Nothing to merge.");
        return Ok(());
    }
    if dry {
        eprintln!("--dry-run: messages.json not written.");
        return Ok(());
    }

    let mut merged = existing;
    merged.extend(added);
    merged.sort_by_key(|m| m["ts"].as_i64().unwrap_or(0));
    let count = merged.len();
    root["matches"][0]["messages"] = Value::Array(merged);
    root["matches"][0]["count"] = json!(count);
    fs::write(&msg_json, serde_json::to_string_pretty(&root)?)?;
    eprintln!("Wrote {} ({} total messages).", msg_json.display(), count);
    Ok(())
}
