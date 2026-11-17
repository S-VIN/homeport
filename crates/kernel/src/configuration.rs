//! Explicit configuration exchange. Temporary files are private and removed on drop;
//! no scheduled copies or backup directory are created.
use crate::{
    Error, Host, Kernel, Record, Result, State,
    state::{OrderRow, PluginRow, SettingRow},
    store::Store,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{io::Write, path::Path, sync::Arc};

pub const MAX_BYTES: usize = 32 * 1024 * 1024;
pub const FILE_NAME: &str = "rproxy-config.sqlite3";

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
            crate::bail!("Choose a location outside rproxy's working data directory.");
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
            crate::bail!("Choose a configuration exported by rproxy (.sqlite3).");
        }
        let mut file = tempfile::NamedTempFile::new().map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        Store::check_configuration(file.path())?;
        let candidate = Store::open(file.path())?;
        // Plugins may create files in their data folders while they register:
        // keep the real ones untouched.
        let scratch = tempfile::tempdir().map_err(io)?;
        Kernel::load_configuration(&self.catalog, &candidate, scratch.path(), true)
            .map_err(|_| Error::msg("This configuration is damaged or incompatible with this version of rproxy. Your settings have not changed."))?;
        let active = Kernel::load_configuration(&self.catalog, &candidate, scratch.path(), false)?;
        crate::core::compose(&active.reg, &active)?;
        Ok(Arc::new(Import {
            name,
            size: bytes.len(),
            file,
        }))
    }
}

/// A new configuration for another rproxy installation (a server being set
/// up), in the format of [`Host::export_configuration`]. It holds only what
/// is put in; everything else starts with its defaults there.
///
/// ```ignore
/// let bytes = Profile::new()?.enable("vless-inbound")?.setting(&CONFIG, &settings)?.finish()?;
/// ```
pub struct Profile {
    store: Store,
    dir: tempfile::TempDir,
}
impl Profile {
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(io)?;
        let store = Store::open(dir.path().join("profile.sqlite3"))?;
        store.register::<SettingRow>()?;
        store.register::<PluginRow>()?;
        store.register::<OrderRow>()?;
        Ok(Self { store, dir })
    }
    /// Turn on a plugin that is off by default.
    pub fn enable(self, plugin: &str) -> Result<Self> {
        let row = PluginRow {
            id: plugin.into(),
            enabled: true,
        };
        self.store.transaction(|tx| tx.put(&row))?;
        Ok(self)
    }
    /// The value of a setting (`Setup::setting` / `editable_setting`).
    pub fn setting<T: Serialize>(self, state: &State<T>, value: &T) -> Result<Self> {
        let row = SettingRow {
            id: state.name.into(),
            json: serde_json::to_string(value).map_err(|e| Error::msg(e.to_string()))?,
        };
        self.store.transaction(|tx| tx.put(&row))?;
        Ok(self)
    }
    /// The items of a list (`Setup::list` with its default table), in order.
    pub fn list<T: Record + DeserializeOwned>(
        self,
        state: &State<Vec<T>>,
        items: &[T],
    ) -> Result<Self> {
        self.store.register::<T>()?;
        self.store.transaction(|tx| {
            for (position, item) in items.iter().enumerate() {
                tx.put(item)?;
                tx.put(&OrderRow {
                    id: format!("{}|{}", state.name, item.id()),
                    list: state.name.into(),
                    position: position as u32,
                })?;
            }
            Ok(())
        })?;
        Ok(self)
    }
    pub fn finish(self) -> Result<Vec<u8>> {
        let path = self.dir.path().join(FILE_NAME);
        self.store.backup_to(&path)?;
        std::fs::read(path).map_err(io)
    }
}

pub(crate) fn io(e: std::io::Error) -> Error {
    Error::msg(e.to_string())
}
