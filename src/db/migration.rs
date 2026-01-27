use std::path::Path;

use crate::error::{DbError, DbResult};

/// Migrate legacy database (db/index.db) to new source-specific format
pub fn migrate_legacy_database() -> DbResult<bool> {
    let legacy_path = Path::new("db/index.db");
    let new_path = Path::new("db/wandering-inn.db");

    // Check if legacy database exists and new one doesn't
    if !legacy_path.exists() {
        return Ok(false);
    }

    if new_path.exists() {
        println!("New database already exists, skipping migration");
        return Ok(false);
    }

    println!("Migrating legacy database from db/index.db to db/wandering-inn.db");

    // Simply rename the file
    std::fs::rename(legacy_path, new_path).map_err(|e| DbError::Migration(e.to_string()))?;

    // Open the renamed database and add source metadata
    let conn = rusqlite::Connection::open(new_path)?;

    // Add source metadata table if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS source_metadata(
            key TEXT PRIMARY KEY,
            value TEXT
        )",
        (),
    )?;

    // Insert source ID
    conn.execute(
        "INSERT OR REPLACE INTO source_metadata(key, value) VALUES ('source_id', 'wandering-inn')",
        (),
    )?;

    println!("Migration complete");
    Ok(true)
}

/// Check if a legacy database exists that needs migration
pub fn needs_migration() -> bool {
    let legacy_path = Path::new("db/index.db");
    let new_path = Path::new("db/wandering-inn.db");

    legacy_path.exists() && !new_path.exists()
}
