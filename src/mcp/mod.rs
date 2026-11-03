pub mod handler;
pub mod schema;

use crate::ops;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

/// Run the MCP stdio server: newline-delimited JSON-RPC 2.0 on stdin/stdout.
/// NOTHING except JSON-RPC responses may be written to stdout — diagnostics
/// go to stderr (design §6).
pub fn run(explicit_db: Option<&str>, allow_destructive: bool) -> Result<()> {
    let root = std::env::current_dir().context("cannot determine current directory")?;
    let ctx = ops::mcp_context_at(&root, explicit_db)?;

    eprintln!(
        "dbsnap mcp {} — serving {} ({})",
        env!("CARGO_PKG_VERSION"),
        ctx.db.engine,
        ctx.db.display()
    );

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();
    let mut elicitation = false;
    let mut approval_id = 0u64;

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break; // client closed the stream
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response: Option<Value> = match serde_json::from_str::<Value>(trimmed) {
            Ok(mut msg) => {
                if msg["method"] == "initialize" {
                    let version = msg["params"]["protocolVersion"].as_str().unwrap_or("");
                    let capability = msg["params"]["capabilities"]["elicitation"].as_object();
                    elicitation = version.starts_with("2025-")
                        && version >= "2025-06-18"
                        && capability.is_some_and(|m| {
                            m.is_empty() || m.get("form").is_some_and(Value::is_object)
                        });
                }
                let name = msg["params"]["name"].as_str().unwrap_or("");
                let destructive = msg["method"] == "tools/call"
                    && msg.get("id").is_some()
                    && matches!(name, "dbsnap_restore_snapshot" | "dbsnap_prune");
                if destructive && allow_destructive {
                    let request_id = msg["id"].clone();
                    let preparation = if name == "dbsnap_restore_snapshot" {
                        let argument = msg["params"]["arguments"].get("tag_or_id");
                        let selector = match argument {
                            None => Ok(None),
                            Some(value) => value
                                .as_str()
                                .map(Some)
                                .ok_or_else(|| anyhow::anyhow!("tag_or_id must be a string")),
                        };
                        selector.and_then(|selector| ops::resolve_restore_id(&ctx, selector)).map(|id| {
                            msg["params"]["arguments"]["tag_or_id"] = json!(id);
                            format!("Restore database {} to snapshot {}? Current data will be replaced.", ctx.db.display(), id)
                        })
                    } else {
                        let keep = msg["params"]["arguments"]["keep"]
                            .as_u64()
                            .filter(|n| *n > 0);
                        keep.map(|n| format!("Prune snapshots for database {}, retaining the newest {n} and pinned snapshots?", ctx.db.display()))
                            .ok_or_else(|| anyhow::anyhow!("keep must be a positive integer"))
                    };
                    let approval = match preparation {
                        Ok(message) if elicitation => {
                            approval_id += 1;
                            let id = format!("dbsnap-approval-{approval_id}");
                            let request = json!({"jsonrpc":"2.0", "id":id, "method":"elicitation/create", "params":{
                                "message": crate::util::mask(&message),
                                "requestedSchema":{"type":"object", "properties":{"confirm":{"type":"boolean", "title":"Approve this operation"}}, "required":["confirm"]}
                            }});
                            writeln!(out, "{request}")?;
                            out.flush()?;
                            let mut answer = String::new();
                            let read = reader.read_line(&mut answer)?;
                            let reply: Value = serde_json::from_str(&answer).unwrap_or(Value::Null);
                            read > 0
                                && reply["id"] == id
                                && reply["result"]["action"] == "accept"
                                && reply["result"]["content"]["confirm"] == true
                        }
                        _ => false,
                    };
                    if approval {
                        handler::handle_with_policy(&ctx, &msg, true)
                    } else {
                        Some(json!({"jsonrpc":"2.0", "id":request_id, "result":{
                            "content":[{"type":"text","text":"Operation not approved, invalid, or client does not support MCP elicitation; no changes made."}], "isError":true,
                            "structuredContent":{"ok":false,"code":"APPROVAL_REQUIRED","message":"Operation not approved, invalid, or client does not support MCP elicitation; no changes made."}
                        }}))
                    }
                } else {
                    handler::handle_with_policy(&ctx, &msg, allow_destructive)
                }
            }
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("Parse error: {e}") }
            })),
        };
        if let Some(response) = response {
            out.write_all(response.to_string().as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    Ok(())
}
