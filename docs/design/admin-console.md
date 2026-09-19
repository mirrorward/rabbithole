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
| 3 | People: create, role, class, disable, password and 2FA reset, delete; classes with named capabilities; invites | |
| 4 | Boards: create, edit, delete, reorder; moderators can remove posts | |
| 5 | File areas and files: create, edit, delete areas; delete, describe, move files; quarantine and deny-hashes | |
| 6 | Moderation: reports queue, sessions with Kick (no typed ids), broadcast, audit log | |
| 7 | Federation peers, backups | |
