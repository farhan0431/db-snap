use serde_json::{json, Value};

/// MCP tool definitions. Descriptions are instructions to the AI agent —
/// phrased to make snapshot-before-migration the default behavior (design §7).
pub fn tools() -> Value {
    json!([
        {
            "name": "dbsnap_create_snapshot",
            "description": "Create a physical snapshot of the local development database (instant, works while the dev server is running). MUST be called BEFORE running any schema migration, DDL statement (ALTER TABLE, DROP COLUMN, ...), or bulk data change, so a safe restore point exists.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tag": {
                        "type": "string",
                        "maxLength": 64,
                        "pattern": "^[A-Za-z0-9._ -]+$",
                        "description": "Optional label describing what comes next; defaults to a timestamp."
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "dbsnap_restore_snapshot",
            "description": "Restore the local database to a previous snapshot (point-in-time undo). Call IMMEDIATELY when a migration fails, the schema breaks, or data looks corrupted. Omit tag_or_id to restore the most recent snapshot. The current state is automatically kept as a safety snapshot before restoring.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tag_or_id": {
                        "type": "string",
                        "description": "Snapshot id (or unambiguous id prefix) or tag. Omit for the most recent snapshot."
                    }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "dbsnap_list_snapshots",
            "description": "List all available database snapshot restore points with their ids, tags, creation times and sizes.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "dbsnap_prune",
            "description": "Permanently delete older recorded snapshots, retaining at least the requested number of newest snapshots. Use only when the user requests cleanup.",
            "inputSchema": {
                "type": "object",
                "properties": { "keep": { "type": "integer", "minimum": 1, "description": "Number of newest snapshots to retain." } },
                "required": ["keep"],
                "additionalProperties": false
            }
        },
        {
            "name": "dbsnap_status",
            "description": "Show which local database dbsnap is bound to, engine/version details, and snapshot storage usage. Use this first if unsure which database is protected.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }
    ])
}
