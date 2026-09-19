# 0xVector — Topics Design Prototype (nostr-live)

A clickable, phone-and-desktop design prototype for **Telegram-style Topics on
0xVector** — forum groups whose messages split into named topics — built to be
100% implementable on Vector's existing Concord core, with **no new
cryptography**.

Open `index.html` in any browser. No build step.

## Why this exists

Telegram's forum-topics UX, but on a messenger where relays see only
ciphertext. Topics ride as a **signed `["topic", id]` tag inside the sealed
Concord rumor** (CORD-01's binding mechanism anticipates exactly this) — so:

- **zero new relay-visible metadata** (one address per channel regardless of
  topic count or activity)
- **rekey stays O(1)** per channel on member removal (CORD-06)
- **members can create topics** (roster-gated `MANAGE_TOPICS`, unlike
  `MANAGE_CHANNELS`)
- **graceful degradation**: stock Vector / Armada clients render topic
  messages as ordinary channel messages until they adopt the convention
- topic id = the topic-creation rumor's id (same trick as Telegram's
  `message_thread_id`)

## What's inside

- **Topics panel** — chat-list-style topic rows: icons, unread + mention
  badges, pins, mutes, closed state; sections for Announcement channels
  (Concord public channels with a staff-only post gate), Topics, Voice
  (CORD-07 blind-broker rooms)
- **Full message ops** — reply / copy / pin / edit (kind 3302) / delete
  (kind 5) / reactions, pinned bar, per-topic search, disappearing-message
  timer (CORD-08 / NIP-40 expiration tags)
- **Real nostr pipeline, in-browser** — real secp256k1 keypair, real
  HKDF-SHA256 channel-key derivation (`"concord/channel" ‖ 0x00 ‖ id ‖ epoch`),
  real NIP-44 v2 seals, schnorr-signed kind-1059 wraps; flip **SIM → LIVE** in
  the protocol inspector to publish/subscribe against `relay.ditto.pub` and
  `relay.armada.buzz`
- **Protocol inspector** — every UI action shows its rumor → seal → wrap
  chain as expandable JSON
- **Projects (NIP-34)** — Armada `ProjectsView` parity: Overview with stat
  pills / 26-week contribution heatmap / people / activity feed, repository
  grid with `ngit clone nostr://…` lines, PR/Issue tracker with
  open/draft/merged/closed gates; **+ New issue publishes a real signed
  kind-1621**; live REQ folding when LIVE
- **Login** — mirrors Vector's `#login-form`: Create Account (real keypair),
  nsec/seed import (real bech32 validation), Remote Signer with scannable
  `nostrconnect://` QR, Amber link
- **CORD-05 invites** — link + QR + direct-npub gift-wrap + pending
  accept/decline + revoke-without-rekey
- **Six themes** — `vector` (default, from `src/themes/vector/dark.css`),
  `armada` (Corsair, from Armada's `themes.ts`), `terminal`, `satoshi`,
  `monero`, `cyberpunk` — all via the same CSS-variable override architecture
- **Mobile** — Telegram-style drill-down navigation + bottom tab bar;
  QA-tested at 420×880

## Files

- `index.html` — the whole prototype (single file + one vendored dep)
- `vendor/qrcode-generator.js` — qrcode-generator@2.0.4 (MIT), the same
  vendored lib Vector ships (`src/js/qrcode-generator.js`)

## Design source

Architecture blueprint: see `0xvector-topics-blueprint.md` (design notes) —
topic-as-tag vs topic-as-channel analysis, CORD citations, phased
implementation plan (M1 CORD-09 draft → M2 vector-core → M3 Tauri commands →
M4 UX → M5 SDK/MCP → M6 polish/interop).

_Prototype state is local mock data except the nostr layer, which is real._
