use std::fs;
use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};

use crate::cli::{DbCommand, DbResetCommand};
use crate::config;

const DB_FILENAME: &str = "tx.sqlite3";

/// Execute database maintenance commands.
///
/// # Errors
///
/// Returns an error if configuration loading or file removal fails.
pub fn run(cmd: &DbCommand, config_dir: Option<&Path>, quiet: bool) -> Result<()> {
    match cmd {
        DbCommand::Reset(cmd) => reset(config_dir, quiet, cmd),
    }
}

fn reset(config_dir: Option<&Path>, quiet: bool, cmd: &DbResetCommand) -> Result<()> {
    if !cmd.yes {
        return Err(eyre!(
            "refusing to delete database without --yes confirmation"
        ));
    }

    let loaded = config::load(config_dir)?;
    let db_path = loaded.directories.data_dir.join(DB_FILENAME);
    let candidates = [
        db_path.clone(),
        db_path.with_extension("sqlite3-wal"),
        db_path.with_extension("sqlite3-shm"),
    ];

    let mut removed = Vec::new();
    for path in candidates {
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
            removed.push(path);
        }
    }

    if quiet {
        return Ok(());
    }

    if removed.is_empty() {
        println!("Database not found at {}", db_path.display());
    } else {
        println!("Deleted database files:");
        for path in removed {
            println!("  {}", path.display());
        }
    }

    Ok(())
}
