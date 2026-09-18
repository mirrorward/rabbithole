---
target: RabbitHole client UI (crates/ui-web/src/components.rs)
total_score: 21
max_score: 40
na_heuristics: 
p0_count: 1
p1_count: 4
target_identity: "file:/Users/kevin/Projects/RabbitHole/crates/ui-web/src/components.rs"
target_fingerprint: "sha256:c1d51f4b84ad917cae9b40aeea2043d31014f1ab93e5a69359655d4587e2346d"
target_path: /Users/kevin/Projects/RabbitHole/crates/ui-web/src/components.rs
timestamp: 2026-09-18T03-32-44Z
slug: crates-ui-web-src-components-rs
closed: true
---
Method: dual-agent (A: critique-A · B: critique-B)

# Design critique: RabbitHole client (crates/ui-web/src/components.rs)

Mode: Operate. Reviewed at 1280×800 and 390×844 against a live seeded burrow, plus the source. The capture set caught the app in a routine failure state (a resume token a restarted server no longer honours); that rendered state became the top finding.

## Design Health Score

| # | Heuristic | Score | Key Issue |
|---|-----------|-------|-----------|
| 1 | Visibility of System Status | 2 | Header shows green dot + "Online" while signed out; Boards/DMs/Directory skeletons persist indefinitely; only signal is a 5-second toast pushed four times. |
| 2 | Match System / Real World | 2 | First-run field is `ws://localhost:4654`; Radio asks for an "Icecast delivery address"; You explains nonces, QUIC, TLS certificates. |
| 3 | User Control and Freedom | 2 | Leave disconnects instantly and forgets the resume token, no confirm/undo; a signed-out session has no sign-in path except Leave. |
| 4 | Consistency and Standards | 2 | Three empty-state idioms; phone bottom bar changes composition between layers; sidebar vanishes on warren routes (184px jump); two "Online"s in one header row. |
| 5 | Error Prevention | 2 | Post/Send correctly disabled; Leave and tracker Remove unconfirmed; pack icon one misclick from Retro mono. |
| 6 | Recognition Rather Than Recall | 2 | Icon-only rail; Settings has no desktop entry point except ⌘K, and ⌘K is never advertised. |
| 7 | Flexibility and Efficiency | 3 | ⌘K, ⌘1–9, arrow-key lists, ⌘B/I, Enter-to-send, type-to-filter, drag-drop, recent chips. No burrow-switch shortcut. |
| 8 | Aesthetic and Minimalist Design | 2 | 9-control format bar on every composer, permanent hint line, 5-control title-bar cluster, always-open new-thread form. |
| 9 | Error Recovery | 2 | Transfers do it right; elsewhere "please sign in again" with nothing to click, "Couldn't reach a directory." with no next step, skeletons never become errors. |
| 10 | Help and Documentation | 2 | Contextual hints exist; no help entry point, no shortcut list, About desktop-only. |
| **Total** | | **21/40** | **Acceptable** |

## Design Specificity Verdict

LLM assessment: partly authored, structurally interchangeable. Vocabulary layer (burrow/warren/Looking Glass, burrow-hole mark, warren marks, empty-state voice, the You page) is this product; the structure (one indigo + three whites, flat rail-sidebar-pane, GitHub composer, native selects, stacked toasts) could be any team-chat product. The spec's warm/cool split is absent from the tokens. ANSI Art is a three-line sample with a developer caption.

Deterministic scan: source 0 findings (verified). Rendered: desktop /lobby, /boards/general, /dms, /files clean; /servers 4× line-length (p.rh-server-desc ~152ch); /admin 4× line-length (p.rh-hint), 4× heading-rhythm (.rh-panel-title), 1× first-viewport-column-overflow (.rh-body.rh-admin), advisory em-dash count; phone /lobby 9× undersized-ui-text (bottom-bar labels 9.92px < 11px) + body-text-viewport-edge (p.rh-front-motd); /servers 5× undersized-ui-text. False positive: dark-glow on /admin from the detector's own overlay nodes.

Visual overlays: injected and captured in the reviewer's tab; tab closed and live server stopped, no overlay left open.

## Priority Issues

1. [P0] A signed-out session renders as healthy, with no way back in (app.rs resume-failure path: one toast per attempt, socket-level ConnState keeps "Online", no signed-out banner case, skeletons persist, Admin vanishes). Fix: stop retrying, drop the dead session, land on the connect form prefilled with a banner; collapse identical toasts; clear loading flags on failure with an in-panel Retry. Command: harden.
2. [P1] Phone shell: 8–9 tabs at 9.92px labels that change between layers; composer + bar eat ~200px; DM empty-state copy points "left/below". Fix: five-tab bar + horizontally scrolling section control, labels ≥11px, phone-specific copy. Command: adapt.
3. [P1] The composer is the heaviest object on every conversational screen (8 buttons + Markdown + hint + Send above an empty log). Fix: single-line chat composer with the bar behind a toggle; tall composer for posts; new-thread form behind a button. Command: distill.
4. [P1] Title bar says "Online" twice, hides chimes/appearance/mode behind icons, and puts an unconfirmed Leave on every screen. Fix: drop the connection text when healthy, presence as a menu, prefs into Settings with a rail entry, Leave only in burrow scope behind a confirm. Command: quieter.
5. [P1] Spec's warm/cool split, surface-2 and display face are not in the tokens; rail + sidebar read as one pale margin. Fix: add spec §8 tokens; rail on surface-2, unified circles in ember, burrow squircles tinted by overlay. Command: colorize, then typeset.

## Persona Red Flags

Alex (power user): ⌘K unadvertised; no burrow-switch shortcut; Looking Glass Connect routes through the full form; inert who-list rows; Leave forgets the token; signed-out state has no exit but Leave.
Jordan (first-timer): socket URL first; icon-only rail; jargon (Icecast, nonce, QUIC); "no admin access" with no why; "sign in again" with no where.
Casey (phone): 10px tabs, truncated "Direct…", bar re-lists itself on Servers, toasts cover titles, DM empty state points at a left that does not exist, "Leave Wonderland" biggest lowest unconfirmed control.

## Minor Observations

Connect screen leads with the socket URL (Looking Glass should be the front door); admin heading rhythm + one-column overflow + 143ch hints; Looking Glass 152ch descriptions, phone rows wrap tags to 3 lines; sidebar vanishes on warren routes; three empty-state idioms; Radio asks listeners for server config; Art sample + dev caption; rail dot pulses forever; tracker Remove unconfirmed, add field truncates placeholder; skeletons don't match rows; mode/pack buttons cycle with tooltip-only state; dead CSS (.rh-compose, .rh-status, .rh-kbd-jump).

## Questions to Consider

- What if there were no connect screen and the Looking Glass opened first?
- What if the lobby composer were one line and the who-list the widest column?
- If the rail went ember and the place went indigo, would Transfers and Boards look like two layers of one app?
