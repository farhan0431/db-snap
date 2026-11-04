use serde_json::{json, Value};

/// MCP tool definitions. Descriptions are instructions to the AI agent —
/// phrased to make snapshot-before-migration the default behavior (design §7).
pub fn tools() -> Value {
    let mut tools = json!([
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
            "description": "Compare snapshot schema/counts for SQLite, two PostgreSQL clones, two PostgreSQL dumps (offline pg_restore), or standard MySQL/MariaDB dumps. No row values or live state are compared. Clone diff covers tables/views; archive schema is archive-wide excluding owner/grants. MySQL covers tables only. Mixed PostgreSQL clone/archive inputs are unsupported.",
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
    ]);
    tools.as_array_mut().unwrap().extend([
        json!({"name":"dbsnap_diff_rows","description":"Bounded row-change counts by primary key without returning values. PostgreSQL dumps require explicit replay_dumps for trusted SQL. MySQL/MariaDB requires an explicit Docker image and trusted dumps.","inputSchema":{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"},"table":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100000},"columns":{"type":"array","items":{"type":"string"}},"key_min":{"type":"integer"},"key_max":{"type":"integer"},"replay_dumps":{"type":"boolean"},"docker_image":{"type":"string","enum":["mysql:8.0","mariadb:11"]}},"required":["from","to","table"],"additionalProperties":false}}),
        json!({"name":"dbsnap_diff_live","description":"Capture and retain a regular snapshot of current state, then compare it with from. Creates artifacts; PostgreSQL briefly fences connections. Does not compare individual row contents.","inputSchema":{"type":"object","properties":{"from":{"type":"string"}},"required":["from"],"additionalProperties":false}}),
        json!({"name":"dbsnap_verify","description":"Check snapshot hashes/sizes/chunks or PostgreSQL clone existence without replaying SQL. Does not prove restore success.","inputSchema":{"type":"object","properties":{"tag_or_id":{"type":"string"}},"additionalProperties":false}}),
        json!({"name":"dbsnap_doctor","description":"Diagnose connectivity, pending restore journals, and untracked files; never deletes orphan files or repairs the target.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
    ]);
    for tool in tools.as_array_mut().unwrap() {
        let name = tool["name"].as_str().unwrap().to_string();
        // Existing status/list/diff may invoke PostgreSQL journal healing in
        // their shared context; do not claim read-only semantics for them.
        tool["annotations"] = json!({"readOnlyHint":matches!(name.as_str(),"dbsnap_verify"|"dbsnap_doctor"),
            "destructiveHint":matches!(name.as_str(),"dbsnap_restore_snapshot"|"dbsnap_prune"),"openWorldHint":false});
        tool["outputSchema"] = json!({"type":"object","properties":{"ok":{"type":"boolean"},"operation":{"type":"string"},"message":{"type":"string"},"code":{"type":"string"},"data":{}},"required":["ok","message"]});
    }
    tools
}
