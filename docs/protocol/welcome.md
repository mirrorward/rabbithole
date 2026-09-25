# RHP Welcome, Theme & Keyword (session family, Wave 2.3)

| type | name | direction | payload |
|---|---|---|---|
| 42/43 | WelcomeScreenRequest → WelcomeScreen | Request/Reply | ordered `widgets`: Motd, UnreadDms, OnlineNow{count,sample}, Featured{title,body}, Ticker |
| 44/45 | ThemeGet → ThemeReply | Request/Reply | signed bundle (`NotFound` if none configured **or the account opted out**) |
| 46/47 | KeywordGo → KeywordTarget | Request/Reply | `word` → {Room \| User \| Url \| Unknown, target} |
| 48 | ThemeChanged | Push | empty; re-fetch ThemeGet for this session |
| 57 | ThemePrefGet | Request | → ThemePrefState (accounts only) |
| 58 | ThemePrefSet | Request | `disable_server_theme: bool` → ThemePrefState (accounts only) |
| 59 | ThemePrefState | Reply | `disable_server_theme: bool` |

## Theme bundle (signed)

`ThemeReply.bundle` is a postcard-encoded `ThemeBundle` (name, optional
accent RGB, optional ANSI logo, optional banner blob, icon overrides —
plus, since Wave 8, structured `--rh-*` design-token maps `tokens_light`,
`tokens_dark`, `tokens_shared`, canonically sorted by name);
`ThemeReply.signature` is Ed25519 over those exact bytes with the server
identity key from `HelloAck.server_key`. **Clients MUST verify before
applying** and cache by blake3 of the bundle bytes.

Server-side (Wave 8), an admin activates a bundle via the admin family's
`ThemeBundleSet` (see `admin.md`): tokens are validated against a closed
grammar and WCAG contrast rails (rejected below 4.5:1, ratio reported)
before anything is served. The per-account `ThemePrefSet` opt-out is the
user safety valve: with it set, `ThemeGet` answers `NotFound` and the
client renders its default tokens.

Peers advertise `server-theme-updates` in `Hello`/`HelloAck` to negotiate
live refreshes. Already-connected authenticated clients that offered this
capability receive the empty `ThemeChanged` push after a successful
operator theme update or clear (including theme configuration changes).
A successful `ThemePrefSet` notifies every live
session of that account only. On receipt, clients re-fetch `ThemeGet` for
the originating session, verify the signed reply, and remove its server
theme on `NotFound`. The push carries no theme data and does not bypass
account preferences. It is ephemeral; clients fetch current theme state
after each fresh authentication or reconnect. Clients that do not offer
the capability receive no new push type.

The core palette helper (`rabbithole-core::theme`) applies only an accent
that meets its contrast rail. The shared web/desktop renderer additionally
layers structured tokens over the user's pack and light/dark choice. Its
`ThemeCache` independently verifies signatures and the closed token grammar,
rejects bundles larger than 128 KiB, and caches by content hash within one
connection only. Reconnect resets the signing-key binding and cached theme.
High Contrast retains its local colors and geometry.

## Keyword resolution order

1. operator keyword map (`room:` / `user:` / `url:` prefixes),
2. a live room by name, 3. an online-or-known persona, else `Unknown`
(echoing the query). This is the AOL keyword-teleport primitive.
