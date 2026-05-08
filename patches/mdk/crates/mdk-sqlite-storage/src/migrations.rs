use rusqlite::Connection;

use crate::error::Error;

// Embed the SQL migrations
refinery::embed_migrations!("migrations");

/// Run database migrations to set up or upgrade the database schema.
/// We use a custom migration table name to avoid conflicts with migrations from the OpenMls SqliteStorage crate.
///
/// # Arguments
///
/// * `conn` - The SQLite database connection.
///
/// # Returns
///
/// Result indicating success or failure of the migration process.
pub fn run_migrations(conn: &mut Connection) -> Result<(), Error> {
    // We use this custom migration table name for legacy reasons
    // As the code used to be part of the rust-nostr project
    // and we need to keep the same migration table name for backwards compatibility
    let migration_table_name = "_refinery_schema_history_nostr_mls";
    let report = migrations::runner()
        .set_migration_table_name(migration_table_name)
        .run(conn)?;

    // Log the results
    for migration in report.applied_migrations() {
        tracing::info!(
            "Applied migration: {} (version: {})",
            migration.name(),
            migration.version()
        );
    }

    Ok(())
}
