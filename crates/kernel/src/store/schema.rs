use super::{db, quoted, Table};
use crate::{Error, Result};
use rusqlite::{types::FromSql, Connection, ToSql};

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
    fn definition(&self, key: &str) -> String {
        let null = if self.nullable { "" } else { " NOT NULL" };
        let pk = if self.name == key { " PRIMARY KEY" } else { "" };
        format!("{} {}{null}{pk}", quoted(self.name), self.sql_type)
    }
}

pub(super) fn ensure<T: Table>(conn: &Connection) -> Result<()> {
    let table = quoted(T::NAME);
    let definitions: Vec<_> = T::COLUMNS.iter().map(|c| c.definition(T::KEY)).collect();
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} ({}) STRICT",
            definitions.join(", ")
        ),
        [],
    )
    .map_err(db)?;

    let mut stmt = conn
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?)")
        .map_err(db)?;
    let existing = stmt
        .query_map([T::NAME], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, i32>(3)?,
            ))
        })
        .map_err(db)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db)?;
    let migration = |name: &str| {
        Error::new(
            "schema",
            format!("Column {}.{name} requires an explicit migration", T::NAME),
        )
    };
    for (name, ty, required, pk) in &existing {
        let column = T::COLUMNS
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| migration(name))?;
        if ty != column.sql_type
            || *required == column.nullable
            || *pk != i32::from(column.name == T::KEY)
        {
            return Err(migration(name));
        }
    }
    for column in T::COLUMNS
        .iter()
        .filter(|c| !existing.iter().any(|(n, ..)| n == c.name))
    {
        if !column.nullable {
            return Err(migration(column.name));
        }
        conn.execute(
            &format!(
                "ALTER TABLE {table} ADD COLUMN {}",
                column.definition(T::KEY)
            ),
            [],
        )
        .map_err(db)?;
    }
    Ok(())
}
