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

/// A closed set of labelled values: `Display` (the label), `FromStr`, `ALL`
/// and a TEXT column. Saved data holds the stable value name (`Proxy`), so a
/// label can change freely; labels it had before are still read:
/// `Proxy => "Proxy" | "Via server"`.
#[macro_export]
macro_rules! choices {
    ($(#[$m:meta])* $vis:vis enum $name:ident { $($item:ident => $label:literal $(| $old:literal)*),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        $vis enum $name { $($item),+ }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$item),+];
            /// The name saved for this value.
            pub fn key(self) -> &'static str {
                match self { $(Self::$item => stringify!($item)),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(match self { $(Self::$item => $label),+ })
            }
        }
        impl std::str::FromStr for $name {
            type Err = ();
            fn from_str(s: &str) -> std::result::Result<Self, ()> {
                $(if s == stringify!($item) || s == $label $(|| s == $old)* {
                    return Ok(Self::$item);
                })+
                Err(())
            }
        }
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(self.key())
            }
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                let s = <std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(d)?;
                s.parse().map_err(|()| {
                    serde::de::Error::custom(format!("unknown {} {s:?}", stringify!($name)))
                })
            }
        }
        impl $crate::store::ColumnType for $name {
            const SQL_TYPE: &'static str = "TEXT";
        }
        impl $crate::store::rusqlite::ToSql for $name {
            fn to_sql(
                &self,
            ) -> $crate::store::rusqlite::Result<$crate::store::rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.key().into())
            }
        }
        impl $crate::store::rusqlite::types::FromSql for $name {
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

#[cfg(test)]
mod tests {
    use crate::store::rusqlite::Connection;

    crate::choices! {
        enum Route { Proxy => "Proxy" | "Via server" }
    }

    #[test]
    fn choices_save_the_name_and_read_old_labels() {
        let read = |json: &str| serde_json::from_str::<Route>(json).unwrap();
        assert_eq!(read("\"Via server\""), Route::Proxy);
        assert_eq!(read("\"Proxy\""), Route::Proxy);
        assert_eq!(Route::ALL, [Route::Proxy]);
        assert_eq!(serde_json::to_string(&Route::Proxy).unwrap(), "\"Proxy\"");
        let db = Connection::open_in_memory().unwrap();
        let old: Route = db
            .query_row("SELECT 'Via server'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old, Route::Proxy);
        let saved: String = db
            .query_row("SELECT ?1", [Route::Proxy], |r| r.get(0))
            .unwrap();
        assert_eq!(saved, "Proxy");
    }
}
