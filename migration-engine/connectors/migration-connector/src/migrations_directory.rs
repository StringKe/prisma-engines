//! Migrations directory management.
//!
//! This module is responsible for the management of the contents of the
//! migrations directory. At the top level it contains a migration_lock.toml file which lists the provider.
//! It also contains multiple subfolders, named after the migration id, and each containing:
//! - A migration script

use crate::{checksum, ConnectorError, ConnectorResult};
use std::{
    error::Error,
    fmt::Display,
    fs::{read_dir, DirEntry},
    io::{self, Write as _},
    path::{Path, PathBuf},
};
use tracing_error::SpanTrace;
use user_facing_errors::migration_engine::ProviderSwitchedError;

/// The file name for migration scripts, not including the file extension.
pub const MIGRATION_SCRIPT_FILENAME: &str = "migration";

/// The file name for the migration lock file, not including the file extension.
pub const MIGRATION_LOCK_FILENAME: &str = "migration_lock";

/// Create a directory for a new migration.
pub fn create_migration_directory(
    migrations_directory_path: &Path,
    migration_name: &str,
) -> io::Result<MigrationDirectory> {
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let directory_name = format!(
        "{timestamp}_{migration_name}",
        timestamp = timestamp,
        migration_name = migration_name
    );
    let directory_path = migrations_directory_path.join(directory_name);

    if directory_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "The migration directory already exists at {}",
                directory_path.to_string_lossy()
            ),
        ));
    }

    std::fs::create_dir_all(&directory_path)?;

    Ok(MigrationDirectory { path: directory_path })
}

/// Write the migration_lock file to the directory.
#[tracing::instrument]
pub fn write_migration_lock_file(migrations_directory_path: &str, provider: &str) -> std::io::Result<()> {
    let directory_path = Path::new(migrations_directory_path);
    let mut file_path = directory_path.join(MIGRATION_LOCK_FILENAME);

    file_path.set_extension("toml");

    tracing::debug!("Writing migration lockfile at {:?}", &file_path);

    let mut file = std::fs::File::create(&file_path)?;
    let content = format!(
        r##"# Please do not edit this file manually
# It should be added in your version-control system (i.e. Git)
provider = "{}""##,
        provider
    );

    file.write_all(content.as_bytes())?;

    Ok(())
}

/// Error if the provider in the schema does not match the one in the schema_lock.toml
#[tracing::instrument]
pub fn error_on_changed_provider(migrations_directory_path: &str, provider: &str) -> ConnectorResult<()> {
    match match_provider_in_lock_file(migrations_directory_path, provider) {
        None => Ok(()),
        Some(Err(expected_provider)) => Err(ConnectorError::user_facing(ProviderSwitchedError {
            provider: provider.into(),
            expected_provider,
        })),
        Some(Ok(())) => Ok(()),
    }
}

/// Check whether provider matches.
#[tracing::instrument]
fn match_provider_in_lock_file(migrations_directory_path: &str, provider: &str) -> Option<Result<(), String>> {
    let directory_path = Path::new(migrations_directory_path);
    let file_path = directory_path.join("migration_lock.toml");

    std::fs::read_to_string(file_path).ok().map(|content| {
        let found_provider = content
            .lines()
            .find(|line| line.starts_with("provider"))
            .map(|line| line.trim_start_matches("provider = ").trim_matches('"'))
            .unwrap_or("<PROVIDER NOT FOUND>");

        if found_provider == provider {
            Ok(())
        } else {
            Err(found_provider.to_owned())
        }
    })
}

/// An IO error that occurred while reading the migrations directory.
#[derive(Debug)]
pub struct ListMigrationsError(io::Error);

impl Display for ListMigrationsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("An error occurred when reading the migrations directory.")
    }
}

impl Error for ListMigrationsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

impl From<io::Error> for ListMigrationsError {
    fn from(err: io::Error) -> Self {
        ListMigrationsError(err)
    }
}

/// List the migrations present in the migration directory, lexicographically sorted by name.
///
/// If the migrations directory does not exist, it will not error but return an empty Vec.
pub fn list_migrations(migrations_directory_path: &Path) -> Result<Vec<MigrationDirectory>, ListMigrationsError> {
    let mut entries: Vec<MigrationDirectory> = Vec::new();

    let read_dir_entries = match read_dir(migrations_directory_path) {
        Ok(read_dir_entries) => read_dir_entries,
        Err(err) if matches!(err.kind(), std::io::ErrorKind::NotFound) => return Ok(entries),
        Err(err) => return Err(err.into()),
    };

    for entry in read_dir_entries {
        let entry = entry?;

        if entry.file_type()?.is_dir() {
            entries.push(entry.into());
        }
    }

    entries.sort_by(|a, b| a.migration_name().cmp(b.migration_name()));

    Ok(entries)
}

/// Proxy to a directory containing one migration, as returned by
/// `create_migration_directory` and `list_migrations`.
#[derive(Debug, Clone)]
pub struct MigrationDirectory {
    path: PathBuf,
}

/// Error while reading a migration script.
#[derive(Debug)]
pub struct ReadMigrationScriptError(pub(crate) io::Error, pub(crate) SpanTrace, pub(crate) String);

impl ReadMigrationScriptError {
    fn new(err: io::Error, file_path: &Path) -> Self {
        ReadMigrationScriptError(err, SpanTrace::capture(), file_path.to_string_lossy().into_owned())
    }
}

impl Display for ReadMigrationScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Failed to read migration script at ")?;
        Display::fmt(&self.2, f)
    }
}

impl Error for ReadMigrationScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

impl MigrationDirectory {
    /// Initialize a MigrationDirectory at the provided path. This will not
    /// validate that the path is valid and exists.
    pub fn new(path: PathBuf) -> MigrationDirectory {
        MigrationDirectory { path }
    }

    /// The `{timestamp}_{name}` formatted migration name.
    pub fn migration_name(&self) -> &str {
        self.path
            .file_name()
            .expect("MigrationDirectory::migration_id")
            .to_str()
            .expect("Migration directory name is not valid UTF-8.")
    }

    /// Check whether the checksum of the migration script matches the provided one.
    #[tracing::instrument]
    pub fn matches_checksum(&self, checksum_str: &str) -> Result<bool, ReadMigrationScriptError> {
        let filesystem_script = self.read_migration_script()?;
        Ok(checksum::script_matches_checksum(&filesystem_script, checksum_str))
    }

    /// Write the migration script to the directory.
    #[tracing::instrument]
    pub fn write_migration_script(&self, script: &str, extension: &str) -> std::io::Result<()> {
        let mut path = self.path.join(MIGRATION_SCRIPT_FILENAME);

        path.set_extension(extension);

        tracing::debug!("Writing migration script at {:?}", &path);

        let mut file = std::fs::File::create(&path)?;
        file.write_all(script.as_bytes())?;

        Ok(())
    }

    /// Read the migration script to a string.
    #[tracing::instrument]
    pub fn read_migration_script(&self) -> Result<String, ReadMigrationScriptError> {
        let path = self.path.join("migration.sql"); // todo why is it hardcoded here?
        std::fs::read_to_string(&path).map_err(|ioerr| ReadMigrationScriptError::new(ioerr, &path))
    }

    /// The filesystem path to the directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl From<DirEntry> for MigrationDirectory {
    fn from(entry: DirEntry) -> MigrationDirectory {
        MigrationDirectory { path: entry.path() }
    }
}
