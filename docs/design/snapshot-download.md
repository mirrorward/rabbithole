# Getting a snapshot off the machine

Status: design, not built. Written 2026-09-22 against 0.266.0, rewritten the
same day after an adversarial review found the first draft's premise false.

The Backups pane (0.222.0) makes a snapshot, checks one, and removes one.
`docs/design/admin-console.md` lists two things it does not do: restore, and
**downloading a snapshot**. Restore is deliberate and stays that way — a
running burrow cannot replace its own database, so restore is offline
(`burrow restore <snapshot-dir> --data-dir <dir>`) and the pane says so.

This note is about the other one.

## What a snapshot is

`apps/server/src/backup.rs` writes a directory holding, for a burrow of any
size, the whole burrow:

- `burrow.db` — every account and password hash, every DM, every board post,
  every moderation record.
- `identity/` — **the Ed25519 signing seed and the TLS key**. Whoever holds
  these is the burrow, to every peer and every client that has pinned it.
- `federation/` — approved peers, origin-key bindings, the signed catalog.
- `blobs/` — every file anybody has uploaded.

A snapshot is not a document an operator downloads. It is the burrow's
identity plus everything anyone ever told it in confidence, in one file.

## What the first draft got wrong

The draft argued against a browser download on the grounds that nothing
serves the seed today, so adding a way to would be the first. That was not
true, and the review said so with a working chain:

1. `http_web_root` was an ordinary config key. `ConfigSet` needs only
   `CONFIG_ADMIN`, and `surfaces::reconcile` applied it live.
2. `http.rs`'s static route serves **any** regular file under that root to
   **anonymous** `GET`s, with unknown extensions falling through to
   `application/octet-stream`.
3. So `ConfigSet http_web_root=<data_dir>` followed by
   `GET /identity/server_ed25519.seed` handed the burrow's signing key to
   anybody on the internet, with no session and nothing in the audit log.
   `ConfigSet backup_dir=<web root>/x` plus `BackupCreate` was the same hole
   through the snapshot folder.

Run against a throwaway burrow, that returned the 64-byte seed and the whole
database. **Closed in 0.267.0** (see below); the lesson stands: the premise of
a security design is the first thing to attack, and "nothing serves this
today" is a claim about every surface at once.

## What 0.267.0 closed

- The HTTP server never serves out of the burrow's own directories — the
  data directory or the snapshot folder — whatever the web root says. That
  check canonicalizes, so a symlink into them is refused too, and it holds
  when validation could not run: a config file edited by hand, a `--web-root`
  flag, a directory that became private afterwards.
- `http_web_root` refuses a path that contains, equals or sits inside the
  data directory or the snapshot folder, and `backup_dir` refuses one inside
  the web root. Both sides are made absolute first, so `target/x` and
  `/here/target/x` are recognised as the same place.
- `data_dir` is refused over the wire. The console was *told* it was
  read-only and the burrow did not enforce it; setting it would have moved
  what every other path resolves against, so the real data directory would
  have stopped counting as private.
- A snapshot is kept to its owner: the directory `0700` and `burrow.db`
  `0600`. `VACUUM INTO` wrote a fresh file at the umask, so every snapshot
  made from the console held a world-readable copy of every password hash.

## Three ways, and what each costs

### A. Copy to a path on the burrow's machine

The console names a destination directory; the burrow writes the snapshot
there.

- No new anonymous surface.
- Meets the real need whenever the destination is a mounted volume, an
  external disk, or a network share — which is what an operator backing a
  server up actually uses.
- Does **not** meet it for an operator who administers the burrow remotely
  and has no filesystem access. The obvious thing such an operator invents
  is "copy it into the web root and fetch it with my browser", which is
  exactly the hole above. The pane has to say why that will not work.
- A destination an operator names is a write primitive, and the checks it
  needs are listed below. This is not the thin wrapper the first draft
  claimed: nothing in the codebase copies an *existing* snapshot, so it is
  either new copy-and-verify code driven from the manifest, or it is
  "snapshot straight to a named directory" — a different action, which makes
  a new snapshot rather than the one the operator checked and clicked.

### B. Stream it over the authenticated wire

Family 5 (`crates/proto/src/transfer.rs`) already moves large files with a
ticket, windowed ranged chunks over the WebSocket, resume, and a blake3 root
check.

- Reuses a transport with authentication, rate classes and integrity built
  in, and it is the only option that serves the remote operator.
- `TransferOpen` is keyed on a file-library `node_id`, so this means a
  second, admin-only way to open a transfer — a new door into the
  byte-serving path.
- Hands the seed to a browser at the end, and `a[download]` is a no-op in
  the desktop webview, so the shell needs a native save path of its own.

### C. Put the snapshot in a file area

Rejected. File-area permissions are a living thing an operator edits; one
wrong grant and the burrow's identity is a download link.

## Recommendation

Neither A nor B as a next slice. 0.267.0 closed the hole that mattered, and
the honest position now is that the Backups pane says where snapshots are,
that they hold the burrow's signing key, and that copying them off the
machine is done where the machine is.

If the need is pressed, **B is the one that solves the stated problem** (a
remote operator with no filesystem access), and A is a convenience for
operators who could already use `scp`. Whichever is built, it is its own
slice with its own review, and it needs all of the following — every item
below is something the review found missing from the first draft.

### What either one needs

- **The gate as it exists**: `CONFIG_ADMIN` **and** `Role::Admin` — reuse
  `backup_operators_only!` verbatim. "Admin role only" describes a weaker
  check than the one already in place.
- **Every attempt audited**, not only the ones that work: `backup-copy` and
  `backup-copy-refused`, with the destination and the reason. A refusal that
  differs from a success is a filesystem oracle; the vague text goes on the
  wire and the specific reason goes to the audit log.
- **Rate limiting**, so probing is bounded.

### What A needs on top

- **A destination denylist computed from live config at the moment of the
  copy**, compared both ways (dest inside X, X inside dest) on canonical
  paths: `data_dir`, `backups_dir`, `http_web_root`, `doors_dir`,
  `qwk_spool_dir`, `ftn_inbound_dir`, `ftn_outbound_dir`. All are settable
  live, so a value read at startup is the wrong one. `qwk_spool_dir` is a
  destruction hazard, not a disclosure one: it removes `<spool>/<login>/`
  wholesale on every packet build.
- **A denylist at copy time cannot bind the future.** `http_web_root` can be
  pointed at the destination afterwards. That is why the serving-side check
  shipped in 0.267.0 is the real boundary, and any new destination needs the
  same treatment.
- **No check-then-use.** Create `<dest>/<snapshot-name>` with `fs::create_dir`
  (refusing `AlreadyExists` rather than merging), re-canonicalize the
  directory that was actually created, re-run the denylist against it, and
  write every file `create_new` so nothing is ever truncated. A failed copy
  removes the partial directory it made. Never write a file directly into
  the operator-named directory.
- **Owner-only at the destination, verified**: `0700` on the directory and
  `0600` on every file, read back afterwards, and a loud, specific answer —
  not a bare ack — when the destination filesystem will not hold it. The
  destinations this option recommends (USB sticks, SMB shares) are exactly
  the ones that drop those bits. If the intent is removable media, the
  snapshot should be encrypted rather than relying on a mode bit.

### What B needs on top

- **A single-use ticket that dies with the session**, and a stated answer for
  what happens to it on reconnect.
- **A snapshot packed without `identity/` unless the operator asks for it in
  so many words.** Restoring the data and re-keying the burrow is the usual
  thing an operator wants; the dangerous one should be the one they say out
  loud. This needs `restore` to have a defined behaviour for a snapshot with
  no `identity/`, which it does not have today — check `verify_snapshot` and
  the restore path before promising it.
