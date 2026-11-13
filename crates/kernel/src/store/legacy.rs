//! One-time import from the former document store, inside the schema transaction.
use super::{db, insert_sql, Table};
use crate::{Error, Result};
use rusqlite::{params_from_iter, Connection};
use serde::de::DeserializeOwned;

pub(super) fn import<T: Table + DeserializeOwned>(conn: &Connection) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'hk_documents')",
        [], |r| r.get(0)
    ).map_err(db)?;
    if !exists {
        return Ok(());
    }
    {
        let mut source = conn
            .prepare("SELECT id, body FROM hk_documents WHERE collection = ? ORDER BY rowid")
            .map_err(db)?;
        let mut rows = source.query([T::NAME]).map_err(db)?;
        let mut insert = conn.prepare(&insert_sql::<T>()).map_err(db)?;
        while let Some(row) = rows.next().map_err(db)? {
            let id: String = row.get(0).map_err(db)?;
            let body: String = row.get(1).map_err(db)?;
            let record: T = serde_json::from_str(&body)
                .map_err(|_| Error::new("migration", "Invalid legacy record"))?;
            if record.legacy_id() != id {
                return Err(Error::new("migration", "Legacy primary key mismatch"));
            }
            // Plain INSERT deliberately rejects collisions with an existing relational row.
            insert
                .execute(params_from_iter(record.values()))
                .map_err(db)?;
        }
    }
    conn.execute("DELETE FROM hk_documents WHERE collection = ?", [T::NAME])
        .map_err(db)?;
    let empty: bool = conn
        .query_row("SELECT NOT EXISTS(SELECT 1 FROM hk_documents)", [], |r| {
            r.get(0)
        })
        .map_err(db)?;
    if empty {
        conn.execute("DROP TABLE hk_documents", []).map_err(db)?;
    }
    Ok(())
}
