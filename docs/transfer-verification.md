# Transfer verification details

Native downloads show rejected source data in the Transfers manager and the
Files transfer queue. Each detail identifies the source by its position in that
download's source list, the first affected byte offset, a fixed explanation,
and the number of rejected responses. Source numbers are local to one attempt;
they are not peer identities or reputation scores.

The download may continue through another source. A completed row keeps the
details and says that the download is verified. A failed attempt keeps its
source details separately from the reason the transfer ended; a stopped
attempt keeps the details the UI has already received. Local
storage errors, worker failures, and transport failures do not accuse a source
of supplying invalid data. Reaching the last chunk is not success: only the
final verification can mark a native download Done, including origin fallback.

## Bounds and routing

The swarm scheduler retains at most 32 source/reason pairs per attempt, with
saturating occurrence counts and a separate count for additional rejected
responses. Observer notifications coalesce. The native bridge forwards
cumulative snapshots, including the final snapshot before a fetch returns
success or failure, and the UI validates and caps the records again. Stopping
a download drops the fetch and may omit its last unforwarded update. Byte
offsets cross JavaScript as decimal strings so large offsets remain exact.
Remote labels, paths, and
untrusted error messages are not included in these details.

Each native attempt receives a fresh process-wide ID that JavaScript can
represent exactly. Native attempts and server transfer tickets occupy distinct
UI namespaces. Events update only the owning burrow's active attempt; late
events cannot create a row, alter a retry, or revive a terminal attempt. Retry
uses the row's burrow, even if another burrow is focused. Concurrent attempts
cannot write the same destination or share the same burrow/root partial file.

Diagnostics stay with the in-memory transfer row. They are not a persistent
peer reputation database, and the browser's inline download path does not
invent swarm source details.

## Validation

Scheduler tests cover a rejected source alongside a healthy source, correct
final bytes, no progress credit for rejected data, bounded/coalesced snapshots,
retention after failure or cancellation, and local failures without false
source blame. Desktop tests cover IPC bounds, exact offsets, attempt IDs,
destination reservations, and rejecting an origin file with the wrong root
before reporting Done. UI reducer tests cover event routing, terminal states,
retry isolation, and retained diagnostics.

These are automated engine, native, and UI checks. Manual native visual
inspection is a separate check; no such inspection is claimed for RH-61.
