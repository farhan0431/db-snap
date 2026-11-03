use super::schema;
use crate::ops::{self, Ctx};
use serde_json::{json, Value};

/// Handle one JSON-RPC message. Returns `None` for notifications (no reply).
pub fn handle(ctx: &Ctx, msg: &Value) -> Option<Value> {
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
            "instructions": "dbsnap is a physical time-machine for the local development database. Create a snapshot with dbsnap_create_snapshot BEFORE risky migrations or DDL, and restore with dbsnap_restore_snapshot when something breaks. Run dbsnap_status first if unsure which database is protected."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": schema::tools() })),
        "tools/call" => tools_call(ctx, msg.get("params")),
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

fn tools_call(ctx: &Ctx, params: Option<&Value>) -> Result<Value, (i64, String)> {
    let params = params.cloned().unwrap_or(Value::Null);
    let name = params["name"].as_str().unwrap_or_default().to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let tool_result: Result<String, anyhow::Error> = match name.as_str() {
        "dbsnap_create_snapshot" => {
            let tag = match args.get("tag") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(tag) => Some(tag.to_string()),
                    None => return Ok(error_result("tag must be a string")),
                },
            };
            ops::save_with(ctx, tag, crate::engine::SaveMethod::Clone)
        }
        "dbsnap_restore_snapshot" => {
            let id = args["tag_or_id"].as_str().map(str::to_string);
            ops::restore_with(ctx, id, true)
        }
        "dbsnap_list_snapshots" => ops::list_with(ctx),
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
        other => {
            return Ok(error_result(&format!(
                "unknown tool: {other} — available: dbsnap_create_snapshot, dbsnap_restore_snapshot, dbsnap_list_snapshots, dbsnap_prune, dbsnap_status"
            )));
        }
    };

    Ok(match tool_result {
        Ok(text) => json!({
            "content": [ { "type": "text", "text": crate::util::mask(&text) } ],
            "isError": false
        }),
        Err(e) => json!({
            "content": [ { "type": "text", "text": crate::util::mask(&format!("Error: {e:#}")) } ],
            "isError": true
        }),
    })
}

fn error_result(text: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": crate::util::mask(text) } ], "isError": true })
}
