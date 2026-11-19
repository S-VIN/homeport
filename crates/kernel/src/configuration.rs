//! Explicit configuration exchange. Temporary files are private and removed on drop;
//! no scheduled copies or backup directory are created.
use crate::{Error, Host, Kernel, Plugin, Result, store::Store};
use std::{io::Write, path::Path, sync::Arc};

pub const MAX_BYTES: usize = 32 * 1024 * 1024;
pub const FILE_NAME: &str = "homeport-config.sqlite3";

#[derive(Clone, Copy)]
pub enum FileAction {
    Export,
    Import,
}

pub struct Import {
    pub name: String,
    pub size: usize,
    pub(crate) file: tempfile::NamedTempFile,
}

impl Host {
    /// Save beside the chosen destination, then atomically replace it. Never
    /// overwrite an installation's live database, lock, identity or sidecars.
    pub fn export_configuration_to(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = parent.canonicalize().map_err(io)?;
        let live = self.dir.join(crate::host::DATABASE);
        if parent.starts_with(self.dir.canonicalize().map_err(io)?)
            || (path.exists() && same_file::is_same_file(path, &live).map_err(io)?)
        {
            crate::bail!("Choose a location outside homeport's working data directory.");
        }
        let bytes = self.export_configuration()?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
        file.write_all(&bytes).map_err(io)?;
        file.as_file().sync_all().map_err(io)?;
        file.persist(path).map_err(|e| io(e.error))?;
        Ok(())
    }

    /// All plugin settings and lists in one consistent, portable file.
    pub fn export_configuration(&self) -> Result<Vec<u8>> {
        let dir = tempfile::tempdir().map_err(io)?;
        let path = dir.path().join(FILE_NAME);
        self.store.backup_to(&path)?;
        if std::fs::metadata(&path).map_err(io)?.len() > MAX_BYTES as u64 {
            crate::bail!("Configuration exceeds the 32 MiB file limit.");
        }
        std::fs::read(path).map_err(io)
    }

    /// Validate before showing the replacement confirmation. Never touches the live database.
    pub fn prepare_import(&self, name: String, bytes: &[u8]) -> Result<Arc<Import>> {
        if bytes.len() > MAX_BYTES {
            crate::bail!("Configuration exceeds the 32 MiB file limit.");
        }
        if !bytes.starts_with(b"SQLite format 3\0") {
            crate::bail!("Choose a configuration exported by Homeport (.sqlite3).");
        }
        let mut file = tempfile::NamedTempFile::new().map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        Store::check_configuration(file.path())?;
        let candidate = Store::open(file.path())?;
        // Plugins may create files in their data folders while they register:
        // keep the real ones untouched.
        let scratch = tempfile::tempdir().map_err(io)?;
        Kernel::load_configuration(&self.catalog, &candidate, scratch.path(), true)
            .map_err(|_| Error::msg("This configuration is damaged or incompatible with this version of Homeport. Your settings have not changed."))?;
        let active = Kernel::load_configuration(&self.catalog, &candidate, scratch.path(), false)?;
        crate::core::compose(&active.reg, &active)?;
        Ok(Arc::new(Import {
            name,
            size: bytes.len(),
            file,
        }))
    }
}

pub(crate) fn io(e: std::io::Error) -> Error {
    Error::msg(e.to_string())
}

/// Create `database` (which must not exist yet) for a new installation: the
/// migration scripts of the kernel and of every plugin in `catalog` that runs
/// here, then the `preset` script in one transaction. Nothing is created when
/// the preset fails.
pub fn seed(catalog: &[&'static Plugin], preset: &str, database: &Path) -> Result<()> {
    let parent = database.parent().unwrap_or_else(|| Path::new("."));
    let file = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    {
        let store = Store::open(file.path())?;
        let plugins: Vec<_> = catalog.iter().copied().filter(|p| p.supported()).collect();
        crate::kernel::migrate(&store, &plugins)?;
        store
            .transaction(|tx| Ok(tx.sql().execute_batch(preset)?))
            .map_err(|e| Error::msg(format!("The preset failed: {}", e.message)))?;
    }
    file.persist_noclobber(database).map_err(|e| io(e.error))?;
    Ok(())
}
