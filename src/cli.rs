use crate::engine::SaveMethod;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "dbsnap",
    version,
    about = "Instant database time-machine for AI agents & vibe coders",
    long_about = "dbsnap saves and restores snapshots of local development databases.\nSQLite uses page backups, PostgreSQL supports clones and dumps, and\nMySQL/MariaDB uses logical dumps. Includes an MCP stdio server.\nMySQL restores require stopped writers and are not atomic."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Bounded SQLite row-change counts by primary key, without printing row values
    DiffRows {
        from: String,
        to: String,
        #[arg(long)]
        table: String,
        #[arg(long, default_value_t = 10000)]
        limit: usize,
        #[arg(long)]
        db: Option<String>,
    },
    /// Capture and retain a live checkpoint, then compare it with a snapshot
    DiffLive {
        tag_or_id: String,
        #[arg(long)]
        db: Option<String>,
    },
    /// Fork SQLite/PostgreSQL data, or explicitly create an isolated MySQL Docker server
    Sandbox {
        tag_or_id: String,
        #[arg(long)]
        db: Option<String>,
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Explicit isolated Docker server for MySQL/MariaDB; never publishes ports
        #[arg(long, value_parser=["mysql:8.0","mariadb:11"])]
        docker_image: Option<String>,
    },
    /// Check snapshot artifact integrity without replaying or restoring SQL
    Verify {
        /// Omit to check all snapshots
        tag_or_id: Option<String>,
        #[arg(long)]
        db: Option<String>,
    },
    /// Diagnose connections, pending recovery and untracked staging files
    Doctor {
        #[arg(long)]
        db: Option<String>,
    },
    /// Resume the recovery recorded in the database restore journal
    Recover {
        #[arg(long)]
        db: Option<String>,
    },
    /// Save a checkpoint then run a command; never automatically restore on failure
    Exec {
        #[arg(long)]
        db: Option<String>,
        #[arg(required = true, last = true)]
        command: Vec<String>,
    },
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
        /// Skip the automatic safety snapshot (not supported for fresh MySQL restores)
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
    /// Compare snapshot schema and row counts (SQLite, PostgreSQL clones, MySQL/MariaDB)
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
