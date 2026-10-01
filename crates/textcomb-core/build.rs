use sqlx_core::migrate::{MigrationType, resolve_blocking};
use std::{env, error::Error, fmt::Write, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let directory = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?)
        .join("../../migrations")
        .canonicalize()?;
    println!("cargo:rerun-if-changed={}", directory.display());
    // Use the same resolver as SQLx's migrate! macro, retaining descriptions,
    // ordering and transaction directives without enabling unused drivers.
    let migrations = resolve_blocking(&directory)?;
    if migrations.is_empty() {
        return Err("embedded migrations must not be empty".into());
    }
    let mut previous = 0;
    let mut output =
        String::from("fn embedded_migrations() -> Vec<sqlx_core::migrate::Migration> { vec![\n");
    for (migration, path) in migrations {
        if migration.version <= previous || migration.migration_type != MigrationType::Simple {
            return Err("migrations must have unique positive versions and be forward-only".into());
        }
        previous = migration.version;
        let path = path.canonicalize()?;
        let path = path.to_str().ok_or("migration path must be UTF-8")?;
        println!("cargo:rerun-if-changed={path}");
        writeln!(
            output,
            "sqlx_core::migrate::Migration::new({}, std::borrow::Cow::Borrowed({:?}), sqlx_core::migrate::MigrationType::Simple, std::borrow::Cow::Borrowed(include_str!({:?})), {}),",
            migration.version, migration.description, path, migration.no_tx
        )?;
    }
    output.push_str("] }\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("migrations.rs"),
        output,
    )?;
    Ok(())
}
