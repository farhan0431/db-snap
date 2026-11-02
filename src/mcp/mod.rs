pub mod handler;
pub mod schema;

use crate::ops;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

/// Run the MCP stdio server: newline-delimited JSON-RPC 2.0 on stdin/stdout.
/// NOTHING except JSON-RPC responses may be written to stdout — diagnostics
/// go to stderr (design §6).
pub fn run(explicit_db: Option<&str>) -> Result<()> {
    let root = std::env::current_dir().context("cannot determine current directory")?;
    let ctx = ops::context_at(&root, explicit_db, false)?;

    eprintln!(
        "dbsnap mcp {} — serving {} ({})",
        env!("CARGO_PKG_VERSION"),
        ctx.db.engine,
        ctx.db.path.display()
    );

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();

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
        let response: Option<Value> = match serde_json::from_str(trimmed) {
            Ok(msg) => handler::handle(&ctx, &msg),
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
