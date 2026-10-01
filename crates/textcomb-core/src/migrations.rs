use futures::future::BoxFuture;
use sqlx_core::{
    error::BoxDynError,
    migrate::{MigrateError, Migration, MigrationSource, Migrator},
};

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

#[derive(Debug)]
struct EmbeddedMigrations;

impl MigrationSource<'static> for EmbeddedMigrations {
    fn resolve(self) -> BoxFuture<'static, Result<Vec<Migration>, BoxDynError>> {
        Box::pin(async { Ok(embedded_migrations()) })
    }
}

pub(crate) async fn migrator() -> Result<Migrator, MigrateError> {
    // SQLx retains its default migration locking, transactions and checksum
    // validation. Runtime containers do not need the source SQL directory.
    Migrator::new(EmbeddedMigrations).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Connection, PgConnection};
    use std::path::Path;
    use uuid::Uuid;

    #[tokio::test]
    async fn embedded_migrations_match_sqlx_directory_resolution() {
        let embedded = migrator().await.expect("embedded migrations");
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let from_directory = Migrator::new(directory)
            .await
            .expect("SQLx directory resolver");
        assert_eq!(embedded.iter().count(), from_directory.iter().count());
        for (actual, expected) in embedded.iter().zip(from_directory.iter()) {
            assert_eq!(actual.version, expected.version);
            assert_eq!(actual.description, expected.description);
            assert_eq!(actual.migration_type, expected.migration_type);
            assert_eq!(actual.sql, expected.sql);
            assert_eq!(actual.checksum, expected.checksum);
            assert_eq!(actual.no_tx, expected.no_tx);
        }
    }

    #[tokio::test]
    async fn embedded_migrations_preserve_existing_history_and_reject_drift() {
        let Ok(database_url) = std::env::var("TEXTCOMB_TEST_DATABASE_URL") else {
            return;
        };
        let mut connection = PgConnection::connect(&database_url)
            .await
            .expect("test PostgreSQL connection");
        // The identifier contains only a fixed prefix and generated hex digits.
        // Isolate migration history from other integration tests and user tables.
        let schema = format!("migration_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&mut connection)
            .await
            .expect("isolated migration schema");
        sqlx::query(&format!("SET search_path TO {schema}"))
            .execute(&mut connection)
            .await
            .expect("isolated search path");
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        Migrator::new(directory)
            .await
            .expect("SQLx directory resolver")
            .run(&mut connection)
            .await
            .expect("existing SQLx migration history");
        let embedded = migrator().await.expect("embedded migrations");
        for _ in 0..2 {
            embedded
                .run(&mut connection)
                .await
                .expect("unchanged checksums and idempotent migrations");
        }
        let first = embedded.iter().next().expect("at least one migration");
        sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = decode('00', 'hex') WHERE version = $1",
        )
        .bind(first.version)
        .execute(&mut connection)
        .await
        .expect("synthetic checksum drift");
        assert!(matches!(
            embedded.run(&mut connection).await,
            Err(MigrateError::VersionMismatch(version)) if version == first.version
        ));
        sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2")
            .bind(first.checksum.to_vec())
            .bind(first.version)
            .execute(&mut connection)
            .await
            .expect("restore synthetic checksum");
        sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (999999, 'synthetic unknown version', true, decode('00', 'hex'), 0)")
            .execute(&mut connection)
            .await
            .expect("synthetic unknown version");
        assert!(matches!(
            embedded.run(&mut connection).await,
            Err(MigrateError::VersionMissing(999999))
        ));
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&mut connection)
            .await
            .expect("remove isolated migration schema");
    }
}
