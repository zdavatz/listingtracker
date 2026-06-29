# erica-house/sync — WhatsApp sync for the Baugeschichte

Self-contained WhatsApp fetcher for Erica Baumann's chat, feeding the
`baugeschichte` photo book. Keeps the whole Erica-house stack in this repo:

```
erica-house/
  messages.json      # the synced chat (consumed by src/baugeschichte.rs)
  *.jpg              # downloaded photos (consumed by baugeschichte)
  sync/
    read-erica.mjs   # this fetcher (Node + Baileys)
    package.json
```

## Why Node and not Rust

The PDF side is already pure Rust (`src/baugeschichte.rs` parses
`messages.json` + images via `genpdf`). Only the *live WhatsApp protocol*
lives here in Node: WhatsApp multi-device needs a full Noise handshake +
libsignal session layer + history-sync protobufs, and the only mature,
maintained implementation is [Baileys](https://github.com/WhiskeySockets/Baileys)
(JS). There is no equivalent Rust crate (the old `whatsappweb-rs` predates
multi-device and is unmaintained). So the split is deliberate: Node does the
network/decrypt, Rust does everything downstream.

## Install

```
cd erica-house/sync
npm install
```

`node_modules/` and `auth/` are gitignored.

## Run

```
# Normal warm fetch (no QR if ./auth is still paired). Writes images into
# ../ (the erica-house dir) and prints messages.json on stdout:
node read-erica.mjs ../  120 > /tmp/erica.json

# First run, or when a warm reconnect returns nothing (a paired device emits
# no history.set, and a desynced session shows "Bad MAC" on every message):
# wipe auth and re-link via QR, which triggers a full history sync.
node read-erica.mjs --repair ../ 180 > /tmp/erica.json
```

`--repair` prints a QR to the terminal and to `/tmp/wa-login-qr.png`. Scan it
with WhatsApp → Linked Devices → Link a Device.

Then merge new messages (ts greater than the newest already in
`messages.json`) into `messages.json` and rebuild:

```
cargo run --release --bin baugeschichte -- --lang both
```

## Gotchas

- **Warm reconnect = no history.** An already-paired device does not re-emit
  `messaging-history.set`. Only a fresh pairing (`--repair`) or an on-demand
  `fetchMessageHistory` (needs a real anchor message key) backfills old
  messages. The script anchors on the newest message it sees in the online
  window; if Erica sent nothing during that window there is no anchor, so use
  `--repair` to force a full sync.
- **"Bad MAC" spam** means the local libsignal session is desynced — re-pair
  with `--repair`.
- Erica = +41 76 507 39 11 → `41765073911@s.whatsapp.net`
  (LID `161881780133908@lid`). The script matches both.
