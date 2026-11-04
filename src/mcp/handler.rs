use super::schema;
use crate::ops::{self, Ctx};
use serde_json::{json, Value};

/// Handle one JSON-RPC message. Returns `None` for notifications (no reply).
pub fn handle(ctx: &Ctx, msg: &Value) -> Option<Value> {
    handle_with_policy(ctx, msg, false)
}

pub(super) fn row_diff_replays(args: &Value) -> bool {
    args["replay_dumps"] == true || args.get("docker_image").is_some()
}

pub fn handle_with_policy(ctx: &Ctx, msg: &Value, allow_destructive: bool) -> Option<Value> {
    let method = msg.get("method")?.as_str()?;
    let id = msg.get("id").cloned();
    let is_request = matches!(id, Some(ref v) if !v.is_null());
    if !is_request {
        // notifications/initialized, notifications/cancelled, ...
        return None;
    }
    let id = id.unwrap();

    let outcome: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiated_version(msg),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "dbsnap", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "dbsnap provides checkpoints for local development databases. Check dbsnap_status, save before risky changes, and retain the exact snapshot_id from structuredContent.data. Verify checks artifact integrity/existence, not restore success. Doctor diagnoses journals and untracked files without applying recovery. Live diff creates a retained checkpoint; row preview returns counts, with explicit opt-in for trusted dump replay. Restore/prune require authorized destructive sessions and per-call elicitation. Stop MySQL writers/events first, and preserve recovery journals and pinned snapshots on failure."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": schema::tools() })),
        "tools/call" => tools_call(ctx, msg.get("params"), allow_destructive),
        other => Err((-32601, format!("Method not found: {other}"))),
    };

    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    })
}

fn negotiated_version(msg: &Value) -> String {
    match msg["params"]["protocolVersion"].as_str() {
        Some(v) if v.starts_with("2024-") || v.starts_with("2025-") => v.to_string(),
        _ => "2024-11-05".to_string(),
    }
}

fn tools_call(
    ctx: &Ctx,
    params: Option<&Value>,
    allow_destructive: bool,
) -> Result<Value, (i64, String)> {
    let params = params.cloned().unwrap_or(Value::Null);
    let name = params["name"].as_str().unwrap_or_default().to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    if !allow_destructive && matches!(name.as_str(), "dbsnap_restore_snapshot" | "dbsnap_prune") {
        return Ok(error_result("destructive MCP tools are disabled; explicitly start the server with --allow-destructive to enable them"));
    }

    let tool_result: Result<String, anyhow::Error> = match name.as_str() {
        "dbsnap_create_snapshot" => {
            let tag = match args.get("tag") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(tag) => Some(tag.to_string()),
                    None => return Ok(error_result("tag must be a string")),
                },
            };
            ops::save_json_with(ctx, tag, crate::engine::SaveMethod::Clone)
        }
        "dbsnap_restore_snapshot" => {
            let id = args["tag_or_id"].as_str().map(str::to_string);
            ops::restore_with(ctx, id, true)
        }
        "dbsnap_list_snapshots" => ops::list_with(ctx),
        "dbsnap_diff" => {
            let (Some(from), Some(to)) = (args["from"].as_str(), args["to"].as_str()) else {
                return Ok(error_result("from and to must be snapshot IDs or tags"));
            };
            ops::diff_with(ctx, from, to)
        }
        "dbsnap_prune" => {
            let Some(keep) = args["keep"]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .filter(|v| *v > 0)
            else {
                return Ok(error_result("keep must be a positive integer"));
            };
            ops::prune_with(ctx, keep)
        }
        "dbsnap_status" => ops::status_with(ctx),
        "dbsnap_verify" => {
            let selector = match args.get("tag_or_id") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(s) => Some(s),
                    None => return Ok(error_result("tag_or_id must be a string")),
                },
            };
            ops::verify_with(ctx, selector)
        }
        "dbsnap_doctor" => ops::doctor_with(ctx),
        "dbsnap_diff_rows" => {
            let (Some(from), Some(to), Some(table)) = (
                args["from"].as_str(),
                args["to"].as_str(),
                args["table"].as_str(),
            ) else {
                return Ok(error_result("from, to and table must be strings"));
            };
            let limit = match args.get("limit") {
                None => 10000,
                Some(value) => match value.as_u64().and_then(|v| usize::try_from(v).ok()) {
                    Some(v) => v,
                    None => return Ok(error_result("limit must be an integer")),
                },
            };
            let replay_dumps = match args.get("replay_dumps") {
                None => false,
                Some(value) => match value.as_bool() {
                    Some(value) => value,
                    None => return Ok(error_result("replay_dumps must be a boolean")),
                },
            };
            let docker_image = match args.get("docker_image") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(image @ ("mysql:8.0" | "mariadb:11")) => Some(image),
                    _ => return Ok(error_result("docker_image must be mysql:8.0 or mariadb:11")),
                },
            };
            if row_diff_replays(&args) && !allow_destructive {
                return Ok(error_result(
                    "dump replay requires an authorized MCP session and per-call approval",
                ));
            }
            let columns = match args.get("columns") {
                None => Vec::new(),
                Some(value) => match value.as_array().and_then(|values| {
                    values
                        .iter()
                        .map(|v| v.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                }) {
                    Some(columns) => columns,
                    None => return Ok(error_result("columns must be an array of strings")),
                },
            };
            let bounds = |field| args.get(field).map(|v| v.as_i64().ok_or(field)).transpose();
            let (Ok(key_min), Ok(key_max)) = (bounds("key_min"), bounds("key_max")) else {
                return Ok(error_result("key_min and key_max must be signed integers"));
            };
            ops::diff_rows_options_with(
                ctx,
                from,
                to,
                table,
                limit,
                &ops::RowOptions {
                    replay_dumps,
                    docker_image,
                    columns: &columns,
                    key_min,
                    key_max,
                    ..Default::default()
                },
            )
        }
        "dbsnap_diff_live" => {
            let Some(from) = args["from"].as_str() else {
                return Ok(error_result("from must be a snapshot ID or tag"));
            };
            ops::diff_live_with(ctx, from)
        }
        other => {
            return Ok(error_result(&format!(
                "unknown tool: {other} — available: dbsnap_create_snapshot, dbsnap_restore_snapshot, dbsnap_list_snapshots, dbsnap_diff, dbsnap_prune, dbsnap_status, dbsnap_verify, dbsnap_doctor, dbsnap_diff_live, dbsnap_diff_rows"
            )));
        }
    };

    Ok(match tool_result {
        Ok(text) => json!({
            "content": [ { "type": "text", "text": crate::util::mask(&text) } ],
            "isError": false,
            "structuredContent": {"ok":true,"operation":name,"message":crate::util::mask(&text),"data":serde_json::from_str::<Value>(&crate::util::mask(&text)).ok()}
        }),
        Err(e) => json!({
            "content": [ { "type": "text", "text": crate::util::mask(&format!("Error: {e:#}")) } ],
            "isError": true,
            "structuredContent": {"ok":false,"operation":name,"code":"OPERATION_FAILED","message":crate::util::mask(&format!("{e:#}")),
                "data": recovery_hint(ctx)}
        }),
    })
}

fn recovery_hint(ctx: &Ctx) -> Value {
    let path = ctx.store.root.join("mysql-restore.json");
    if !path.exists() {
        return Value::Null;
    }
    let id = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<String>(&bytes).ok());
    match id {
        Some(id)
            if !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
        {
            json!({"stage":"recovery_required","recovery_id":id,"next_action":"resolve the failure cause, stop writers/events, then restore recovery_id through the approved restore tool or CLI recover"})
        }
        _ => {
            json!({"stage":"manual_inspection","next_action":"preserve malformed MySQL recovery journal and inspect it"})
        }
    }
}

fn error_result(text: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": crate::util::mask(text) } ], "isError": true,
        "structuredContent":{"ok":false,"code":"INVALID_OR_DENIED","message":crate::util::mask(text)} })
}
