# Federation ingest policy

Every authenticated federation peer has independent, shared control-traffic
budgets. Concurrent inbound and outbound links and reconnections spend the same
buckets, keyed by the peer's proven Ed25519 identity. An exhausted budget closes
the offending session; another peer retains its own allowance. This adds to the
existing connection/authentication limits, approval checks, immutable origin pins,
message-size bounds, signature verification, and event authorization.

| Live setting | Default | Meaning |
| --- | ---: | --- |
| `federation_ingest_frames_per_sec` | 32 | Control frames replenished each second |
| `federation_ingest_frames_burst` | 128 | Maximum available control frames |
| `federation_ingest_bytes_per_sec` | 4194304 | Control payload bytes replenished each second |
| `federation_ingest_bytes_burst` | 16777216 | Maximum available control payload bytes |
| `federation_ingest_events_per_sec` | 1024 | Event work items replenished each second |
| `federation_ingest_events_burst` | 4096 | Maximum available event work items |
| `federation_denied_keys` | `[]` | Explicitly denied complete 64-digit public keys |

Rates and bursts are unsigned 32-bit whole numbers. Zero rate gives a finite,
non-refilling burst. Zero burst refuses every nonempty operation in that budget;
it does not disable enforcement. These settings apply through `ctl config set`
and the operator console and persist in the existing config file. They are
independent of `ratelimit_enabled`, which controls the other endpoint classes.

Frame and payload-byte charges happen before message dispatch/decoding, including
unknown messages and the initial dialer-side catalog exchange. The event budget
charges all delivered events, offered IDs, and requested IDs before event-level
signature work or database scans. Invalid, duplicate, and unknown-board work is
not free. A batch that exceeds its event allowance is rejected whole. Existing
wire-size limits still apply before and after transport decoding. These are
control-channel budgets; S2S bulk file streams retain their separate transfer
limits and grant checks.

For an immediate operator deny, set the list to a TOML array, for example:

```text
ctl config set federation_denied_keys '["<complete public key>"]'
```

Use actual 64-digit hexadecimal keys; placeholders are refused. At most 4096
entries are accepted. This deny overrides configured outbound peers and stored
approvals, without erasing approval or changing an origin pin. Removing the deny
allows the normal approval and provenance checks to govern reconnection again.
Denied and unapproved identities allocate no quota buckets. The deny is checked
at authentication completion, outbound dial admission, each incoming frame and
delivered event, and the existing one-second idle-session policy tick. A handler
already performing I/O finishes its current operation before the next check.

Live changes retain existing balances. On the next policy observation, elapsed
time is replenished at the previous rate and balances clamp to the new capacity;
increasing a burst does not grant existing peers a fresh allowance. The new rate
then governs future refill. Denying, reconnecting, or changing limits never clears
quota debt. Each of the three maps holds at most 4096 identities. Only fully
refilled entries may be reclaimed; when a table is full, a new identity is refused
rather than evicting another peer's debt. Restarting the process resets these
in-memory quotas, as with other rate limits.

Validation uses controlled-clock budget tests, saved/live configuration tests,
production ingest fixtures, and isolated authenticated QUIC sessions. The QUIC
fixtures verify simultaneous-link/reconnect accounting, healthy-peer isolation,
and live deny/removal while approvals and origin pins remain intact. They do not
claim an external federation deployment or third-party client interoperability.
