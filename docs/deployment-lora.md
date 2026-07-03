# Deploying RabbitHole over LoRa / packet radio

Operator guide for running RabbitHole reachability over [Reticulum](https://reticulum.network)
(RNS) on constrained RF links — LoRa, packet radio, and other low-bandwidth,
duty-cycle-limited, intermittently-connected bearers. It is a **bandwidth and
power budgeting reference**: how RabbitHole's own size caps and rate governor map
onto the airtime a slow radio actually gives you.

> **Status — forward-looking.** RabbitHole's RNS/LXMF layer
> (`crates/reticulum`, `rabbithole-reticulum`) is today a **pure-Rust, sans-I/O
> protocol *model*** — identity, destination hashing, the packet codec, announce
> ingestion, link establishment, LXMF packing, and the delay-tolerant tunnel
> core. It is exhaustively unit-tested but **no bytes have ever crossed a real
> RNS link**: there is no interface layer, no socket loop, no LoRa driver. A live
> **RNS interface/sidecar adapter is future work** (see the decision record,
> [`docs/research/reticulum-decision.md`](research/reticulum-decision.md), which
> adopts the in-repo pure-Rust path and gates any live deployment on reconciling
> seven `// SPEC-CHECK:` points first). Read this guide as the budgeting you will
> need *when that adapter lands* — the RabbitHole-side numbers are real and
> grep-checkable now; the physical-layer numbers are illustrative and must be
> re-measured against your actual radio and region.

Throughout, two kinds of number are kept strictly apart:

- **RabbitHole facts** — exact constants from `crates/reticulum/src`, cited by
  name and confirmed by the crate's own tests. These do not change with your
  hardware.
- **LoRa general characteristics** — nominal physical-layer figures that are
  **region- and hardware-dependent**. They are given as ranges/illustrations to
  size a deployment, not as guarantees. Always calibrate against your radio,
  region, coding rate, and antenna.

---

## 1. Scope and status

| Layer | State in-repo today | Where |
| --- | --- | --- |
| Identity / destination / packet codec | Modelled, tested, never-panic | `packet.rs`, `identity.rs`, `destination.rs` |
| Announce + ingestion cache | Modelled (`AnnounceCache`) | `announce.rs` |
| Link establishment (request→proof→RTT) | Modelled FSM (`LinkInitiator`/`LinkResponder`) | `link.rs` |
| Delay-tolerant tunnel (store/flood/batch) | Modelled (Wave 14) | `tunnel.rs`, `floodfill.rs`, `batch.rs` |
| **RNS interface / LoRa driver / socket loop** | **Not present — future work** | — |
| **Payload fragmentation** | **Deferred — single-packet only** | `tunnel.rs` (`MAX_TUNNEL_PAYLOAD`) |

The tunnel core is deliberately a **model** of what an LXMF *propagation node*
does — accept messages for offline peers, hold them, sync them onward — with
RabbitHole's own framing, not the LXMF or RNS wire format (see the module docs in
`tunnel.rs` and divergence 6 in `lib.rs`). The interop mapping is what the future
adapter owns.

---

## 2. The link budget

### 2a. RabbitHole size caps (facts)

Every RabbitHole constant below is defined in `crates/reticulum/src` and pinned
by a test (`packet::max_data_len_budgets`, `batch::budget_math_is_pinned`).

| Constant | Value | Meaning | Source |
| --- | ---: | --- | --- |
| `packet::MTU` | **500** | Reticulum wire MTU; no encoded packet may exceed it | `packet.rs` |
| `max_data_len(Header1)` | 481 | Payload budget, single-address packet (19-byte header) | `packet.rs` |
| `max_data_len(Header2)` | **465** | Payload budget, transport-routed packet (35-byte header) | `packet.rs` |
| `BATCH_FRAMING_RESERVE` | 24 | Link-cipher reserve: 8-byte counter + `crypto::TAG_LENGTH` (16) | `batch.rs` |
| `DEFAULT_BATCH_BUDGET` | **441** | `465 − 24` — the encoded-size cap of one tunnel batch | `batch.rs` |
| `MAX_TUNNEL_PAYLOAD` | **384** | Largest payload in one `TunnelMessage` (single-packet, no fragmentation) | `tunnel.rs` |
| `TUNNEL_MESSAGE_HEADER_LEN` | 29 | Per-message header (`id 16 · created 8 · hops 1 · ttl 1 · prio 1 · len 2`) | `tunnel.rs` |
| `BATCH_ENVELOPE_HEADER_LEN` | 2 | Batch envelope (`version 1 · count 1`) | `batch.rs` |

**Why 441.** A tunnel batch rides in the *payload* of a Reticulum packet. The
budget starts from the payload space of the largest routed header a
store-and-forward packet uses — HEADER_2 = `max_data_len(Header2)` = **465** —
and reserves **24** bytes (`BATCH_FRAMING_RESERVE`) for the per-direction
link-cipher framing (an 8-byte counter plus the 16-byte AEAD tag,
`crypto::TAG_LENGTH`) in case the adapter sends the batch over an encrypted link.
`465 − 24 = 441 = DEFAULT_BATCH_BUDGET`. A single maximum message
(`TUNNEL_MESSAGE_HEADER_LEN + MAX_TUNNEL_PAYLOAD = 29 + 384 = 413`) plus the
2-byte envelope always fits (415 ≤ 441), so no valid message is ever unbatchable
— this invariant is checked at compile time in `batch.rs`.

**Messages per full batch (facts).** A 441-byte batch carries between **1**
message (a 384-byte payload → 413 bytes encoded) and **15** messages
(empty/tiny payloads → 29 bytes each: `⌊(441−2)/29⌋ = 15`). The `count` field is
a `u8`, so `MAX_BATCH_MESSAGES` = 255 is the hard ceiling, but the size budget
binds first.

### 2b. LoRa data rates (general characteristics — region/hardware dependent)

The figures below are **nominal LoRa PHY bitrates** at coding rate 4/5, the
common LoRaWAN data-rate points. They are **illustrative** and vary with region,
radio, coding rate, low-data-rate-optimize, and link margin. Treat them as
order-of-magnitude sizing only.

| SF / BW (kHz) | Nominal bitrate | ≈ bytes/sec | Airtime character |
| --- | ---: | ---: | --- |
| SF12 / 125 | ~250 bit/s | ~31 B/s | Longest range, seconds per packet |
| SF11 / 125 | ~440 bit/s | ~55 B/s | Long range |
| SF10 / 125 | ~980 bit/s | ~122 B/s | |
| SF9 / 125 | ~1760 bit/s | ~220 B/s | |
| SF8 / 125 | ~3125 bit/s | ~390 B/s | |
| SF7 / 125 | ~5470 bit/s | ~684 B/s | Shortest range, fastest |
| SF7 / 250 | ~11 kbit/s | ~1375 B/s | Wider band |
| SF7 / 500 | ~22 kbit/s | ~2740 B/s | Widest common band |

> **Caveat.** These are raw PHY bitrates before preamble and header overhead, and
> before duty-cycle limits (§3). The LoRa PHY frame also caps a *single* on-air
> payload at ~255 bytes, so a 441-byte batch is more than one radio frame in
> practice — the interface layer (future work) fragments it at the bearer level,
> adding a preamble per frame. Real airtime is therefore **higher** than the
> `bytes ÷ bytes-per-sec` first approximation used below. Calibrate with your
> radio's airtime calculator.

### 2c. Worked example — airtime for a full 441-byte batch

`s/batch ≈ DEFAULT_BATCH_BUDGET (441) ÷ bytes-per-sec`. "msgs/min" uses the
1–15 messages-per-batch range from §2a; pick the end that matches your typical
payload size. Continuous transmission (no duty cycle yet — see §3).

| SF / BW | ≈ B/s | s per 441 B batch | batches/min | msgs/min (1–15 per batch) |
| --- | ---: | ---: | ---: | --- |
| SF12 / 125 | 31 | ~14.1 s | ~4 | ~4 – 64 |
| SF11 / 125 | 55 | ~8.0 s | ~7 | ~7 – 112 |
| SF10 / 125 | 122 | ~3.6 s | ~17 | ~17 – 250 |
| SF9 / 125 | 220 | ~2.0 s | ~30 | ~30 – 450 |
| SF8 / 125 | 390 | ~1.1 s | ~53 | ~53 – 795 |
| SF7 / 125 | 684 | ~0.6 s | ~93 | ~93 – 1400 |
| SF7 / 250 | 1375 | ~0.3 s | ~187 | (fast) |
| SF7 / 500 | 2740 | ~0.16 s | ~372 | (fast) |

These are **ceilings ignoring duty cycle**. On a regulated band the duty-cycle
governor (§3) is what you actually deploy against, and it is far lower.

### 2d. Link establishment cost (facts) — prefer the tunnel on slow links

Establishing an encrypted `link` (`link.rs`) is a three-packet, 1.5-round-trip
handshake **before any data flows**:

| Handshake packet | Body | Encoded (HEADER_1, +19) | Source |
| --- | ---: | ---: | --- |
| Link request | `LINK_REQUEST_LENGTH` = 64 | 83 B | `link.rs` |
| Link proof | `LINK_PROOF_LENGTH` = 96 | 115 B | `link.rs` |
| RTT activation | 8 B plaintext → 32 B enc (`LINK_MESSAGE_MIN_LENGTH` 24 + 8) | 51 B | `link.rs` |

That is ~249 bytes over three packets (three preambles, three airtime windows)
just to reach `LinkState::Active`, and it must complete inside
`DEFAULT_ESTABLISHMENT_TIMEOUT_MS` = **6000 ms** — which `link.rs` documents as
upstream's *single-hop* budget (SPEC-CHECK: upstream scales it by hop count and
per-interface latency). At SF12/125 (~31 B/s) those 249 bytes alone are ~8 s of
airtime — the 6 s single-hop timeout is blown before the handshake finishes.

**Operator takeaway:** on the slowest SFs, prefer the **delay-tolerant tunnel
path** (`tunnel`/`floodfill`/`batch` — unreliable, store-and-forward, no
handshake, no per-packet ACK) over establishing interactive links. Reserve links
for faster SF7-class hops or for when the timeout is rescaled by the adapter.

---

## 3. Governor tuning

The batcher (`batch.rs`) throttles each peer with an independent **token bucket**
so a slow LoRa link is never overrun. All three knobs below are **policy, not
protocol** (SPEC-CHECK in `batch.rs`) — tune them freely; the values here are
**illustrative starting points**, not code defaults.

### 3a. Per-peer `TokenBucket` → duty cycle

`Batcher::new(budget, max_batch_age_ms, capacity_bytes, refill_per_sec)` creates
each peer's bucket with:

- **`capacity_bytes`** — the burst size. Start at `DEFAULT_BATCH_BUDGET` (441):
  "let one full packet burst, then throttle." Larger allows a burst of several
  packets after a quiet period.
- **`refill_per_sec`** — the *sustained* byte rate. This is the knob you map to
  your regulatory duty cycle.

The bucket tracks tokens in **milli-bytes** internally (`MILLI` = 1000), so
sub-byte-per-second refill accumulates exactly — but `refill_per_sec` itself is
an integer bytes/sec, so **1 B/s is the finest nonzero sustained rate**.

**Duty-cycle mapping.** For a band that permits an airtime fraction *d* (EU868
default sub-bands are **1%**), the safe sustained byte rate is

```
refill_per_sec  ≈  nominal_bytes_per_sec  ×  d
```

because airtime = bytes ÷ bytes-per-sec, and you may spend fraction *d* of each
hour on air. EU868 1% = **36 s airtime/hour**:

| SF / BW | ≈ B/s | Bytes/hour at 1% | Safe `refill_per_sec` (1%) |
| --- | ---: | ---: | ---: |
| SF12 / 125 | 31 | ~1125 | ~0.3 B/s → **see note** |
| SF11 / 125 | 55 | ~1980 | ~0.5 B/s → **see note** |
| SF10 / 125 | 122 | ~4410 | ~1 B/s |
| SF9 / 125 | 220 | ~7920 | ~2 B/s |
| SF8 / 125 | 390 | ~14060 | ~4 B/s |
| SF7 / 125 | 684 | ~24600 | ~7 B/s |
| SF7 / 250 | 1375 | ~49500 | ~14 B/s |
| SF7 / 500 | 2740 | ~98550 | ~27 B/s |

> **Honest limit at the slowest SFs.** At SF12/125 and SF11/125 the 1% budget
> works out to *below* 1 B/s, but `refill_per_sec` can go no finer than 1 B/s.
> A refill of 1 B/s at SF12 = 3600 B/hour ≈ 116 s airtime/hour ≈ **3.2% duty** —
> over the EU 1% limit. So on the two slowest data rates the token bucket alone
> cannot enforce compliance; you must additionally rely on `max_batch_age_ms`
> (below) plus the adapter's own per-hour airtime accounting to stay legal. This
> is a real gap to close in the interface layer, not something the model hides.

### 3b. `max_batch_age_ms` — the latency/efficiency knob

A partial (under-budget) batch is normally **held** to fill up — fewer, fuller
packets amortize the preamble/header overhead and save airtime. To stop a message
waiting forever, a partial batch flushes once its oldest queued message has waited
`max_batch_age_ms` (a *full* batch always flushes immediately, subject to the
token bucket).

| Deployment | Illustrative `max_batch_age_ms` | Rationale |
| --- | ---: | --- |
| Interactive-ish (SF7, short hop) | 1000–5000 | Low latency, accept smaller batches |
| Delay-tolerant mesh | 30000–120000 | Fill batches; airtime efficiency wins |
| Intermittent / offline-heavy | up to hours | Batches fill while the peer is unreachable anyway |

Smaller = lower latency, more (emptier) packets, more airtime. Larger = fuller
packets, less airtime, higher delivery latency.

### 3c. Flood-fill `ttl_hops` — mesh depth vs amplification

Each `TunnelMessage` carries a hop horizon `ttl_hops` (`tunnel.rs`); `FloodFill`
(`floodfill.rs`) relays to **every peer except the source and known holders** and
drops the message once `hops >= ttl_hops`. Because each hop re-floods, a node with
*N* tunnel peers can produce up to ~*N* transmissions per hop; the message-level
horizon bounds how deep that goes. The `ForwardLedger` seen-by set suppresses
re-sends to peers already known to hold a message, so real amplification is far
below the *N^hops* worst case — but keep `ttl_hops` **small** on RF:

| `ttl_hops` | Reaches | Airtime/amplification |
| ---: | --- | --- |
| 1 | Direct peers only | Minimal |
| 2–3 | Small mesh / neighbourhood | Bounded, recommended default range |
| 4+ | Wider mesh | Grows fast; justify per topology |

This message-level horizon is **distinct** from `RNS.Packet` transport hop limits
(SPEC-CHECK in `floodfill.rs`); real RNS/LXMF propagation would additionally bound
fan-out by learned topology and per-peer sync state.

---

## 4. Topology patterns

| Pattern | RabbitHole pieces | TTL sizing |
| --- | --- | --- |
| **Single gateway + LoRa uplink** | One burrow behind an RNS gateway with a LoRa interface; outbound queued in the `Batcher`, one governed peer (the uplink); inbound announces filtered by `AnnounceCache` | `AnnounceCache` `min_interval_ms` long enough that flooded re-announces don't burn airtime; `ttl_ms` ≥ your announce interval |
| **Multi-hop store-and-forward mesh** | Intermediate nodes *hold* (`MessageStore` TTL + de-dup) and *relay* (`FloodFill` + `ForwardLedger`); each node governs each peer separately | `MessageStore` `ttl_ms` **> worst-case end-to-end delivery time across all hops**; `ForwardLedger` `ttl_ms` ≥ the window over which a re-flood of the same id should still be suppressed |
| **Intermittent / delay-tolerant** | A node offline for hours: neighbours keep the message in `MessageStore` until it returns; content-addressed ids + the seen-set make redelivery a `Duplicate`, not a re-queue | `MessageStore` `ttl_ms` **> the longest expected offline window** (a message older than its own TTL is `Expired` *forever*, since `created_ms` is part of its content id) |

All three are the model's analogue of an **LXMF propagation node** — accept for
offline peers, hold, sync onward — as `tunnel.rs` states explicitly. The
`MessageStore` de-dup discipline (TTL + seen-set + injected clock) mirrors
`AnnounceCache` one layer up. A message's `OfferOutcome` is one of `Accept`,
`Duplicate`, `Expired`, or `TooLarge`; only `Accept` stores a body.

**TTL sizing rule of thumb:** a store/ledger TTL must exceed the sum of
(max per-hop queueing under the governor) + (max offline window) across the path,
or in-flight messages expire before they arrive. On a 1%-duty SF12 link where a
single peer drains only ~1 KB/hour, per-hop queueing can be *hours*, so size TTLs
in hours-to-days for a genuinely off-grid mesh.

---

## 5. Power

RabbitHole cannot quote milliwatts — draw is entirely a function of your radio,
TX power, MCU, and sleep support. What it *can* say is directional and firm:

- **Transmit airtime dominates energy.** On LoRa nodes the radio's TX current for
  the duration of on-air time is the largest, most controllable draw. Therefore
  **minimizing airtime *is* minimizing power**, and every airtime lever in this
  crate is also a power lever:
  - **Batching** (`Batcher`, `max_batch_age_ms`) — fuller packets amortize the
    preamble/header per message: fewer, larger transmissions.
  - **De-duplication** (`MessageStore` seen-set, `ForwardLedger`, `AnnounceCache`)
    — never spend airtime re-sending a message a peer already holds, or a
    re-flooded announce.
  - **Hop limits** (`ttl_hops`) — bound how many times a message is retransmitted
    across the mesh.
- **Duty-cycle compliance is power management too.** The token-bucket
  `refill_per_sec` you set for legality (§3a) is also a hard cap on TX energy per
  hour.
- **Sleep between polls.** The whole tunnel core is **sans-I/O and reads no
  clock** — it never spins. The future adapter injects `now_ms` on each poll, so
  its loop is free to sleep the MCU/radio between polls; nothing in the model
  forces a continuous wake. Poll cadence is an adapter/power trade-off, not a
  model constraint.

> **No fabricated figures.** Exact mA/mW and battery-life numbers are
> hardware-specific (radio, TX dBm, region, antenna, MCU sleep). Measure them on
> your board; this section is directional only.

---

## 6. Deferred — what's needed next

Before any live RF deployment, the following are outstanding. Nothing here is a
today-fact; it is the roadmap the future adapter must complete.

1. **Live RNS interface / sidecar adapter.** The entire transport slice — socket
   loop, RNS interfaces (LoRa/serial/TCP/…), peer discovery from announces, and
   wrapping batches in real `Packet`s or link/resource transfers. Today `nothing`
   in `crates/reticulum` opens a socket.
2. **Payload fragmentation.** The tunnel model is **single-packet**:
   `MAX_TUNNEL_PAYLOAD` = 384, and a larger payload is *rejected*
   (`TunnelError::TooLarge` / `OfferOutcome::TooLarge`), not split across packets.
   Multi-packet messages (and the LoRa PHY's own ~255-byte frame fragmentation)
   are deferred.
3. **Real hardware airtime calibration.** Every physical-layer number in §2–§3 is
   a nominal, region/hardware-dependent illustration. Replace them with measured
   airtime for your actual radio, region, coding rate, and preamble before
   trusting any duty-cycle or battery budget.

### The 7 SPEC-CHECK reconciliation points (interop gate)

The decision record ([`docs/research/reticulum-decision.md`](research/reticulum-decision.md))
gates **any exchange with real RNS/LXMF/NomadNet peers** on reconciling seven
test-pinned `// SPEC-CHECK:` anchors (each isolated so it can be fixed in one
place). Verbatim from that record:

| # | Point | File |
| --- | --- | --- |
| 1 | **Link cipher / HKDF `info`** — per-direction ChaCha20-Poly1305 pair + crate-versioned HKDF `info` vs upstream's AES-128-CBC+HMAC Fernet token / empty context | `link.rs` |
| 2 | **Link RTT encoding** — `LinkRtt` is BE `u64` ms; upstream is a MessagePack float of seconds | `link.rs` |
| 3 | **Establishment timeout scaling** — `DEFAULT_ESTABLISHMENT_TIMEOUT_MS` = 6000 is upstream's *single-hop* budget; upstream scales by hop count + per-interface latency (**directly relevant to slow LoRa hops, §2d**) | `link.rs` |
| 4 | **0.8-era link-request MTU signalling** — `LinkRequest::from_bytes` requires exactly 64 bytes; newer upstream may append link-MTU signalling to tolerate | `link.rs` |
| 5 | **Packet MDU / IFAC modelling** — upstream's flat `Packet.MDU = 464` (max header + 1 IFAC byte) vs this codec's exact per-header budgets (481 / 465) and **no IFAC body**; adding IFAC revisits `max_data_len` | `packet.rs` |
| 6 | **Packet context bytes** — `NONE`/`LRPROOF` match upstream; the link-lifecycle contexts follow RNS constants understood at 0.7/0.8 | `packet.rs` |
| 7 | **Announce de-duplication policy** — `AnnounceCache` is a simplified per-destination TTL + min-interval policy, not upstream `Transport`'s packet-hash dedup + per-interface `announce_rate_target` | `announce.rs` |

Two further **module-level divergences** feed the same gate: the **announce wire
field order** (signature last here vs before app-data upstream; signed content is
identical) and the **LXMF payload packing** (deterministic `postcard` + hash-only
signing vs upstream MessagePack + stamp/PoW cost). Ratchets and IFAC bodies are
out of scope for the model and are net-new transport-slice work.

**Gate summary (from the decision record):** no `rabbithole-reticulum` code path
may open a socket to the public RNS mesh until all seven SPEC-CHECK points and the
two divergences are reconciled *and* a conformance run against a reference Python
`RNS` peer passes for identity announce, link establishment, and an LXMF
round-trip.

---

### Provenance of the numbers in this guide

- **Grep-checkable RabbitHole facts:** `MTU` = 500, `max_data_len(Header2)` = 465,
  `DEFAULT_BATCH_BUDGET` = 441, `BATCH_FRAMING_RESERVE` = 24 (8 + `TAG_LENGTH` 16),
  `MAX_TUNNEL_PAYLOAD` = 384, `TUNNEL_MESSAGE_HEADER_LEN` = 29,
  `BATCH_ENVELOPE_HEADER_LEN` = 2, `MAX_BATCH_MESSAGES` = 255,
  `DEFAULT_ESTABLISHMENT_TIMEOUT_MS` = 6000, `LINK_REQUEST_LENGTH` = 64,
  `LINK_PROOF_LENGTH` = 96, `LINK_MESSAGE_MIN_LENGTH` = 24. All in
  `crates/reticulum/src`, all test-pinned.
- **Illustrative, hardware/region-dependent:** every LoRa bitrate, bytes/sec,
  airtime, duty-cycle byte budget, and every suggested `capacity_bytes`,
  `refill_per_sec`, `max_batch_age_ms`, and `ttl_hops` value. These are sizing
  aids, not defaults or guarantees — calibrate against your radio and regulator.
