# The admin console: one place to run a burrow

> Kevin's requests of 2026-09-18 (three messages), what the code had, and the
> slice plan. Status is kept current as slices land.

## The requests

1. A Save button after every setting is bad design.
2. True/false settings are switches.
3. Each setting changed from its default can be put back with a Reset link.
4. Settings that only take certain values are dropdowns.
5. A sections navbar; a much cleaner admin area; things described.
6. The admin area manages everything: file areas and files, message boards,
   user editing, and the rest.
7. When a feature is enabled it actually starts (NNTP and the other gateways).

## What the code had (2026-09-18)

- **Config changes were never saved.** `ConfigSet` on the wire
  (`apps/server/src/handlers2.rs`) and `ctl config-set` only changed memory;
  `ServerConfig::save` was called from tests alone, and `--config` "defaults
  next to --data-dir" was not implemented (no path = defaults, no file read).
  "Saved; a restart is required" therefore lost the change. The old console
  copy claimed the opposite.
- **The console guessed.** It loaded 14 hard-coded keys one `ConfigGet` at a
  time, as text boxes, raw key names as labels. 102 keys exist.
- **No listener can start late.** `Burrow::start` snapshots every
  `*_enabled`/`*_addr` before the config goes live; the `spawn_*` handles sit in
  one unlabelled `Vec` that only `shutdown` aborts. There is no config-change
  event. `GatewayStats.enabled` is the config flag, not the listener, so a
  surface flipped on reads "enabled" with nothing bound. Two nested tasks
  (`ftn` outbound scanner, `federation` dialer) are untracked.
- **There is no FTP surface.** The gateways are telnet, finger, NNTP (reader,
  TLS, feed, feed TLS), Hotline, FidoNet (binkp), the HTTP surface and radio.
- **Much of moderation exists on the wire and nowhere in the client**:
  `PostEdit`/`PostDelete`, `NodeDelete`/`SetMetadata`, reports (ADMIN 30-34),
  quarantine (35/36), deny-hashes (37-40), `RoomKick`/`RoomMute`, `BoardCreate`
  (BOARD 12), `AreaCreate` (FILE 7), `ClassSet` (no form calls it),
  `AccountSet` role and class (only disable is surfaced).
- **Missing at every layer**: account create / delete / password reset / 2FA
  reset (ctl `account-create` only); board edit / delete / reorder; area edit /
  delete; file move / rename; invite list / revoke; class delete; audit log,
  backups and federation peers over the wire (ctl only); any ACL editing
  (`AclRepo` is never called); server-wide ban (`USER_BAN` is unchecked).

## Decisions

- **The burrow describes its own settings** (`ConfigDescribeRequest` /
  `ConfigDescription`, ADMIN 15/16): value, default, kind, choices, live or
  restart, secret, read-only. Derived from `set_key` itself on a scratch copy
  of the defaults, so it cannot disagree with the setter. Reset and dropdowns
  are then right for whichever burrow version you are on. Words (sections,
  labels, descriptions) are the client's (`ui-web::admin_catalog`); a key the
  client has no words for lands in Advanced under its own name.
- **Credentials are described, never disclosed**, and the audit log records
  that one changed, not what to.
- **One save.** Edits are staged; a bar says how many, how many need a restart,
  and offers Discard and Save. Each row still reports its own outcome.
- **A change is written to `burrow.toml` or it is refused**: validate on a copy,
  write the one key with `toml_edit` (comments and layout kept, env and CLI
  overrides never baked in), then swap it live.
- **Surfaces are supervised**: a reconciler owns each listener, starts and stops
  it when its keys change, and reports `Listening(addr)`, `Off` or
  `Failed(why)`, which the console shows beside the switch.

## Slices

| # | Slice | Status |
|---|---|---|
| 1 | Described settings, saved to disk; the console's shell: sections, switches, dropdowns, Reset, one save bar, descriptions | 0.216.0 |
| 2 | Surfaces start and stop live, and say whether they are listening | 0.217.0 (`apps/server/src/surfaces.rs`; federation, port mapping and doors still wait for a restart) |
| 3 | People: create, role, class, disable, password and 2FA reset; classes with named capabilities; invites | 0.218.0. Closed two privilege holes on the way (`AccountSet` had no role ordering, `ClassSet` no capability check). Deleting an account and searching past the loaded page followed in 0.264.0 (ADMIN 63/64): removal is one transaction that retires the account’s names, withdraws its unused invitations and keeps a burrow’s last administrator, and a retired name cannot be taken again (migration 0015), so a byline cannot be inherited |
| 4 | Boards: create, edit, delete; moderators can remove posts | 0.219.0. Reordering followed in 0.265.0 (BOARD 17 `BoardMove`, migration 0016: a position among siblings, read as a tree), and retention in 0.266.0 (BOARD 18/19 `BoardKeeping`, migration 0017): the editor says what each board keeps and how many threads it holds, and sends the number only when the operator moves it |
| 5 | File areas and files: create, edit, delete areas; new folders and drop boxes, describe and remove files | 0.220.0. Moving and renaming followed in slice 8 (0.223.0); quarantine and deny-hashes belong with Moderation, slice 6 |
| 6 | Moderation: reports queue, sessions with Disconnect by name, broadcast, hash-deny list, audit log | 0.221.0. Quarantining from the console followed in 0.263.0 (ADMIN 61/62: what is held back is listed, held and let through where the reports are), and per-room mute, slow mode and taking somebody out in 0.262.0 (CHAT 28/29) |
| 8 | Files: rename and move | 0.223.0. `NodeRename`/`NodeMove` (FILE 29/30); in Files, the selected file or folder gets a Name field and Move, which picks it up: a bar follows the person through the folders and offers "Move here" wherever it may go, and says why not where it may not (already here, inside itself, another area). Closes the file gap left by slice 5 |
| 7 | Federation peers, backups | 0.222.0. Peers (approve with the announced origin or a typed one, revoke; configured dial targets say so and cannot be revoked), trusted origins with pin-by-hand, and Backups (make, check, remove; the folder is the new `backup_dir` setting; restore stays offline and the pane says how). Backups need the Admin role, not just `CONFIG_ADMIN`. **Not done: restore from the console** (a running burrow cannot replace its own database — this one stays out) and getting a snapshot off the machine, designed in `docs/design/snapshot-download.md`: a snapshot holds the Ed25519 seed, the TLS key and every password hash, so the answer is a copy to a path on the burrow’s machine rather than a download into a browser |
