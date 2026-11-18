use super::{Table, db};
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
}
impl Column {
    pub const fn of<T: ColumnType>(name: &'static str) -> Self {
        Self {
            name,
            sql_type: T::SQL_TYPE,
            nullable: T::NULLABLE,
        }
    }
}

/// The table, created by migration scripts, has exactly the columns of `T`:
/// same names, types, nullability and primary key.
pub(super) fn verify<T: Table>(conn: &Connection, name: &str) -> Result<()> {
    let existing = columns_of(conn, name)?;
    let wrong = |what: String| {
        Error::new(
            "schema",
            format!("Table {name} {what}: change it with a migration script."),
        )
    };
    if existing.is_empty() {
        return Err(wrong("does not exist".into()));
    }
    for (column, ty, required, pk) in &existing {
        let Some(c) = T::COLUMNS.iter().find(|c| c.name == column) else {
            return Err(wrong(format!("has column {column}, which its type lacks")));
        };
        let key = i32::from(c.name == T::KEY);
        if ty != c.sql_type || *required == c.nullable || *pk != key {
            let null = if c.nullable { "" } else { " NOT NULL" };
            let primary = if key == 1 { " PRIMARY KEY" } else { "" };
            return Err(wrong(format!(
                "declares {column} differently from its type ({} {}{null}{primary})",
                column, c.sql_type
            )));
        }
    }
    if let Some(c) = T::COLUMNS
        .iter()
        .find(|c| !existing.iter().any(|(n, ..)| n == c.name))
    {
        return Err(wrong(format!("lacks column {}", c.name)));
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
