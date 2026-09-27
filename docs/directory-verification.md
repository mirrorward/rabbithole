# Directory signatures and saved listings

Looking Glass separates a server's signed statement from a directory's health
observations. A valid signature says that the displayed signing key made the
claim. It does not establish a person's identity, reputation, control of a DNS
name, current reachability, or uptime.

Clients retain the complete statement and verify it locally, including when a
saved listing is read without a network connection. They never persist a
`verified` flag as evidence. A missing proof is unverified; an invalid signature
or a statement that does not match the selected name/address is invalid. An
expired statement retains its valid-signature distinction but is visibly stale.
Future timestamps more than five minutes ahead of the client clock are not
accepted as current.

Two existing wire contracts are supported:

- The HTTP coordinator announce is the existing `{descriptor, signature}` JSON
  produced by `burrow::announce::signed_body`. Ed25519 covers recursively sorted,
  compact UTF-8 canonical JSON. Verification binds the exact announced name and
  selectable URI, including scheme, port, and path. The declared `ttl` must be
  30–3600 seconds, matching the server signer, and controls freshness. Displayed
  health and other directory metadata are separate observations.
- The legacy tracker statement is the existing `rhp-trk-descriptor-v1` plus
  postcard encoding. Its signature covers a name and `SocketAddr`, not a
  WebSocket scheme. The client explicitly labels this an address signature. The
  historical wire bytes and signing domain are unchanged. In the absence of an
  expiry field, clients treat statements older than 24 hours as stale; this is
  independent of the tracker's registration TTL and health timers.

The tracker `INDEX` response keeps its first nine columns and adds a tenth:
`proof`, the complete signed postcard document as hex, or `-` when absent.
Older replies still parse, but `signed=yes`, a key prefix, and a generation are
not proof and do not produce a verified badge. JSON listings may carry that same
hex document in `signedDescriptor`; coordinator rows carry their existing full
`descriptor` and `signature` fields.

The public aggregate currently omits signatures. Clients may obtain matching
proofs from the fixed standard Looking Glass endpoint. Only an exact endpoint
and signed-name match can enrich a row, and an invalid supplied proof is never
hidden by another source's good proof. No URL supplied by a directory row is
fetched. Missing proofs remain explicitly unverified. Changing a bookmark from
`ws://` to `wss://`, or changing a path, cannot borrow the old endpoint's badge.

Saved listings are source-bound, versioned envelopes capped at 128 rows and
512 KiB. Individual proof documents are capped at 16 KiB (twice that for hex).
They contain original proofs and source/time metadata, not trusted state. Proofs
are rechecked against the current clock when displayed. Cached health is not
shown as a current observation. A separate bounded history keeps the newest valid claims through missing,
invalid, future-dated, and empty refreshes. Invalid current proof remains visible.
Within the retained history, a lower signed generation for the same exact
endpoint and key cannot overwrite the saved newer statement; a different key
is a separate self-certified claim, not a continuity guarantee or key pin.
History is capped at 128 claims and shares the 512 KiB envelope limit; oldest
claims are evicted when necessary, so this is bounded rollback protection, not
an indefinite signed history or an external transparency log.

The native directory client saves bounded snapshots below
`$XDG_CACHE_HOME/rabbithole`, or `~/.cache/rabbithole`. Browser and desktop webview
storage use the same envelope and verification rules. Native TCP replies include
the actual tracker source; a local Looking Glass never borrows the public
tracker's label or cache. Older shells with unknown provenance remain explicitly
labeled and are not persisted as a named tracker. Failure to write a cache
never makes a live listing fail. Invalid or oversized saved data is discarded.

Validation uses deterministic signed and tampered fixtures, cache expiry and
rollback tests, byte compatibility with the old tracker signature representation,
and a real loopback tracker INDEX round trip. Public coordinator compatibility
was separately checked by verifying its current two public row signatures and
exact WebSocket endpoint bindings; that read does not establish future service
availability or trust in the servers themselves.
