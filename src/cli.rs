use crate::engine::SaveMethod;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "dbsnap",
    version,
    about = "Instant database time-machine for AI agents & vibe coders",
    long_about = "dbsnap saves and restores physical snapshots of your local database —\nlike git commit / git checkout for your data. Works while your dev server\nis running, in milliseconds, and ships with a built-in MCP server so AI\nagents can protect and heal the database themselves."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Bind this project to a database (auto-detects .env / common paths)
    Init {
        /// Database path or URL (overrides DATABASE_URL), e.g. --db ./prisma/dev.db
        #[arg(long)]
        db: Option<String>,
        /// Deliberately operate on a non-local host (safety guard override)
        #[arg(long)]
        allow_remote: bool,
    },
    /// Save a snapshot of the current database state
    Save {
        /// Short label for the snapshot, e.g. "before-auth-migration"
        tag: Option<String>,
        /// Database path or URL (overrides project binding)
        #[arg(long)]
        db: Option<String>,
        /// Snapshot method: clone (default) or dump (portable pg_dump -Fc)
        #[arg(long, value_enum, default_value_t = SaveMethod::Clone)]
        method: SaveMethod,
        /// Deliberately operate on a non-local host (safety guard override)
        #[arg(long)]
        allow_remote: bool,
        /// Operate even if the database differs from the project binding
        #[arg(long)]
        force: bool,
    },
    /// Restore the database to a snapshot (default: the most recent)
    Restore {
        /// Snapshot id prefix or tag
        tag_or_id: Option<String>,
        #[arg(long)]
        db: Option<String>,
        /// Skip the automatic safety snapshot of the current state
        #[arg(long)]
        no_safety_snapshot: bool,
        /// Deliberately operate on a non-local host (safety guard override)
        #[arg(long)]
        allow_remote: bool,
        /// Operate even if the database differs from the project binding
        #[arg(long)]
        force: bool,
    },
    /// Shorthand for `restore` with no arguments (jump to the newest snapshot)
    Rollback {
        #[arg(long)]
        db: Option<String>,
        #[arg(long)]
        no_safety_snapshot: bool,
        /// Deliberately operate on a non-local host (safety guard override)
        #[arg(long)]
        allow_remote: bool,
        #[arg(long)]
        force: bool,
    },
    /// List all saved snapshots
    List {
        #[arg(long)]
        db: Option<String>,
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Show binding, engine info and snapshot storage usage
    Status {
        #[arg(long)]
        db: Option<String>,
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Compare schema and row counts between two SQLite snapshots
    Diff {
        /// Older snapshot ID or tag
        from: String,
        /// Newer snapshot ID or tag
        to: String,
        #[arg(long)]
        db: Option<String>,
    },
    /// Protect a snapshot from pruning
    Pin {
        tag_or_id: String,
        #[arg(long)]
        db: Option<String>,
    },
    /// Remove snapshot pruning protection
    Unpin {
        tag_or_id: String,
        #[arg(long)]
        db: Option<String>,
    },
    /// Delete old snapshots, keeping only the newest N
    Prune {
        /// How many snapshots to keep
        #[arg(long, default_value_t = 10)]
        keep: usize,
        /// Delete every snapshot (keep 0)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        db: Option<String>,
    },
    /// Run the MCP (Model Context Protocol) stdio server for AI agents
    Mcp {
        #[arg(long)]
        db: Option<String>,
        /// Enable destructive MCP tools for this server session
        #[arg(long)]
        allow_destructive: bool,
    },
}
