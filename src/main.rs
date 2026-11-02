use clap::Parser;
use colored::Colorize;
use dbsnap::cli::{Cli, Command};
use dbsnap::{mcp, ops};

fn main() {
    let cli = Cli::parse();
    std::process::exit(run(cli.command));
}

fn run(command: Command) -> i32 {
    let root = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "{} cannot determine current directory: {e}",
                "error:".red().bold()
            );
            return 1;
        }
    };

    let result = match command {
        Command::Init { db, allow_remote } => ops::init_at(&root, db.as_deref(), allow_remote),
        Command::Save {
            tag,
            db,
            method,
            allow_remote,
            force,
        } => ops::save_at(&root, db.as_deref(), tag, force, allow_remote, method),
        Command::Restore {
            tag_or_id,
            db,
            no_safety_snapshot,
            allow_remote,
            force,
        } => ops::restore_at(
            &root,
            db.as_deref(),
            tag_or_id,
            !no_safety_snapshot,
            force,
            allow_remote,
        ),
        Command::Rollback {
            db,
            no_safety_snapshot,
            allow_remote,
            force,
        } => ops::restore_at(
            &root,
            db.as_deref(),
            None,
            !no_safety_snapshot,
            force,
            allow_remote,
        ),
        Command::List { db } => ops::list_at(&root, db.as_deref()),
        Command::Status { db } => ops::status_at(&root, db.as_deref()),
        Command::Prune { keep, all, db } => {
            ops::prune_at(&root, db.as_deref(), if all { 0 } else { keep })
        }
        Command::Mcp { db } => {
            return match mcp::run(db.as_deref()) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{} {:#}", "error:".red().bold(), e);
                    1
                }
            };
        }
    };

    match result {
        Ok(message) => {
            println!("{message}");
            0
        }
        Err(e) => {
            eprintln!("{} {:#}", "error:".red().bold(), e);
            1
        }
    }
}
