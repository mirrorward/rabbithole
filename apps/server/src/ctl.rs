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
