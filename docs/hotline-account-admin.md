# Hotline account administration

## Access masks

`NewUser` (350) and `SetUser` (353) persist supported Hotline access choices as
shared account grants and revokes. A mask is exactly eight bytes in Hotline's
big-endian bit order. A malformed mask fails the whole request, including any
password change. A missing mask leaves existing access unchanged; on creation,
it selects the ordinary member defaults.

The following classic bits have independent native capabilities:

| Hotline privilege | Native capability |
| --- | --- |
| Get client information | `WHO` |
| Read / send chat | `CHAT_READ` / `CHAT_SEND` |
| Open / close private chats | `CHAT_CREATE_ROOM` / `CHAT_MODERATE` |
| Send private messages | `DM_SEND` |
| Read / post news | `BOARD_READ` / `BOARD_POST` |
| Download / upload files | `FILE_DOWNLOAD` / `FILE_UPLOAD` |
| View drop boxes | `DROPBOX_VIEW` |
| Disconnect users | `USER_KICK` |
| Cannot be disconnected | `CANNOT_BE_KICKED` |
| Broadcast | `BROADCAST` |

Three groups each share one native capability. Every bit in a group must be
enabled together or disabled together. A partial group is refused rather than
silently granting its other actions:

- `ACCOUNT_ADMIN`: create, delete, open, and modify accounts.
- `BOARD_MODERATE`: delete news articles; create/delete news categories and
  folders.
- `FILE_MANAGE`: delete/rename/move files; create/delete/rename/move folders;
  upload anywhere; set file/folder comments; make aliases.

`ShowInList`, `ChangeOwnPassword`, and `AnyName` are policy indicators in this
projection, not editable capabilities. Their incoming values do not change
native identity policy: readback always includes ShowInList, and includes the
other two for the User role and above. `NoAgreement` and unknown/reserved enabled
bits are refused. Upload destination access continues to follow the native
resource permissions rather than a folder literally named “Uploads.”

The nearest role still supplies coarse standing: account administration selects
Admin, moderation selects Moderator, participation selects User, otherwise
Guest. An unchanged GetUser/SetUser roundtrip preserves the existing role, class,
and explicit overrides, including accounts with custom native classes. If an
edit changes the inferred role, its class switches to the corresponding native
class; grants/revokes keep the requested mapped capabilities exact and preserve
the target's previously effective native capabilities outside the projection.
For example, adding account-administration bits does not also grant
`CONFIG_ADMIN`, `USER_BAN`, or the native moderation suite. Creating with an
explicit mask adds only the chosen mapped capabilities plus native visibility
and file browsing (`SEE` and `FILE_LIST`); it does not implicitly grant door or
swarm rights. Existing explicit overrides outside the projection are retained.
After an explicit mask edit, mapped grants and revokes retain those choices
across later shared-class changes. Resource ACLs continue to layer on the
resulting base capabilities under the shared native evaluator.

The current caller must hold `ACCOUNT_ADMIN`. Account and class authority is
reloaded for each mutation, so a connected operator cannot retain a revoked
grant. Actual edits use native `Standing`: no self edits, only lower-role
targets unless the caller is a superuser, and no assignment above the caller's
role. Newly added effective capabilities must also be held by the caller.
Superuser cannot be assigned through a bitmap; changing a superuser's access
bitmap is refused because that role bypasses native revokes. Saving an unchanged
projected bitmap is a safe no-op, including for peers and superusers.

Creation commits credentials, overrides, and the default persona together.
SetUser validates every field before writing password and access together; a
concurrently changed target is refused without applying either edit. Changed
accounts have saved sign-ins revoked in that same transaction, then their live
native, Hotline, and telnet sessions are closed so stale permission snapshots
cannot retain removed access. Hotline receives an updated access bitmap and a
disconnect explanation. Unchanged saves and rejected edits keep sessions valid.
Account IDs, personas, and signed content are unchanged. Audit records identify
the current actor and whether access, password/access, or no fields changed;
passwords and hashes are never included.

`apps/server/tests/e2e_w75_hotline_admin/access.rs` covers non-role masks,
shared permission enforcement, saved sign-ins and all three live surfaces,
atomic rejection, authority changes, and protected targets. Store tests exercise
creation rollback and concurrent-edit rejection, and mapping tests cover every
subset of the supported native groups. These are automated fixtures, not a
claim of a manual vintage-client compatibility run.

## Permanent removal

Hotline `DeleteUser` (transaction 351) permanently removes the shared RabbitHole
account named by the obfuscated `USER_LOGIN` field. It uses the same store
transaction as native account removal. It also accepts disabled accounts,
including accounts disabled by older Hotline versions' soft-delete behavior.
The operation cannot be undone by enabling the account again.

The caller must currently be enabled and hold `ACCOUNT_ADMIN` on `admin`.
The server reloads the caller's account and class before checking this
permission and their standing; an old Hotline login does not retain a removed
role or class grant. The shared native `Standing` rules apply:

- Nobody can delete their own account, including a superuser.
- Other callers can delete only accounts below their current role.
- A superuser can delete another account, including another superuser.
- The store transaction refuses removal of the last enabled account with the
  Admin or Superuser role. It does not count lower-role accounts with custom
  capability grants as keepers.

Successful removal deletes the account's password hash, personas, saved session
tokens, two-factor enrollment, identity keys, and other account-owned rows.
Unused invitations issued by the account are withdrawn. The shared
`sign_out_everywhere` path closes its active native, Hotline, and telnet sessions;
Hotline receives a `DisconnectMsg` explaining `account removed`. Permission,
standing, missing-account and store failures do not kick the target. A repeated
delete reports an absent account.

Signed posts and their original attribution stay intact. The deleted account's
login and stored persona names remain retired, case-insensitively, so creating a
new account or persona cannot take over those historical bylines. Removal is
account administration, not content erasure; it does not withdraw signed posts
or erase files and audit history. Display names supplied only for a transient
Hotline session are not stored personas.

Successful operations use the shared `account-delete` audit action, with the
actor's login and a detail such as `alice via=hotline permanent`. Audit writes use
the existing asynchronous admin logging path.

The scripted socket regression tests in
`apps/server/tests/e2e_w75_hotline_admin/delete.rs` verify permanent store cleanup,
unchanged signed posts, retired names, native/Hotline/telnet disconnects, resume
refusal, disabled targets, self and role protection, and changed caller roles or
class grants. These are local protocol tests; vintage-client interoperability
remains tracked separately.
