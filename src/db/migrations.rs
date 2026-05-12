use anyhow::{Context, Result};
use rusqlite::{Connection, params};

const MIGRATIONS: &[(&str, &str)] = &[
    (
        "001_initial",
        include_str!("../../migrations/001_initial.sql"),
    ),
    (
        "002_symbols",
        include_str!("../../migrations/002_symbols.sql"),
    ),
    (
        "003_relationships",
        include_str!("../../migrations/003_relationships.sql"),
    ),
    ("004_fts", include_str!("../../migrations/004_fts.sql")),
];

pub fn apply(conn: &mut Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )?;

    let tx = conn.transaction()?;
    for (version, sql) in MIGRATIONS {
        let already_applied: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            params![version],
            |row| row.get(0),
        )?;
        if !already_applied {
            tx.execute_batch(sql)
                .with_context(|| format!("database migration failed: {version}"))?;
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                params![version],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
