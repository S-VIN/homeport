use crate::store::{
    ColumnType, PrimaryKey,
    rusqlite::{
        self, ToSql,
        types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef},
    },
};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// Stable record identity.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Id(pub Uuid);
impl Id {
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
    /// Deterministic child id (same parent + bytes → same id).
    pub fn child(self, bytes: &[u8]) -> Self {
        Self(Uuid::new_v5(&self.0, bytes))
    }
    pub const NIL: Self = Self(Uuid::nil());
}
impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for Id {
    type Err = uuid::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}
impl ColumnType for Id {
    const SQL_TYPE: &'static str = "TEXT";
}
impl ToSql for Id {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}
impl FromSql for Id {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        v.as_str()?.parse().map_err(|_| FromSqlError::InvalidType)
    }
}
impl PrimaryKey for Id {
    type Lookup = Self;
}

/// Bytes from the system's secure random source (keys, secrets, ids).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0; N];
    getrandom::fill(&mut b).expect("system randomness");
    b
}
/// Lowercase hex, e.g. for short ids and digests.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Unix seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Store a `Display + FromStr` type as a TEXT column.
#[macro_export]
macro_rules! sql_text {
    ($ty:ty) => {
        impl $crate::store::ColumnType for $ty {
            const SQL_TYPE: &'static str = "TEXT";
        }
        impl $crate::store::rusqlite::ToSql for $ty {
            fn to_sql(
                &self,
            ) -> $crate::store::rusqlite::Result<$crate::store::rusqlite::types::ToSqlOutput<'_>>
            {
                Ok(self.to_string().into())
            }
        }
        impl $crate::store::rusqlite::types::FromSql for $ty {
            fn column_result(
                v: $crate::store::rusqlite::types::ValueRef<'_>,
            ) -> $crate::store::rusqlite::types::FromSqlResult<Self> {
                v.as_str()?
                    .parse()
                    .map_err(|_| $crate::store::rusqlite::types::FromSqlError::InvalidType)
            }
        }
    };
}

/// A closed set of labelled values: `Display`, `FromStr`, `ALL` and a TEXT column.
#[macro_export]
macro_rules! choices {
    ($(#[$m:meta])* $vis:vis enum $name:ident { $($item:ident => $label:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        $vis enum $name { $(#[serde(rename = $label)] $item),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$item),+];
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(match self { $(Self::$item => $label),+ })
            }
        }
        impl std::str::FromStr for $name {
            type Err = ();
            fn from_str(s: &str) -> std::result::Result<Self, ()> {
                match s { $($label => Ok(Self::$item),)+ _ => Err(()) }
            }
        }
        $crate::sql_text!($name);
    };
}
