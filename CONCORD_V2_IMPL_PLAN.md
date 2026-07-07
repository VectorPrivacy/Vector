# Concord v2 — Implementation Plan (dual-stack + migration)

Local design doc for the `concord-v2` branch (worktree `Projects/Vector-concord-v2`). **Do not commit.**
Companion to `Vector/CONCORD_V2_DECISIONS.md` (locked decisions log, main tree). Spec refs: `Projects/concord-v2-refs/concord/` (CORD-01..07 + examples.md, upstream-canonical), Armada reference impl at `Projects/concord-v2-refs/armada/` (unverified third party — spec-audit found real divergences, listed §9).

Research basis: 5-agent deep-read pass (v1 impl map, Armada study, spec extraction, consumer map, test-harness map) + completeness critic, 2026-07-07. Full reports archived in the session scratchpad; the load-bearing conclusions are inlined here.

---

## 0. The mission (JSKitty's brief, 2026-07-07)

- Vector carries **both** protocols for a release or two: v1 (shipped v0.4.0) keeps working read/write; v2 is the new default for creation.
- **Migration** = owner recreates the community as v2: new self-certifying `community_id` (unavoidable — the id IS the owner commitment, this is the #309 fix), **same channel_ids and role_ids**, members reintroduced via CORD-05 §6 Direct Invites, v1 links revoked and reminted as v2 links.
- **SDK**: v2-first; v1 support may be dropped/thinned.
- All work on branch `concord-v2` in this worktree. Tests offline (MemoryRelay) in per-test tempdirs — never the real data dir.

## 1. Locked architecture decisions (this pass)

**A1 — Upstream spec is canonical.** The local `CORD-05-rekeys.md` draft is superseded: labels have NO `/v1/` infix (`concord/rekey-pseudonym` etc.), rekey seals are kind 20013 (13 is Direct-Invite-only), `["chunk", i, n]` (1-based) is mandatory, refounding fork tiebreak = **lexicographically lowest new key** (not lowest inner id), no `["v","2"]` outer tag ever (version tags forbidden — camouflage). D2 (BAN authority inherits to removal-forced channel rekeys, discriminated by prior-root addressing) is a LOCAL ADDITION upstream lacks — we implement it and propose it upstream.

**A2 — Module layout: v1 frozen in place, v2 as a sibling module, pure engine shared by import.**
- `community/` (v1) — no file moves, no churn; it is shipped and stable. Only additions: the `vsk=11` migration-pointer edition (§6) and small `pub` visibility bumps where v2 imports pure functions.
- `community/v2/` — new: `derive.rs` (concord/* labels), `stream.rs` (CORD-01 envelope), `control.rs`, `chat.rs`, `guestbook.rs`, `rekey.rs`, `invite.rs` (link+registry+direct), `list.rs` (13302/13303), `service.rs`, `realtime.rs`, `migrate.rs`.
- Shared verbatim (imported, not copied): `version::{fold, bootstrap_head, edition_hash, edition_signing_bytes, EDITION_LABEL}` (v2 froze the exact v1 construction incl. the `vector-community/v1/edition` label), `roles.rs` whole (bits/positions/outranking — bit layout identical), `edition.rs` (3308 tag grammar + vac, identical), `roster.rs` fold+delegation layer (`fold_roster`, `authorize_delegation`, `authority_citation_satisfied`, builders), `transport.rs` (Query already has `authors`; + MemoryRelay + durable_broadcast), `attachments.rs` (imeta), `cache.rs`, the list-merge engines in `list.rs`/`invite_list.rs`, `rekey::{epoch_key_commitment preimage shape, bound_plaintext 72-byte layout, MAX_REKEY_BLOBS}` — note the commitment LABEL changes (`concord/epoch-key-commitment`), so the shared piece is the shape, v2 passes its own label.
- v1-only, not ported: the `#z` pseudonym plane, ephemeral-outer-signer envelope + `community_message_keys`, secret pairwise rekey locators + locator-as-authenticity, `owner.rs` (vco attestation), outer `v="1"` tags.

**A3 — Version discriminator.** `Community.protocol: ConcordProtocol { V1, V2 }` (serde/DB as integer 1|2). DB: `communities.protocol INTEGER NOT NULL DEFAULT 1` + v2 columns `owner_pubkey`, `owner_salt` (commitment inputs; v2 rows only). Frontend sees `CommunitySummary.version` (additive) + `custom_fields.protocol_version` (string, matching the `dissolved === 'true'` string-typed pattern). `ChatType::Community` unchanged (frontend string-checks survive). Migration ids: **start at 65** (64 = ghost-row sweep; 33-39 + 45-61 burned).

**A4 — Kind-1059 dispatch (the DM-collision fix).** v2 community wraps, DM gift wraps, and 3313 direct invites are ALL kind 1059. Routing precedence, applied identically on the targeted subs and the Android pool-wide sub:
1. `event.pubkey ∈ V2_ROUTES` (the author→plane map, the v2 analog of v1's `#z` route map) → v2 stream processing.
2. else `#p == me` → the existing NIP-17 DM unwrap path. Direct invites arrive HERE (they are standard NIP-59 to me) — `rumor.rs` learns kind 3313 → park as pending v2 invite (consent-gated, no network reaction). The outer `#k=3313` tag is only an index hint for the dedicated invite fetch; the unwrap path must not require it.
3. else drop (not ours). Never hand a stream wrap to the DM decrypt (p=random ⇒ wasted bunker round-trips).
v2 stream wraps never match the DM sub filter (`#p=me`) so desktop targeted subs are naturally disjoint; the pool-wide Android path MUST apply the author-set check before the p-tag check.

**A5 — Rekey blob NIP-44 payload = base64 (standard alphabet, padded) of the 72 bytes.** Spec says raw 72 bytes; NIP-44/NIP-46 encrypt APIs are string-typed (`nip44_encrypt(&self, pk, content: &str)`) and raw 72 bytes aren't valid UTF-8 — bunker parity forces a string carrier. Armada already ships base64 for the same reason. Interop > spec-literalism here: match Armada, pin a full-blob golden vector against their bytes, file the CORD-06 erratum with Gleason. (Verify the exact Armada helper output during implementation — `armada/client/src/concord-v2/lib/rekey.ts:67-77`.)

**A6 — vac citations: block-until-synced with the compaction carve-out.** Spec-literal vac (3-part pin, hash-checked, park-until-synced) deadlocks fresh joiners against compaction (a cited pre-compaction Grant version can never hash-resolve post-refounding). Rule: a citation whose version ≤ the entity's compaction-baseline head AND whose actor is authorized by the CURRENT folded roster is satisfied; otherwise park with the hash check. Armada ignores vac entirely — we do not copy that (their reader silently drops what we park; expected divergence, flag upstream).

**A7 — Tracking clients fail closed on chain gaps (spec §04.md:82), per entity, exactly like v1.** Armada gives everyone bootstrap semantics (their `gap` flag is never consumed) — do not copy. Expected render divergence under a truncating relay documented for Gleason.

**A8 — Private-channel key delivery on Grant = Direct Invite carrying the channel subset.** CORD-03 says keys are "delivered on grant" with no mechanism (real spec hole). We reuse the 3313 Direct Invite: the bundle's `channels[]` carries exactly the granted private channels; a recipient already in the community MERGES delivered channel keys (guarded: same community_id + owner commitment verified + delivered `community_root` matches a held root, else reject — the v1 same-root hijack defense pattern). Zero new wire format; propose upstream.

**A9 — Ephemeral tier (23311 typing, 21059 wrap): best-effort, never load-bearing.** Implement send/receive; never gate logic on delivery; MemoryRelay grows a broadcast lane so tests can't false-green on "stored" ephemerals. Voice (CORD-07) is OUT OF SCOPE this effort (needs broker+SFU infra); we round-trip the `voice` metadata flag per the unknown-field discipline so Armada communities render.

**A10 — v2 sync discipline: per-relay `since` cursors** (Armada's one architectural win we adopt wholesale, and it's the fix-shape for our own community_message_sync_gap bug): a fast relay never advances a shared cursor past what a lagging relay still owes; live subs never advance cursors — only each relay's own query results do; write-own-edition to the local store immediately after publish.

**A11 — Strictness deltas vs Armada we keep (spec-required, they lack them):** private-channel rekeys (they don't implement channel rekeys at all), banlist re-heal loop, chunk strictness (missing chunk tag = malformed; all chunks must agree on scope+prevepoch+n; correlation key = rotator+scope+newepoch+prevcommit), Direct-Invite seal Schnorr verify (they skip verifyEvent on that path), snapshot authority strictly = the epoch's minting refounder (no owner fallback), reader-side name caps.

**A12 — ms resolver is v2-strict and separate from v1's.** absent tag = offset 0; present-but-invalid (non-integer, outside 0..999) = DROP the event (never clamp — v1's `resolve_message_timestamp` clamps and must not be reused). True time = `created_at*1000 + ms`; +1h future-drop on guestbook coalesce (and defensively on chat ordering inputs).

## 2. What v2 shares with v1 at the byte level (why this is cheaper than it looks)

- `edition_hash` construction + label — IDENTICAL (upstream froze Vector's).
- 3308 edition tag grammar (`vsk/eid/ev/ep/vac`) — IDENTICAL. vsk numbers identical (7 retired, 11 we claim for v1-side migration pointer).
- Permission bits, position algebra, owner-supremacy, delegation fixpoint — IDENTICAL (v2 adds: permissions as decimal STRING on the wire, read-either/write-string; 100-role + 64/member caps).
- 72-byte rekey bound-plaintext, 120-blob cap, prevcommit shape (label differs), monotonic epochs, prior-root addressing for base+removal rekeys, multi-epoch read model — IDENTICAL semantics.
- Fold rules (refuse-downgrade, bootstrap-vs-tracking, same-version tiebreak lowest inner id) — IDENTICAL. NEW on top: authority-filtered head selection is already spec'd upstream (04.md:80) matching our in-progress "revocation authority cutoff v2" design — implement it in the SHARED consumer path so v1 gets it too (CONCORD_NOTES.md "IN PROGRESS" item folds into this work).
- Banlist semantics + precedence — IDENTICAL (+~500 practical cap + re-heal).
- CommunityInvite bundle JSON — near-identical field set (renames: `server_root_key`→`community_root`, `server_root_epoch`→`root_epoch`; adds `owner`, `owner_salt`; drops `owner_attestation`). Pin exact field names against Armada `types.ts` + `05.md` — a name mismatch is a silent join failure (critic U5). Round-trip Armada's `held_roots`/`refounder` extensions.
- Community/Invite List merge algebra (seed/current, tombstones-terminal, token-keyed) — IDENTICAL engine; new carriers (13302/13303, no d-tag), ms timestamps, 50-cap + byte-cap, canonical-JSON tiebreak = recursive sorted-keys UTF-8 no-whitespace (pin vs Armada `canonicalJson`, golden vector w/ nested objects — critic U7; verify v1 list.rs:115 tiebreak agrees or keep the two engines' byte-functions separate).

## 3. New surfaces (no v1 counterpart)

1. **CORD-01 stream envelope**: 1059 wrap signed by the plane's group key (reversed NIP-59: fixed author, random ephemeral `p`), seal 20013 (encrypted, double-wrap) / 20014 (plaintext, control only — byte-verbatim rumor JSON so compaction re-wraps preserve sigs), 21059 ephemeral. NIP-44 65,535-byte cap enforced at EVERY layer. `openWrap` chain: wrap kind → wrap.pubkey==stream.pk → decrypt → seal kind → Schnorr verify seal → rumor.pubkey==seal.pubkey → rumor.id==computed hash (never trust claimed ids).
2. **group_key derivation** (A.1-A.3): hkdf info = `utf8(label)||0x00||id[32]||epoch_be[8]?`, scalar_normalize with counter byte appended AFTER present fields, counter starts 0 (first attempt = NO counter byte; verify against Armada `hkdfToSecretKey`).
3. **Self-certifying community_id** = `sha256("concord/community"||owner_xonly||owner_salt)` — kills #309; owner proven by commitment, vsk 7 gone.
4. **Guestbook plane** (3306 join/leave w/ invite attribution, 3309 kick vac-cited, 3312 snapshots 400/chunk refounder-signed): coalesce latest-per-npub on ms basis, +1h future drop, lower-rumor-id tie (firsthand beats snapshot at equal ms), forward-only observation re-entry, minus banlist ⇒ Complete Memberlist. Supersedes the old "3rd plane guestbook" design memory — this is now spec.
5. **Public channels derive from community_root** (`concord/channel`, ikm=root, per-channel address via channel_id in the id slot) — no key delivery, rotate free with base. Private channels: independent key, monotonic channel_epoch. Public⇄private conversions per CORD-03 §2.
6. **Genesis = exactly 2 owner editions** (metadata + `#general`) — no auto-Admin role (v1 minted one); the owner shapes roles deliberately.
7. **Invites**: 33301 addressable bundle at `(33301, link_signer, d="")`, bundle sealed under `hkdf(token16, "concord/invite-key")`, naddr+fragment URL (`[4][flags][relays?][token:16]` base64url no-pad, dictionary gen-4: 1=jskitty.com 2=asia.vectorapp.io 3=relay.ditto.pub 4=relay.dreamith.to), link_signer keypair in the 13303 Invite List, refresh/revoke by re-posting the coordinate (vsk 6 live / 9 tombstone), Registry vsk=8 as the Public/Private source of truth, Direct Invites 3313 (classic NIP-59 + `k` tag + NIP-40 seconds vs bundle `expires_at` ms — unit conversion!).
8. **Realtime by authors**: sub filters = `{kinds:[1059], authors:[...]}` per plane per held epoch + precomputed NEXT rekey addresses (base @ root_epoch+1 under prior=current root; each private channel @ channel_epoch+1). Route map: author_pk → (community, plane, channel?, epoch).
9. **Dissolution** at `group_key("concord/dissolved", community_id, ZERO32, no-epoch)` — chainless vsk 10, eid=zeros. Envelope: plaintext seal 20014 (spec leaves the seal kind open; plaintext keeps it verifiable by anyone holding only the community_id — flag to Gleason).

## 4. Persistence plan (migration ids 65+)

- 65: `communities` + `protocol` col (default 1), `owner_pubkey` TEXT, `owner_salt` TEXT (v2, encrypted-at-rest like peers); `community_epoch_keys` + `rotator` TEXT col (snapshot-authority persistence, critic U6; also backfills v1 refounder tracking).
- 66: `community_guestbook` table: (community_id, member) PK, state (join|leave|kick), at_ms, source (self|snapshot|observed), snapshot_id, kick_actor — the coalesced read-model; raw rumors stay in the event store.
- 67: `community_direct_invites` (pending v2 invites, replaces-parallel to `pending_community_invites`): community_id, inviter, bundle_json (encrypted), received_at + consent state.
- 68: v2 link bookkeeping: reuse `community_public_invites` + new cols `link_signer_sk` (encrypted), `naddr`; token col already TEXT (16-byte hex fits; verify no 32-byte assumption — critic U9).
- Settings keys: `community_list_v2_json` + `community_list_v2_published_at` (13302 mirror), `invite_list_v2_json` + published_at (13303). v1 keys untouched (dual period).
- At-rest encryption: every new secret BLOB/text goes through the `maybe_*` wrappers like the v1 tables (db/community.rs pattern).
- Chats: unchanged schema. Chat id stays = channel hex id ⇒ **history stitching on migration is a custom_fields update, not a data move** (§6).

## 5. Service layer + SessionGuard

Free functions in `community/v2/service.rs` mirroring v1's shape: guard at entry, re-check before every persist, guard-before-spawn, debounce = capture-before-sleep. Every new tokio::spawn + long fetch follows the CLAUDE.md contract. The v2 route maps live beside v1's and BOTH clear in `swap_session` (v1's `CONTROL_ROUTES` clear gap noted in CONCORD_NOTES Area 6 N1 — fix it for both while here).

Send path: command → v2 service → stream seal (group-key signer — NOT the ephemeral-key model; wrap sk = plane's derived key; optionally retain the random `p` ephemeral sk for NIP-09 wrap-scrub parity with v1's delete… note: v2 wrap deletion needs the relay to allow author-delete of 1059 which CORD-01 says relays should PREVENT — so v2 self-scrub = kind-5 in-plane delete + best-effort p-tag NIP-09; do NOT port v1's `community_message_keys` mechanism, keep the ephemeral p sk anyway, it's 32 cheap bytes) → `Transport::publish`. Kind-5/3302 semantics bridge into the existing `IncomingEvent`/`InboundEventHandler` seam — the trait is untouched (SDK/semver safe), src-tauri handlers unchanged, frontend events unchanged (`message_new`/`message_update`/`message_removed` contract preserved).

Reply mapping: v2 kind-9 replies use NIP-C7 `q` tags (v1 used `e`+"reply") — adapt in v2 chat build/parse only; `OpenedMessage.citation` stays the internal representation.

## 6. The migration flow (owner-driven, two-lane)

**Design principle: the v1 control plane authenticates the pointer; Direct Invites carry the keys.** Embedding v2 keys in the v1 plane would hand them to banned-but-unrekeyed v1 root-holders; direct invites to `(folded roster ∪ observed authors) − banlist` (the reseal_base_to_observed recipient machinery, reused) exclude exactly them.

Owner clicks **Upgrade to v2**:
1. Preflight: full v1 control fold must be complete (compaction discipline — abort if gapped); bunker accounts CAN migrate (v2 needs no raw-ECDH: genesis + grants + direct invites are all sign/nip44 ops — verify each step stays signer-agnostic).
2. Create v2 community: mint `owner_salt`, compute id; genesis (metadata copied from v1 GroupRoot incl. icon/banner pointers; `#general`… channels recreated with the SAME `channel_id`s, same names, same private flags — private ones mint fresh independent keys at epoch 1); roles recreated with SAME `role_id`s/names/positions/bits; grants re-issued owner-signed for every v1 grant-holder; banlist carried verbatim (same npubs).
3. Direct-Invite blast: 3313 to every member of the computed set, bundle carrying root + the private-channel keys that member's roles entitle (public channels need no keys). Batched, resumable, idempotent (re-send safe — accepting twice is a no-op merge). Progress event to UI.
4. v1-side pointer: publish v1 control edition **vsk=11 "MigratedToV2"** (claim vsk 11 in v1's registry; chainless like dissolution, owner-signed, content `{v2_community_id, owner, owner_salt, relays, name}` — NO keys) at the current epoch + (like the dissolution tombstone) at the rotation-stable dissolved-style coordinate so post-rotation stragglers find it. Also publish a plain metadata edit appending "(migrated)" marker? No — the pointer edition is the signal; UI renders it.
5. Revoke all v1 public links (registry tombstones + bundle tombstones), with the "last link retired → privatize refound" trigger SUPPRESSED (dissolution-pattern; migration replaces re-founding).
6. Owner's own client: v2 community live in 13302; v1 entry stays in 30078 (old devices render read-only + pointer); local chat re-parent (below).

**Member client, on folding the vsk-11 pointer** (or receiving the 3313 first — order-independent):
- Verify: pointer's inner signer == v1 proven owner; `sha256("concord/community"||owner||salt) == v2_community_id`; pointer owner == the direct invite bundle's owner. All three bind ⇒ auto-accept the matching pending 3313 WITHOUT a consent prompt (the consent was joining the v1 community; continuity is owner-proven). A 3313 for an unknown/non-pointed community stays consent-gated as normal.
- Join v2 (guestbook Join published), re-parent chats: for each retained channel_id, update the chat's `custom_fields.community_id` v1→v2 (+ `protocol_version`) — v1 history and v2 messages share the chat row keyed by channel hex. v1 community entry → state `migrated` (read-only, composer off, banner "This community upgraded — you're already in the new one"), 30078 list entry retained, tombstone-free.
- No invite received (offline too long / expired): pointer still folds → UI shows migrated state + "request a new invite"; any v2 member with CREATE_INVITE (or the owner, who can re-run the blast — it recomputes v2-guestbook-absent members) re-admits. Fresh v1 joins post-migration are impossible (links tombstoned) and a stale bundle join lands on the pointer.

**Owner UI wizard**: Upgrade button (owner-only, v1 communities) → summary (members to invite, channels/roles carried, links to revoke) → progress (create/grants/invites/pointer/revocations) → done + optional "mint new v2 link". All steps resumable; state machine persisted per community (`migration_state` settings key) so a crash mid-blast resumes.

## 7. SDK (task #12)

Surface stays protocol-agnostic (opaque ids; capabilities/roles already `serde_json::Value`): VectorCore facade routes by `Community.protocol` internally. SDK 0.4:
- `create_community` → v2 always. `join_community(url)` → fragment version byte dispatches (v2=4; v1 bytes rejected with "ask for a new invite" error — SDK drops v1 JOIN, keeps v1 READ/SEND working through the facade so deployed bots survive the window).
- Direct-invite acceptance: `pending_invites()`/`InvitePolicy` extended to 3313s (additive).
- No `InboundEventHandler` signature changes (semver). New hooks additive if needed.
- price-bot: DM-focused; compile-check against 0.4 before publish.

## 8. Test strategy (task #13, woven through every module task)

- Hoist `init_test_db`/`make_test_npub` into `#[cfg(test)] pub(crate) mod testutil` (5 copies today).
- Transport: add `p_tags` to Query + filter mapping + parity test (incl. authors — untested today); giftwrap publish/fetch through Transport so MemoryRelay is the 3313 mailbox; ephemeral broadcast lane (21059 never stored, subscribe API) so typing tests can't false-green; 33301 replaceable already modeled.
- `TestAccount`/`swap_to` two-actor harness (mirrors swap_session: set_current_account + init_database + become_local + bump generation + clear id caches, under DB_TEST_GUARD) — unlocks true e2e: owner creates → direct-invites → member joins → messages → refound → **migration e2e (v1 community → upgrade → member lands in v2, same channel ids, history re-parented, v1 read-only)**.
- Golden vectors, house style (fixed patterned inputs, independent-impl hex, GOLDEN_ consts, flanked by binding/domain-separation property tests): every A.6 label, A.1 info layout + epoch-omission + counter-retry, community_id, epoch-key commitment, edition hash (shared w/ v1 — pin again under v2 usage), 72-byte blob + base64 carrier + full-blob vector vs Armada bytes, fragment codec (stock flag/dict/wss-implied/verbatim/trailing-byte-fatal/version 3+5 reject), canonical-JSON tiebreak w/ nested objects, snapshot chunking 1-based, stream wrap→seal→rumor round-trips both seal kinds + byte-verbatim re-wrap preserving rumor id + seal sig.
- Adversarial catalog port (CONCORD_NOTES.md Areas 1-8 re-targeted at v2): fold/authority unchanged (shared code — v1 tests still bind), new: guestbook coalesce attacks (future-dated squat, ms smuggling, snapshot forgery from non-refounder, observation resurrection), chunk-withholding (missing chunk ≠ removal), prevcommit fork/gap discrimination, link squatting (different author ≠ coordinate), bundle owner-commitment forgery, direct-invite consent gate, migration-pointer forgery (non-owner signer / wrong commitment / mismatched invite → no auto-join).
- Offline guarantee: everything against MemoryRelay in tempdirs (existing pattern); no test touches `~/.local/share/io.vectorapp`.

## 9. Divergence ledger (to raise with Gleason / track upstream)

1. Rekey blob base64 carrier (A5) — CORD-06 erratum needed; Armada agrees de facto.
2. D2 BAN-inheritance for removal-forced channel rekeys — upstream ambiguous (06.md:102 strictly read leaves a BAN-only mod unable to complete a ban).
3. vac-vs-compaction deadlock + our carve-out rule (A6).
4. Private-channel key delivery on grant unspecified — our 3313 answer (A8).
5. Dissolution tombstone seal kind unspecified — we pick 20014.
6. Chunk correlation should include scope; chunk index 1-based; receiver over-cap rejection — all unstated upstream.
7. Armada divergences we intentionally don't mirror: no vac enforcement, no tracking fail-closed, no channel rekeys, no banlist re-heal, direct-invite seal unverified, owner-fallback snapshot authority, missing-chunk-tag default 1/1.
8. `ms` absent = 0 (we assume; upstream silent), stock-set flag exempt from the 3-relay cap (Armada reading; pin in spec).
9. NIP-42 AUTH-as-stream-key (Armada streamAuth): deferred — verify our relays don't AUTH-gate 1059; revisit if any community relay does.
11. **CRITICAL — dissolution tombstone is replayable across a same-owner's communities** (found + fixed by the 2026-07-07 adversarial review): the CORD-02 §9 tombstone rumor is community-agnostic (`eid = 0…0`, no community_id in the signed payload), and the dissolved plane is addressed by the *public* `community_id`. So an owner's genuine tombstone for community X can be lifted and re-wrapped at the dissolved address of any OTHER community Y the same owner runs (both ids are public, they ride in invites) — the owner's signature validates for both, irreversibly killing Y with no membership or ownership. Vector's fix: bind `community_id` INTO the signed rumor (`eid = community_id`) and require `eid == community_id` on read (`dissolution.rs`; regression test `a_tombstone_cannot_be_replayed_onto_another_community_of_the_same_owner`). This diverges from the frozen §9 `eid=0…0` shape — a spec erratum Gleason should adopt (Armada carries the same replay hole). Two low-severity findings from the same review also fixed: a valueless `["ms"]` tag silently defaulting to 0 instead of dropping (stream.rs), and an order-dependent snapshot torn-chunk pin breaking coalesce determinism (guestbook.rs — removed the pin per §5 "no torn state to defend against"). The crypto, rekey, control, and invite dimensions came back CLEAN.
12. **120-blob rekey cap is unachievable under a 64KB relay with the CORD-01 double-wrap** (MEASURED, 2026-07-07): a v2 rekey nests the blob array (each blob already a base64 NIP-44 payload) in an encrypted seal in a wrap — two more NIP-44 base64 expansions. A 120-blob event = ~77KB, over strfry's 64KB `maxEventSize`; 80 blobs = ~55KB. Vector SENDS ≤80/event (`MAX_REKEY_BLOBS_PER_EVENT`), ACCEPTS ≤120 (`MAX_REKEY_BLOBS_RECEIVED`, so an Armada 120-event via a larger-limit relay still parses). CORD-06 erratum: state the cap as a byte budget, or lower the count, given the mandated double-wrap. Armada's 120 is likely untested against a real relay (it doesn't implement channel rekeys and its refounding rolls only the root).

## 10. Implementation order (tasks #3→#13)

Each module lands WITH its tests + golden vectors; compile+test green before moving on (cd crates && cargo test -p vector-core).

1. **#3 primitives**: v2/derive.rs (all labels, group_key, scalar_normalize, community_id, commitment) + v2/stream.rs (wrap/seal/rumor build+open, strict ms resolver, binding checks, 65535 caps, re-wrap) + testutil hoist + transport p_tags/ephemeral. GOLDEN VECTORS FIRST.
2. **#4 control**: control_pk plane, editions over plaintext seals, fold wiring (shared engine), genesis, self-certifying owner, metadata (custom round-trip), caps.
3. **#5 chat**: channels public/private keying, kinds 9/7/5/3302/3310 + q-tag replies, multi-epoch read, pager, 23311 typing.
4. **#6 guestbook**: coalesce + snapshots + Complete Memberlist + DB read-model.
5. **#7 rekeys**: chunked 3303, public locators (locator = lookup ONLY), base64 blobs, prevcommit, refounding compaction + snapshot seed, D2 authority split, lowest-key tiebreak + down-only heal, per-relay cursors.
6. **#8 invites**: bundle, link mint/parse (fragment codec + dictionary), 33301 lifecycle, registry, 13303, direct invites end-to-end (send via inbox relays, indexed fetch, consent gate, channel-key-merge on grant).
7. **#9 list + dissolution + storage**: 13302 dual-list period, dissolution plane, migrations 65-68.
8. **service/realtime integration** (spans #4-#9): v2/service.rs orchestrations, v2/realtime.rs authors-subs + A4 dispatch, boot sync, SessionGuard sweep.
9. **#10 migration**: vsk-11 pointer (v1 side), owner wizard state machine, member auto-join, chat re-parent, link revocation, resumability, e2e test.
10. **#11 tauri/frontend**: create_community(version param defaulting v2), invite preview/accept for naddr#fragment URLs + 3313s, migration UX, member list from guestbook, ACL triples for new commands, custom_fields.protocol_version.
11. **#12 SDK**, **#13 test closeout** (adversarial sweep + full-chain e2e + coverage review vs the catalog).

## 11. Open items / verify-during-implementation

- nostr-sdk 0.44 `nip44_encrypt` signature (A5 confirmation) — compiler will answer.
- Armada blob base64 exact alphabet/padding — read rekey.ts:67-77 when pinning the vector.
- Bundle field names byte-exact vs Armada types.ts (critic U5) — pin before invite work.
- v1 `list.rs:115` canonical-bytes construction vs v2 canonical JSON (critic U7).
- Token column 16-vs-32-byte assumptions (critic U9).
- Relay behavior: ephemeral kinds on jskitty.com/asia strfry (U10) + no AUTH on 1059 (U2) — live probe later, not blocking offline work.
- Whether `q`-tag replies need frontend changes (render path reads OpenedMessage.citation — should be transparent).
- vector-hub website `/invite` page must learn naddr+fragment v2 links (out-of-repo; file when #11 lands).
