use serde_json::{json, Value};

/// MCP tool definitions. Descriptions are instructions to the AI agent —
/// phrased to make snapshot-before-migration the default behavior (design §7).
pub fn tools() -> Value {
    json!([
        {
            "name": "dbsnap_create_snapshot",
            "description": "Create a snapshot of the local development database before migrations or bulk changes. SQLite uses page backups, PostgreSQL uses clones, MySQL/MariaDB uses logical dumps. PostgreSQL clone creation briefly fences connections; MySQL dump consistency requires InnoDB.",
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
            "description": "Restore the local database to a previous snapshot after user approval. Omit tag_or_id for the most recent snapshot. The current state is kept as a safety snapshot. MySQL/MariaDB restore is non-atomic: stop writers and scheduled events first; interrupted restores must recover the pinned safety snapshot named in the recovery journal.",
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
            "name": "dbsnap_diff",
            "description": "Compare schema and row counts between SQLite snapshots, PostgreSQL clones, or standard MySQL/MariaDB dumps. Does not compare row contents or live database state. PostgreSQL compares tables/views; MySQL compares tables only, excluding routines, triggers and events.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "First snapshot ID or tag." },
                    "to": { "type": "string", "description": "Second snapshot ID or tag." }
                },
                "required": ["from", "to"],
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
