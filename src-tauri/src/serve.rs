//! `submarine serve` — headless gateway mode (Epic A, upstream #270).
//!
//! Serves the built frontend over HTTP and mirrors the Tauri `invoke`
//! surface over a WebSocket endpoint, so a plain browser can drive the real
//! backend (SSH/PTY, SFTP, vault) with no WebView. Auth token from day one:
//! every `/api/*` request (WS + file endpoints) must carry `?token=` or a
//! Bearer token. The static bundle itself is public (it carries no secrets —
//! browsers fetch JS/CSS without query strings, so gating it would 401 the
//! app on every load); all data stays behind the token.
//! Binds loopback by default; anything else needs `--allow-remote`, and
//! remote use wants TLS (`--tls-cert/--tls-key` or a TLS-terminating
//! reverse proxy). Idle sessions auto-lock (`--lock-after`, default 30m).

use std::{collections::{HashMap, HashSet}, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Multipart, Query, State,
    },
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("[SERVE] MISSING_ARG: {}", key))
}

fn req_u32(args: &Value, key: &str) -> Result<u32, String> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("[SERVE] BAD_ARG {}: expected u32", key))
}

fn req_opt_u32(args: &Value, key: &str) -> Option<u32> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
}

fn req_bool(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("[SERVE] MISSING_ARG: {}", key))
}

fn req_i32(args: &Value, key: &str) -> Result<i32, String> {
    args.get(key)
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| format!("[SERVE] BAD_ARG {}: expected i32", key))
}

fn req_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn req_opt_i32(args: &Value, key: &str) -> Option<i32> {
    args.get(key)
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
}

fn req_opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn req_str_vec(args: &Value, key: &str) -> Result<Vec<String>, String> {
    match args.get(key) {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("[SERVE] BAD_ARG {}: {}", key, e)),
        None => Err(format!("[SERVE] MISSING_ARG: {}", key)),
    }
}

fn req_i32_vec(args: &Value, key: &str) -> Result<Vec<i32>, String> {
    match args.get(key) {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("[SERVE] BAD_ARG {}: {}", key, e)),
        None => Err(format!("[SERVE] MISSING_ARG: {}", key)),
    }
}

fn req_value_vec(args: &Value, key: &str) -> Result<Vec<Value>, String> {
    match args.get(key) {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("[SERVE] BAD_ARG {}: {}", key, e)),
        None => Err(format!("[SERVE] MISSING_ARG: {}", key)),
    }
}

fn req_opt_value_vec(args: &Value, key: &str) -> Result<Option<Vec<Value>>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("[SERVE] BAD_ARG {}: {}", key, e)),
    }
}

/// Frontend sends camelCase arg keys (`sessionId`, `localPath`,
/// `privateKey`); Rust commands take snake_case. The desktop Tauri bridge
/// accepts both, so the gateway must too — normalize top-level keys here so
/// no dispatch arm has to care. Values pass through untouched, including
/// nested objects (`quick_auth.private_key`, `spec.type` already match the
/// Rust shapes). An explicit snake_case key always wins over its camelCase
/// twin when both are present.
fn normalize_args(args: &Value) -> Value {
    let Value::Object(map) = args else {
        return args.clone();
    };
    let mut out = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        let mut snake = String::with_capacity(k.len() + 4);
        for (i, c) in k.chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    snake.push('_');
                }
                snake.push(c.to_ascii_lowercase());
            } else {
                snake.push(c);
            }
        }
        if snake != *k && !map.contains_key(&snake) {
            out.insert(snake, v.clone());
        }
        out.insert(k.clone(), v.clone());
    }
    Value::Object(out)
}

fn req_bytes(args: &Value, key: &str) -> Result<Vec<u8>, String> {
    args.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("[SERVE] MISSING_ARG: {}", key))
        .and_then(|arr| {
            arr.iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or_else(|| format!("[SERVE] BAD_ARG {}: expected byte array", key))
                })
                .collect::<Result<Vec<u8>, String>>()
        })
}

use crate::{
    add_command_inner, add_credential_inner, add_folder_inner, add_mirror_to_server_inner,
    add_note_inner, add_server_inner, add_ssh_key_inner, check_db_exists_inner,
    clone_server_inner, close_profile_inner, close_terminal_inner, cmd_history_add_inner,
    cmd_history_clear_inner, cmd_history_list_inner, create_profile_inner,
    delete_credential_inner, delete_command_inner, delete_folder_inner, delete_note_inner, delete_profile_inner,
    delete_server_inner, delete_ssh_key_inner, disconnect_session_inner, docker,
    edit_command_inner, edit_credential_inner, edit_note_inner,
    edit_server_inner, edit_ssh_key_inner, generate_ssh_key_inner, get_commands_inner,
    get_credentials_inner, get_folders_inner, get_notes_inner, get_servers_inner,
    get_ssh_keys_inner, initiate_connection_inner, list_mirrors_inner, list_profiles_inner,
    list_tunnels_inner,
    mirror::{new_mirror_map, MirrorMap},
    mirror_dry_run_inner, monitor_add_inner, monitor_get_settings_inner, monitor_list_inner,
    monitor_pause_all_inner, monitor_pause_inner, monitor_remove_inner, monitor_resume_all_inner,
    monitor_resume_inner, monitor_set_custom_metrics_inner, monitor_set_metrics_inner,
    monitor_set_settings_inner,
    monitor::{new_monitor_map, MonitorMap, SharedSettings},
    new_db_state, open_terminal_inner, parse_client_import_inner, parse_ssh_config_inner,
    persist_session_tunnels_inner, persist_vault_inner, rename_folder_inner,
    reorder_servers_inner, resize_terminal_inner, restart_session_tunnels_inner,
    reveal_credential_password_inner, reveal_server_password_inner, reveal_ssh_key_inner,
    save_quick_connect_node_inner, select_profile_inner, set_folder_color_inner,
    set_server_color_inner, set_server_jump_host_inner, set_server_notes_inner,
    set_server_run_on_connect_inner, setup_master_db_inner, sftp_cancel_transfer_inner,
    sftp_create_dir_inner, sftp_download_file_inner, sftp_list_dir_inner,
    sftp_remove_dir_inner, sftp_remove_file_inner, sftp_rename_inner, sftp_set_owner_inner,
    sftp_set_permissions_inner, sftp_upload_file_inner, ssh_info_probe_section_inner,
    ssh_iptables_chain_inner, ssh_kill_process_inner, ssh_manager::SshState,
    ssh_nft_chain_inner, ssh_systemctl_action_inner, start_mirror_inner, start_tunnel_inner,
    stop_mirror_inner, stop_tunnel_inner, submit_kbi_response_inner,
    verify_fingerprint_response_inner, write_terminal_data_inner, DbState, Emit, QuickAuth,
};

const DEFAULT_PORT: u16 = 17531;
const DEFAULT_BIND: &str = "127.0.0.1";

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Process-owned backend state. Mirrors what `run()` hands to Tauri's
/// `.manage(...)`, minus the AppHandle-bound pieces (cloud state, window
/// plugins) which have no meaning headless. More states join as their first
/// command is ported — `initiate_connection` (with its tunnel worker tree)
/// is 2c.
#[derive(Clone)]
pub struct ServeCtx {
    pub data_dir: PathBuf,
    pub db: Arc<DbState>,
    pub ssh: Arc<SshState>,
    pub mirrors: Arc<MirrorMap>,
    pub monitors: Arc<MonitorMap>,
    pub settings: SharedSettings,
    pub docker_streams: Arc<docker::DockerStreams>,
}

/// Broadcast hub for backend → UI events. One sender is shared by every
/// connection; each connection holds its own receiver plus a subscription
/// set, so a chatty session only ships bytes to listeners that asked.
pub type EventBus = broadcast::Sender<crate::emit::BusEvent>;

#[derive(Clone)]
struct ServeState {
    ctx: Arc<ServeCtx>,
    token: String,
    bus: EventBus,
    /// Last authenticated activity (any WS message or HTTP request). The
    /// idle-lock timer reads it; every authed entry point bumps it.
    /// `std` mutex: never held across `.await`.
    activity: Arc<std::sync::Mutex<std::time::Instant>>,
}

fn touch_activity(state: &ServeState) {
    if let Ok(mut t) = state.activity.lock() {
        *t = std::time::Instant::now();
    }
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Client → server. `args` mirrors the object form of Tauri `invoke` args.
#[derive(Deserialize)]
struct WsReq {
    id: u64,
    cmd: String,
    #[serde(default)]
    args: Value,
}

/// Server → client. Exactly one of `result` / `error` is present.
#[derive(Serialize)]
struct WsResp {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Subscription management. `{ "type": "sub", "events": [...] }` replaces
/// the connection's subscription set... no — it ADDS; `"unsub"` removes.
/// The server answers every sub/unsub with `{ "type": "sub-ack", "events":
/// [...] }` carrying the resulting set; the frontend resolves `listen()`
/// on that ack so no event can slip through the registration gap.
#[derive(Deserialize)]
struct SubMsg {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    events: Vec<String>,
}

enum Incoming {
    Req(WsReq),
    Sub(SubMsg),
}

fn parse_incoming(text: &str) -> Result<Incoming, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| format!("[SERVE] BAD_REQUEST: {}", e))?;
    if v.get("cmd").is_some() {
        serde_json::from_value(v)
            .map(Incoming::Req)
            .map_err(|e| format!("[SERVE] BAD_REQUEST: {}", e))
    } else {
        serde_json::from_value(v)
            .map(Incoming::Sub)
            .map_err(|e| format!("[SERVE] BAD_REQUEST: {}", e))
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Command set. Each arm funnels into the same `*_inner` the desktop Tauri
/// command calls — one behaviour, two transports. Thin `#[tauri::command]`
/// shells stay on the desktop side; `serve` calls the inners directly.
/// `disconnect_session` is the first emitting command: its events prove the
/// fan-out path end to end. Session-establishing commands are 2c.
async fn dispatch(ctx: &ServeCtx, bus: &EventBus, cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "app_info" => Ok(json!(crate::about::app_info())),
        "list_profiles" => list_profiles_inner(&ctx.data_dir).await.map(|v| json!(v)),
        "check_db_exists" => check_db_exists_inner(&ctx.data_dir, &ctx.db).await.map(|v| json!(v)),
        "select_profile" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("[SERVE] MISSING_ARG: name")?;
            select_profile_inner(&ctx.data_dir, &ctx.db, name.to_string()).await.map(|v| json!(v))
        }
        "setup_master_db" => {
            let password = args
                .get("password")
                .and_then(Value::as_str)
                .ok_or("[SERVE] MISSING_ARG: password")?;
            setup_master_db_inner(&ctx.data_dir, password.to_string(), &ctx.db, true)
                .await
                .map(|_| Value::Null)
        }
        "create_profile" => {
            create_profile_inner(&ctx.data_dir, &ctx.db, req_str(args, "name")?, req_str(args, "password")?)
                .await
                .map(|_| Value::Null)
        }
        "delete_profile" => {
            delete_profile_inner(&ctx.data_dir, req_str(args, "name")?)
                .await
                .map(|_| Value::Null)
        }
        "get_servers" => get_servers_inner(&ctx.db).await.map(|v| json!(v)),
        "get_notes" => get_notes_inner(&ctx.db).await.map(|v| json!(v)),
        "get_ssh_keys" => get_ssh_keys_inner(&ctx.db).await.map(|v| json!(v)),
        "get_commands" => get_commands_inner(&ctx.db).await.map(|v| json!(v)),
        "get_folders" => get_folders_inner(&ctx.db).await.map(|v| json!(v)),
        "get_credentials" => get_credentials_inner(&ctx.db).await.map(|v| json!(v)),
        "add_server" => {
            add_server_inner(
                &ctx.db,
                req_str(args, "name")?,
                req_str(args, "host")?,
                req_i32(args, "port")?,
                req_opt_str(args, "username"),
                req_opt_str(args, "password"),
                req_opt_i32(args, "credential_id"),
                req_opt_i32(args, "folder_id"),
                req_str(args, "proxy_type")?,
                req_str(args, "proxy_host")?,
                req_i32(args, "proxy_port")?,
                req_value_vec(args, "tunnels")?,
                req_str(args, "auth_type")?,
                req_opt_i32(args, "key_id"),
                req_opt_bool(args, "autostart"),
                req_opt_value_vec(args, "mirrors")?,
                req_opt_str(args, "color"),
            )
            .await
            .map(|v| json!(v))
        }
        "save_quick_connect_node" => {
            let auth: QuickAuth = match args.get("auth") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| format!("[SERVE] BAD_ARG auth: {}", e))?,
                None => return Err("[SERVE] MISSING_ARG: auth".to_string()),
            };
            save_quick_connect_node_inner(&ctx.db, auth)
                .await
                .map(|v| json!(v))
        }
        "edit_server" => {
            edit_server_inner(
                &ctx.db,
                &ctx.ssh,
                req_i32(args, "id")?,
                req_str(args, "name")?,
                req_str(args, "host")?,
                req_i32(args, "port")?,
                req_opt_str(args, "username"),
                req_opt_str(args, "password"),
                req_opt_i32(args, "credential_id"),
                req_opt_i32(args, "folder_id"),
                req_str(args, "proxy_type")?,
                req_str(args, "proxy_host")?,
                req_i32(args, "proxy_port")?,
                req_value_vec(args, "tunnels")?,
                req_str(args, "auth_type")?,
                req_opt_i32(args, "key_id"),
                req_opt_bool(args, "autostart"),
                req_opt_value_vec(args, "mirrors")?,
                req_opt_str(args, "color"),
                req_opt_bool(args, "preserve_password"),
            )
            .await
            .map(|_| Value::Null)
        }
        "add_mirror_to_server" => {
            let mirror: Value = match args.get("mirror") {
                Some(v) => v.clone(),
                None => return Err("[SERVE] MISSING_ARG: mirror".to_string()),
            };
            add_mirror_to_server_inner(&ctx.db, req_i32(args, "server_id")?, mirror)
                .await
                .map(|_| Value::Null)
        }
        "delete_server" => {
            delete_server_inner(&ctx.db, &ctx.monitors, req_i32(args, "id")?)
                .await
                .map(|_| Value::Null)
        }
        "clone_server" => {
            clone_server_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|v| json!(v))
        }
        "reorder_servers" => {
            reorder_servers_inner(&ctx.db, req_i32_vec(args, "ids")?)
                .await
                .map(|_| Value::Null)
        }
        "set_server_color" => {
            set_server_color_inner(&ctx.db, req_i32(args, "id")?, req_opt_str(args, "color"))
                .await
                .map(|_| Value::Null)
        }
        "set_server_notes" => {
            set_server_notes_inner(&ctx.db, req_i32(args, "id")?, req_str(args, "notes")?)
                .await
                .map(|_| Value::Null)
        }
        "set_server_run_on_connect" => {
            set_server_run_on_connect_inner(&ctx.db, req_i32(args, "id")?, req_str(args, "value")?)
                .await
                .map(|_| Value::Null)
        }
        "set_server_jump_host" => {
            set_server_jump_host_inner(&ctx.db, req_i32(args, "id")?, req_opt_i32(args, "value"))
                .await
                .map(|_| Value::Null)
        }
        "set_folder_color" => {
            set_folder_color_inner(&ctx.db, req_i32(args, "id")?, req_opt_str(args, "color"))
                .await
                .map(|_| Value::Null)
        }
        "reveal_server_password" => {
            reveal_server_password_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|v| json!(v))
        }
        "reveal_credential_password" => {
            reveal_credential_password_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|v| json!(v))
        }
        "reveal_ssh_key" => {
            reveal_ssh_key_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|v| json!(v))
        }
        "add_credential" => {
            add_credential_inner(
                &ctx.db,
                req_str(args, "name")?,
                req_str(args, "auth_type")?,
                req_str(args, "username")?,
                req_opt_str(args, "password"),
                req_opt_i32(args, "key_id"),
            )
            .await
            .map(|_| Value::Null)
        }
        "edit_credential" => {
            edit_credential_inner(
                &ctx.db,
                req_i32(args, "id")?,
                req_str(args, "name")?,
                req_str(args, "auth_type")?,
                req_str(args, "username")?,
                req_opt_str(args, "password"),
                req_opt_i32(args, "key_id"),
            )
            .await
            .map(|_| Value::Null)
        }
        "delete_credential" => {
            delete_credential_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|_| Value::Null)
        }
        "add_folder" => {
            add_folder_inner(&ctx.db, req_str(args, "name")?, req_opt_i32(args, "parent_id"))
                .await
                .map(|_| Value::Null)
        }
        "rename_folder" => {
            rename_folder_inner(&ctx.db, req_i32(args, "id")?, req_str(args, "name")?)
                .await
                .map(|_| Value::Null)
        }
        "delete_folder" => {
            delete_folder_inner(&ctx.db, &ctx.monitors, req_i32(args, "id")?)
                .await
                .map(|_| Value::Null)
        }
        "add_ssh_key" => {
            add_ssh_key_inner(
                &ctx.db,
                req_str(args, "name")?,
                req_str(args, "public_key")?,
                req_str(args, "private_key")?,
                req_opt_str(args, "passphrase"),
            )
            .await
            .map(|_| Value::Null)
        }
        "edit_ssh_key" => {
            edit_ssh_key_inner(
                &ctx.db,
                req_i32(args, "id")?,
                req_str(args, "name")?,
                req_str(args, "public_key")?,
                req_str(args, "private_key")?,
                req_opt_str(args, "passphrase"),
            )
            .await
            .map(|_| Value::Null)
        }
        "delete_ssh_key" => {
            delete_ssh_key_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|_| Value::Null)
        }
        "generate_ssh_key" => {
            generate_ssh_key_inner(&ctx.db, req_str(args, "name")?)
                .await
                .map(|_| Value::Null)
        }
        "add_command" => {
            add_command_inner(&ctx.db, req_str(args, "title")?, req_str(args, "content")?)
                .await
                .map(|_| Value::Null)
        }
        "edit_command" => {
            edit_command_inner(&ctx.db, req_i32(args, "id")?, req_str(args, "title")?, req_str(args, "content")?)
                .await
                .map(|_| Value::Null)
        }
        "delete_command" => {
            delete_command_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|_| Value::Null)
        }
        "cmd_history_add" => {
            cmd_history_add_inner(&ctx.db, req_opt_i32(args, "server_id"), req_str(args, "server_name")?, req_str(args, "command")?)
                .await
                .map(|v| json!(v))
        }
        "cmd_history_list" => {
            cmd_history_list_inner(&ctx.db, req_opt_str(args, "query"), req_opt_u32(args, "limit"))
                .await
                .map(|v| json!(v))
        }
        "cmd_history_clear" => {
            cmd_history_clear_inner(&ctx.db).await.map(|_| Value::Null)
        }
        "add_note" => {
            add_note_inner(&ctx.db, req_str(args, "title")?, req_str(args, "body")?)
                .await
                .map(|_| Value::Null)
        }
        "edit_note" => {
            edit_note_inner(&ctx.db, req_i32(args, "id")?, req_str(args, "title")?, req_str(args, "body")?)
                .await
                .map(|_| Value::Null)
        }
        "delete_note" => {
            delete_note_inner(&ctx.db, req_i32(args, "id")?)
                .await
                .map(|_| Value::Null)
        }
        "disconnect_session" => {
            let session_id = req_str(args, "session_id")?;
            disconnect_session_inner(Emit::serve(&bus), &ctx.ssh, &ctx.mirrors, session_id)
                .await
                .map(|_| Value::Null)
        }
        "close_profile" => {
            close_profile_inner(&ctx.db, &ctx.ssh, &ctx.monitors)
                .await
                .map(|_| Value::Null)
        }
        "persist_vault" => {
            persist_vault_inner(&ctx.db).await.map(|_| Value::Null)
        }
        "verify_fingerprint_response" => {
            verify_fingerprint_response_inner(&ctx.ssh, req_str(args, "nonce")?, req_bool(args, "accepted")?)
                .await
                .map(|_| Value::Null)
        }
        "submit_kbi_response" => {
            let responses: Option<Vec<String>> = match args.get("responses") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    serde_json::from_value(v.clone())
                        .map_err(|e| format!("[SERVE] BAD_ARG responses: {}", e))?,
                ),
            };
            submit_kbi_response_inner(&ctx.ssh, req_str(args, "nonce")?, responses)
                .await
                .map(|_| Value::Null)
        }
        "start_tunnel" => {
            let spec: crate::tunnel::TunnelSpec = match args.get("spec") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| format!("[SERVE] BAD_ARG spec: {}", e))?,
                None => return Err("[SERVE] MISSING_ARG: spec".to_string()),
            };
            start_tunnel_inner(Emit::serve(&bus), &ctx.ssh, req_str(args, "session_id")?, spec)
                .await
                .map(|v| json!(v))
        }
        "stop_tunnel" => {
            stop_tunnel_inner(&ctx.ssh, req_str(args, "tunnel_id")?)
                .await
                .map(|_| Value::Null)
        }
        "list_tunnels" => {
            list_tunnels_inner(&ctx.ssh, req_opt_str(args, "session_id"))
                .await
                .map(|v| json!(v))
        }
        "persist_session_tunnels" => {
            persist_session_tunnels_inner(&ctx.ssh, &ctx.db, req_str(args, "session_id")?, req_i32(args, "server_id")?)
                .await
                .map(|_| Value::Null)
        }
        "restart_session_tunnels" => {
            restart_session_tunnels_inner(
                Emit::serve(&bus),
                &ctx.ssh,
                &ctx.db,
                req_str(args, "session_id")?,
                req_i32(args, "server_id")?,
            )
            .await
            .map(|_| Value::Null)
        }
        "open_terminal" => {
            open_terminal_inner(
                Emit::serve(&bus),
                &ctx.ssh,
                req_str(args, "session_id")?,
                req_str(args, "terminal_id")?,
                req_u32(args, "cols")?,
                req_u32(args, "rows")?,
            )
            .await
            .map(|_| Value::Null)
        }
        "write_terminal_data" => {
            write_terminal_data_inner(&ctx.ssh, req_str(args, "terminal_id")?, req_bytes(args, "data")?)
                .await
                .map(|_| Value::Null)
        }
        "resize_terminal" => {
            resize_terminal_inner(&ctx.ssh, req_str(args, "terminal_id")?, req_u32(args, "cols")?, req_u32(args, "rows")?)
                .await
                .map(|_| Value::Null)
        }
        "close_terminal" => {
            close_terminal_inner(&ctx.ssh, req_str(args, "terminal_id")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_list_dir" => {
            sftp_list_dir_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "path")?)
                .await
                .map(|v| json!(v))
        }
        "sftp_create_dir" => {
            sftp_create_dir_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "path")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_remove_file" => {
            sftp_remove_file_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "path")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_remove_dir" => {
            sftp_remove_dir_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "path")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_rename" => {
            sftp_rename_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "oldpath")?, req_str(args, "newpath")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_set_permissions" => {
            sftp_set_permissions_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "path")?, req_u32(args, "permissions")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_set_owner" => {
            sftp_set_owner_inner(
                &ctx.ssh,
                req_str(args, "session_id")?,
                req_str(args, "path")?,
                req_opt_u32(args, "uid"),
                req_opt_u32(args, "gid"),
            )
            .await
            .map(|_| Value::Null)
        }
        "sftp_cancel_transfer" => {
            sftp_cancel_transfer_inner(&ctx.ssh, req_str(args, "transfer_id")?)
                .await
                .map(|_| Value::Null)
        }
        "parse_ssh_config" => {
            parse_ssh_config_inner(req_opt_str(args, "path")).map(|v| json!(v))
        }
        "parse_client_import" => {
            parse_client_import_inner(req_str(args, "text")?).map(|v| json!(v))
        }
        "mirror_dry_run" => {
            let spec: crate::mirror::MirrorSpec = match args.get("spec") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| format!("[SERVE] BAD_ARG spec: {}", e))?,
                None => return Err("[SERVE] MISSING_ARG: spec".to_string()),
            };
            mirror_dry_run_inner(&ctx.ssh, req_str(args, "session_id")?, spec)
                .await
                .map(|v| json!(v))
        }
        "start_mirror" => {
            let spec: crate::mirror::MirrorSpec = match args.get("spec") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| format!("[SERVE] BAD_ARG spec: {}", e))?,
                None => return Err("[SERVE] MISSING_ARG: spec".to_string()),
            };
            start_mirror_inner(Emit::serve(&bus), &ctx.ssh, &ctx.mirrors, req_str(args, "session_id")?, spec)
                .await
                .map(|v| json!(v))
        }
        "stop_mirror" => {
            stop_mirror_inner(&ctx.mirrors, req_str(args, "mirror_id")?)
                .await
                .map(|_| Value::Null)
        }
        "list_mirrors" => {
            list_mirrors_inner(&ctx.mirrors, req_opt_str(args, "session_id"))
                .await
                .map(|v| json!(v))
        }
        "monitor_list" => {
            monitor_list_inner(&ctx.db, &ctx.monitors)
                .await
                .map(|v| json!(v))
        }
        "monitor_add" => {
            monitor_add_inner(&ctx.db, req_i32(args, "node_id")?)
                .await
                .map(|_| Value::Null)
        }
        "monitor_remove" => {
            monitor_remove_inner(&ctx.db, &ctx.monitors, req_i32(args, "node_id")?)
                .await
                .map(|_| Value::Null)
        }
        "monitor_set_metrics" => {
            monitor_set_metrics_inner(&ctx.db, &ctx.monitors, req_i32(args, "node_id")?, req_str_vec(args, "metrics")?)
                .await
                .map(|_| Value::Null)
        }
        "monitor_set_custom_metrics" => {
            let customs: Vec<crate::monitor::CustomMetric> = match args.get("customs") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| format!("[SERVE] BAD_ARG customs: {}", e))?,
                None => return Err("[SERVE] MISSING_ARG: customs".to_string()),
            };
            monitor_set_custom_metrics_inner(&ctx.db, &ctx.monitors, req_i32(args, "node_id")?, customs)
                .await
                .map(|_| Value::Null)
        }
        "monitor_resume" => {
            monitor_resume_inner(Emit::serve(&bus), &ctx.db, &ctx.monitors, &ctx.settings, req_i32(args, "node_id")?)
                .await
                .map(|_| Value::Null)
        }
        "monitor_pause" => {
            monitor_pause_inner(&ctx.db, &ctx.monitors, req_i32(args, "node_id")?)
                .await
                .map(|_| Value::Null)
        }
        "monitor_pause_all" => {
            monitor_pause_all_inner(&ctx.db, &ctx.monitors)
                .await
                .map(|_| Value::Null)
        }
        "monitor_resume_all" => {
            monitor_resume_all_inner(Emit::serve(&bus), &ctx.db, &ctx.monitors, &ctx.settings)
                .await
                .map(|_| Value::Null)
        }
        "monitor_get_settings" => {
            monitor_get_settings_inner(&ctx.db, &ctx.settings)
                .await
                .map(|v| json!(v))
        }
        "monitor_set_settings" => {
            let new_settings: crate::monitor::MonitorSettings = match args.get("new_settings") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| format!("[SERVE] BAD_ARG new_settings: {}", e))?,
                None => return Err("[SERVE] MISSING_ARG: new_settings".to_string()),
            };
            monitor_set_settings_inner(&ctx.db, &ctx.settings, new_settings)
                .await
                .map(|v| json!(v))
        }
        "ssh_info_probe_section" => {
            ssh_info_probe_section_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "section")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_iptables_chain" => {
            ssh_iptables_chain_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "table")?, req_str(args, "chain")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_nft_chain" => {
            ssh_nft_chain_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "family")?, req_str(args, "table")?, req_str(args, "chain")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_systemctl_action" => {
            ssh_systemctl_action_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "unit")?, req_str(args, "action")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_kill_process" => {
            ssh_kill_process_inner(&ctx.ssh, req_str(args, "session_id")?, req_i32(args, "pid")?, req_opt_str(args, "signal"))
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_container_action" => {
            docker::ssh_docker_container_action_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "container")?, req_str(args, "action")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_inspect" => {
            docker::ssh_docker_inspect_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "kind")?, req_str(args, "name")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_stats" => {
            docker::ssh_docker_stats_inner(&ctx.ssh, req_str(args, "session_id")?, req_opt_str(args, "container"))
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_logs" => {
            docker::ssh_docker_logs_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "container")?, req_u32(args, "tail")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_networks" => {
            docker::ssh_docker_networks_inner(&ctx.ssh, req_str(args, "session_id")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_containers" => {
            docker::ssh_docker_containers_inner(&ctx.ssh, req_str(args, "session_id")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_images_list" => {
            docker::ssh_docker_images_list_inner(&ctx.ssh, req_str(args, "session_id")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_volumes_list" => {
            docker::ssh_docker_volumes_list_inner(&ctx.ssh, req_str(args, "session_id")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_compose_list" => {
            docker::ssh_docker_compose_list_inner(&ctx.ssh, req_str(args, "session_id")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_compose_per_container" => {
            docker::ssh_docker_compose_per_container_inner(&ctx.ssh, req_str(args, "session_id")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_compose_services" => {
            docker::ssh_docker_compose_services_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "compose_file")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_compose_action" => {
            docker::ssh_docker_compose_action_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "compose_file")?, req_str(args, "action")?, req_opt_str(args, "service"))
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_compose_view" => {
            docker::ssh_docker_compose_view_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "compose_file")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_prune" => {
            docker::ssh_docker_prune_inner(&ctx.ssh, req_str(args, "session_id")?, req_str(args, "scope")?)
                .await
                .map(|v| json!(v))
        }
        "ssh_docker_logs_start" => {
            docker::ssh_docker_logs_start_inner(Emit::serve(&bus), &ctx.ssh, &ctx.docker_streams, req_str(args, "session_id")?, req_str(args, "stream_id")?, req_str(args, "container")?, req_u32(args, "tail")?)
                .await
                .map(|_| Value::Null)
        }
        "ssh_docker_logs_stop" => {
            docker::ssh_docker_logs_stop_inner(&ctx.docker_streams, req_str(args, "stream_id")?)
                .await
                .map(|_| Value::Null)
        }
        "open_container_terminal" => {
            docker::open_container_terminal_inner(Emit::serve(&bus), &ctx.ssh, req_str(args, "session_id")?, req_str(args, "terminal_id")?, req_str(args, "container")?, req_u32(args, "cols")?, req_u32(args, "rows")?, req_bool(args, "use_sudo")?)
                .await
                .map(|_| Value::Null)
        }
        "sftp_upload_file" | "sftp_upload_dir" | "sftp_download_file" | "sftp_download_dir" => {
            Err("[SERVE] MOVED: use the HTTP file endpoints (/api/files/upload, /api/files/download) — local paths don't cross the network".to_string())
        }
        "sftp_open_remote_file" | "local_open_file" | "local_open_in_explorer" | "sftp_prepare_drag"
        | "select_local_folder" | "pick_local_directory" | "local_list_dir" | "local_home_dir"
        | "local_desktop_dir" | "local_create_dir" | "local_remove" | "local_rename" | "android_quick_dirs"
        | "android_default_local_dir" | "export_profile" | "import_profile_pick" | "import_profile_save" => {
            Err("[SERVE] DESKTOP_ONLY: needs the server's local desktop".to_string())
        }
        "initiate_connection" => {
            // Field names mirror the frontend QuickAuth verbatim (snake_case
            // on both sides), so quick_auth deserializes directly.
            let session_id = args
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or("[SERVE] MISSING_ARG: session_id")?;
            let server_id = args
                .get("server_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| "[SERVE] MISSING_ARG: server_id".to_string())
                .and_then(|v| {
                    i32::try_from(v).map_err(|_| "[SERVE] BAD_ARG server_id: out of range".to_string())
                })?;
            let custom_password = args
                .get("custom_password")
                .and_then(Value::as_str)
                .map(str::to_string);
            let quick_auth: Option<QuickAuth> = match args.get("quick_auth") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    serde_json::from_value(v.clone())
                        .map_err(|e| format!("[SERVE] BAD_ARG quick_auth: {}", e))?,
                ),
            };
            let session_role = args
                .get("session_role")
                .and_then(Value::as_str)
                .map(str::to_string);
            let separate_sessions = args.get("separate_sessions").and_then(Value::as_bool);
            initiate_connection_inner(
                Emit::serve(&bus),
                &ctx.ssh,
                &ctx.db,
                &ctx.mirrors,
                session_id.to_string(),
                server_id,
                custom_password,
                quick_auth,
                session_role,
                separate_sessions,
            )
            .await
            .map(|_| Value::Null)
        }
        "cloud_status" | "cloud_signup" | "cloud_consume_verify_link" | "cloud_set_password"
        | "cloud_login" | "cloud_logout" | "cloud_request_password_reset" | "cloud_reset_password"
        | "cloud_request_login_link" | "cloud_login_with_link" | "sync_now" | "profile_sync_stats"
        | "identity_status" | "setup_identity" | "reset_identity" | "share_current_profile"
        | "invite_to_share" | "list_shares" | "share_member_list" | "accept_share"
        | "import_shared_profile" | "restore_personal_profile" | "share_set_role" | "share_revoke"
        | "share_leave" | "share_delete" | "profile_share_status" | "stop_sharing"
        | "set_editor_label" | "force_push_profile" | "cloud_delete_profile"
        | "cloud_list_sync_profiles" => {
            Err("[SERVE] CLOUD_ONLY: cloud sync not supported in gateway mode".to_string())
        }
        _ => Err(format!("[SERVE] UNKNOWN_COMMAND: {}", cmd)),
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// Constant-time token compare — the token gates an SSH vault, so no
/// early-exit byte loop.
fn token_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn request_token<B>(req: &Request<B>) -> Option<String> {
    // `?token=` first (WS clients, curl), then `Authorization: Bearer`.
    // No percent-decoding: tokens are hex, so `&`/`=` never appear in one.
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("token=") {
                return Some(v.to_string());
            }
        }
    }
    req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.to_string())
}

async fn require_token(State(state): State<Arc<ServeState>>, req: Request<axum::body::Body>, next: Next) -> impl IntoResponse {
    match request_token(&req) {
        Some(t) if token_eq(&t, &state.token) => {
            touch_activity(&state);
            next.run(req).await.into_response()
        }
        _ => (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response(),
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn ws_handler(
    State(state): State<Arc<ServeState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth is enforced solely by the `require_token` middleware, which runs
    // before the upgrade — one gate, so `?token=` and Bearer clients behave
    // identically. A second check here would risk the two gates diverging.
    ws.on_upgrade(move |socket| {
        ws_loop(state.ctx.clone(), state.bus.clone(), state.activity.clone(), socket)
    })
        .into_response()
}

/// Serialise + send one JSON frame. Serialization of our own structs is
/// infallible in practice, but a loop-breaking `Err` beats an `unwrap`
/// that could take down the connection task on a pathological payload.
async fn send_json(socket: &mut WebSocket, v: &Value) -> Result<(), axum::Error> {
    match serde_json::to_string(v) {
        Ok(text) => socket.send(Message::Text(text.into())).await,
        Err(_) => Ok(()),
    }
}

async fn send_resp(socket: &mut WebSocket, resp: &WsResp) -> Result<(), axum::Error> {
    send_json(socket, &serde_json::to_value(resp).unwrap_or(Value::Null)).await
}

/// One connection: request/response dispatch plus broadcast fan-out.
/// Subscriptions live only in this task — no shared mutable state, no lock.
/// A lagged receiver skips missed events rather than stalling the loop;
/// chatty sessions can't wedge a slow browser.
async fn ws_loop(
    ctx: Arc<ServeCtx>,
    bus: EventBus,
    activity: Arc<std::sync::Mutex<std::time::Instant>>,
    mut socket: WebSocket,
) {
    let mut rx = bus.subscribe();
    let mut subs: HashSet<String> = HashSet::new();
    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(msg) = msg else { break };
                // Any client frame counts as activity — keeps an idle-but-
                // connected UI (subscribed, no clicks) from being locked out
                // from under itself while events still flow to it.
                if let Ok(mut t) = activity.lock() {
                    *t = std::time::Instant::now();
                }
                let text = match msg {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue, // binary/ping/pong: ignored
                };
                match parse_incoming(&text) {
                    Ok(Incoming::Req(req)) => {
                        let args = normalize_args(&req.args);
                        let resp = match dispatch(&ctx, &bus, &req.cmd, &args).await {
                            Ok(result) => WsResp { id: req.id, ok: true, result: Some(result), error: None },
                            Err(error) => WsResp { id: req.id, ok: false, result: None, error: Some(error) },
                        };
                        if send_resp(&mut socket, &resp).await.is_err() {
                            break;
                        }
                    }
                    Ok(Incoming::Sub(sub)) => {
                        if sub.kind == "sub" {
                            subs.extend(sub.events);
                        } else if sub.kind == "unsub" {
                            for e in &sub.events {
                                subs.remove(e);
                            }
                        } else {
                            let resp = WsResp { id: 0, ok: false, result: None, error: Some(format!("[SERVE] BAD_REQUEST: unknown type {:?}", sub.kind)) };
                            if send_resp(&mut socket, &resp).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        let ack = json!({"type": "sub-ack", "events": subs.iter().collect::<Vec<_>>()});
                        if send_json(&mut socket, &ack).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let resp = WsResp { id: 0, ok: false, result: None, error: Some(error) };
                        if send_resp(&mut socket, &resp).await.is_err() {
                            break;
                        }
                    }
                }
            }
            evt = rx.recv() => {
                match evt {
                    Ok((event, payload)) => {
                        if subs.contains(&event) {
                            let push = json!({"event": event, "payload": payload});
                            if send_json(&mut socket, &push).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP file bridge
// ---------------------------------------------------------------------------

/// Staging path for one upload/download. Random hex name (no client input in
/// the path), cleaned up by the caller on every exit path.
fn temp_stage_path(prefix: &str) -> PathBuf {
    let mut b = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    std::env::temp_dir().join(format!("submarine-{}-{}.part", prefix, hex::encode(b)))
}

/// POST /api/files/upload — multipart form with `session_id`, `remote_path`,
/// optional `overwrite` ("true"), and one `file` field. Streams the upload
/// to a server temp file (bounded memory — chunks, never the whole body),
/// runs it through the normal SFTP upload path (progress events included),
/// then deletes the temp file. Responds with the standard `{ok,result,error}`
/// JSON envelope, mirroring the WS protocol.
async fn files_upload(
    State(state): State<Arc<ServeState>>,
    mut multipart: Multipart,
) -> Json<Value> {
    let fail = |e: String| Json(json!({"ok": false, "error": e}));
    let mut session_id: Option<String> = None;
    let mut remote_path: Option<String> = None;
    let mut overwrite = false;
    let mut staged: Option<PathBuf> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(f) => f,
            Err(e) => return fail(format!("[SERVE] BAD_MULTIPART: {}", e)),
        };
        let Some(mut field) = field else { break };
        match field.name() {
            Some("session_id") => {
                session_id = field.text().await.ok().filter(|s| !s.is_empty());
            }
            Some("remote_path") => {
                remote_path = field.text().await.ok().filter(|s| !s.is_empty());
            }
            Some("overwrite") => {
                overwrite = field.text().await.map(|v| v == "true").unwrap_or(false);
            }
            Some("file") => {
                // One file per request — drop any previous staging so a
                // malicious multi-file body can't pile up temp files.
                if let Some(old) = staged.take() {
                    let _ = tokio::fs::remove_file(&old).await;
                }
                let path = temp_stage_path("upload");
                let mut f = match tokio::fs::File::create(&path).await {
                    Ok(f) => f,
                    Err(e) => return fail(format!("[SERVE] TEMP_CREATE: {}", e)),
                };
                let mut ok = true;
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            if let Err(e) = f.write_all(&chunk).await {
                                let _ = tokio::fs::remove_file(&path).await;
                                return fail(format!("[SERVE] TEMP_WRITE: {}", e));
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    let _ = tokio::fs::remove_file(&path).await;
                    return fail("[SERVE] BAD_CHUNK: truncated multipart body".to_string());
                }
                staged = Some(path);
            }
            _ => {}
        }
    }

    let (Some(session_id), Some(remote_path), Some(tmp)) = (session_id, remote_path, staged) else {
        return fail("[SERVE] MISSING_ARG: need session_id + remote_path + file".to_string());
    };
    let tmp_str = tmp.to_string_lossy().to_string();
    let res = sftp_upload_file_inner(
        Emit::serve(&state.bus),
        &state.ctx.ssh,
        session_id,
        tmp_str,
        remote_path,
        Some(overwrite),
    )
    .await
    .map(|_| Value::Null)
    .map_err(|e| e.to_string());
    let _ = tokio::fs::remove_file(&tmp).await;
    match res {
        Ok(v) => Json(json!({"ok": true, "result": v})),
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

/// GET /api/files/download?session_id=&remote_path= — runs the normal SFTP
/// download path into a server temp file, then streams the bytes back with
/// an attachment disposition. Temp file is deleted before responding.
/// Binary body, so errors use HTTP status codes (no JSON envelope possible):
/// 400 missing params, 502 SFTP failure, 500 local IO failure.
async fn files_download(
    State(state): State<Arc<ServeState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let err = |status: StatusCode, msg: String| (status, msg).into_response();
    let (Some(session_id), Some(remote_path)) =
        (params.get("session_id").cloned(), params.get("remote_path").cloned())
    else {
        return err(StatusCode::BAD_REQUEST, "[SERVE] MISSING_ARG: session_id + remote_path".to_string());
    };
    let tmp = temp_stage_path("download");
    let tmp_str = tmp.to_string_lossy().to_string();
    if let Err(e) = sftp_download_file_inner(
        Emit::serve(&state.bus),
        &state.ctx.ssh,
        session_id,
        remote_path.clone(),
        tmp_str,
        Some(true),
    )
    .await
    {
        let _ = tokio::fs::remove_file(&tmp).await;
        return err(StatusCode::BAD_GATEWAY, e);
    }
    let bytes = match tokio::fs::read(&tmp).await {
        Ok(b) => b,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("[SERVE] TEMP_READ: {}", e));
        }
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    let filename: String = remote_path
        .rsplit('/')
        .next()
        .unwrap_or("download")
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') { c } else { '_' })
        .collect();
    let mut headers = header::HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/octet-stream"));
    // Sanitised above to a header-safe alphabet; fall back rather than fail.
    let disp = format!("attachment; filename=\"{}\"", filename);
    headers.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&disp).unwrap_or_else(|_| header::HeaderValue::from_static("attachment")),
    );
    (StatusCode::OK, headers, bytes).into_response()
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

fn print_usage() {
    eprintln!("usage: submarine serve [--port N] [--bind ADDR] [--data-dir P] [--frontend-dir P]");
    eprintln!("  [--token T | SUBMARINE_SERVE_TOKEN=...] [--allow-remote]");
    eprintln!("  [--tls-cert F --tls-key F] [--lock-after MINUTES (0 = never, default 30)]");
}

/// Idle auto-lock task. MUST be spawned from inside the runtime (call this
/// at the top of the `block_on` body, not before it — `tokio::spawn`
/// panics without a running runtime). Same teardown as a manual lock plus
/// a `vault-locked` broadcast so a connected UI can react.
fn spawn_lock_task(state: Arc<ServeState>, lock_after_mins: u64) {
    if lock_after_mins == 0 {
        return;
    }
    let lock_ctx = state.ctx.clone();
    let lock_bus = state.bus.clone();
    let lock_activity = state.activity.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let idle = lock_activity
                .lock()
                .map(|t| t.elapsed())
                .unwrap_or(std::time::Duration::ZERO);
            if idle >= std::time::Duration::from_secs(lock_after_mins * 60) {
                if close_profile_inner(&lock_ctx.db, &lock_ctx.ssh, &lock_ctx.monitors)
                    .await
                    .is_ok()
                {
                    let _ = lock_bus.send((
                        "vault-locked".to_string(),
                        serde_json::json!({"reason": "idle-timeout"}),
                    ));
                }
                // Reset even on failure: already locked (or nothing open),
                // so re-running every minute would just spin.
                if let Ok(mut t) = lock_activity.lock() {
                    *t = std::time::Instant::now();
                }
            }
        }
    });
}

fn default_data_dir() -> Option<PathBuf> {
    // Same directory the desktop build uses: the platform app-data base +
    // the bundle identifier this binary was built with, so serve sees the
    // operator's real profiles instead of a fresh empty vault.
    let ctx: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let id = ctx.config().identifier.clone();
    directories::BaseDirs::new().map(|b| b.data_dir().join(id))
}

fn default_frontend_dir() -> Option<PathBuf> {
    // Dev flow runs from the repo root (`./dist`) or `src-tauri` (`../dist`).
    // Installed layouts pass `--frontend-dir` explicitly.
    for cand in ["./dist", "../dist"] {
        let p = PathBuf::from(cand);
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    None
}

/// Parse `serve` argv, print the token + URL, and block on the server.
///
/// Hardening model: loopback by default; anything else needs an explicit
/// `--allow-remote` (the gateway holds the decrypted vault in memory).
/// Remote use additionally wants TLS — either in-process
/// (`--tls-cert/--tls-key`) or, better for a public host, a TLS-terminating
/// reverse proxy in front (Caddy: two lines + automatic Let's Encrypt; see
/// the note printed on remote binds).
pub fn run_serve(raw_args: Vec<String>) {
    let mut port = DEFAULT_PORT;
    let mut bind = DEFAULT_BIND.to_string();
    let mut data_dir: Option<PathBuf> = None;
    let mut frontend_dir: Option<PathBuf> = None;
    let mut token_flag: Option<String> = None;
    let mut tls_cert: Option<PathBuf> = None;
    let mut tls_key: Option<PathBuf> = None;
    let mut allow_remote = false;
    let mut lock_after_mins: u64 = 30;

    let mut it = raw_args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" => match it.next().and_then(|v| v.parse::<u16>().ok()) {
                Some(p) => port = p,
                None => { print_usage(); return; }
            },
            "--bind" => match it.next() {
                Some(b) => bind = b.clone(),
                None => { print_usage(); return; }
            },
            "--data-dir" => match it.next() {
                Some(d) => data_dir = Some(PathBuf::from(d)),
                None => { print_usage(); return; }
            },
            "--frontend-dir" => match it.next() {
                Some(d) => frontend_dir = Some(PathBuf::from(d)),
                None => { print_usage(); return; }
            },
            "--token" => match it.next() {
                Some(t) if !t.is_empty() => token_flag = Some(t.clone()),
                _ => { print_usage(); return; }
            },
            "--allow-remote" => allow_remote = true,
            "--tls-cert" => match it.next() {
                Some(p) => tls_cert = Some(PathBuf::from(p)),
                None => { print_usage(); return; }
            },
            "--tls-key" => match it.next() {
                Some(p) => tls_key = Some(PathBuf::from(p)),
                None => { print_usage(); return; }
            },
            "--lock-after" => match it.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(m) => lock_after_mins = m,
                None => { print_usage(); return; }
            },
            "--help" | "-h" => { print_usage(); return; }
            other => { eprintln!("unknown flag: {}", other); print_usage(); return; }
        }
    }
    if tls_cert.is_some() != tls_key.is_some() {
        eprintln!("[SERVE] --tls-cert and --tls-key must be passed together");
        return;
    }

    let data_dir = match data_dir.or_else(default_data_dir) {
        Some(d) => d,
        None => { eprintln!("[SERVE] cannot locate app data dir — pass --data-dir"); return; }
    };
    // Profiles live one level down; the dir may not exist on first run.
    // Warn (don't abort) when creation fails — reads will surface the OS
    // error with context instead of silently listing nothing.
    let profiles = data_dir.join("profiles");
    if let Err(e) = std::fs::create_dir_all(&profiles) {
        eprintln!("[SERVE] cannot create profiles dir {}: {}", profiles.display(), e);
    }

    let frontend_dir = match frontend_dir.or_else(default_frontend_dir) {
        Some(d) => d,
        None => { eprintln!("[SERVE] frontend not found — pass --frontend-dir <dist> (run `npm run build` first)"); return; }
    };

    // Token precedence: explicit flag (automation) > env (secrets managers,
    // no shell history) > fresh random per boot. Printed once below — the
    // operator pastes it into the URL.
    let token = match token_flag
        .or_else(|| std::env::var("SUBMARINE_SERVE_TOKEN").ok().filter(|s| !s.is_empty()))
    {
        Some(t) => t,
        None => {
            let mut bytes = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
            hex::encode(bytes)
        }
    };

    let non_loopback = !(bind == "127.0.0.1" || bind == "::1" || bind == "localhost");
    if non_loopback && !allow_remote {
        eprintln!(
            "[SERVE] refusing to bind {} without --allow-remote: the gateway holds your decrypted vault in memory, so network exposure needs an explicit opt-in. Remote use also needs TLS — pass --tls-cert/--tls-key or front with a TLS reverse proxy (e.g. `caddy reverse-proxy --from <host> --to 127.0.0.1:{}`), see #270.",
            bind, port
        );
        return;
    }
    if non_loopback && tls_cert.is_none() {
        eprintln!("[SERVE] WARNING: no TLS on a network bind — the auth token travels in plaintext. Front with a TLS reverse proxy or pass --tls-cert/--tls-key.");
    }

    // Fan-out bus: 512 slots of headroom so a burst of terminal output
    // can't force a slow receiver to choose between lagging and stalling.
    let (bus, _) = broadcast::channel::<crate::emit::BusEvent>(512);

    let activity = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let ctx = Arc::new(ServeCtx {
        data_dir,
        db: Arc::new(new_db_state()),
        ssh: Arc::new(SshState::new()),
        mirrors: Arc::new(new_mirror_map()),
        monitors: Arc::new(new_monitor_map()),
        settings: std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::monitor::MonitorSettings::default(),
        )),
        docker_streams: std::sync::Arc::new(docker::DockerStreams::new()),
    });
    let state = Arc::new(ServeState { ctx, token: token.clone(), bus, activity });

    // Idle auto-lock: same teardown as a manual lock, on a timer. Fires at
    // most once per idle window (the clock resets after locking, so an
    // abandoned unlocked gateway re-locks itself instead of spinning).
    // Cloned here because `state` moves into the router below.
    let lock_state = state.clone();

    // The token gate applies to /api/* ONLY. Static files stay public:
    // the bundle holds no secrets, and browsers request JS/CSS/favicon
    // without query strings — gating them 401s the app itself on load.
    // All vault/SSH/SFTP data flows through the gated API.
    // Liveness probe. Gated like everything else under /api, so a 200
    // proves the token is accepted and a 401 proves it isn't — the
    // frontend uses this to tell "server restarted, token stale" apart
    // from "server unreachable" (WS handshakes never expose the status).
    async fn api_ping() -> Json<Value> {
        Json(json!({"ok": true}))
    }

    let api = Router::new()
        .route("/api/ws", get(ws_handler))
        .route("/api/ping", get(api_ping))
        .route("/api/files/upload", post(files_upload))
        .route("/api/files/download", get(files_download))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state);

    let app = Router::new()
        .merge(api)
        .fallback_service(ServeDir::new(&frontend_dir).append_index_html_on_directories(true));

    // `SocketAddr::from_str` doesn't resolve hostnames — map the conventional
    // loopback name to its IP so `--bind localhost` doesn't die confusingly.
    let bind_host = if bind == "localhost" { "127.0.0.1" } else { bind.as_str() };
    let addr: SocketAddr = match format!("{}:{}", bind_host, port).parse() {
        Ok(a) => a,
        Err(e) => { eprintln!("[SERVE] bad bind address: {}", e); return; }
    };

    let scheme = if tls_cert.is_some() { "https" } else { "http" };
    println!("submarine serve on {}://{}:{}/?token={}", scheme, bind, port, token);
    println!("data: {} | frontend: {}", profiles.display(), frontend_dir.display());
    if lock_after_mins > 0 {
        println!("idle auto-lock: {} min", lock_after_mins);
    } else {
        println!("idle auto-lock: off");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        spawn_lock_task(lock_state, lock_after_mins);
        if let (Some(cert), Some(key)) = (tls_cert, tls_key) {
            // The dependency tree enables multiple rustls crypto providers
            // (reqwest + axum-server pull both aws-lc-rs and ring), so the
            // automatic choice fails — pin aws-lc-rs explicitly. Idempotent:
            // AlreadyExists (something else installed first) is fine.
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                .await
                .expect("load TLS cert/key");
            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                shutdown_handle.graceful_shutdown(None);
            });
            axum_server::bind_rustls(addr, config)
                .handle(handle)
                .serve(app.into_make_service())
                .await
                .expect("serve error");
        } else {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("bind serve socket");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .expect("serve error");
        }
    });
}
