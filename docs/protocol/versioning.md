# Wire compatibility policy

How the RabbitHole Protocol (RHP) evolves without breaking peers, and the
mechanical guard that keeps *accidental* wire changes from shipping.

The rules here govern the `crates/proto` crate — the wire types compiled by
the server, every native client, and the wasm web client. If the code and
this document disagree, that is a bug in one of them.

## The one invariant

**`(family, message_type)` is a permanent routing key.** A frame is decoded
by looking up its `(family, message_type)` pair and deserializing the payload
into the struct registered for it (see [`framing.md`](framing.md)). Once a
pair has shipped, its meaning is frozen: the bytes a peer sends under
`(2, 1)` must always be a `ChatSend`. Reusing a retired number for a new
message is the worst kind of break — no decode error, just one message's
bytes silently interpreted as another.

## Additive-only within a protocol version

`PROTOCOL_VERSION` (in `crates/proto/src/version.rs`) only bumps on an
*incompatible* framing change. Everything below is compatible and does **not**
bump it:

**Compatible (additive) — no version bump:**

- **New message types.** Give them a fresh, unused `MESSAGE_TYPE` in their
  family (or a new family number). Old peers that don't know the type decode
  the frame fine and answer `Unsupported` — they never fail to parse the
  stream, because the payload is opaque to the frame's own serde tree.
- **New optional fields on a `#[non_exhaustive]` payload struct**, appended at
  the end, deserializing to a sensible default when absent.
- **New enum variants on a `#[non_exhaustive]` enum** (e.g. a new
  [`ErrorCode`]) — unknown variants degrade rather than fail, via the explicit
  `Other(u16)` escape hatch where one exists.
- **New capability strings** in `Hello`/`HelloAck` (see below).

**Breaking — requires a `PROTOCOL_VERSION` bump (and, pre-1.0, a minor bump):**

- Renumbering an existing message (changing its `MESSAGE_TYPE`).
- Removing or repurposing a message type or family number.
- Changing the type, order, or meaning of an existing payload field.
- Removing an enum variant, or changing an existing one's shape.
- Any change to the frame header layout or the length-delimited framing.

## Version negotiation and capability flags

Two independent axes, deliberately kept separate:

1. **Protocol version** — the *framing* contract. Negotiated once, in the
   first `Hello`/`HelloAck` exchange: each side offers its version and the
   lower common one is chosen (`ProtocolVersion::negotiate`), rejected below
   `MIN_SUPPORTED_VERSION`. This axis moves rarely, only for the breaking
   changes listed above.

2. **Capabilities** — *optional features* layered on a given version.
   String-keyed (`CapabilitySet`), so a server advertises `session-resume`,
   `key-auth`, etc., the client intersects with its own, and both sides use
   only what they share. New features ride this axis, not a version bump:
   adding a capability (and the message types that back it) is additive.
   Third parties can namespace their own (`x-example-thing`) without a central
   registry.

The upshot: a new feature is *a new capability string plus new message types
with fresh numbers*. That is always backward compatible, so the version stays
put.

## `#[non_exhaustive]` discipline

Payload structs and enums that are expected to grow are marked
`#[non_exhaustive]`. This:

- forces downstream `match` on payload enums to carry a wildcard arm, so a
  peer that learns a new variant later still compiles;
- forces struct construction through constructors (e.g. `Hello::new`), so
  appending a field is not a breaking source change for callers;
- signals intent: "this type is designed to gain fields/variants additively."

When you add a field to a `#[non_exhaustive]` struct, append it and make it
default-able on decode. When you add an enum variant, prefer keeping an
`Other(u16)`-style escape hatch for forward compatibility.

## The mechanical guard: the registry golden

Discipline that relies on remembering is discipline that eventually fails.
The `proto` crate encodes the invariant as code:

- `crates/proto/src/registry.rs` holds `REGISTRY` — every
  `(family, message_type, name)` triple that exists today, one entry per
  `impl Message`, each built from the type's **own** `Message` consts so a
  renamed/renumbered const breaks compilation or shifts the snapshot.
- `crates/proto/tests/registry.rs` asserts three things:
  - **no collisions** — no two messages share a `(family, message_type)`;
  - **completeness** — `REGISTRY.len()` equals the checked-in `EXPECTED`
    count, so adding or removing a message without updating the registry
    fails the build (the "did you mean to change the wire?" tripwire);
  - **stability** — the registry, serialized to a canonical sorted form,
    matches the checked-in golden `crates/proto/tests/wire-registry.golden`.

Any intentional wire change is re-blessed deliberately:

```
BLESS=1 cargo test -p rabbithole-proto --test registry golden_matches
```

and the updated golden is committed alongside the change. An *un*intentional
change — a fat-fingered number, a duplicated type, a stray removal — fails the
test before it can reach a peer. That is the whole point: the golden turns
"be careful with wire numbers" into a check the CI runs for you.

## Reserved families

Family numbers `8` (FEDERATION) and `9` (RADIO) are allocated in
`Family` but carry no native message types yet — federation runs on its own
S2S endpoint and radio is a future wave. They are reserved: do not reuse the
numbers for anything else.

## Pre-1.0 note

Before `1.0`, the wire may still change between minor releases — the project
is young and families are still settling. What the registry golden guarantees
is that such changes are always *deliberate and reviewed*, never accidental.
After `1.0`, the additive-only policy above becomes a hard compatibility
promise.

[`ErrorCode`]: ../../crates/proto/src/error.rs
