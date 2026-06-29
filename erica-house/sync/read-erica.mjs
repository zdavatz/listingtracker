#!/usr/bin/env node
// Sync Erica Baumann's WhatsApp chat for the Baugeschichte photo book.
//
// Connects with the linked-device auth in ./auth (shared with the rest of the
// stack). Goes online so pending messages deliver, captures everything from
// Erica's chat, on-demand-fetches older history anchored on the newest message,
// downloads every photo inline, and prints the erica-house messages.json shape
// on stdout. Images are written to <outDir> (default: the erica-house dir, where
// baugeschichte.rs reads them).
//
// A warm reconnect of an already-paired device emits NO messaging-history.set,
// so to backfill older messages you usually need a fresh pairing: pass --repair
// to wipe ./auth and re-link (QR printed to the terminal and /tmp/wa-login-qr.png).
// Scan it with WhatsApp > Linked Devices > Link a Device; a full history sync
// (syncType=0) then delivers the whole chat.
//
// Usage:
//   node read-erica.mjs [--repair] [outDir] [waitSeconds]
//   Erica = +41 76 507 39 11  → 41765073911@s.whatsapp.net (LID 161881780133908@lid)

import makeWASocket, {
  useMultiFileAuthState,
  makeCacheableSignalKeyStore,
  fetchLatestBaileysVersion,
  downloadMediaMessage,
  DisconnectReason,
} from "@whiskeysockets/baileys";
import QRCode from "qrcode";
import qrcodeTerminal from "qrcode-terminal";
import pino from "pino";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import { mkdirSync, writeFileSync, rmSync } from "fs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const AUTH_DIR = resolve(__dirname, "auth");
const QR_PNG = "/tmp/wa-login-qr.png";
const logger = pino({ level: "silent" });

let argv = process.argv.slice(2);
const repair = argv.includes("--repair");
// --pair <number>: link via an 8-char pairing code typed into WhatsApp
// (Linked Devices > Link a Device > "Link with phone number instead") instead
// of scanning a QR. <number> is the account's own number, digits only with
// country code (e.g. 41792364544). More reliable than QR in a headless/chat UI.
const pairIdx = argv.indexOf("--pair");
const PAIR_NUMBER = pairIdx >= 0 ? (argv[pairIdx + 1] || "").replace(/\D/g, "") : "";
if (pairIdx >= 0) argv.splice(pairIdx, 2);
argv = argv.filter((a) => a !== "--repair");
const OUT_DIR = resolve(argv[0] || resolve(__dirname, ".."));
const WAIT = parseInt(argv[1] || "120", 10) * 1000;
mkdirSync(OUT_DIR, { recursive: true });
if (repair) { try { rmSync(AUTH_DIR, { recursive: true, force: true }); console.error("[repair] wiped auth — fresh QR pairing required"); } catch {} }

// Erica's chat: match either her phone-jid or her LID.
const ERICA_NUM = "41765073911";
const ERICA_JID = `${ERICA_NUM}@s.whatsapp.net`;
const ERICA_LID = "161881780133908@lid";
const isErica = (jid) =>
  jid === ERICA_JID || jid === ERICA_LID || (jid || "").replace(/\D/g, "").includes(ERICA_NUM);

const byKey = new Map(); // dedupe key -> message record
let newest = null;       // {key, ts} anchor for on-demand history
let ericaName = "Erika Baumann";

function describe(msg) {
  msg = msg || {};
  if (msg.imageMessage) return { type: "image", text: msg.imageMessage.caption || "", media: msg.imageMessage, mimetype: msg.imageMessage.mimetype || "image/jpeg" };
  if (msg.videoMessage) return { type: "video", text: msg.videoMessage.caption || "", media: msg.videoMessage, mimetype: msg.videoMessage.mimetype || "video/mp4" };
  if (msg.documentWithCaptionMessage) { const d = msg.documentWithCaptionMessage.message?.documentMessage || {}; return { type: "document", text: d.caption || "", media: d, mimetype: d.mimetype || "application/octet-stream" }; }
  if (msg.documentMessage) return { type: "document", text: msg.documentMessage.caption || "", media: msg.documentMessage, mimetype: msg.documentMessage.mimetype || "application/octet-stream" };
  if (msg.audioMessage) return { type: "audio", text: "", media: msg.audioMessage, mimetype: msg.audioMessage.mimetype || "audio/ogg" };
  const text = msg.conversation || msg.extendedTextMessage?.text || "";
  return { type: "text", text, media: null, mimetype: null };
}

async function record(sock, m, source) {
  const jid = m.key?.remoteJid;
  if (!isErica(jid)) return;
  const ts = Number(m.messageTimestamp) || 0;
  const d = describe(m.message);
  if (!d.text && !d.media) return;
  if (m.pushName && !m.key?.fromMe) ericaName = m.pushName;
  const dedupe = `${ts}|${d.type}|${d.text}`;
  if (byKey.has(dedupe)) return;

  let file = null, bytes = null;
  if (d.media) {
    const ext0 = (d.mimetype || "image/jpeg").split("/")[1]?.split(";")[0] || "bin";
    const fname = `img_${ts}.${ext0 === "jpeg" ? "jpg" : ext0}`;
    try {
      const buf = await downloadMediaMessage(m, "buffer", {}, { logger, reuploadRequest: sock.updateMediaMessage });
      writeFileSync(resolve(OUT_DIR, fname), buf);
      file = fname; bytes = buf.length;
      console.error(`[downloaded] ${fname} (${bytes} bytes)`);
    } catch (e) {
      console.error(`[download failed] ts=${ts}: ${e?.message || e}`);
    }
  }

  byKey.set(dedupe, {
    id: `${ts}-0`,
    jid,
    ts,
    iso: ts ? new Date(ts * 1000).toISOString() : "",
    fromMe: !!m.key?.fromMe,
    sender: m.key?.fromMe ? "Zeno R.R. Davatz" : ericaName,
    type: d.type,
    text: d.text || "",
    media: d.media ? { mimetype: d.mimetype } : null,
    ...(file ? { file, bytes } : {}),
  });
  if (!newest || ts > newest.ts) newest = { key: m.key, ts };
  console.error(`[${source}] ${d.type} @ ${ts ? new Date(ts * 1000).toISOString() : "?"} fromMe=${!!m.key?.fromMe}${d.media ? " *media*" : ""}`);
}

async function main() {
  const { state, saveCreds } = await useMultiFileAuthState(AUTH_DIR);
  const { version } = await fetchLatestBaileysVersion();
  const sock = makeWASocket({
    version, logger,
    auth: { creds: state.creds, keys: makeCacheableSignalKeyStore(state.keys, logger) },
    browser: ["listingtracker", "CLI", "1.0"],
    markOnlineOnConnect: true,
    syncFullHistory: true,
  });
  sock.ev.on("creds.update", saveCreds);

  // Pairing-code path: request the code once, as soon as we'd otherwise show a QR.
  let pairRequested = false;
  async function maybeRequestPairCode() {
    if (pairRequested || !PAIR_NUMBER || sock.authState.creds.registered) return;
    pairRequested = true;
    try {
      const code = await sock.requestPairingCode(PAIR_NUMBER);
      const pretty = code.match(/.{1,4}/g)?.join("-") || code;
      console.error("\n=====================================================");
      console.error(`  PAIRING CODE:  ${pretty}`);
      console.error("  WhatsApp > Linked Devices > Link a Device >");
      console.error("  \"Link with phone number instead\" — enter this code.");
      console.error("=====================================================\n");
    } catch (e) {
      console.error("[requestPairingCode failed]", e?.message || e);
      pairRequested = false;
    }
  }
  if (PAIR_NUMBER) setTimeout(maybeRequestPairCode, 3000);

  sock.ev.on("messaging-history.set", async ({ messages, chats, contacts, progress, syncType }) => {
    console.error(`[history.set] chats=${chats?.length || 0} contacts=${contacts?.length || 0} messages=${messages?.length || 0} syncType=${syncType} progress=${progress}`);
    for (const m of messages || []) await record(sock, m, "history");
  });
  sock.ev.on("messages.upsert", async ({ messages, type }) => {
    for (const m of messages || []) await record(sock, m, `upsert:${type}`);
  });

  let askedHistory = false;
  sock.ev.on("connection.update", async (u) => {
    const { connection, qr, lastDisconnect } = u;
    if (qr && PAIR_NUMBER) {
      await maybeRequestPairCode();
    } else if (qr) {
      try { await QRCode.toFile(QR_PNG, qr, { width: 480, margin: 2 }); console.error(`[qr] ${QR_PNG} — scan with WhatsApp > Linked Devices > Link a Device`); } catch {}
      qrcodeTerminal.generate(qr, { small: true });
    }
    if (connection === "open") {
      console.error("[connected, online] collecting…");
      setTimeout(async () => {
        if (!askedHistory && newest?.key) {
          askedHistory = true;
          try {
            console.error(`[fetchMessageHistory] requesting 50 older than ${newest.key.id}…`);
            await sock.fetchMessageHistory(50, newest.key, newest.ts);
          } catch (e) { console.error("[fetchMessageHistory failed]", e?.message || e); }
        }
      }, 9000);
      setTimeout(finish, WAIT);
    }
    if (connection === "close") {
      const code = lastDisconnect?.error?.output?.statusCode;
      console.error(`[close] code=${code}`);
      if (code === DisconnectReason.loggedOut) process.exit(2);
      if (code === 515) return setTimeout(() => main().catch(() => process.exit(1)), 1500);
    }
  });

  let done = false;
  function finish() {
    if (done) return; done = true;
    const messages = [...byKey.values()].sort((a, b) => a.ts - b.ts);
    const out = {
      me: state.creds?.me ? { id: state.creds.me.id, lid: state.creds.me.lid } : null,
      outDir: OUT_DIR,
      matches: [{ jid: ERICA_JID, name: ericaName, count: messages.length, messages }],
    };
    console.log(JSON.stringify(out, null, 2));
    sock.end();
    setTimeout(() => process.exit(0), 500);
  }
}
main().catch((e) => { console.error("fatal:", e?.message || e); process.exit(1); });
