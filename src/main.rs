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
        Command::DiffRows {
            from,
            to,
            table,
            limit,
            db,
        } => ops::diff_rows_at(&root, db.as_deref(), &from, &to, &table, limit),
        Command::DiffLive { tag_or_id, db } => ops::diff_live_at(&root, db.as_deref(), &tag_or_id),
        Command::Sandbox {
            tag_or_id,
            db,
            output,
            docker_image,
        } => ops::sandbox_at(
            &root,
            db.as_deref(),
            &tag_or_id,
            output.as_deref(),
            docker_image.as_deref(),
        ),
        Command::Verify { tag_or_id, db } => {
            ops::verify_at(&root, db.as_deref(), tag_or_id.as_deref())
        }
        Command::Doctor { db } => ops::doctor_at(&root, db.as_deref()),
        Command::Recover { db } => ops::recover_at(&root, db.as_deref()),
        Command::Exec { db, command } => ops::exec_at(&root, db.as_deref(), &command),
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
        Command::List { db, json } => {
            if json {
                ops::list_json_at(&root, db.as_deref())
            } else {
                ops::list_at(&root, db.as_deref())
            }
        }
        Command::Status { db, json } => {
            if json {
                ops::status_json_at(&root, db.as_deref())
            } else {
                ops::status_at(&root, db.as_deref())
            }
        }
        Command::Diff { from, to, db } => ops::diff_at(&root, db.as_deref(), &from, &to),
        Command::Pin { tag_or_id, db } => ops::pin_at(&root, db.as_deref(), &tag_or_id, true),
        Command::Unpin { tag_or_id, db } => ops::pin_at(&root, db.as_deref(), &tag_or_id, false),
        Command::Prune { keep, all, db } => {
            ops::prune_at(&root, db.as_deref(), if all { 0 } else { keep })
        }
        Command::Mcp {
            db,
            allow_destructive,
        } => {
            return match mcp::run(db.as_deref(), allow_destructive) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!(
                        "{} {:#}",
                        "error:".red().bold(),
                        dbsnap::util::mask(&format!("{e:#}"))
                    );
                    1
                }
            };
        }
    };

    match result {
        Ok(message) => {
            println!("{}", dbsnap::util::mask(&message));
            0
        }
        Err(e) => {
            eprintln!(
                "{} {:#}",
                "error:".red().bold(),
                dbsnap::util::mask(&format!("{e:#}"))
            );
            1
        }
    }
}
