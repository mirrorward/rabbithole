# Hotline account removal

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
