use super::{Table, db, quoted};
use crate::{Error, Result};
use rusqlite::{Connection, ToSql, types::FromSql};

pub trait ColumnType: FromSql + ToSql {
    const SQL_TYPE: &'static str;
    const NULLABLE: bool = false;
}
pub trait PrimaryKey: ColumnType + std::fmt::Display {
    type Lookup: ToSql + ?Sized;
}

macro_rules! column_types {
    ($sql:literal => $($ty:ty),+) => {$(
        impl ColumnType for $ty { const SQL_TYPE: &'static str = $sql; }
    )+};
}
// INT, unlike INTEGER PRIMARY KEY, keeps the model's key independent of insertion order.
column_types!("INT" => i8, i16, i32, i64, u8, u16, u32, u64, bool);
column_types!("REAL" => f32, f64);
column_types!("TEXT" => String);
column_types!("BLOB" => Vec<u8>);
impl<T: ColumnType> ColumnType for Option<T> {
    const SQL_TYPE: &'static str = T::SQL_TYPE;
    const NULLABLE: bool = true;
}
macro_rules! integer_keys {
    ($($ty:ty),+) => {$(
        impl PrimaryKey for $ty { type Lookup = Self; }
    )+};
}
integer_keys!(i8, i16, i32, i64, u8, u16, u32, u64);
impl PrimaryKey for String {
    type Lookup = str;
}

pub struct Column {
    pub name: &'static str,
    pub sql_type: &'static str,
    pub nullable: bool,
    /// SQL literal for rows that existed before the column (`#[column(default)]`).
    pub default: Option<&'static str>,
}
impl Column {
    pub const fn of<T: ColumnType>(name: &'static str) -> Self {
        Self {
            name,
            sql_type: T::SQL_TYPE,
            nullable: T::NULLABLE,
            default: None,
        }
    }
    pub const fn with_default(mut self, default: &'static str) -> Self {
        self.default = Some(default);
        self
    }
    fn definition(&self, key: &str) -> String {
        let null = if self.nullable { "" } else { " NOT NULL" };
        let pk = if self.name == key { " PRIMARY KEY" } else { "" };
        let default = self
            .default
            .map(|d| format!(" DEFAULT {d}"))
            .unwrap_or_default();
        format!("{} {}{null}{default}{pk}", quoted(self.name), self.sql_type)
    }
}

/// Bring the table in line with `T`.
///
/// The lenient pass (before migrations) only adds new columns that need no
/// data (nullable or with a default) to a table that exists, and leaves
/// everything else to the migrations: a renamed table, a changed column, a
/// column that has to be filled. The strict pass (after them) creates missing
/// tables and refuses anything that still differs.
pub(super) fn ensure<T: Table>(conn: &Connection, name: &str, strict: bool) -> Result<()> {
    let table = quoted(name);
    let existing = columns_of(conn, name)?;
    if existing.is_empty() {
        if strict {
            let definitions: Vec<_> = T::COLUMNS.iter().map(|c| c.definition(T::KEY)).collect();
            conn.execute(
                &format!("CREATE TABLE {table} ({}) STRICT", definitions.join(", ")),
                [],
            )
            .map_err(db)?;
        }
        return Ok(());
    }
    let migration = |column: &str| {
        Error::new(
            "schema",
            format!("Column {table}.{column} requires an explicit migration"),
        )
    };
    if strict {
        for (name, ty, required, pk) in &existing {
            let column = T::COLUMNS
                .iter()
                .find(|c| c.name == name)
                .ok_or_else(|| migration(name))?;
            let key = i32::from(column.name == T::KEY);
            if ty != column.sql_type || *required == column.nullable || *pk != key {
                return Err(migration(name));
            }
        }
    }
    for column in T::COLUMNS
        .iter()
        .filter(|c| !existing.iter().any(|(n, ..)| n == c.name))
    {
        if column.nullable || column.default.is_some() {
            let sql = format!(
                "ALTER TABLE {table} ADD COLUMN {}",
                column.definition(T::KEY)
            );
            conn.execute(&sql, []).map_err(db)?;
        } else if strict {
            return Err(migration(column.name));
        }
    }
    Ok(())
}

/// `(name, type, not null, primary key)` of each column; empty when the table
/// does not exist.
fn columns_of(conn: &Connection, table: &str) -> Result<Vec<(String, String, bool, i32)>> {
    let mut stmt = conn
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?)")
        .map_err(db)?;
    stmt.query_map([table], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .map_err(db)?
    .collect::<rusqlite::Result<Vec<_>>>()
    .map_err(db)
}
