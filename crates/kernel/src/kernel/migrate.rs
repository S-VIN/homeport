//! Tables: the migration scripts of the kernel and the plugins, and reading
//! every persisted state at start.
use super::*;

/// The kernel's own scripts (crates/kernel/migrations).
const SCRIPTS: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// Run the scripts of the kernel and of `plugins` that have not run in this
/// database yet, in name order, each in its own transaction. `kernel_migrations`
/// records `<plugin>/<file>` of every script that ran.
pub(crate) fn migrate(store: &Store, plugins: &[&'static Plugin]) -> Result<()> {
    let owners = std::iter::once(("kernel", "homeport", SCRIPTS))
        .chain(plugins.iter().map(|p| (p.id, p.name, p.migrations)));
    let mut applied = Vec::new();
    for (owner, name, scripts) in owners {
        for (file, sql) in scripts {
            let id = format!("{owner}/{file}");
            let fresh = store
                .transaction(|tx| {
                    if ran(tx, &id)? {
                        return Ok(false);
                    }
                    tx.sql().execute_batch(sql)?;
                    tx.put(&crate::state::MigrationRow {
                        id: id.clone(),
                        at: crate::now() as i64,
                    })?;
                    Ok(true)
                })
                .map_err(|e| Error::msg(format!("{name}: migration {file} failed: {e}")))?;
            if fresh && !applied.contains(&name) {
                applied.push(name);
            }
        }
    }
    if !applied.is_empty() {
        log::info!("Database tables updated for {}", applied.join(", "));
    }
    Ok(())
}

/// The script ran here (before the kernel's first one there is no journal).
fn ran(tx: &crate::store::Tx<'_>, id: &str) -> Result<bool> {
    let journal: bool = tx.sql().query_row(
        "SELECT count(*) > 0 FROM sqlite_schema WHERE type = 'table' AND name = 'kernel_migrations'",
        [],
        |r| r.get(0),
    )?;
    Ok(journal && tx.get::<crate::state::MigrationRow>(id)?.is_some())
}

/// Read every persisted state. A setting that no longer reads (an older or
/// damaged value) starts from its default instead of stopping the app, with a
/// warning; import validation (`strict`) refuses it.
pub(super) fn load_cells(
    store: &Store,
    reg: &mut Registry,
    strict: bool,
) -> Result<HashMap<&'static str, Cell>> {
    let mut warnings = Vec::new();
    let cells = store.transaction(|tx| {
        let mut cells = HashMap::new();
        for (name, plugin, p) in &reg.persist {
            let value = match ((p.load)(tx), &p.fallback) {
                (Ok(value), _) => value,
                (Err(e), Some(default)) if !strict && e.code == "setting" => {
                    set_aside(tx, name)?;
                    let owner = plugin.map_or("homeport", |i| reg.plugins[i].name);
                    let warning = format!("{owner}: the saved setting {name} could not be read, so its default is used.");
                    warnings.push(("Settings", warning));
                    default()
                }
                (Err(e), _) => return Err(e),
            };
            cells.insert(*name, Cell { value });
        }
        Ok(cells)
    })?;
    reg.warnings.extend(warnings);
    Ok(cells)
}

/// Keep an unreadable setting as `<name>.broken` and drop it, so the profile
/// still exports and imports cleanly.
fn set_aside(tx: &crate::store::Tx<'_>, name: &str) -> Result<()> {
    if let Some(row) = tx.get::<SettingRow>(name)? {
        tx.put(&SettingRow {
            id: format!("{name}.broken"),
            json: row.json,
        })?;
        tx.delete::<SettingRow>(name)?;
    }
    Ok(())
}
