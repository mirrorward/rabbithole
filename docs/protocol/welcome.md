# RHP Welcome, Theme & Keyword (session family, Wave 2.3)

| type | name | direction | payload |
|---|---|---|---|
| 42/43 | WelcomeScreenRequest → WelcomeScreen | Request/Reply | ordered `widgets`: Motd, UnreadDms, OnlineNow{count,sample}, Featured{title,body}, Ticker |
| 44/45 | ThemeGet → ThemeReply | Request/Reply | signed bundle (`NotFound` if none configured **or the account opted out**) |
| 46/47 | KeywordGo → KeywordTarget | Request/Reply | `word` → {Room \| User \| Url \| Unknown, target} |
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

Safety rails (client side, in `rabbithole-core::theme`): a server bundle
only overrides the **accent** and supplies art — never the structural
palette — and the accent is dropped if it falls below 3:1 contrast against
the active background. Server theming layers on top of the user's
light/dark + theme-pack (Clean / Retro / High Contrast) choice.

## Keyword resolution order

1. operator keyword map (`room:` / `user:` / `url:` prefixes),
2. a live room by name, 3. an online-or-known persona, else `Unknown`
(echoing the query). This is the AOL keyword-teleport primitive.
