//! The local admin socket: `<data_dir>/ctl.sock`.
//!
//! JSON-lines over a unix domain socket, owner-only permissions — local
//! root/operator administration without a network round trip. The remote
//! admin protocol family (RHP family 7) supersedes none of this; it's the
//! always-works escape hatch. Unix-only for now (Windows named pipes when
//! a Windows server deployment story lands).
//!
//! Request:  `{"cmd": "config-get", "key": "motd"}`
//! Response: `{"ok": true, "data": …}` or `{"ok": false, "error": "…"}`

use std::sync::Arc;

use serde_json::{json, Value};

use rabbithole_server_core::Role;
use rabbithole_store_server::repo::AuditRepo;

use crate::Shared;

/// Handle one parsed ctl request. Exposed for tests and for the `ctl`
/// subcommand's error messages.
pub async fn handle(shared: &Shared, req: &Value) -> Value {
    match dispatch(shared, req).await {
        Ok(data) => json!({"ok": true, "data": data}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

async fn dispatch(shared: &Shared, req: &Value) -> Result<Value, String> {
    let cmd = req
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or("missing cmd")?;
    let str_arg = |key: &str| -> Result<String, String> {
        req.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing {key}"))
    };
    let parse_hash = |s: &str| -> Result<[u8; 32], String> {
        let bytes = hex::decode(s.trim()).map_err(|_| "hash must be hex".to_string())?;
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| "hash must be 32 bytes (64 hex chars)".to_string())
    };

    let audit = |action: &str, detail: String| {
        let pool = shared.pool.clone();
        let action = action.to_string();
        tokio::spawn(async move {
            let _ = AuditRepo(&pool).record("ctl", &action, &detail).await;
        });
    };

    match cmd {
        "status" => {
            let cfg = shared.config.read();
            Ok(json!({
                "name": cfg.name,
                "online": shared.presence.count(),
                "quic_addr": cfg.quic_addr.to_string(),
                "ws_addr": cfg.ws_addr.to_string(),
                "fingerprint": shared.fingerprint_hex,
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }
        "config-get" => {
            let key = str_arg("key")?;
            Ok(Value::String(
                shared.config.get_key(&key).map_err(|e| e.to_string())?,
            ))
        }
        "audit-log" => {
            let limit = req
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(50)
                .clamp(1, 1000);
            let actor = req.get("actor").and_then(Value::as_str);
            let action = req.get("action").and_then(Value::as_str);
            let rows = AuditRepo(&shared.pool)
                .recent(limit)
                .await
                .map_err(|e| e.to_string())?;
            // Optional actor/action filters applied within the recent window.
            let entries: Vec<Value> = rows
                .iter()
                .filter(|r| actor.is_none_or(|a| r.actor == a))
                .filter(|r| action.is_none_or(|a| r.action == a))
                .map(|r| {
                    json!({
                        "id": r.id,
                        "at": r.at,
                        "actor": r.actor,
                        "action": r.action,
                        "detail": r.detail,
                    })
                })
                .collect();
            Ok(json!({"count": entries.len(), "entries": entries}))
        }
        "config-set" => {
            let key = str_arg("key")?;
            let value = str_arg("value")?;
            let live = shared
                .config
                .set_key(&key, &value)
                .map_err(|e| e.to_string())?;
            audit("config-set", format!("{key}={value}"));
            Ok(json!({"applied_live": live}))
        }
        "account-create" => {
            let login = str_arg("login")?;
            let password = str_arg("password")?;
            let role = match req.get("role").and_then(Value::as_str).unwrap_or("user") {
                "guest" => Role::Guest,
                "user" => Role::User,
                "moderator" => Role::Moderator,
                "admin" => Role::Admin,
                "superuser" => Role::Superuser,
                other => return Err(format!("unknown role: {other}")),
            };
            let account = shared
                .auth
                .create_account(&login, &password, role)
                .await
                .map_err(|e| e.to_string())?;
            audit("account-create", format!("{login} role={role:?}"));
            Ok(json!({"id": account.id, "login": account.login}))
        }
        "board-create" => {
            let slug = str_arg("slug")?;
            let title = str_arg("title")?;
            let description = req
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // kind 2 = a board that holds posts (0 category, 1 bundle).
            let board = shared
                .boards
                .create_board(&slug, &title, &description, 2, None, 0)
                .await
                .map_err(|e| e.to_string())?;
            audit("board-create", slug.clone());
            Ok(json!({"slug": board.slug, "title": board.title}))
        }
        "file-area-create" => {
            let slug = str_arg("slug")?;
            let title = str_arg("title")?;
            let description = req
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let area = shared
                .files
                .create_area(&slug, &title, &description)
                .await
                .map_err(|e| e.to_string())?;
            audit("file-area-create", slug.clone());
            Ok(json!({"slug": area.slug, "title": area.title}))
        }
        "board-post" => {
            let board = str_arg("board")?;
            let author = str_arg("author")?;
            let subject = str_arg("subject")?;
            let body = str_arg("body")?;
            let account = rabbithole_store_server::repo::AccountsRepo(&shared.pool)
                .by_login(&author)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no such account: {author}"))?;
            let seed = crate::handlers6::author_seed(shared, account.id);
            // Match the network post path's author string (`screen_name@origin`)
            // so ctl-seeded and live-posted authors are consistent.
            let display = format!("{}@{}", author, shared.origin_name());
            let now = chrono::Utc::now().timestamp_millis();
            let post = shared
                .boards
                .post(
                    &board,
                    None,
                    &display,
                    &seed,
                    &subject,
                    &body,
                    "text/plain",
                    now,
                )
                .await
                .map_err(|e| e.to_string())?;
            audit("board-post", format!("{board}: {subject}"));
            Ok(json!({"board": post.board_slug, "subject": post.subject, "author": post.author}))
        }
        "invite-tree" => {
            let login = str_arg("login")?;
            // Cap the walk; a downline this deep is already an anomaly worth a
            // separate look.
            let max_nodes = req
                .get("max")
                .and_then(Value::as_u64)
                .unwrap_or(1000)
                .min(10_000) as usize;
            let nodes = shared
                .auth
                .invite_subtree(&login, max_nodes)
                .await
                .map_err(|e| e.to_string())?;
            let tree: Vec<Value> = nodes
                .iter()
                .map(|n| json!({"account_id": n.account_id, "login": n.login, "depth": n.depth}))
                .collect();
            // The root itself is node 0, so the invitee count is the remainder.
            Ok(
                json!({"root": login, "count": tree.len(), "invited": tree.len().saturating_sub(1), "tree": tree}),
            )
        }
        // ---- Content hash deny-list (Wave 13): a local-operator ctl surface
        // over the moderation service's blake3 hash-deny list. -----------
        "hash-deny" => {
            let hash = parse_hash(&str_arg("hash")?)?;
            let reason = req.get("reason").and_then(Value::as_str).unwrap_or("");
            shared
                .moderation
                .deny_add(&hash, reason, "ctl")
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({"denied": hex::encode(hash), "reason": reason}))
        }
        "hash-allow" => {
            let hash = parse_hash(&str_arg("hash")?)?;
            let removed = shared
                .moderation
                .deny_remove(&hash, "ctl")
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({"removed": removed}))
        }
        "hash-deny-list" => {
            let rows = shared
                .moderation
                .deny_list()
                .await
                .map_err(|e| e.to_string())?;
            let denied: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "hash": hex::encode(r.hash),
                        "reason": r.reason,
                        "added_by": r.added_by,
                        "created_at": r.created_at,
                    })
                })
                .collect();
            Ok(json!({"count": denied.len(), "denied": denied}))
        }
        "who" => {
            let users: Vec<Value> = shared
                .presence
                .snapshot()
                .into_iter()
                .map(|e| {
                    json!({
                        "session_id": e.session_id,
                        "screen_name": e.screen_name,
                        "role": format!("{:?}", e.role),
                        "transport": e.transport,
                        "connected_secs": e.connected_at.elapsed().as_secs(),
                    })
                })
                .collect();
            Ok(Value::Array(users))
        }
        // ---- Server theme bundle (Wave 8) -------------------------------
        "theme-status" => {
            let cfg = shared.config.read();
            match rabbithole_server_core::theme::applied_from_config(&cfg) {
                Some(applied) => Ok(json!({
                    "present": true,
                    "id": hex::encode(applied.id),
                    "name": applied.bundle.name,
                    "applied_at_unix": cfg.theme_applied_at_unix,
                    "applied_by": cfg.theme_applied_by,
                    "accent": cfg.theme_accent,
                    "has_logo": !cfg.theme_logo_ansi.is_empty(),
                    "banner": cfg.theme_banner,
                    "icons": applied.bundle.icons.len(),
                    "tokens": {
                        "light": applied.bundle.tokens_light.len(),
                        "dark": applied.bundle.tokens_dark.len(),
                        "shared": applied.bundle.tokens_shared.len(),
                    },
                })),
                None => Ok(json!({"present": false})),
            }
        }
        "theme-clear" => {
            shared
                .config
                .update(rabbithole_server_core::theme::clear_config);
            audit("theme-clear", String::new());
            Ok(json!({"cleared": true}))
        }
        // ---- Gateway/feed activity stats (Wave 10) ---------------------
        "gateway-stats" => {
            let snap = crate::handlers13::snapshot(shared);
            let feeds: Vec<Value> = snap
                .feeds
                .iter()
                .map(|f| {
                    json!({
                        "url": f.url,
                        "last_poll_ms": f.last_poll_ms,
                        "last_status": f.last_status,
                        "items_seen": f.items_seen,
                        "items_posted": f.items_posted,
                        "dupes_dropped": f.dupes_dropped,
                    })
                })
                .collect();
            let gateways: Vec<Value> = snap
                .gateways
                .iter()
                .map(|g| {
                    let counters: serde_json::Map<String, Value> = g
                        .counters
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect();
                    json!({"name": g.name, "enabled": g.enabled, "counters": counters})
                })
                .collect();
            Ok(json!({
                "generated_at_ms": snap.generated_at_ms,
                "feeds": feeds,
                "gateways": gateways,
            }))
        }
        // ---- QWK offline mail (Wave 10) --------------------------------
        // Admin/testing surfaces over crate::qwk: both are gated on
        // `qwk_enabled` inside the service (same gate as the telnet `qwk`
        // command). The upload path here takes an already-unzipped
        // `<BBSID>.MSG` member; the interactive (zmodem) upload path is a
        // documented follow-up.
        "qwk-build" => {
            let login = str_arg("login")?;
            let account = rabbithole_store_server::repo::AccountsRepo(&shared.pool)
                .by_login(&login)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("no such account")?;
            let build = crate::qwk::build_for(shared, &account)
                .await
                .map_err(|e| e.to_string())?;
            audit(
                "qwk-build",
                format!("{login} messages={}", build.total_messages),
            );
            let members: Vec<Value> = build
                .members
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "size": m.size,
                        "path": m.path.display().to_string(),
                    })
                })
                .collect();
            let conferences: Vec<Value> = build
                .conferences
                .iter()
                .map(|(n, slug)| json!({"conference": n, "board": slug}))
                .collect();
            Ok(json!({
                "spool_dir": build.spool_dir.display().to_string(),
                "packet": build.packet_path.display().to_string(),
                "total_messages": build.total_messages,
                "conferences": conferences,
                "members": members,
            }))
        }
        "qwk-ingest" => {
            let login = str_arg("login")?;
            let path = str_arg("path")?;
            let account = rabbithole_store_server::repo::AccountsRepo(&shared.pool)
                .by_login(&login)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("no such account")?;
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| format!("read {path}: {e}"))?;
            let report = crate::qwk::ingest_rep_for(shared, &account, &bytes)
                .await
                .map_err(|e| e.to_string())?;
            audit(
                "qwk-ingest",
                format!(
                    "{login} accepted={} duplicates={} rejected={}",
                    report.accepted,
                    report.duplicates,
                    report.rejected.len()
                ),
            );
            let rejected: Vec<Value> = report
                .rejected
                .iter()
                .map(|(subject, reason)| json!({"subject": subject, "reason": reason}))
                .collect();
            Ok(json!({
                "accepted": report.accepted,
                "duplicates": report.duplicates,
                "rejected": rejected,
            }))
        }
        // ---- Backups (Wave 13) ------------------------------------------
        "backup" => {
            let dest = str_arg("dest")?;
            let outcome = crate::backup::snapshot(shared, std::path::Path::new(&dest))
                .await
                .map_err(|e| format!("{e:#}"))?;
            audit(
                "backup",
                format!(
                    "{} files={} bytes={}",
                    outcome.dir.display(),
                    outcome.files,
                    outcome.total_bytes
                ),
            );
            Ok(json!({
                "snapshot_dir": outcome.dir.display().to_string(),
                "files": outcome.files,
                "total_bytes": outcome.total_bytes,
            }))
        }
        "backup-verify" => {
            let path = str_arg("path")?;
            let dir = std::path::PathBuf::from(&path);
            let manifest = tokio::task::spawn_blocking({
                let dir = dir.clone();
                move || crate::backup::verify_snapshot(&dir)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
            let integrity = rabbithole_store_server::integrity_check(&dir.join("burrow.db"))
                .await
                .map_err(|e| e.to_string())?;
            if integrity != "ok" {
                return Err(format!("database integrity_check failed: {integrity}"));
            }
            Ok(json!({
                "files": manifest.files.len(),
                "total_bytes": manifest.total_bytes(),
                "integrity_check": integrity,
                "created_at": manifest.created_at,
                "workspace_version": manifest.workspace_version,
            }))
        }
        "restore" => {
            // A running server can't safely swap out its own database, so
            // this always refuses and points at the offline path.
            let path = str_arg("path")?;
            Err(format!(
                "restore must run offline: a running burrow cannot replace its own \
                 database. Stop the server, then run `burrow restore {path} --data-dir \
                 <dir>`, then start it again."
            ))
        }
        // ---- S2S federation peers (Wave 9) -----------------------------
        "peer-list" => {
            let peers: Vec<Value> = shared
                .peers
                .snapshot()
                .into_iter()
                .map(|p| {
                    json!({
                        "key": p.key_hex(),
                        "name": p.name,
                        "addr": p.addr,
                        "state": p.state.as_str(),
                        "approved": p.approved,
                    })
                })
                .collect();
            Ok(Value::Array(peers))
        }
        "peer-approve" => {
            let key_hex = str_arg("key")?;
            let key = crate::federation::hex_key(&key_hex).ok_or("key must be 32-byte hex")?;
            let existed = shared.peers.approve(&key);
            crate::federation::persist_approved(shared).map_err(|e| e.to_string())?;
            audit("peer-approve", key_hex.clone());
            Ok(json!({"key": key_hex, "was_known": existed}))
        }
        "peer-revoke" => {
            let key_hex = str_arg("key")?;
            let key = crate::federation::hex_key(&key_hex).ok_or("key must be 32-byte hex")?;
            let existed = shared.peers.revoke(&key);
            crate::federation::persist_approved(shared).map_err(|e| e.to_string())?;
            audit("peer-revoke", key_hex.clone());
            Ok(json!({"key": key_hex, "was_known": existed}))
        }
        // ---- Federated catalogs + cross-server search (Wave 9.x) -------
        "fed-catalogs" => {
            // The local catalog (rebuilt on demand) plus every stored,
            // verified peer catalog.
            let local = crate::fed_catalog::local_catalog(shared)
                .await
                .map_err(|e| e.to_string())?;
            let mut rows = vec![json!({
                "server": crate::fed_catalog::server_display_name(shared, &shared.server_key),
                "key": hex::encode(shared.server_key),
                "local": true,
                "generation": local.catalog.generation,
                "entries": local.catalog.entries.len(),
            })];
            for cat in shared.catalogs.peer_catalogs() {
                let key = cat.catalog.server_key;
                rows.push(json!({
                    "server": crate::fed_catalog::server_display_name(shared, &key),
                    "key": hex::encode(key),
                    "local": false,
                    "generation": cat.catalog.generation,
                    "entries": cat.catalog.entries.len(),
                }));
            }
            Ok(Value::Array(rows))
        }
        "fed-search" => {
            // Whitespace-separated, case-insensitive substring terms (AND).
            let terms = str_arg("terms")?;
            let query = rabbithole_federation::SearchQuery::new(terms.split_whitespace());
            let deduped = crate::fed_catalog::federated_search(shared, &query)
                .await
                .map_err(|e| e.to_string())?;
            let rows: Vec<Value> = deduped
                .iter()
                .map(|d| {
                    let sources: Vec<Value> = d
                        .sources
                        .iter()
                        .map(|s| {
                            json!({
                                "server": crate::fed_catalog::server_display_name(
                                    shared,
                                    &s.server_key,
                                ),
                                "server_key": hex::encode(s.server_key),
                                "local": s.server_key == shared.server_key,
                                "generation": s.generation,
                                "area": s.area,
                                "path": s.path,
                                "name": s.name,
                            })
                        })
                        .collect();
                    json!({
                        "hash": hex::encode(d.hash),
                        "size": d.size,
                        "sources": sources,
                    })
                })
                .collect();
            Ok(Value::Array(rows))
        }
        other => Err(format!("unknown cmd: {other}")),
    }
}

/// Serve the ctl socket until the task is aborted.
#[cfg(unix)]
pub async fn serve(shared: Arc<Shared>) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let path = shared.config.read().data_dir.join("ctl.sock");
    let _ = std::fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    tracing::info!(path = %path.display(), "ctl socket up");

    loop {
        let (stream, _) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let response = match serde_json::from_str::<Value>(&line) {
                    Ok(req) => handle(&shared, &req).await,
                    Err(e) => json!({"ok": false, "error": format!("bad json: {e}")}),
                };
                let mut out = response.to_string();
                out.push('\n');
                if write.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

#[cfg(not(unix))]
pub async fn serve(_shared: Arc<Shared>) -> anyhow::Result<()> {
    tracing::warn!("ctl socket is unix-only; skipping");
    std::future::pending::<()>().await;
    unreachable!()
}
